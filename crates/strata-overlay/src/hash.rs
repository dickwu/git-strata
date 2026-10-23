//! Content identity (§5.2): streaming BLAKE3 of a file or a symlink target,
//! git modes, and the race-safe read A1 → content → A2.

use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;

use strata_apfs::{Attrs, FileKind, IoSide, attrs_of, classify_io_error, open_rooted_file};
use strata_types::{BlobId, Code, RepoPath, StrataError};

use crate::Result;
use crate::fault;
use crate::overlay::{MODE_EXEC, MODE_FILE, MODE_LINK};

/// Files at least this large are hashed through mmap with rayon (store's rule).
const MMAP_MIN: u64 = 16 << 20;

fn blob_id(h: blake3::Hash) -> BlobId {
    BlobId::try_from(h.to_hex().to_string()).expect("blake3 hex is a blob id")
}

/// BLAKE3 of an open file and the number of bytes hashed.
pub(crate) fn hash_file(f: &File, shown: &Path) -> Result<(BlobId, u64)> {
    let len = f
        .metadata()
        .map_err(|e| classify_io_error(&e, IoSide::Read, shown))?
        .len();
    let mut h = blake3::Hasher::new();
    if len >= MMAP_MIN {
        // SAFETY: read-only map of a regular file; a concurrent writer is the
        // race §5.2 detects with A2, as store does for put.
        let m = unsafe { memmap2::Mmap::map(f) }
            .map_err(|e| classify_io_error(&e, IoSide::Read, shown))?;
        h.update_rayon(&m);
        return Ok((blob_id(h.finalize()), m.len() as u64));
    }
    let mut buf = vec![0u8; 256 * 1024];
    let mut off = 0u64;
    loop {
        let n = match f.read_at(&mut buf, off) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(classify_io_error(&e, IoSide::Read, shown)),
        };
        h.update(&buf[..n]);
        off += n as u64;
    }
    Ok((blob_id(h.finalize()), off))
}

pub(crate) fn hash_bytes(b: &[u8]) -> BlobId {
    let mut h = blake3::Hasher::new();
    if b.len() as u64 >= MMAP_MIN {
        h.update_rayon(b);
    } else {
        h.update(b);
    }
    blob_id(h.finalize())
}

/// git mode of a file or symlink; `None` for anything else.
pub(crate) fn git_mode(a: &Attrs) -> Option<u32> {
    match a.kind {
        FileKind::Symlink { .. } => Some(MODE_LINK),
        FileKind::File if a.mode & 0o100 != 0 => Some(MODE_EXEC),
        FileKind::File => Some(MODE_FILE),
        _ => None,
    }
}

/// setuid / setgid / sticky on a file, as octal permission text ("4755").
pub(crate) fn special_bits(a: &Attrs) -> Option<String> {
    (matches!(a.kind, FileKind::File) && a.mode & 0o7000 != 0)
        .then(|| format!("{:o}", a.mode & 0o7777))
}

pub(crate) fn is_special(k: &FileKind) -> bool {
    matches!(
        k,
        FileKind::Fifo | FileKind::Socket | FileKind::CharDevice | FileKind::BlockDevice
    )
}

/// The path is not (or no longer) an openable file of the expected shape.
pub(crate) fn is_shape_error(e: &StrataError) -> bool {
    matches!(
        e.code,
        Code::NoSuchPath | Code::IsDirectory | Code::NotFile | Code::NotDirectory
    )
}

/// An attribute-cache entry: `blob` is vouched for while the attrs are unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cached {
    pub attrs: Attrs,
    pub blob: BlobId,
}

#[derive(Clone, Debug)]
pub(crate) struct ReadOut {
    pub blob: BlobId,
    pub mode: u32,
    pub size: u64,
    /// The attrs to cache (`None` = do not cache: unstable).
    pub cache: Option<Attrs>,
    pub attrs: Attrs,
    pub retried: bool,
    pub unstable: bool,
    pub vouched: bool,
    pub hashed: u64,
    pub files_hashed: u64,
}

#[derive(Clone, Debug)]
pub(crate) enum Got {
    File(Box<ReadOut>),
    /// Not a file or symlink any more (removed, or a directory now).
    Gone,
    /// A FIFO, socket or device: never opened.
    Special,
}

fn content(root: &Path, path: &RepoPath, a: &Attrs) -> Result<Option<(BlobId, u64)>> {
    match &a.kind {
        FileKind::Symlink { target } => {
            let b = target.as_os_str().as_bytes();
            Ok(Some((hash_bytes(b), b.len() as u64)))
        }
        _ => match open_rooted_file(root, path) {
            Ok(f) => hash_file(&f, &root.join(path.as_str())).map(Some),
            Err(e) if is_shape_error(&e) => Ok(None),
            Err(e) => Err(e),
        },
    }
}

fn reattr(root: &Path, path: &RepoPath) -> Result<Option<Attrs>> {
    match attrs_of(root, path) {
        Ok(a) => Ok(Some(a)),
        Err(e) if is_shape_error(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// §5.2: read one candidate whose first attrs are `a1`.
pub(crate) fn read_path(
    root: &Path,
    path: &RepoPath,
    a1: Attrs,
    cache: Option<&Cached>,
) -> Result<Got> {
    if let Some(c) = cache
        && a1.unchanged_since(&c.attrs)
        && let Some(mode) = git_mode(&a1)
    {
        return Ok(Got::File(Box::new(ReadOut {
            blob: c.blob.clone(),
            mode,
            size: a1.size,
            cache: Some(a1.clone()),
            attrs: a1,
            retried: false,
            unstable: false,
            vouched: true,
            hashed: 0,
            files_hashed: 0,
        })));
    }
    let mut a1 = a1;
    let mut retried = false;
    let (mut hashed, mut files) = (0u64, 0u64);
    loop {
        if is_special(&a1.kind) {
            return Ok(Got::Special);
        }
        let Some(mode) = git_mode(&a1) else {
            return Ok(Got::Gone);
        };
        let Some((blob, size)) = content(root, path, &a1)? else {
            // The entry changed shape between A1 and open.
            match reattr(root, path)? {
                Some(a) if !retried => {
                    a1 = a;
                    retried = true;
                    continue;
                }
                _ => return Ok(Got::Gone),
            }
        };
        hashed += size;
        files += 1;
        fault::after_hash(root, path);
        let Some(a2) = reattr(root, path)? else {
            return Ok(Got::Gone);
        };
        let out = |unstable: bool, retried: bool, cache: Option<Attrs>| {
            Got::File(Box::new(ReadOut {
                blob: blob.clone(),
                mode,
                size,
                cache,
                attrs: a2.clone(),
                retried,
                unstable,
                vouched: false,
                hashed,
                files_hashed: files,
            }))
        };
        if !a1.gen_count_known() || !a2.gen_count_known() {
            // Step 6: no gen count, no proof; read once, never cache.
            return Ok(out(true, retried, None));
        }
        if a2.unchanged_since(&a1) {
            return Ok(out(false, retried, Some(a2.clone())));
        }
        if retried {
            return Ok(out(true, true, None));
        }
        retried = true;
        a1 = a2;
    }
}

/// What one race-safe read saw (test seam, §3).
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadProbe {
    pub blob: BlobId,
    pub mode: u32,
    pub retried: bool,
    pub unstable: bool,
}

/// Runs the §5.2 read on one path with an empty cache.
#[cfg(feature = "test-seams")]
pub fn read_candidate(root: &Path, path: &RepoPath) -> Result<ReadProbe> {
    let a1 = attrs_of(root, path)?;
    match read_path(root, path, a1, None)? {
        Got::File(o) => Ok(ReadProbe {
            blob: o.blob,
            mode: o.mode,
            retried: o.retried,
            unstable: o.unstable,
        }),
        Got::Gone => Err(StrataError::new(
            Code::NoSuchPath,
            format!("{} vanished while read", path.as_str()),
        )
        .with_detail(serde_json::json!({ "path": path.as_str() }))),
        Got::Special => Err(StrataError::new(
            Code::Unsupported,
            format!("{} is a special file", path.as_str()),
        )
        .with_detail(serde_json::json!({ "path": path.as_str() }))),
    }
}

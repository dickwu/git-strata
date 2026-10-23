//! Filesystem helpers: rooted reads (G11), gitfile parsing, lexical
//! normalisation, realpath of not-yet-existing paths.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use serde_json::json;
use strata_types::{Code, RepoPath, StrataError};

use crate::Result;
use crate::err::{errd, invalid, path_str};

/// Stat key of a file read in-process (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileKey {
    pub size: u64,
    pub mtime_ns: i128,
    pub ino: u64,
    pub dev: u64,
}

impl FileKey {
    pub(crate) fn of(f: &File) -> io::Result<FileKey> {
        let m = f.metadata()?;
        Ok(FileKey {
            size: m.len(),
            mtime_ns: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
            ino: m.ino(),
            dev: m.dev(),
        })
    }
}

/// A relative path as a `RepoPath`, or `InvalidInput`.
pub(crate) fn rp(s: &str) -> Result<RepoPath> {
    RepoPath::try_from(s.to_string()).map_err(|e| invalid(format!("bad path {s:?}: {}", e.message)))
}

/// Relative path (as RepoPath) from bytes of a relative `Path`.
pub(crate) fn rp_of(p: &Path) -> Result<RepoPath> {
    match p.to_str() {
        Some(s) => rp(s),
        None => Err(errd(Code::NotText, "path is not UTF-8", json!({ "path": path_str(p) }))),
    }
}

fn io_unknown(e: &io::Error, what: &str, p: &Path) -> StrataError {
    errd(
        Code::Unknown,
        format!("{what} {}: {e}", p.display()),
        json!({ "path": path_str(p), "errno": e.raw_os_error() }),
    )
}

/// Read a whole regular file through `open_rooted_file`. `NoSuchPath` →
/// `None`; other refusals pass through.
pub(crate) fn read_rooted(root: &Path, rel: &str) -> Result<Option<(Vec<u8>, FileKey)>> {
    read_rooted_limited(root, rel, None)
}

pub(crate) fn read_rooted_limited(
    root: &Path,
    rel: &str,
    limit: Option<u64>,
) -> Result<Option<(Vec<u8>, FileKey)>> {
    let rel = rp(rel)?;
    let f = match strata_apfs::open_rooted_file(root, &rel) {
        Ok(f) => f,
        Err(e) if e.code == Code::NoSuchPath => return Ok(None),
        Err(e) => return Err(e),
    };
    let key = FileKey::of(&f).map_err(|e| io_unknown(&e, "stat", &root.join(rel.as_str())))?;
    let mut buf = Vec::new();
    let r = match limit {
        Some(n) => (&f).take(n).read_to_end(&mut buf),
        None => (&f).read_to_end(&mut buf),
    };
    r.map_err(|e| io_unknown(&e, "read", &root.join(rel.as_str())))?;
    Ok(Some((buf, key)))
}

/// Read a human-written file (G11): realpath first (ENOENT / ENOTDIR →
/// absent, other errors → Unknown), then rooted at its directory.
/// Returns the resolved path too, for cache keys.
pub(crate) fn read_human(p: &Path) -> Result<Option<(Vec<u8>, FileKey, PathBuf)>> {
    let real = match std::fs::canonicalize(p) {
        Ok(r) => r,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) => return Ok(None),
        Err(e) => return Err(io_unknown(&e, "realpath", p)),
    };
    let (dir, name) = match (real.parent(), real.file_name()) {
        (Some(d), Some(n)) => (d.to_path_buf(), n.to_os_string()),
        _ => return Err(errd(Code::NotFile, "not a file", json!({ "path": path_str(p) }))),
    };
    let name = name.to_str().ok_or_else(|| errd(Code::NotText, "path is not UTF-8", json!({ "path": path_str(p) })))?;
    match read_rooted(&dir, name)? {
        None => Ok(None),
        Some((b, k)) => Ok(Some((b, k, real))),
    }
}

/// realpath of `p`, or of its nearest existing ancestor with the rest
/// appended (for G1 "inside the main checkout" checks).
pub(crate) fn resolve_lenient(p: &Path) -> PathBuf {
    let p = lexical_normalize(p);
    let mut tail: Vec<&OsStr> = Vec::new();
    let mut cur: &Path = &p;
    loop {
        if let Ok(r) = std::fs::canonicalize(cur) {
            let mut out = r;
            for c in tail.iter().rev() {
                out.push(c);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                cur = parent;
            }
            _ => return p.clone(),
        }
    }
}

/// Lexically normalise: drop `.`, resolve `..` against the previous
/// component, collapse separators.
pub(crate) fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

/// Strip git's line endings (`\n` and `\r`, repeatedly) from the end.
pub(crate) fn strip_eol(mut b: &[u8]) -> &[u8] {
    while let [rest @ .., b'\n' | b'\r'] = b {
        b = rest;
    }
    b
}

/// Max bytes of a gitfile gitio reads (`gitdir: ` + PATH_MAX + CRLF).
pub(crate) const GITFILE_MAX: usize = 8 + strata_apfs::PATH_MAX_BYTES + 2;

fn bad_gitfile(p: &Path, why: &str) -> StrataError {
    errd(
        Code::InvalidInput,
        format!("bad gitfile {}: {why}", p.display()),
        json!({ "reason": "bad-gitfile", "path": path_str(p) }),
    )
}

/// Parse gitfile bytes into the raw gitdir path (not normalised; relative
/// paths are joined onto `tree_dir`).
pub(crate) fn parse_gitfile(bytes: &[u8], tree_dir: &Path) -> Result<PathBuf> {
    let at = tree_dir.join(".git");
    if bytes.len() > GITFILE_MAX {
        return Err(bad_gitfile(&at, "too long"));
    }
    let rest = bytes.strip_prefix(b"gitdir: ").ok_or_else(|| bad_gitfile(&at, "no gitdir: prefix"))?;
    let rest = strip_eol(rest);
    if rest.is_empty() {
        return Err(bad_gitfile(&at, "empty path"));
    }
    if rest.contains(&0) {
        return Err(bad_gitfile(&at, "NUL in path"));
    }
    let p = Path::new(OsStr::from_bytes(rest));
    Ok(if p.is_absolute() { p.to_path_buf() } else { tree_dir.join(p) })
}

/// A fresh random hex nonce from /dev/urandom.
pub(crate) fn nonce() -> String {
    let mut b = [0u8; 8];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    if b == [0u8; 8] {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        b = (t.as_nanos() as u64 ^ (std::process::id() as u64) << 32).to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// lstat without following; NotFound → None.
pub(crate) fn lstat(p: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(p) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => Ok(None),
        Err(e) => Err(io_unknown(&e, "lstat", p)),
    }
}

pub(crate) fn require_absolute(p: &Path, what: &str) -> Result<()> {
    if p.is_absolute() {
        Ok(())
    } else {
        Err(errd(
            Code::InvalidInput,
            format!("{what} must be an absolute path: {}", p.display()),
            json!({ "path": path_str(p) }),
        ))
    }
}

/// Write `bytes` to a new file at `rel` under `root` (rooted, exclusive).
pub(crate) fn write_new_rooted(root: &Path, rel: &str, bytes: &[u8], mode: u32) -> Result<File> {
    use std::io::Write;
    let rel_rp = rp(rel)?;
    let mut f = strata_apfs::create_file_rooted(root, &rel_rp, mode)?;
    f.write_all(bytes).map_err(|e| io_unknown(&e, "write", &root.join(rel)))?;
    Ok(f)
}

pub(crate) fn io_err(e: &io::Error, what: &str, p: &Path) -> StrataError {
    io_unknown(e, what, p)
}

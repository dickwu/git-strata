//! Seed files (§5.3): which configured seeds a workspace gets, and copying
//! them under the workspace root without following symlinks.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::{FileKind, IoSide, classify_io_error};
use strata_gitio::Git;
use strata_types::{Code, RepoConfig, RepoPath, StrataError, TreeOid};

use crate::Result;
use crate::sys::{lstat, rp, special_kind};

/// A seed that `seed_plan` accepted: the source's lstat, plus the link
/// target read by `attrs_of` when the source is a symlink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedSource {
    pub path: RepoPath,
    /// Absolute path in the main checkout.
    pub source: PathBuf,
    /// The link target when the source is a symlink; always UTF-8.
    pub link: Option<PathBuf>,
    pub size: u64,
    pub source_mtime: SystemTime,
}

/// One copied seed (one store `SeedRow`). Provenance and times, never content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedRecord {
    pub path: RepoPath,
    pub source: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<PathBuf>,
    pub size: u64,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub source_mtime: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub copied: SystemTime,
}

impl From<&SeedRecord> for strata_store::SeedRow {
    fn from(r: &SeedRecord) -> Self {
        strata_store::SeedRow {
            path: r.path.clone(),
            source: r.source.clone(),
            link: r.link.clone(),
            size: r.size,
            source_mtime: r.source_mtime,
            copied: r.copied,
        }
    }
}

/// Decide, against base generation directory `base` (of tree `tree`),
/// which seed entries of `cfg` are copied from main checkout `main`. Read
/// only. Explicit entries that fail a rule are refused; candidates are
/// skipped silently.
pub fn seed_plan(cfg: &RepoConfig, git: &Git, main: &Path, base: &Path, tree: &TreeOid) -> Result<Vec<SeedSource>> {
    let (entries, explicit) = cfg.seed_files();
    let case_sensitive = strata_apfs::volume_caps(base)?.case_sensitive;
    let key = |p: &RepoPath| if case_sensitive { p.as_str().to_string() } else { strata_apfs::case_fold_key(p.as_str()) };
    let mut first: BTreeMap<String, usize> = BTreeMap::new();
    let mut plan = Vec::new();
    for (index, p) in entries.iter().enumerate() {
        let earlier = first.get(&key(p)).copied();
        first.entry(key(p)).or_insert(index);
        let refuse = |code: Code, msg: String, extra: serde_json::Value| -> StrataError {
            let mut d = json!({ "field": "seed_files", "rule": "seed_files", "index": index, "path": p.as_str() });
            if let (Some(d), Some(x)) = (d.as_object_mut(), extra.as_object()) {
                for (k, v) in x {
                    d.insert(k.clone(), v.clone());
                }
            }
            StrataError::new(code, msg).with_detail(d)
        };
        macro_rules! fail {
            ($code:expr, $msg:expr, $extra:expr) => {{
                if explicit {
                    return Err(refuse($code, $msg, $extra));
                }
                continue;
            }};
        }

        // 1. source
        let src = main.join(p.as_str());
        let Some(m) = lstat(&src).map_err(|e| classify_io_error(&e, IoSide::Read, &src))? else {
            continue;
        };
        let size = m.len();
        let mtime = mtime_of(&m);
        let mut link = None;
        if m.file_type().is_symlink() {
            match strata_apfs::attrs_of(main, p) {
                Ok(a) => match a.kind {
                    FileKind::Symlink { target } => match target.to_str() {
                        Some(_) => link = Some(target),
                        None => fail!(
                            Code::InvalidInput,
                            format!("seed file {p}: its link target is not UTF-8"),
                            json!({ "reason": "link_not_utf8" })
                        ),
                    },
                    // Replaced between lstat and attrs_of: skip or refuse as it now is.
                    _ => fail!(
                        Code::InvalidInput,
                        format!("seed file {p} changed type while it was planned"),
                        json!({ "reason": "changed" })
                    ),
                },
                Err(e) if e.code == Code::NotDirectory => fail!(
                    Code::NotDirectory,
                    format!("seed file {p} lies under a symlink in the main checkout"),
                    json!({ "reason": "symlink" })
                ),
                Err(e) if e.code == Code::NoSuchPath => continue,
                Err(e) => return Err(e),
            }
        } else if m.is_dir() {
            fail!(Code::IsDirectory, format!("seed file {p} is a directory"), json!({}));
        } else if let Some(kind) = special_kind(&m) {
            fail!(Code::NotFile, format!("seed file {p} is a {kind}"), json!({ "file_type": kind }));
        } else {
            // A regular file: its parents must not be symlinks either.
            match strata_apfs::attrs_of(main, p) {
                Ok(_) => {}
                Err(e) if e.code == Code::NotDirectory => fail!(
                    Code::NotDirectory,
                    format!("seed file {p} lies under a symlink in the main checkout"),
                    json!({ "reason": "symlink" })
                ),
                Err(e) if e.code == Code::NoSuchPath => continue,
                Err(e) => return Err(e),
            }
        }

        // 2. tracked
        if git.tree_contains(tree, p)? {
            fail!(Code::InvalidInput, format!("seed file {p} is tracked"), json!({ "reason": "tracked" }));
        }

        // 3. destination
        let mut bad_parent = None;
        for a in ancestors(p) {
            match strata_apfs::attrs_of(base, &a) {
                Ok(at) if matches!(at.kind, FileKind::Dir) => {}
                Ok(_) => {
                    bad_parent = Some(a);
                    break;
                }
                Err(e) if e.code == Code::NoSuchPath => break,
                Err(e) if e.code == Code::NotDirectory => {
                    bad_parent = Some(a);
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(a) = bad_parent {
            fail!(
                Code::InvalidInput,
                format!("seed file {p}: {a} is not a directory in the base"),
                json!({ "reason": "parent_not_directory", "parent": a.as_str() })
            );
        }
        match strata_apfs::attrs_of(base, p) {
            Ok(_) => fail!(
                Code::InvalidInput,
                format!("seed file {p} already exists in the base"),
                json!({ "reason": "exists_in_base" })
            ),
            Err(e) if e.code == Code::NoSuchPath => {}
            Err(e) => return Err(e),
        }
        if let Some(other) = earlier {
            fail!(
                Code::InvalidInput,
                format!("seed file {p} is the same file as entry {other}"),
                json!({ "reason": "duplicate", "other": other })
            );
        }

        // 4. ignored
        if !git.is_ignored_in_export(base, tree, p)? {
            fail!(Code::InvalidInput, format!("seed file {p} is not ignored"), json!({ "reason": "not_ignored" }));
        }

        // 5.
        plan.push(SeedSource { path: p.clone(), source: src, link, size, source_mtime: mtime });
    }
    Ok(plan)
}

/// Clone the planned seeds into workspace `to`, under the root, never
/// following symlinks; a symlink seed is recreated from the planned target.
pub fn seed_files(plan: &[SeedSource], to: &Path) -> Result<Vec<SeedRecord>> {
    let mut out = Vec::with_capacity(plan.len());
    for s in plan {
        if let Some(parent) = ancestors(&s.path).last() {
            strata_apfs::mkdir_rooted(to, parent, 0o755, true)?;
        }
        match &s.link {
            Some(target) => strata_apfs::symlink_rooted(to, &s.path, target.as_os_str())?,
            None => strata_apfs::clone_file_rooted(&s.source, to, &s.path, None)?,
        }
        out.push(SeedRecord {
            path: s.path.clone(),
            source: s.source.clone(),
            link: s.link.clone(),
            size: s.size,
            source_mtime: s.source_mtime,
            copied: SystemTime::now(),
        });
    }
    Ok(out)
}

/// `a`, `a/b`, … for `a/b/c` (proper ancestors, outermost first).
pub(crate) fn ancestors(p: &RepoPath) -> Vec<RepoPath> {
    let s = p.as_str();
    s.match_indices('/').map(|(i, _)| rp(&s[..i])).collect()
}

pub(crate) fn mtime_of(m: &std::fs::Metadata) -> SystemTime {
    m.modified().unwrap_or_else(|_| UNIX_EPOCH + Duration::new(m.mtime().max(0) as u64, m.mtime_nsec().max(0) as u32))
}

/// The xattr a refreshed seed copy carries: `v1 <mtime_ns> <size>` of the
/// copy as strata wrote it. Present only after a refresh; a copy from `new`
/// is described by its store row (size, source mtime: clonefile keeps the
/// source's mtime). Any write to the copy changes its mtime; an editor that
/// replaces the file drops the xattr. Either way the copy no longer matches
/// what strata wrote and is never overwritten.
pub const SEED_MARK: &str = "dev.git-strata.seed";

/// Staging name prefix (layout root) of a seed being refreshed.
pub const SEED_STAGING_PREFIX: &str = ".strata-tmp-seed-";

/// One seed copy `refresh_seeds` replaced with the main checkout's current file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedRefresh {
    pub repo: strata_types::RepoName,
    pub path: RepoPath,
}

/// What strata last wrote at a seed copy: (mtime, size).
pub(crate) type SeedStamp = (SystemTime, u64);

/// What strata last wrote at `copy` (seed row `s`): the refresh mark when
/// the copy carries one, else the row's source mtime and size.
pub(crate) fn written_stamp(copy: &Path, s: &SeedRecord) -> SeedStamp {
    read_mark(copy).unwrap_or((s.source_mtime, s.size))
}

fn ns_of(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

fn time_of(ns: i128) -> Option<SystemTime> {
    let d = Duration::from_nanos(u64::try_from(ns.unsigned_abs()).ok()?);
    if ns >= 0 { UNIX_EPOCH.checked_add(d) } else { UNIX_EPOCH.checked_sub(d) }
}

fn cpath(p: &Path) -> Option<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).ok()
}

fn read_mark(p: &Path) -> Option<SeedStamp> {
    let (c, n) = (cpath(p)?, std::ffi::CString::new(SEED_MARK).ok()?);
    let mut buf = [0u8; 96];
    // SAFETY: valid NUL-terminated strings; buf outlives the call and its
    // length is passed.
    let r = unsafe { libc::getxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, libc::XATTR_NOFOLLOW) };
    if r < 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..r as usize]).ok()?;
    let mut it = s.split(' ');
    if it.next()? != "v1" {
        return None;
    }
    let m = time_of(it.next()?.parse::<i128>().ok()?)?;
    let size = it.next()?.parse::<u64>().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((m, size))
}

fn write_mark(p: &Path, st: SeedStamp) -> std::io::Result<()> {
    let c = cpath(p).ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let n = std::ffi::CString::new(SEED_MARK).expect("no NUL");
    let v = format!("v1 {} {}", ns_of(st.0), st.1);
    // SAFETY: valid NUL-terminated strings; v outlives the call.
    let r = unsafe { libc::setxattr(c.as_ptr(), n.as_ptr(), v.as_ptr().cast(), v.len(), 0, libc::XATTR_NOFOLLOW) };
    if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

fn stamp_of(a: &strata_apfs::Attrs) -> SeedStamp {
    (a.mtime, a.size)
}

impl crate::Sessions<'_> {
    /// Keep seed copies current (seed refresh): every regular-file seed of
    /// an active session whose main-checkout source changed since strata
    /// last wrote the copy, and whose copy nobody touched since, is replaced
    /// by a fresh clone of the source (staged at the layout root, then
    /// swapped in with one rename). A copy that was edited, replaced or
    /// removed in the workspace, a seed that is no longer ignored, a symlink
    /// seed and a missing source are left alone (`status` keeps reporting
    /// `SeedStale`). Never reads or logs file contents.
    pub fn refresh_seeds(&self, id: &strata_types::SessionId) -> Result<Vec<SeedRefresh>> {
        self.seeds_pass(id, false)
    }

    /// Read only: the seeds `refresh_seeds` would replace now (before the
    /// ignore check). Lets a caller take the session's queue only when
    /// there is something to do.
    pub fn seeds_due(&self, id: &strata_types::SessionId) -> Result<Vec<SeedRefresh>> {
        self.seeds_pass(id, true)
    }

    fn seeds_pass(&self, id: &strata_types::SessionId, dry: bool) -> Result<Vec<SeedRefresh>> {
        let rec = self.live_session(id)?;
        if rec.lifecycle != strata_store::Lifecycle::Active {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (repo, row) in &rec.workspaces {
            if row.health != strata_store::Health::Ok || row.seeds.is_empty() {
                continue;
            }
            let Some(ws) = rec.manifest.repos.get(repo) else { continue };
            let git = self.git(repo)?;
            for s in row.seeds.iter().map(SeedRecord::from) {
                if s.link.is_none() && refresh_one(git, &ws.path, repo, &s, dry)? {
                    out.push(SeedRefresh { repo: repo.clone(), path: s.path.clone() });
                }
            }
        }
        Ok(out)
    }
}

fn refresh_one(git: &Git, ws: &Path, repo: &strata_types::RepoName, s: &SeedRecord, dry: bool) -> Result<bool> {
    // The source: a regular file whose stamp differs from what was written.
    let src = match lstat(&s.source).map_err(|e| classify_io_error(&e, IoSide::Read, &s.source))? {
        Some(m) if m.file_type().is_file() => m,
        _ => return Ok(false),
    };
    let src_stamp = (mtime_of(&src), src.len());
    // The copy: a regular file, reached without following symlinks.
    let copy = match strata_apfs::attrs_of(ws, &s.path) {
        Ok(a) if matches!(a.kind, FileKind::File) => a,
        Ok(_) => return Ok(false),
        Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory) => return Ok(false),
        Err(e) => return Err(e),
    };
    let copy_path = ws.join(s.path.as_str());
    let written = written_stamp(&copy_path, s);
    if stamp_of(&copy) != written || src_stamp == written {
        return Ok(false);
    }
    if dry {
        return Ok(true);
    }
    if !git.is_ignored(ws, &s.path)? {
        return Ok(false);
    }
    let Some(layout) = ws.parent() else { return Ok(false) };
    let tmp = rp(format!("{SEED_STAGING_PREFIX}{}", crate::sys::random_hex(8)));
    let dst = rp(format!("{repo}/{}", s.path));
    strata_apfs::clone_file_rooted(&s.source, layout, &tmp, None)?;
    let tmp_path = layout.join(tmp.as_str());
    let undo = |e: StrataError| -> StrataError {
        let _ = strata_apfs::remove_rooted(layout, &tmp, strata_apfs::RemoveKind::File);
        e
    };
    let fresh = strata_apfs::attrs_of(layout, &tmp).map_err(undo)?;
    write_mark(&tmp_path, stamp_of(&fresh)).map_err(|e| undo(classify_io_error(&e, IoSide::Write, &tmp_path)))?;
    strata_apfs::rename_rooted(layout, &tmp, &dst, strata_apfs::RenameMode::Swap).map_err(undo)?;
    // `tmp` now holds the old copy. Written to between the check and the
    // swap: put it back and keep the edit.
    let old = strata_apfs::attrs_of(layout, &tmp)?;
    if stamp_of(&old) != written || old.inode != copy.inode {
        strata_apfs::rename_rooted(layout, &tmp, &dst, strata_apfs::RenameMode::Swap)?;
        strata_apfs::remove_rooted(layout, &tmp, strata_apfs::RemoveKind::File)?;
        return Ok(false);
    }
    strata_apfs::remove_rooted(layout, &tmp, strata_apfs::RemoveKind::File)?;
    Ok(true)
}

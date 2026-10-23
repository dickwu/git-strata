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

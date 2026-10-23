//! `new` phase A (§5.1 steps 1-7): every refusal that needs no repo lock and
//! writes nothing. Uses `lstat` and directory opens only; never opens a
//! seed source (a FIFO would block).

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::json;
use strata_apfs::{IoSide, classify_io_error, nearest_existing_ancestor, same_volume, volume_caps};
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::sys::{lstat, real_dir, special_kind, within};
use crate::{Result, Sessions, no_such_repo, ticket};

/// What phase A settled.
pub(crate) struct Validated {
    /// The requested repos, deduplicated, in `RepoName` order.
    pub(crate) tracked: Vec<RepoName>,
    pub(crate) id: SessionId,
}

impl Sessions<'_> {
    pub(crate) fn validate_new(&self, ticket_: Option<&str>, repos: &[RepoName]) -> Result<Validated> {
        // 1. ticket
        if let Some(t) = ticket_ {
            ticket::validate(t)?;
        }
        // 2. repos
        if repos.is_empty() {
            return Err(StrataError::new(Code::InvalidInput, "new needs at least one repo")
                .with_detail(json!({ "field": "repos" })));
        }
        let mut tracked = BTreeSet::new();
        for r in repos {
            if !self.config.repos.contains_key(r) {
                return Err(no_such_repo(r));
            }
            tracked.insert(r.clone());
        }
        // 3. every configured checkout, and the root when it exists
        for (name, rc) in &self.config.repos {
            check_dir(&rc.path, Some(name), "path")?;
        }
        let root = &self.config.paths.root;
        if lstat(root).map_err(|e| classify_io_error(&e, IoSide::Read, root))?.is_some() {
            check_dir(root, None, "paths.root")?;
        }
        // 4. aliasing
        let anc = nearest_existing_ancestor(root).map_err(|e| classify_io_error(&e, IoSide::Read, root))?;
        let mut real_root = real_dir(&anc)?;
        if let Ok(rest) = root.strip_prefix(&anc) {
            real_root.push(rest);
        }
        let caps = volume_caps(&anc)?;
        let fold = !caps.case_sensitive;
        for (name, rc) in &self.config.repos {
            let w = real_dir(&rc.path)?;
            let c = self.git(name)?.repo_id().get().to_path_buf();
            let c = real_dir(&c).unwrap_or(c);
            for (key, other) in [("checkout", &w), ("common_dir", &c)] {
                if within(&real_root, other, fold) || within(other, &real_root, fold) {
                    return Err(StrataError::new(
                        Code::InvalidInput,
                        format!(
                            "paths.root {} and repo {name}'s {key} {} contain one another",
                            real_root.display(),
                            other.display()
                        ),
                    )
                    .with_detail(json!({
                        "rule": "nesting",
                        "repo": name.as_str(),
                        "root": real_root.to_string_lossy(),
                        key: other.to_string_lossy(),
                    })));
                }
            }
        }
        // 5. volume
        for (name, rc) in &self.config.repos {
            let same = same_volume(&anc, &rc.path).map_err(|e| classify_io_error(&e, IoSide::Read, &rc.path))?;
            if !same {
                return Err(StrataError::new(
                    Code::CrossVolume,
                    format!("paths.root {} is not on repo {name}'s volume ({})", root.display(), rc.path.display()),
                )
                .with_detail(json!({
                    "root": root.to_string_lossy(),
                    "repo": name.as_str(),
                    "path": rc.path.to_string_lossy(),
                })));
            }
        }
        if caps.fs_type != "apfs" {
            return Err(StrataError::new(
                Code::Unsupported,
                format!("paths.root {} is on {}, not APFS", root.display(), caps.fs_type),
            )
            .with_detail(json!({ "fs_type": caps.fs_type, "root": root.to_string_lossy() })));
        }
        if caps.clone == Some(false) {
            return Err(StrataError::new(
                Code::Unsupported,
                format!("the volume of {} cannot clone files", root.display()),
            )
            .with_detail(json!({ "reason": "no_clone", "fs_type": caps.fs_type, "root": root.to_string_lossy() })));
        }
        // 6. seed sources: types only, by lstat
        for (name, rc) in &self.config.repos {
            let (files, explicit) = rc.seed_files();
            if explicit {
                for (index, p) in files.iter().enumerate() {
                    let src = rc.path.join(p.as_str());
                    let Some(m) = lstat(&src).map_err(|e| classify_io_error(&e, IoSide::Read, &src))? else {
                        continue;
                    };
                    let detail = json!({
                        "repo": name.as_str(),
                        "field": "seed_files",
                        "index": index,
                        "path": p.as_str(),
                    });
                    if m.is_dir() {
                        return Err(StrataError::new(
                            Code::IsDirectory,
                            format!("seed file {p} of repo {name} is a directory"),
                        )
                        .with_detail(detail));
                    }
                    if let Some(kind) = special_kind(&m) {
                        let mut d = detail;
                        d["file_type"] = json!(kind);
                        return Err(StrataError::new(
                            Code::NotFile,
                            format!("seed file {p} of repo {name} is a {kind}"),
                        )
                        .with_detail(d));
                    }
                }
            }
            if tracked.contains(name) {
                for (index, c) in rc.seed_caches.iter().enumerate() {
                    let src = rc.path.join(c.as_str());
                    let Some(m) = lstat(&src).map_err(|e| classify_io_error(&e, IoSide::Read, &src))? else {
                        continue;
                    };
                    let reason = if m.file_type().is_symlink() {
                        "symlink"
                    } else if !m.is_dir() {
                        "not_a_directory"
                    } else {
                        continue;
                    };
                    return Err(StrataError::new(
                        Code::NotDirectory,
                        format!("seed cache {c} of repo {name} is not a real directory ({reason})"),
                    )
                    .with_detail(json!({
                        "repo": name.as_str(),
                        "field": "seed_caches",
                        "index": index,
                        "path": c.as_str(),
                        "reason": reason,
                    })));
                }
            }
        }
        // 7. session id; no retry with another one
        let id = self.seams.take_next_id().unwrap_or_else(crate::sys::random_session_id);
        if self.store.session(&id)?.is_some() {
            return Err(StrataError::new(Code::Exists, format!("session {id} already has a store row"))
                .with_detail(json!({ "session": id.as_str(), "reason": "store_row" })));
        }
        let sessions = root.join("sessions");
        for p in [sessions.join(id.as_str()), sessions.join(".repos").join(id.as_str())] {
            if lstat(&p).map_err(|e| classify_io_error(&e, IoSide::Read, &p))?.is_some() {
                return Err(StrataError::new(Code::Exists, format!("{} already exists", p.display()))
                    .with_detail(json!({ "session": id.as_str(), "path": p.to_string_lossy(), "reason": "directory" })));
            }
        }
        Ok(Validated { tracked: tracked.into_iter().collect(), id })
    }
}

/// §5.1 step 3 for one path: it must exist and be a real directory.
fn check_dir(p: &Path, repo: Option<&RepoName>, field: &str) -> Result<()> {
    let detail = |extra: serde_json::Value| {
        let mut d = json!({ "field": field, "path": p.to_string_lossy() });
        if let Some(r) = repo {
            d["repo"] = json!(r.as_str());
        }
        if let serde_json::Value::Object(m) = extra {
            for (k, v) in m {
                d[k] = v;
            }
        }
        d
    };
    let m = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) => {
            let mut err = classify_io_error(&e, IoSide::Read, p);
            let errno = e.raw_os_error();
            err.detail = Some(detail(json!({ "errno": errno })));
            return Err(err);
        }
    };
    let reason = if m.file_type().is_symlink() {
        "symlink"
    } else if !m.is_dir() {
        "not_a_directory"
    } else {
        return Ok(());
    };
    Err(StrataError::new(Code::NotDirectory, format!("{field} {} is not a real directory ({reason})", p.display()))
        .with_detail(detail(json!({ "reason": reason }))))
}


//! The deps area layout (§4.1) and the staging directory's bookkeeping.

use std::path::PathBuf;

use serde_json::{Value, json};
use strata_apfs::RenameMode;
use strata_types::{Code, DepsSetId, RepoName, RepoPath, StrataError};

use crate::{Ctx, Result};

/// A relative path deps builds itself from valid parts.
pub(crate) fn rp(s: impl Into<String>) -> RepoPath {
    let s = s.into();
    RepoPath::try_from(s.clone())
        .unwrap_or_else(|_| panic!("deps built an invalid relative path {s:?}"))
}

/// `a/b` for a parent and a child (either may be multi-component).
pub(crate) fn join(a: &str, b: &str) -> RepoPath {
    rp(format!("{a}/{b}"))
}

/// The parent of a relative path, `None` at the top level.
pub(crate) fn parent(p: &RepoPath) -> Option<RepoPath> {
    p.as_str().rsplit_once('/').map(|(a, _)| rp(a))
}

/// `detail.reason` of an error, if any.
pub(crate) fn reason(e: &StrataError) -> Option<&str> {
    e.detail
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
}

/// An `Unknown` with `detail.reason`.
pub(crate) fn unknown(reason: &str, message: impl Into<String>, mut detail: Value) -> StrataError {
    if let Some(m) = detail.as_object_mut() {
        m.insert("reason".into(), json!(reason));
    }
    StrataError::new(Code::Unknown, message).with_detail(detail)
}

/// Paths of one repo's deps area.
pub(crate) struct Layout {
    /// `<root>/deps`: the root of every rooted operation on the lock.
    pub deps: PathBuf,
    /// `<root>/deps/<repo>`: the root of rename, seal and staging operations.
    pub repo_dir: PathBuf,
    pub repo: RepoName,
}

impl Layout {
    pub fn of(ctx: &Ctx) -> Layout {
        let deps = ctx.paths.deps();
        Layout {
            repo_dir: deps.join(ctx.repo.as_str()),
            deps,
            repo: ctx.repo.clone(),
        }
    }

    pub fn locks_rel(&self) -> RepoPath {
        join(self.repo.as_str(), ".locks")
    }

    pub fn lock_rel(&self, id: &DepsSetId) -> RepoPath {
        rp(format!(
            "{}/.locks/{}.lock",
            self.repo.as_str(),
            id.as_str()
        ))
    }

    pub fn set_rel(&self, id: &DepsSetId) -> RepoPath {
        rp(id.as_str())
    }

    pub fn staging_rel(a: u64) -> RepoPath {
        rp(format!(".staging/{a}"))
    }
}

/// One attempt's staging directory `.staging/<a>/` and its `discard/` and
/// `inputs/` counters (§4.1: nothing is deleted inside staging).
pub(crate) struct Staging {
    pub a: u64,
    /// Absolute path (the store's `deps_staging_dir`), used for `retire_dir`.
    pub dir: PathBuf,
    discard_n: u64,
    inputs_n: u64,
}

impl Staging {
    pub fn new(a: u64, dir: PathBuf) -> Staging {
        Staging {
            a,
            dir,
            discard_n: 0,
            inputs_n: 0,
        }
    }

    fn ensure_dir(&self, name: &str) -> Result<()> {
        strata_apfs::mkdir_rooted(&self.dir, &rp(name), 0o755, true)
    }

    /// Move `rel` (relative to the staging dir) into `discard/<n>`.
    pub fn discard(&mut self, rel: &RepoPath) -> Result<()> {
        self.ensure_dir("discard")?;
        let to = rp(format!("discard/{}", self.discard_n));
        self.discard_n += 1;
        strata_apfs::rename_rooted(&self.dir, rel, &to, RenameMode::Excl)
    }

    /// A fresh `inputs/<n>` slot (relative to the staging dir), `inputs/`
    /// created on first use.
    pub fn input_slot(&mut self) -> Result<RepoPath> {
        self.ensure_dir("inputs")?;
        let p = rp(format!("inputs/{}", self.inputs_n));
        self.inputs_n += 1;
        Ok(p)
    }
}

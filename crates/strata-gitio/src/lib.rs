//! gitio: every git operation git-strata needs (docs/modules/gitio.md).
//!
//! Subprocess git runs through one sanitized command builder (§5.1);
//! hot-path reads (index, ignore rules, config booleans, gitfiles) are done
//! in-process through `strata_apfs` rooted reads (G11). Errors are
//! `StrataError` with the codes of §6.


use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use strata_types::{RepoId, StrataError};

mod cache;
mod command;
mod commit;
mod config;
mod err;
mod export;
mod fsx;
mod ignore;
mod index;
mod merge;
mod publish_io;
mod push;
mod refs;
mod remote;
mod replay;
mod repo;
mod seams;
mod shadow;
mod stage;
mod staging;
mod tree;
mod worktree;

#[cfg(feature = "test-seams")]
pub use seams::{GitCall, Pause, SyncEvent};
pub use index::IndexInfo;
pub use merge::{Hunk, Merge3, is_binary, merge3};
pub use publish_io::{RefUpdate, TreeBlob, TreeItem};
pub use remote::remote_ref;
pub use shadow::{MirrorDrift, Shadow};

pub type Result<T> = std::result::Result<T, StrataError>;

/// The oldest git gitio accepts (§2).
pub const MIN_GIT_VERSION: (u32, u32) = (2, 44);
/// Prefix of the temporary directory `export_tree` builds next to `dst`.
pub const EXPORT_TEMP_PREFIX: &str = ".export-";

/// The default fetch timeout (§5.3, §9).
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Handle to one repository's main checkout. Cheap to clone; clones share
/// caches and (with `test-seams`) seam state.
#[derive(Clone)]
pub struct Git {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    /// The `path` given to `open`, byte for byte.
    pub(crate) root_arg: PathBuf,
    /// `realpath(root)`.
    pub(crate) root_real: PathBuf,
    /// realpath of the common dir; also the `RepoId`.
    pub(crate) common_dir: PathBuf,
    /// The main checkout's work tree when the common dir is named `.git`.
    pub(crate) main_worktree: Option<PathBuf>,
    /// Extra environment for every child (`open_with_env`, `set_env`).
    pub(crate) env: Mutex<Vec<(OsString, Option<OsString>)>>,
    pub(crate) fetch_timeout: Mutex<Duration>,
    pub(crate) cache: Mutex<cache::Cache>,
    pub(crate) seams: Arc<seams::Seams>,
}

impl fmt::Debug for Git {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Git")
            .field("common_dir", &self.inner.common_dir)
            .field("root", &self.inner.root_real)
            .finish_non_exhaustive()
    }
}

impl Git {
    /// The realpath of the common dir.
    pub fn common_dir(&self) -> &Path {
        &self.inner.common_dir
    }

    /// Review P1-5: remove `refs/strata/**/*.lock` files last modified
    /// before `before` (a killed ref transaction leaves them, and git never
    /// does). Only the daemon writes `refs/strata/`; a lock newer than
    /// `before` may belong to a live git (`pack-refs`) and is kept.
    /// Returns the removed paths.
    pub fn clear_stale_strata_locks(&self, before: std::time::SystemTime) -> Vec<PathBuf> {
        fn walk(dir: &Path, before: std::time::SystemTime, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let p = e.path();
                if ft.is_dir() {
                    walk(&p, before, out);
                } else if ft.is_file() && p.extension().is_some_and(|x| x == "lock") {
                    let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < before);
                    if old && std::fs::remove_file(&p).is_ok() {
                        out.push(p);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.inner.common_dir.join("refs/strata"), before, &mut out);
        out.sort();
        out
    }

    /// The repository's identity: the realpath of its common dir.
    pub fn repo_id(&self) -> RepoId {
        RepoId::new(self.inner.common_dir.clone())
    }

    /// Whether `p` lies inside the main checkout's work tree or common dir
    /// (G1). Resolves the nearest existing ancestor so `/tmp` and
    /// `/private/tmp` compare equal.
    pub(crate) fn inside_main(&self, p: &Path) -> bool {
        let resolved = fsx::resolve_lenient(p);
        let mut roots: Vec<&Path> = vec![&self.inner.common_dir, &self.inner.root_real];
        if let Some(w) = &self.inner.main_worktree {
            roots.push(w);
        }
        roots.iter().any(|r| resolved.starts_with(r))
    }
}

// `Git` is shared across threads by its callers.
const _: fn() = || {
    fn is<T: Send + Sync>() {}
    is::<Git>();
};

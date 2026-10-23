//! gitio: every git operation git-strata needs (docs/modules/gitio.md).
//!
//! Subprocess git runs through one sanitized command builder (§5.1);
//! hot-path reads (index, ignore rules, config booleans, gitfiles) are done
//! in-process through `strata_apfs` rooted reads (G11). Errors are
//! `StrataError` with the codes of §6.

#![allow(dead_code)] // removed by the last task once every helper is used

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use strata_types::{RepoId, StrataError};

mod cache;
mod command;
mod config;
mod err;
mod fsx;
mod ignore;
mod index;
mod refs;
mod remote;
mod repo;
mod seams;

#[cfg(feature = "test-seams")]
pub use seams::{GitCall, Pause, SyncEvent};
pub use index::IndexInfo;
pub use remote::remote_ref;

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

#[allow(dead_code)]
pub(crate) fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<Git>();
}

#[allow(unused)]
pub(crate) fn _unused(_: PathBuf) {}

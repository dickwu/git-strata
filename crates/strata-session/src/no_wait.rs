//! rm-followups §1.6 T2: the catch-up pass never queues behind a request,
//! also where it builds a base through this handle. [`Sessions::no_wait`]
//! makes every repo write-lock take of the current thread a
//! [`RepoWriteLock::try_write`] (`Busy` when held) while it runs.

use std::cell::Cell;

use strata_deps::RepoWriteLock;
use strata_types::RepoName;

use crate::Sessions;

thread_local! {
    static NO_WAIT: Cell<bool> = const { Cell::new(false) };
}

/// The handle's repo write lock: the daemon's, taken without waiting on a
/// thread inside [`Sessions::no_wait`].
pub(crate) struct SessionLock<'a>(pub(crate) &'a dyn RepoWriteLock);

impl RepoWriteLock for SessionLock<'_> {
    fn with_cleanup(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        self.0.with_cleanup(repo, f)
    }
    fn check_fence(&self, repos: &[RepoName]) -> strata_deps::Result<()> {
        self.0.check_fence(repos)
    }

    fn with_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        if NO_WAIT.with(Cell::get) {
            self.0.try_write(repo, f)
        } else {
            self.0.with_write(repo, f)
        }
    }

    fn try_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        self.0.try_write(repo, f)
    }

    fn record_long_wait(&self, repo: &RepoName, attempt_since: std::time::SystemTime) {
        self.0.record_long_wait(repo, attempt_since);
    }

    fn try_write_as(
        &self,
        repo: &RepoName,
        holder: &str,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        self.0.try_write_as(repo, holder, f)
    }

    fn is_shut_down(&self) -> bool {
        self.0.is_shut_down()
    }

    fn ref_snapshot(&self, repo: &RepoName) -> Option<strata_deps::RefSnapshot> {
        self.0.ref_snapshot(repo)
    }

    fn wake(&self, repo: &RepoName) {
        self.0.wake(repo);
    }

    fn config_epoch(&self, repo: &RepoName) -> u64 {
        self.0.config_epoch(repo)
    }
}

impl Sessions<'_> {
    /// Run `f` with every repo write-lock take of this thread through this
    /// handle (base registration, deps seal) only if free right now.
    pub fn no_wait<T>(&self, f: impl FnOnce() -> T) -> T {
        struct Reset(bool);
        impl Drop for Reset {
            fn drop(&mut self) {
                NO_WAIT.with(|c| c.set(self.0));
            }
        }
        let _reset = Reset(NO_WAIT.with(|c| c.replace(true)));
        f()
    }
}

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
    fn with_write(&self, repo: &RepoName, f: &mut dyn FnMut() -> strata_deps::Result<()>) -> strata_deps::Result<strata_deps::Result<()>> {
        if NO_WAIT.with(Cell::get) {
            self.0.try_write(repo, f)
        } else {
            self.0.with_write(repo, f)
        }
    }

    fn try_write(&self, repo: &RepoName, f: &mut dyn FnMut() -> strata_deps::Result<()>) -> strata_deps::Result<strata_deps::Result<()>> {
        self.0.try_write(repo, f)
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

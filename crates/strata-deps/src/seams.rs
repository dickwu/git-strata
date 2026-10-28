//! Per-thread test seams (§3 `test_seams`). Without the `test-seams` feature
//! every hook is a no-op that the optimizer removes.

#![allow(dead_code)]
use std::path::Path;

use strata_types::StrataError;

/// Which kind of sync a seal performed (for `sync_counts`).
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) enum Sync {
    BarrierFile,
    BarrierDir,
    Full,
}

#[cfg(feature = "test-seams")]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    use strata_types::StrataError;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        pub(super) static FAIL_OPEN: RefCell<Vec<(PathBuf, StrataError)>> = const { RefCell::new(Vec::new()) };
        pub(super) static AFTER_ATTEMPT: RefCell<Option<Hook>> = const { RefCell::new(None) };
        pub(super) static BEFORE_MAIN_LOCK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        pub(super) static BEFORE_MAIN_CAS: RefCell<Option<Hook>> = const { RefCell::new(None) };
        pub(super) static SYNCS: Cell<(u64, u64, u64)> = const { Cell::new((0, 0, 0)) };
        pub(super) static SNAPSHOT_CLONE: RefCell<Option<SnapHook>> = const { RefCell::new(None) };
        pub(super) static SNAPSHOT_HASH_WRONG: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) type SnapHook = Box<dyn FnMut(&Path, &strata_types::RepoPath)>;

    /// Take the hook out of `slot`, run it, and put it back afterwards (also
    /// on unwind) unless the hook installed a replacement or `reset` ran.
    pub(super) fn call_kept(slot: &'static std::thread::LocalKey<RefCell<Option<Hook>>>) {
        let Some(hook) = slot.with(|s| s.borrow_mut().take()) else { return };
        struct Back(&'static std::thread::LocalKey<RefCell<Option<Hook>>>, Option<Hook>);
        impl Drop for Back {
            fn drop(&mut self) {
                let h = self.1.take();
                let _ = self.0.try_with(|s| {
                    let mut s = s.borrow_mut();
                    if s.is_none() && !RESET_DURING.with(|r| r.get()) {
                        *s = h;
                    }
                });
            }
        }
        RESET_DURING.with(|r| r.set(false));
        let mut back = Back(slot, Some(hook));
        if let Some(h) = back.1.as_mut() {
            h();
        }
    }

    thread_local! {
        pub(super) static RESET_DURING: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn matches(prefix: &Path, dir: &Path) -> bool {
        dir.starts_with(prefix)
            || std::fs::canonicalize(prefix).is_ok_and(|p| {
                dir.starts_with(&p) || std::fs::canonicalize(dir).is_ok_and(|d| d.starts_with(&p))
            })
    }
}

/// The injected failure for an open-file scan of `dir`, if any.
#[allow(unused_variables)]
pub(crate) fn open_files_failure(dir: &Path) -> Option<StrataError> {
    #[cfg(feature = "test-seams")]
    {
        return imp::FAIL_OPEN
            .with(|f| f.borrow().iter().find(|(p, _)| imp::matches(p, dir)).map(|(_, e)| e.clone()));
    }
    #[allow(unreachable_code)]
    None
}

pub(crate) fn after_attempt_begins() {
    #[cfg(feature = "test-seams")]
    imp::call_kept(&imp::AFTER_ATTEMPT);
}

pub(crate) fn before_main_lock() {
    #[cfg(feature = "test-seams")]
    {
        if let Some(mut h) = imp::BEFORE_MAIN_LOCK.with(|s| s.borrow_mut().take()) {
            h();
        }
    }
}

pub(crate) fn before_main_cas() {
    #[cfg(feature = "test-seams")]
    imp::call_kept(&imp::BEFORE_MAIN_CAS);
}

/// publish's input snapshot: after a file was cloned into its slot, before
/// it is renamed to `dst` (relative to the snapshot directory `snap`).
#[allow(unused_variables)]
pub(crate) fn after_snapshot_clone(snap: &Path, dst: &strata_types::RepoPath) {
    #[cfg(feature = "test-seams")]
    {
        if let Some(mut h) = imp::SNAPSHOT_CLONE.with(|s| s.borrow_mut().take()) {
            h(snap, dst);
        }
    }
}

/// publish's input snapshot: the hash it verifies (a test can make it wrong).
pub(crate) fn snapshot_hash(h: String) -> String {
    #[cfg(feature = "test-seams")]
    {
        if imp::SNAPSHOT_HASH_WRONG.with(|c| c.replace(false)) {
            return format!("{h}-wrong");
        }
    }
    h
}

#[allow(unused_variables)]
pub(crate) fn count_sync(kind: Sync) {
    #[cfg(feature = "test-seams")]
    imp::SYNCS.with(|c| {
        let (mut f, mut d, mut full) = c.get();
        match kind {
            Sync::BarrierFile => f += 1,
            Sync::BarrierDir => d += 1,
            Sync::Full => full += 1,
        }
        c.set((f, d, full));
    });
}

/// Test seams (module-rulings Addendum 4 "Test seams"); per thread.
#[cfg(feature = "test-seams")]
pub mod test_seams {
    use std::path::Path;

    use strata_types::StrataError;

    use super::imp;

    /// Later open-file scans on this thread (`open_files` and the batch scan
    /// of `plan_rollout` / `plan_switch`) return `error` for a directory under
    /// `prefix`.
    pub fn fail_open_files_under(prefix: &Path, error: StrataError) {
        imp::FAIL_OPEN.with(|f| f.borrow_mut().push((prefix.to_path_buf(), error)));
    }

    /// Called after every `begin_deps_attempt` on this thread, before anything
    /// is put into the staging directory.
    pub fn after_attempt_begins(hook: Box<dyn FnMut()>) {
        imp::AFTER_ATTEMPT.with(|s| *s.borrow_mut() = Some(hook));
    }

    /// Called once, by this thread's next `observe_main`, after the keys are
    /// computed and before the repo write lock (taken out before the call).
    pub fn before_main_lock(hook: Box<dyn FnMut()>) {
        imp::BEFORE_MAIN_LOCK.with(|s| *s.borrow_mut() = Some(hook));
    }

    /// Called by this thread's `observe_main` before every compare-and-swap of
    /// the main record, until `reset`.
    pub fn before_main_cas(hook: Box<dyn FnMut()>) {
        imp::BEFORE_MAIN_CAS.with(|s| *s.borrow_mut() = Some(hook));
    }

    /// Called once, by this thread's next publish input snapshot, after the
    /// first file is cloned into its slot and before it is renamed to
    /// `tree/<member>` (`snap` is the snapshot directory).
    pub fn after_snapshot_clone(hook: Box<dyn FnMut(&Path, &strata_types::RepoPath)>) {
        imp::SNAPSHOT_CLONE.with(|s| *s.borrow_mut() = Some(hook));
    }

    /// This thread's next publish input snapshot hashes wrong once.
    pub fn snapshot_hash_wrong() {
        imp::SNAPSHOT_HASH_WRONG.with(|c| c.set(true));
    }

    /// Syncs done by this thread's seals: (files with F_BARRIERFSYNC,
    /// directories with F_BARRIERFSYNC, F_FULLFSYNC calls).
    pub fn sync_counts() -> (u64, u64, u64) {
        imp::SYNCS.with(|c| c.get())
    }

    /// Clear every injection and counter of this thread.
    pub fn reset() {
        imp::FAIL_OPEN.with(|f| f.borrow_mut().clear());
        imp::AFTER_ATTEMPT.with(|s| *s.borrow_mut() = None);
        imp::BEFORE_MAIN_LOCK.with(|s| *s.borrow_mut() = None);
        imp::BEFORE_MAIN_CAS.with(|s| *s.borrow_mut() = None);
        imp::SYNCS.with(|c| c.set((0, 0, 0)));
        imp::SNAPSHOT_CLONE.with(|s| *s.borrow_mut() = None);
        imp::SNAPSHOT_HASH_WRONG.with(|c| c.set(false));
        imp::RESET_DURING.with(|r| r.set(true));
    }
}

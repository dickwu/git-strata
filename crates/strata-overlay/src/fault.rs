//! Test seams (§3): one-shot pauses keyed by the workspace root. Without the
//! `test-seams` feature every hook is an empty inline function.

use std::path::Path;

use strata_types::RepoPath;

#[cfg(feature = "test-seams")]
pub use imp::{OverlayFault, inject_fault, release_fault, wait_fault};

#[cfg(feature = "test-seams")]
mod imp {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    use strata_types::RepoPath;

    /// Where a scan, checkpoint or restore stops until `release_fault`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum OverlayFault {
        /// After the content of `path` was hashed, before its second attrs.
        PauseAfterHash { path: RepoPath },
        /// Scan (compute, checkpoint): after the inbox snapshot was taken,
        /// before the tree is looked at.
        PauseAfterSnapshot,
        /// Checkpoint: after the scan, before any blob is stored (§5.6 step 5).
        PauseBeforePut,
        /// Restore: before the precheck of the workspace.
        PauseBeforePrecheck,
        /// Restore: after the plan, before any write.
        PauseAfterPlan,
        /// Restore: before the staged files get their metadata.
        PauseBeforeTempMeta,
        /// Restore: after `after` apply steps.
        PauseInApply { after: u32 },
    }

    #[derive(Default)]
    struct Slot {
        armed: Vec<OverlayFault>,
        paused: Option<OverlayFault>,
        releases: u64,
    }

    type Reg = (Mutex<HashMap<PathBuf, Slot>>, Condvar);

    fn reg() -> &'static Reg {
        static R: OnceLock<Reg> = OnceLock::new();
        R.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
    }

    fn lock() -> MutexGuard<'static, HashMap<PathBuf, Slot>> {
        reg().0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Arms `fault` for the next operation under `root`; it fires once.
    pub fn inject_fault(root: &Path, fault: OverlayFault) {
        lock()
            .entry(root.to_path_buf())
            .or_default()
            .armed
            .push(fault);
    }

    /// Lets a paused thread go on; when none is paused, disarms every fault
    /// still armed for `root`.
    pub fn release_fault(root: &Path) {
        let mut g = lock();
        let Some(s) = g.get_mut(root) else { return };
        if s.paused.take().is_some() {
            s.releases += 1;
            reg().1.notify_all();
        } else {
            s.armed.clear();
        }
    }

    /// Waits (up to 30 s) until a thread is paused under `root`; returns the
    /// fault it stopped at.
    pub fn wait_fault(root: &Path) -> Option<OverlayFault> {
        let end = Instant::now() + Duration::from_secs(30);
        let mut g = lock();
        loop {
            if let Some(f) = g.get(root).and_then(|s| s.paused.clone()) {
                return Some(f);
            }
            let now = Instant::now();
            if now >= end {
                return None;
            }
            g = reg()
                .1
                .wait_timeout(g, end - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    pub(crate) fn hit(root: &Path, pred: impl Fn(&OverlayFault) -> bool) {
        let mut g = lock();
        let Some(s) = g.get_mut(root) else { return };
        let Some(i) = s.armed.iter().position(&pred) else {
            return;
        };
        let f = s.armed.remove(i);
        s.paused = Some(f);
        let seen = s.releases;
        reg().1.notify_all();
        while g.get(root).is_some_and(|s| s.releases == seen) {
            g = reg().1.wait(g).unwrap_or_else(|p| p.into_inner());
        }
    }
}

#[cfg(feature = "test-seams")]
use imp::{OverlayFault as F, hit};

#[inline]
pub(crate) fn after_hash(_root: &Path, _path: &RepoPath) {
    #[cfg(feature = "test-seams")]
    hit(
        _root,
        |f| matches!(f, F::PauseAfterHash { path } if path == _path),
    );
}

#[inline]
pub(crate) fn after_snapshot(_root: &Path) {
    #[cfg(feature = "test-seams")]
    hit(_root, |f| *f == F::PauseAfterSnapshot);
}

#[inline]
pub(crate) fn before_put(_root: &Path) {
    #[cfg(feature = "test-seams")]
    hit(_root, |f| *f == F::PauseBeforePut);
}

#[inline]
pub(crate) fn before_precheck(_root: &Path) {
    #[cfg(feature = "test-seams")]
    hit(_root, |f| *f == F::PauseBeforePrecheck);
}

#[inline]
pub(crate) fn after_plan(_root: &Path) {
    #[cfg(feature = "test-seams")]
    hit(_root, |f| *f == F::PauseAfterPlan);
}

#[inline]
pub(crate) fn before_temp_meta(_root: &Path) {
    #[cfg(feature = "test-seams")]
    hit(_root, |f| *f == F::PauseBeforeTempMeta);
}

/// `steps` apply steps are done.
#[inline]
pub(crate) fn in_apply(_root: &Path, _steps: u32) {
    #[cfg(feature = "test-seams")]
    hit(
        _root,
        |f| matches!(f, F::PauseInApply { after } if *after == _steps),
    );
}

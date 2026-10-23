//! Test seams (store.md §3 `Seam`, `SyncCounts`). The state lives in every
//! `Store`; only `feature = "test-seams"` exposes `arm` and friends, so in a
//! release build nothing is ever armed.
#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

/// See store.md §3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seam {
    BlobRenameFails { errno: i32 },
    TempNameTaken,
    InitBeforeRename,
    AfterCheckpointRow,
    CommitSyncFails,
    PauseBeforeCommit,
    PauseInPut,
    GcStopAfterRetire { n: u64 },
    GcUnlinkFails { errno: i32 },
    AgeFloor { secs: u64 },
    ScrubBudget { bytes: u64 },
    RetirePrepFails { errno: i32 },
    GcBatchRows { rows: u64 },
    GcPauseBeforeRows,
    GcPauseAfterBatch { n: u64 },
}

/// See store.md §3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncCounts {
    pub wal_full: u64,
    pub wal_normal: u64,
    pub blob_barriers: u64,
    pub dir_barriers: u64,
}

#[derive(Default)]
struct State {
    armed: Vec<Seam>,
    age_floor: Option<u64>,
    scrub_budget: Option<u64>,
    batch_rows: Option<u64>,
    paused: bool,
    released: bool,
}

#[derive(Default)]
pub(crate) struct Seams {
    state: Mutex<State>,
    cv: Condvar,
    pub(crate) blob_barriers: AtomicU64,
    pub(crate) dir_barriers: AtomicU64,
}

thread_local! {
    /// Seams for the next `Store::open` on this thread (a store being created
    /// has no `Store` to arm yet).
    static NEXT_OPEN: RefCell<Vec<Seam>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn arm_next_open(s: Seam) {
    NEXT_OPEN.with(|v| v.borrow_mut().push(s));
}

pub(crate) fn take_next_open(want: &Seam) -> bool {
    NEXT_OPEN.with(|v| {
        let mut v = v.borrow_mut();
        if let Some(i) = v.iter().position(|s| s == want) {
            v.remove(i);
            true
        } else {
            false
        }
    })
}

impl Seams {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn arm(&self, s: Seam) {
        let mut st = self.lock();
        match s {
            Seam::AgeFloor { secs } => st.age_floor = Some(secs),
            Seam::ScrubBudget { bytes } => st.scrub_budget = Some(bytes),
            Seam::GcBatchRows { rows } => st.batch_rows = Some(rows),
            other => st.armed.push(other),
        }
    }

    /// Take (disarm) the first armed seam matching `f`.
    pub(crate) fn take<T>(&self, f: impl Fn(&Seam) -> Option<T>) -> Option<T> {
        let mut st = self.lock();
        let i = st.armed.iter().position(|s| f(s).is_some())?;
        let s = st.armed.remove(i);
        f(&s)
    }

    pub(crate) fn hit(&self, want: &Seam) -> bool {
        self.take(|s| (s == want).then_some(())).is_some()
    }

    /// Whether a seam matching `f` is armed (without taking it).
    pub(crate) fn peek(&self, f: impl Fn(&Seam) -> bool) -> bool {
        self.lock().armed.iter().any(f)
    }

    /// Stop here until `release` when `want` is armed.
    pub(crate) fn pause_if(&self, want: &Seam) {
        if !self.hit(want) {
            return;
        }
        self.pause();
    }

    pub(crate) fn pause(&self) {
        let mut st = self.lock();
        st.paused = true;
        st.released = false;
        self.cv.notify_all();
        while !st.released {
            st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner());
        }
        st.paused = false;
        st.released = false;
        self.cv.notify_all();
    }

    pub(crate) fn wait_paused(&self) {
        let mut st = self.lock();
        while !st.paused {
            st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner());
        }
    }

    pub(crate) fn release(&self) {
        let mut st = self.lock();
        st.released = true;
        self.cv.notify_all();
    }

    pub(crate) fn age_floor(&self) -> Option<u64> {
        self.lock().age_floor
    }

    pub(crate) fn scrub_budget(&self) -> Option<u64> {
        self.lock().scrub_budget
    }

    pub(crate) fn batch_rows(&self) -> Option<u64> {
        self.lock().batch_rows
    }

    pub(crate) fn counts(&self) -> SyncCounts {
        SyncCounts {
            wal_full: 0,
            wal_normal: 0,
            blob_barriers: self.blob_barriers.load(Ordering::SeqCst),
            dir_barriers: self.dir_barriers.load(Ordering::SeqCst),
        }
    }
}

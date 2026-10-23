//! The two clocks of confirm.md §5.7.
//!
//! `Instant` on Darwin is `CLOCK_UPTIME_RAW`, which stops while the system sleeps.
//! `CLOCK_MONOTONIC` keeps running during sleep. Token age is the larger of the two,
//! and the prompt deadline runs on `CLOCK_MONOTONIC`.

use std::time::Instant;

pub(crate) trait Clock: Send + Sync {
    /// `Instant::now()` (does not advance during sleep).
    fn instant(&self) -> Instant;
    /// `CLOCK_MONOTONIC` in nanoseconds (advances during sleep).
    fn mono_ns(&self) -> u64;
}

/// Production clock.
pub(crate) struct RealClock;

impl Clock for RealClock {
    fn instant(&self) -> Instant {
        Instant::now()
    }
    fn mono_ns(&self) -> u64 {
        real_mono_ns()
    }
}

pub(crate) fn real_mono_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec; CLOCK_MONOTONIC is always available on Darwin.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime(CLOCK_MONOTONIC) failed");
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

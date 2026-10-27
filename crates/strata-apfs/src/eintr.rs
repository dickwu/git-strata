//! `retry_eintr`: retry a call that failed with `ErrorKind::Interrupted`.
//!
//! On a removable volume macOS holds `open` / `opendir` / `stat` while TCC
//! decides, and returns `EINTR` when that wait's watchdog expires (or when a
//! signal lands). A bare `?` on such an error used to end daemon startup.

use std::io;
use std::time::Duration;

/// Attempts [`retry_eintr`] makes in all (the first call included).
pub const EINTR_ATTEMPTS: u32 = 5;

/// Run `f`; while it fails with `ErrorKind::Interrupted`, run it again, up to
/// [`EINTR_ATTEMPTS`] calls in all with a short pause (1, 2, 4, 8 ms) between
/// them. Any other result (and the last `Interrupted`) is returned as is.
pub fn retry_eintr<T>(f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_eintr_n(EINTR_ATTEMPTS, f)
}

/// [`retry_eintr`] with an explicit attempt count (at least one call).
pub fn retry_eintr_n<T>(attempts: u32, mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let attempts = attempts.max(1);
    let mut n = 1;
    loop {
        match f() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted && n < attempts => {
                std::thread::sleep(Duration::from_millis(1 << (n - 1).min(6)));
                n += 1;
            }
            r => return r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eintr() -> io::Error {
        io::Error::from_raw_os_error(libc::EINTR)
    }

    #[test]
    fn retries_interrupted_then_succeeds() {
        let mut calls = 0;
        let r = retry_eintr(|| {
            calls += 1;
            if calls < 3 { Err(eintr()) } else { Ok(7) }
        });
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls, 3);
    }

    #[test]
    fn gives_up_after_the_cap_with_the_last_error() {
        let mut calls = 0;
        let r: io::Result<()> = retry_eintr(|| {
            calls += 1;
            Err(eintr())
        });
        assert_eq!(r.unwrap_err().raw_os_error(), Some(libc::EINTR));
        assert_eq!(calls, EINTR_ATTEMPTS);
    }

    #[test]
    fn other_errors_are_not_retried() {
        let mut calls = 0;
        let r: io::Result<()> = retry_eintr(|| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EACCES))
        });
        assert_eq!(r.unwrap_err().raw_os_error(), Some(libc::EACCES));
        assert_eq!(calls, 1);
    }

    #[test]
    fn zero_attempts_still_calls_once() {
        let mut calls = 0;
        let _: io::Result<()> = retry_eintr_n(0, || {
            calls += 1;
            Err(eintr())
        });
        assert_eq!(calls, 1);
    }
}

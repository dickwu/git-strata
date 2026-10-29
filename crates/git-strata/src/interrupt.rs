//! rm-followups R2: Ctrl-C (SIGINT) and SIGTERM during `rm`. The handlers
//! only set a flag; the interruptible call looks at it between writes and
//! while it waits, and `run` exits 128 + the signal after the envelope.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Set by the handlers.
pub static STOP: AtomicBool = AtomicBool::new(false);
/// The signal that set `STOP`.
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// The exit code `run` returns instead of 0/1 (0: none).
static EXIT: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNAL.store(sig, Ordering::SeqCst);
    STOP.store(true, Ordering::SeqCst);
}

/// Install the SIGINT and SIGTERM handlers (no SA_RESTART: a blocked
/// write or read returns EINTR and the flag is looked at).
pub fn install() {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: a plain sigaction with an async-signal-safe handler
        // (two atomic stores).
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0;
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// 130 for SIGINT, 143 for SIGTERM; remembered for `run`.
pub fn exit_with_signal() -> i32 {
    let sig = match SIGNAL.load(Ordering::SeqCst) {
        0 => libc::SIGINT,
        s => s,
    };
    let code = 128 + sig;
    EXIT.store(code, Ordering::SeqCst);
    code
}

/// The exit code an interrupted command chose, or 128 + the signal when
/// one was caught later (while the reply was parsed or the envelope
/// written): a caught signal always shows in the exit code.
pub fn exit_override() -> Option<i32> {
    match EXIT.load(Ordering::SeqCst) {
        0 if STOP.load(Ordering::SeqCst) => Some(exit_with_signal()),
        0 => None,
        c => Some(c),
    }
}

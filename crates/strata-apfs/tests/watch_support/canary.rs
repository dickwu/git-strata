//! An FSEvents liveness canary that is independent of strata's Watcher.
//!
//! Measured on macOS 27.0.1 (impl-notes "## apfs", watch under load): while
//! the machine floods fseventsd (parallel cargo builds, file churn), streams
//! can receive nothing for minutes and events written meanwhile can be lost
//! with no `UserDropped`/`KernelDropped` flag. A plain C FSEvents client saw
//! the same. A watch test that misses an event therefore asks this canary
//! whether fseventsd was delivering at the time; if not, the test is skipped
//! (reported on stderr) instead of failed.
//!
//! The canary is a raw FSEvents stream (own FFI, not `strata_apfs::watch`)
//! on a private directory, with a thread writing a new file every 200 ms.

use std::collections::HashSet;
use std::ffi::{CStr, c_char, c_void};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

type CFRef = *const c_void;
type Callback = extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithBytes(alloc: CFRef, bytes: *const u8, len: isize, enc: u32, external: u8) -> CFRef;
    fn CFArrayCreate(alloc: CFRef, values: *const CFRef, n: isize, cb: *const c_void) -> CFRef;
}
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(alloc: CFRef, cb: Callback, ctx: *const c_void, paths: CFRef, since: u64, latency: f64, flags: u32) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(s: *mut c_void, q: *mut c_void);
    fn FSEventStreamStart(s: *mut c_void) -> u8;
}
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
}

const POKE: Duration = Duration::from_millis(200);

/// (written at, path) for every poke.
static WRITTEN: Mutex<Vec<(Instant, PathBuf)>> = Mutex::new(Vec::new());
static DELIVERED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
static DIR: OnceLock<PathBuf> = OnceLock::new();

extern "C" fn on_events(_: *mut c_void, _: *mut c_void, n: usize, paths: *mut c_void, _: *const u32, _: *const u64) {
    let paths = paths as *const *const c_char;
    let mut d = DELIVERED.lock().unwrap_or_else(|e| e.into_inner());
    let set = d.get_or_insert_with(HashSet::new);
    for i in 0..n {
        // SAFETY: FSEvents passes `n` NUL-terminated C strings (no
        // kFSEventStreamCreateFlagUseCFTypes).
        let p = unsafe { CStr::from_ptr(*paths.add(i)) };
        set.insert(PathBuf::from(std::ffi::OsStr::from_bytes(p.to_bytes())));
    }
}

extern "C" fn cleanup() {
    if let Some(d) = DIR.get() {
        let _ = fs::remove_dir_all(d);
    }
}

/// Start the canary once per process (idempotent).
pub fn start() {
    DIR.get_or_init(|| {
        let tmp = fs::canonicalize(std::env::temp_dir()).unwrap();
        // Remove canary dirs left by processes that died without atexit.
        if let Ok(rd) = fs::read_dir(&tmp) {
            for e in rd.flatten() {
                let name = e.file_name();
                let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("strata-apfs-canary-")).and_then(|p| p.parse::<i32>().ok()) else {
                    continue;
                };
                // SAFETY: kill with signal 0 only checks for existence.
                if unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        let dir = tmp.join(format!("strata-apfs-canary-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bytes = dir.as_os_str().as_bytes();
        // SAFETY: plain CoreFoundation/FSEvents calls with valid arguments; the
        // stream and queue live for the rest of the process on purpose.
        unsafe {
            let s = CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 0x0800_0100, 0);
            let arr = CFArrayCreate(std::ptr::null(), &s, 1, &kCFTypeArrayCallBacks);
            // SinceNow, latency 0.05, NoDefer | FileEvents.
            let st = FSEventStreamCreate(std::ptr::null(), on_events, std::ptr::null(), arr, u64::MAX, 0.05, 0x2 | 0x10);
            assert!(!st.is_null());
            FSEventStreamSetDispatchQueue(st, dispatch_queue_create(c"strata-test-canary".as_ptr(), std::ptr::null()));
            assert!(FSEventStreamStart(st) != 0);
            libc::atexit(cleanup);
        }
        let d = dir.clone();
        thread::spawn(move || {
            for i in 0u64.. {
                let p = d.join(format!("p{i}"));
                let at = Instant::now();
                if fs::write(&p, b"").is_ok() {
                    WRITTEN.lock().unwrap_or_else(|e| e.into_inner()).push((at, p));
                }
                if i >= 2 {
                    let _ = fs::remove_file(d.join(format!("p{}", i - 2)));
                }
                thread::sleep(POKE);
            }
        });
        dir
    });
}

/// Was fseventsd delivering between `since` and now? Every canary poke
/// written in the first half of the window must have arrived by now (so the
/// canary is granted half the window as latency). `None` = no poke was
/// written in that half (cannot tell).
pub fn delivering_since(since: Instant) -> Option<bool> {
    let now = Instant::now();
    let half = since + (now - since) / 2;
    let written: Vec<PathBuf> =
        WRITTEN.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(t, _)| *t >= since && *t <= half).map(|(_, p)| p.clone()).collect();
    if written.is_empty() {
        return None;
    }
    let d = DELIVERED.lock().unwrap_or_else(|e| e.into_inner());
    let set = d.as_ref();
    Some(written.iter().all(|p| set.is_some_and(|s| s.contains(p))))
}

/// Panic payload for "fseventsd was not delivering: skip this test".
pub struct Starved(pub String);

/// Called when an expected delivery did not happen within its window that
/// started at `since`: skip (fseventsd starved) or fail (real miss).
#[track_caller]
pub fn starved_or_fail(since: Instant, msg: String) -> ! {
    match delivering_since(since) {
        Some(true) => panic!("{msg}\n(the independent FSEvents canary was delivering during this window: a real miss)"),
        Some(false) => std::panic::panic_any(Starved(format!("canary pokes written after the wait began were not delivered within {:?}: {msg}", (Instant::now() - since) / 2))),
        None => std::panic::panic_any(Starved(format!("no canary poke written in the window (poker starved): {msg}"))),
    }
}

/// Run a watch test; a `Starved` panic (anywhere in it) becomes a skip.
pub fn run(name: &str, f: impl FnOnce()) {
    start();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(()) => {}
        Err(p) => match p.downcast::<Starved>() {
            Ok(s) => {
                // Straight to fd 2 so libtest's output capture cannot hide
                // a skip in a passing run.
                let line = format!("SKIPPED {name}: fseventsd starved (machine under file-event load): {}\n", s.0);
                // SAFETY: write(2) of a valid buffer.
                unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
                println!("SKIPPED {name}");
            }
            Err(p) => std::panic::resume_unwind(p),
        },
    }
}

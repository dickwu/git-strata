//! FSEvents watcher (§5.7). Hints only: never claims completeness.

mod translate;

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use strata_types::{Code, RepoPath, StrataError};

use crate::error::{errno_error, refusal};
use crate::{Result, WATCH_LATENCY_SECS};
use translate::Translator;

/// Watch events. The callback runs on the Watcher's private serial queue:
/// it must not block or panic, must not `flush` any Watcher and must not
/// synchronously drop any other Watcher (see `Watcher`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// These absolute paths changed (a directory means its whole subtree).
    /// Prefixed with the root passed to `watch`; the rest is the disk spelling.
    Changed(Vec<PathBuf>),
    /// The kernel coalesced this subtree's events: rescan it.
    MustRescan(PathBuf),
    /// Events may have been lost: rescan everything.
    Dropped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchOptions {
    /// Directories under root whose events are dropped in the callback.
    pub exclude: Vec<RepoPath>,
}

// ───────── FFI ─────────

type CFRef = *const c_void;
type StreamRef = *mut c_void;
type Queue = *mut c_void;

#[repr(C)]
struct StreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type Callback = extern "C" fn(StreamRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithBytes(alloc: CFRef, bytes: *const u8, len: isize, enc: u32, external: u8) -> CFRef;
    fn CFArrayCreate(alloc: CFRef, values: *const CFRef, n: isize, cb: *const c_void) -> CFRef;
    fn CFRelease(cf: CFRef);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(alloc: CFRef, cb: Callback, ctx: *const StreamContext, paths: CFRef, since: u64, latency: f64, flags: u32) -> StreamRef;
    fn FSEventStreamSetDispatchQueue(s: StreamRef, q: Queue);
    fn FSEventStreamStart(s: StreamRef) -> u8;
    fn FSEventStreamStop(s: StreamRef);
    fn FSEventStreamInvalidate(s: StreamRef);
    fn FSEventStreamRelease(s: StreamRef);
    fn FSEventStreamFlushAsync(s: StreamRef) -> u64;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> Queue;
    fn dispatch_sync_f(q: Queue, ctx: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_async_f(q: Queue, ctx: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_release(obj: *mut c_void);
}

const UTF8: u32 = 0x0800_0100;
const SINCE_NOW: u64 = u64::MAX;
const CREATE_NO_DEFER: u32 = 0x2;
const CREATE_FILE_EVENTS: u32 = 0x10;

// ───────── state ─────────

thread_local! {
    /// True while any strata Watcher callback runs on this thread.
    static IN_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

fn in_callback() -> bool {
    IN_CALLBACK.with(|c| c.get())
}

struct InCallback(bool);
impl InCallback {
    fn enter() -> InCallback {
        InCallback(IN_CALLBACK.with(|c| c.replace(true)))
    }
}
impl Drop for InCallback {
    fn drop(&mut self) {
        IN_CALLBACK.with(|c| c.set(self.0));
    }
}

/// Heap state behind `info`; owned by the Watcher alone (freed exactly once).
struct State {
    closed: AtomicBool,
    /// Latched on the first `RootChanged`: the stream is path-based and would
    /// otherwise report a new directory created at the old path (measured),
    /// which is not the directory this Watcher was opened on.
    root_gone: AtomicBool,
    /// The root as opened: path, device and inode. Incident 2026-10-27:
    /// `kFSEventStreamCreateFlagWatchRoot` cost a kqueue plus an event-only
    /// descriptor on the root and every ancestor (about 8 per workspace on
    /// an external APFS volume, 15 under /var/folders), enough to run the daemon
    /// out of descriptors. Instead each delivered batch checks that the
    /// path still names the same directory.
    root: (CString, u64, u64),
    on_event: Box<dyn Fn(WatchEvent) + Send>,
    tr: Translator,
}

impl State {
    /// The root path still names the directory the stream was opened on.
    fn root_is_same(&self) -> bool {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and a local stat buffer.
        if unsafe { libc::lstat(self.root.0.as_ptr(), &mut st) } != 0 {
            return std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR);
        }
        st.st_dev as u64 == self.root.1 && st.st_ino as u64 == self.root.2
    }

    fn deliver(&self, ev: WatchEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if catch_unwind(AssertUnwindSafe(|| (self.on_event)(ev))).is_err() {
            eprintln!("strata-apfs: watch callback panicked; reporting Dropped");
            if !self.closed.load(Ordering::Acquire) {
                let _ = catch_unwind(AssertUnwindSafe(|| (self.on_event)(WatchEvent::Dropped)));
            }
        }
    }
}

extern "C" fn callback(_s: StreamRef, info: *mut c_void, n: usize, paths: *mut c_void, flags: *const u32, _ids: *const u64) {
    let _g = InCallback::enter();
    // SAFETY: `info` is the State the Watcher keeps alive until after the
    // queue has drained (or until a block queued behind this callback).
    let st = unsafe { &*(info as *const State) };
    if st.closed.load(Ordering::Acquire) || st.root_gone.load(Ordering::Acquire) {
        return;
    }
    let events = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: FSEvents passes `n` C strings and `n` flags (no UseCFTypes).
        let paths = unsafe { std::slice::from_raw_parts(paths as *const *const c_char, n) };
        let flags = unsafe { std::slice::from_raw_parts(flags, n) };
        if flags.iter().any(|f| f & translate::ROOT_CHANGED != 0) || !st.root_is_same() {
            st.root_gone.store(true, Ordering::Release);
            return vec![WatchEvent::Dropped];
        }
        st.tr.translate(flags.iter().zip(paths).map(|(&f, &p)| (f, unsafe { CStr::from_ptr(p) }.to_bytes())))
    }))
    .unwrap_or_else(|_| vec![WatchEvent::Dropped]);
    for ev in events {
        st.deliver(ev);
    }
}

extern "C" fn noop(_: *mut c_void) {}

struct Finish {
    stream: StreamRef,
    state: *mut State,
    queue: Queue,
}

extern "C" fn finish(ctx: *mut c_void) {
    // SAFETY: produced by Box::into_raw in Watcher::drop; runs once.
    let f = unsafe { Box::from_raw(ctx as *mut Finish) };
    unsafe {
        FSEventStreamRelease(f.stream);
        drop(Box::from_raw(f.state));
        dispatch_release(f.queue);
    }
}

/// Watch handle; dropping it stops the stream. Sole owner of the callback
/// state: the closure and its captures are dropped exactly once, in `drop`
/// (or, when dropped inside any Watcher callback, in a block queued right
/// behind the running callback).
pub struct Watcher {
    stream: StreamRef,
    queue: Queue,
    state: *mut State,
    flush_lock: Mutex<()>,
}

// SAFETY: the raw handles are thread-safe CF/dispatch objects; the state is
// only touched by the serial queue, or after it has drained.
unsafe impl Send for Watcher {}
unsafe impl Sync for Watcher {}

impl Watcher {
    /// `FSEventStreamFlushAsync`, then `dispatch_sync` an empty block on the
    /// private queue (not `FlushSync`: on macOS 26 it blocks for its ~1 s
    /// internal timeout on a few percent of idle calls, see impl-notes). Guarantees only that callbacks queued before the drain
    /// have finished; it is **not** a fence (§4.2 F). A no-op inside any
    /// Watcher callback (`debug_assert!` in debug builds).
    pub fn flush(&self) {
        if in_callback() {
            debug_assert!(false, "Watcher::flush called from inside a Watcher callback");
            return;
        }
        let _g = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            FSEventStreamFlushAsync(self.stream);
            dispatch_sync_f(self.queue, std::ptr::null_mut(), noop);
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            (*self.state).closed.store(true, Ordering::Release);
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            if in_callback() {
                let f = Box::new(Finish { stream: self.stream, state: self.state, queue: self.queue });
                dispatch_async_f(self.queue, Box::into_raw(f) as *mut c_void, finish);
            } else {
                FSEventStreamRelease(self.stream);
                dispatch_sync_f(self.queue, std::ptr::null_mut(), noop);
                drop(Box::from_raw(self.state));
                dispatch_release(self.queue);
            }
        }
    }
}

pub fn watch(root: &Path, on_event: impl Fn(WatchEvent) + Send + 'static) -> Result<Watcher> {
    watch_with(root, &WatchOptions::default(), on_event)
}

pub fn watch_with(root: &Path, opts: &WatchOptions, on_event: impl Fn(WatchEvent) + Send + 'static) -> Result<Watcher> {
    if !root.is_absolute() {
        return Err(refusal(Code::InvalidInput, root, "root_not_canonical", "watch root must be an absolute path"));
    }
    let md = std::fs::metadata(root).map_err(|e| match e.raw_os_error() {
        Some(libc::ENOENT) => errno_error(Code::NoSuchPath, libc::ENOENT, root, None),
        Some(n) => crate::error::classify_errno(n, crate::IoSide::Read, root),
        None => StrataError::new(Code::Unknown, format!("{}: {e}", root.display())),
    })?;
    if !md.is_dir() {
        return Err(errno_error(Code::NotDirectory, libc::ENOTDIR, root, Some("not_a_directory")));
    }
    let canonical = std::fs::canonicalize(root).map_err(|e| crate::classify_io_error(&e, crate::IoSide::Read, root))?;
    let case_sensitive = crate::volume_caps(&canonical)?.case_sensitive;
    let state = Box::into_raw(Box::new(State {
        closed: AtomicBool::new(false),
        root_gone: AtomicBool::new(false),
        root: {
            use std::os::unix::fs::MetadataExt;
            let cm = std::fs::symlink_metadata(&canonical).map_err(|e| crate::classify_io_error(&e, crate::IoSide::Read, root))?;
            let c = CString::new(canonical.as_os_str().as_bytes()).map_err(|_| refusal(Code::InvalidInput, root, "nul_in_path", "watch root has a NUL byte"))?;
            (c, cm.dev(), cm.ino())
        },
        on_event: Box::new(on_event),
        tr: Translator::new(&canonical, root, &opts.exclude, case_sensitive),
    }));
    let fail = |msg: &str| StrataError::new(Code::Unknown, format!("{}: {msg}", root.display()));
    unsafe {
        let bytes = canonical.as_os_str().as_bytes();
        let s = CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, UTF8, 0);
        if s.is_null() {
            drop(Box::from_raw(state));
            return Err(refusal(Code::InvalidInput, root, "not_utf8", "watch root is not UTF-8"));
        }
        let arr = CFArrayCreate(std::ptr::null(), &s, 1, &kCFTypeArrayCallBacks as *const c_void);
        CFRelease(s);
        let ctx = StreamContext {
            version: 0,
            info: state as *mut c_void,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        let flags = CREATE_FILE_EVENTS | CREATE_NO_DEFER;
        let stream = FSEventStreamCreate(std::ptr::null(), callback, &ctx, arr, SINCE_NOW, WATCH_LATENCY_SECS, flags);
        CFRelease(arr);
        if stream.is_null() {
            drop(Box::from_raw(state));
            return Err(fail("FSEventStreamCreate failed"));
        }
        let queue = dispatch_queue_create(c"dev.git-strata.apfs.watch".as_ptr(), std::ptr::null());
        FSEventStreamSetDispatchQueue(stream, queue);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            dispatch_sync_f(queue, std::ptr::null_mut(), noop);
            drop(Box::from_raw(state));
            dispatch_release(queue);
            return Err(fail("FSEventStreamStart failed"));
        }
        Ok(Watcher { stream, queue, state, flush_lock: Mutex::new(()) })
    }
}

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<Watcher>();
};

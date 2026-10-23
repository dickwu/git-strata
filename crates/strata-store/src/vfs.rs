//! Test-seams only: an instrumented Turso I/O backend. It wraps the
//! platform I/O and, for every WAL file (`<db>-wal`), counts `sync` calls by
//! kind (F_FULLFSYNC vs plain fsync) and can fail the next one
//! (`Seam::CommitSyncFails`). Everything else is forwarded untouched.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use turso::core::io::{FileId, FileSyncType, SharedWalLockKind, SharedWalMappedRegion};
use turso::core::{Buffer, Clock, Completion, File, MemoryIO, MonotonicInstant, OpenFlags, PlatformIO, WallClockInstant, IO};

type TResult<T> = std::result::Result<T, turso::core::LimboError>;

/// Per-database counters, keyed by the main database's path.
#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) wal_full: AtomicU64,
    pub(crate) wal_normal: AtomicU64,
    pub(crate) fail_next: AtomicBool,
}

fn registry() -> &'static Mutex<HashMap<String, Arc<Counters>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<Counters>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// The counters for the database at `path` (`Connection::path()`).
pub(crate) fn counters(path: &str) -> Arc<Counters> {
    registry().lock().unwrap_or_else(|p| p.into_inner()).entry(path.to_string()).or_default().clone()
}

/// The process-wide instrumented I/O.
pub(crate) fn io() -> Arc<dyn IO> {
    static IO_: OnceLock<Arc<InstrIo>> = OnceLock::new();
    IO_.get_or_init(|| Arc::new(InstrIo { base: Arc::new(PlatformIO::new().expect("platform io")) })).clone()
}

struct InstrIo {
    base: Arc<dyn IO>,
}

impl InstrIo {
    fn wrap(&self, path: &str, f: Arc<dyn File>) -> Arc<dyn File> {
        match path.strip_suffix("-wal") {
            Some(db) => Arc::new(InstrFile { inner: f, ctr: counters(db) }),
            None => f,
        }
    }
}

impl Clock for InstrIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.base.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.base.current_time_wall_clock()
    }
}

impl IO for InstrIo {
    fn open_file(&self, path: &str, flags: OpenFlags, direct: bool) -> TResult<Arc<dyn File>> {
        let f = self.base.open_file(path, flags, direct)?;
        Ok(self.wrap(path, f))
    }
    fn open_shared_wal_file(&self, path: &str) -> TResult<Arc<dyn File>> {
        self.base.open_shared_wal_file(path)
    }
    fn remove_file(&self, path: &str) -> TResult<()> {
        self.base.remove_file(path)
    }
    fn supports_shared_wal_coordination(&self) -> bool {
        self.base.supports_shared_wal_coordination()
    }
    fn step(&self) -> TResult<()> {
        self.base.step()
    }
    fn cancel(&self, c: &[Completion]) -> TResult<()> {
        self.base.cancel(c)
    }
    fn drain_completions(&self, c: &[Completion]) -> TResult<()> {
        self.base.drain_completions(c)
    }
    fn wait_for_completion(&self, c: Completion) -> TResult<()> {
        self.base.wait_for_completion(c)
    }
    fn generate_random_number(&self) -> i64 {
        self.base.generate_random_number()
    }
    fn fill_bytes(&self, dest: &mut [u8]) {
        self.base.fill_bytes(dest)
    }
    fn get_memory_io(&self) -> Arc<MemoryIO> {
        self.base.get_memory_io()
    }
    fn register_fixed_buffer(&self, ptr: NonNull<u8>, len: usize) -> TResult<u32> {
        self.base.register_fixed_buffer(ptr, len)
    }
    fn yield_now(&self) {
        self.base.yield_now()
    }
    fn sleep(&self, d: std::time::Duration) {
        self.base.sleep(d)
    }
    fn file_id(&self, path: &str) -> TResult<FileId> {
        self.base.file_id(path)
    }
}

struct InstrFile {
    inner: Arc<dyn File>,
    ctr: Arc<Counters>,
}

impl File for InstrFile {
    fn lock_file(&self, exclusive: bool) -> TResult<()> {
        self.inner.lock_file(exclusive)
    }
    fn unlock_file(&self) -> TResult<()> {
        self.inner.unlock_file()
    }
    fn pread(&self, pos: u64, c: Completion) -> TResult<Completion> {
        self.inner.pread(pos, c)
    }
    fn pwrite(&self, pos: u64, b: Arc<Buffer>, c: Completion) -> TResult<Completion> {
        self.inner.pwrite(pos, b, c)
    }
    fn pwritev(&self, pos: u64, b: Vec<Arc<Buffer>>, c: Completion) -> TResult<Completion> {
        self.inner.pwritev(pos, b, c)
    }
    fn sync(&self, c: Completion, t: FileSyncType) -> TResult<Completion> {
        if self.ctr.fail_next.swap(false, Ordering::SeqCst) {
            return Err(turso::core::io_error(std::io::Error::from_raw_os_error(libc::EIO), "sync"));
        }
        match t {
            FileSyncType::FullFsync => self.ctr.wal_full.fetch_add(1, Ordering::SeqCst),
            FileSyncType::Fsync => self.ctr.wal_normal.fetch_add(1, Ordering::SeqCst),
        };
        self.inner.sync(c, t)
    }
    fn size(&self) -> TResult<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, c: Completion) -> TResult<Completion> {
        self.inner.truncate(len, c)
    }
    fn has_hole(&self, pos: usize, len: usize) -> TResult<bool> {
        self.inner.has_hole(pos, len)
    }
    fn punch_hole(&self, pos: usize, len: usize) -> TResult<()> {
        self.inner.punch_hole(pos, len)
    }
    fn shared_wal_lock_byte(&self, o: u64, x: bool, k: SharedWalLockKind) -> TResult<()> {
        self.inner.shared_wal_lock_byte(o, x, k)
    }
    fn shared_wal_try_lock_byte(&self, o: u64, x: bool, k: SharedWalLockKind) -> TResult<bool> {
        self.inner.shared_wal_try_lock_byte(o, x, k)
    }
    fn shared_wal_probe_exclusive_byte(&self, o: u64, k: SharedWalLockKind) -> TResult<bool> {
        self.inner.shared_wal_probe_exclusive_byte(o, k)
    }
    fn shared_wal_probe_exclusive_while_shared_byte(&self, o: u64, k: SharedWalLockKind) -> TResult<bool> {
        self.inner.shared_wal_probe_exclusive_while_shared_byte(o, k)
    }
    fn shared_wal_unlock_byte(&self, o: u64, k: SharedWalLockKind) -> TResult<()> {
        self.inner.shared_wal_unlock_byte(o, k)
    }
    fn shared_wal_set_len(&self, len: u64) -> TResult<()> {
        self.inner.shared_wal_set_len(len)
    }
    fn shared_wal_map(&self, o: u64, len: usize) -> TResult<Box<dyn SharedWalMappedRegion>> {
        self.inner.shared_wal_map(o, len)
    }
}

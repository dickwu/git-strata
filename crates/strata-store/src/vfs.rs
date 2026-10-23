//! The instrumented VFS (store.md §8 Task 8; `feature = "test-seams"` only).
//!
//! A shim named `strata-instr` that wraps SQLite's default `unix` VFS. Every
//! store connection opens through it, so tests can count `xSync` calls on the
//! WAL by `flags & 0x0F` (`SyncCounts::wal_full` / `wal_normal`) and make the
//! next WAL `xSync` fail with `SQLITE_IOERR_FSYNC` (`Seam::CommitSyncFails`).
//! Everything else is forwarded unchanged to the real VFS and file.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::ffi;

pub(crate) const NAME: &str = "strata-instr";
const NAME_C: &CStr = c"strata-instr";

/// Per-database counters, keyed by the main database's full path.
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

/// The counters for the database at `path` (as SQLite names it, i.e.
/// `Connection::path()`).
pub(crate) fn counters(path: &str) -> Arc<Counters> {
    registry().lock().unwrap_or_else(|p| p.into_inner()).entry(path.to_string()).or_default().clone()
}

#[repr(C)]
struct IFile {
    base: ffi::sqlite3_file,
    real: *mut ffi::sqlite3_file,
    ctr: *const Counters,
}

const HDR: usize = (size_of::<IFile>() + 7) & !7;

struct Real(*mut ffi::sqlite3_vfs);
unsafe impl Send for Real {}
unsafe impl Sync for Real {}
static REAL: OnceLock<Real> = OnceLock::new();

/// Register the shim once per process (not as the default VFS).
pub(crate) fn ensure_registered() {
    REAL.get_or_init(|| unsafe {
        let real = ffi::sqlite3_vfs_find(ptr::null());
        assert!(!real.is_null(), "no default SQLite VFS");
        let mut v: ffi::sqlite3_vfs = *real;
        v.zName = NAME_C.as_ptr();
        v.pNext = ptr::null_mut();
        v.szOsFile = (HDR as c_int) + (*real).szOsFile;
        v.xOpen = Some(x_open);
        let v: &'static mut ffi::sqlite3_vfs = Box::leak(Box::new(v));
        let rc = ffi::sqlite3_vfs_register(v, 0);
        assert_eq!(rc, ffi::SQLITE_OK, "register the instrumented VFS");
        Real(real)
    });
}

unsafe fn file(f: *mut ffi::sqlite3_file) -> &'static mut IFile {
    unsafe { &mut *(f as *mut IFile) }
}

unsafe fn rm(f: *mut ffi::sqlite3_file) -> (*mut ffi::sqlite3_file, &'static ffi::sqlite3_io_methods) {
    unsafe {
        let r = file(f).real;
        (r, &*(*r).pMethods)
    }
}

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    zName: ffi::sqlite3_filename,
    f: *mut ffi::sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    unsafe {
        let real_vfs = REAL.get().unwrap().0;
        let real = (f as *mut u8).add(HDR) as *mut ffi::sqlite3_file;
        let me = file(f);
        me.base.pMethods = ptr::null();
        me.real = real;
        me.ctr = ptr::null();
        (*real).pMethods = ptr::null();
        let rc = ((*real_vfs).xOpen.unwrap())(real_vfs, zName, real, flags, out);
        if (*real).pMethods.is_null() {
            return rc;
        }
        if flags & ffi::SQLITE_OPEN_WAL != 0 && !zName.is_null() {
            let db = ffi::sqlite3_filename_database(zName);
            if !db.is_null() {
                let key = CStr::from_ptr(db as *const c_char).to_string_lossy().into_owned();
                me.ctr = Arc::into_raw(counters(&key));
            }
        }
        me.base.pMethods = &METHODS;
        rc
    }
}

unsafe extern "C" fn x_close(f: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let (r, m) = rm(f);
        let rc = (m.xClose.unwrap())(r);
        let me = file(f);
        if !me.ctr.is_null() {
            drop(Arc::from_raw(me.ctr));
            me.ctr = ptr::null();
        }
        rc
    }
}

unsafe extern "C" fn x_sync(f: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    unsafe {
        let me = file(f);
        if !me.ctr.is_null() {
            let c = &*me.ctr;
            if c.fail_next.swap(false, Ordering::SeqCst) {
                return ffi::SQLITE_IOERR_FSYNC;
            }
            match flags & 0x0F {
                ffi::SQLITE_SYNC_FULL => c.wal_full.fetch_add(1, Ordering::SeqCst),
                ffi::SQLITE_SYNC_NORMAL => c.wal_normal.fetch_add(1, Ordering::SeqCst),
                _ => 0,
            };
        }
        let (r, m) = rm(f);
        (m.xSync.unwrap())(r, flags)
    }
}

macro_rules! fwd {
    ($name:ident, $field:ident, ($($a:ident : $t:ty),*) -> $ret:ty, $missing:expr) => {
        unsafe extern "C" fn $name(f: *mut ffi::sqlite3_file $(, $a: $t)*) -> $ret {
            unsafe {
                let (r, m) = rm(f);
                match m.$field {
                    Some(x) => x(r $(, $a)*),
                    None => $missing,
                }
            }
        }
    };
}

fwd!(x_read, xRead, (b: *mut c_void, n: c_int, o: ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_write, xWrite, (b: *const c_void, n: c_int, o: ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_truncate, xTruncate, (s: ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_file_size, xFileSize, (p: *mut ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_lock, xLock, (l: c_int) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_unlock, xUnlock, (l: c_int) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_check_reserved, xCheckReservedLock, (p: *mut c_int) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_file_control, xFileControl, (op: c_int, p: *mut c_void) -> c_int, ffi::SQLITE_NOTFOUND);
fwd!(x_sector_size, xSectorSize, () -> c_int, 4096);
fwd!(x_device_chars, xDeviceCharacteristics, () -> c_int, 0);
fwd!(x_shm_map, xShmMap, (pg: c_int, sz: c_int, e: c_int, pp: *mut *mut c_void) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_shm_lock, xShmLock, (o: c_int, n: c_int, fl: c_int) -> c_int, ffi::SQLITE_IOERR);
fwd!(x_shm_barrier, xShmBarrier, () -> (), ());
fwd!(x_shm_unmap, xShmUnmap, (d: c_int) -> c_int, ffi::SQLITE_OK);
fwd!(x_fetch, xFetch, (o: ffi::sqlite3_int64, n: c_int, pp: *mut *mut c_void) -> c_int, {
    *pp = ptr::null_mut();
    ffi::SQLITE_OK
});
fwd!(x_unfetch, xUnfetch, (o: ffi::sqlite3_int64, p: *mut c_void) -> c_int, ffi::SQLITE_OK);

static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 3,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_chars),
    xShmMap: Some(x_shm_map),
    xShmLock: Some(x_shm_lock),
    xShmBarrier: Some(x_shm_barrier),
    xShmUnmap: Some(x_shm_unmap),
    xFetch: Some(x_fetch),
    xUnfetch: Some(x_unfetch),
};

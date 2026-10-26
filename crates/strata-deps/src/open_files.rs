//! Open files under a directory (§5.11): fds, mapped regions and cwd/root of
//! every same-uid process, through libproc.

use std::ffi::CStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_types::{Code, StrataError};

use crate::Result;

/// A process's hold on a file under some directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    pub pid: i32,
    /// Process name from `proc_name`, for people.
    pub command: String,
    /// Absolute path of the held file or directory, as the kernel reports it.
    pub path: PathBuf,
    pub hold: Hold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hold {
    /// An open file descriptor.
    Fd,
    /// A file mapped into memory (dlopen'd `.node` module, mmap).
    Mapped,
    /// The process's current or root directory is inside.
    Cwd,
}

// ---- libproc pieces libc does not define (sys/proc_info.h) ----

const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
/// Like PROC_PIDREGIONPATHINFO (8) but only vnode-backed regions: the same
/// answer for "mapped files" at a tenth of the syscalls (~0.3 s vs 2.4 s here).
const PROC_PIDREGIONPATHINFO2: libc::c_int = 22;

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: i64,
    fi_type: i32,
    fi_guardflags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcRegionInfo {
    pri_protection: u32,
    pri_max_protection: u32,
    pri_inheritance: u32,
    pri_flags: u32,
    pri_offset: u64,
    pri_behavior: u32,
    pri_user_wired_count: u32,
    pri_user_tag: u32,
    pri_pages_resident: u32,
    pri_pages_shared_now_private: u32,
    pri_pages_swapped_out: u32,
    pri_pages_dirtied: u32,
    pri_ref_count: u32,
    pri_shadow_depth: u32,
    pri_share_mode: u32,
    pri_private_pages_resident: u32,
    pri_shared_pages_resident: u32,
    pri_obj_id: u32,
    pri_depth: u32,
    pri_address: u64,
    pri_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcRegionWithPathInfo {
    prp_prinfo: ProcRegionInfo,
    prp_vip: libc::vnode_info_path,
}

const _: () = assert!(std::mem::size_of::<ProcFileInfo>() == 24);
const _: () = assert!(std::mem::size_of::<ProcRegionInfo>() == 96);

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn scan_error(what: &str, pid: i32, e: i32) -> StrataError {
    StrataError::new(Code::Unknown, format!("open-file scan failed: {what} (pid {pid}): errno {e}"))
        .with_detail(json!({ "reason": "open_files_scan", "call": what, "pid": pid, "errno": e }))
}

fn vip_path(v: &libc::vnode_info_path) -> &[u8] {
    // SAFETY: vip_path is MAXPATHLEN c_chars laid out contiguously.
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(v.vip_path.as_ptr() as *const u8, std::mem::size_of_val(&v.vip_path)) };
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..n]
}

/// `dir`'s canonical path from the kernel, or `None` when it cannot hold
/// anything (missing, not a directory, a symlink).
fn canonical_dir(dir: &Path) -> Result<Option<Vec<u8>>> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
    {
        Ok(f) => f,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)) => return Ok(None),
        Err(e) => {
            return Err(StrataError::new(Code::Unknown, format!("open {}: {e}", dir.display())).with_detail(
                json!({ "reason": "open_files_scan", "path": dir.to_string_lossy(), "errno": e.raw_os_error() }),
            ));
        }
    };
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes into buf.
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } == -1 {
        let e = errno();
        return Err(StrataError::new(Code::Unknown, format!("F_GETPATH {}: errno {e}", dir.display()))
            .with_detail(json!({ "reason": "open_files_scan", "path": dir.to_string_lossy(), "errno": e })));
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(n);
    Ok(Some(buf))
}

/// `path` is `c` or below it, by bytes.
fn is_under(path: &[u8], c: &[u8]) -> bool {
    if path == c {
        return true;
    }
    if c.ends_with(b"/") {
        return path.starts_with(c);
    }
    path.len() > c.len() && path.starts_with(c) && path[c.len()] == b'/'
}

fn all_pids() -> Result<Vec<i32>> {
    let mut cap = 4096usize;
    loop {
        let mut buf = vec![0i32; cap];
        // SAFETY: buffer of cap ints.
        let n = unsafe {
            libc::proc_listallpids(buf.as_mut_ptr() as *mut libc::c_void, (cap * std::mem::size_of::<i32>()) as i32)
        };
        if n < 0 {
            return Err(scan_error("proc_listallpids", 0, errno()));
        }
        let n = n as usize;
        if n >= cap {
            cap *= 2;
            continue;
        }
        buf.truncate(n);
        buf.retain(|&p| p > 0);
        return Ok(buf);
    }
}

/// Every (path, hold, event-only) of process `pid`; `None` when the process
/// is gone or not ours to see. The flag is set on `O_EVTONLY` descriptors.
type Holds = Vec<(Vec<u8>, Hold, bool)>;

fn holds_of(pid: i32, keep: &dyn Fn(&[u8]) -> bool, with_own_evt: bool) -> Result<Option<Holds>> {
    let mut out = Vec::new();
    let skip = |e: i32| e == libc::ESRCH || e == libc::EPERM;

    // cwd and root directory.
    let mut vpi: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    // SAFETY: buffer is a proc_vnodepathinfo.
    let n = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, &mut vpi as *mut _ as *mut libc::c_void, size)
    };
    if n <= 0 {
        let e = errno();
        if skip(e) {
            return Ok(None);
        }
        return Err(scan_error("PROC_PIDVNODEPATHINFO", pid, e));
    }
    for p in [vip_path(&vpi.pvi_cdir), vip_path(&vpi.pvi_rdir)] {
        if !p.is_empty() && keep(p) {
            out.push((p.to_vec(), Hold::Cwd, false));
        }
    }

    // File descriptors.
    // SAFETY: a null buffer asks for the needed size.
    let need = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need < 0 || (need == 0 && errno() != 0) {
        let e = errno();
        if skip(e) {
            return Ok(None);
        }
        return Err(scan_error("PROC_PIDLISTFDS", pid, e));
    }
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    let cap = need as usize / entry + 64;
    let mut fds: Vec<libc::proc_fdinfo> = vec![libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; cap];
    // SAFETY: buffer of cap proc_fdinfo.
    let got = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr() as *mut libc::c_void, (cap * entry) as i32)
    };
    if got < 0 || (got == 0 && errno() != 0) {
        let e = errno();
        if skip(e) {
            return Ok(None);
        }
        return Err(scan_error("PROC_PIDLISTFDS", pid, e));
    }
    fds.truncate(got as usize / entry);
    for fd in fds.iter().filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32) {
        let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<VnodeFdInfoWithPath>() as i32;
        // SAFETY: buffer is a vnode_fdinfowithpath.
        let n = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if n <= 0 {
            let e = errno();
            match e {
                // The fd was closed (or the process exited) since the listing.
                libc::EBADF | libc::ENOENT => continue,
                e if skip(e) => return Ok(None),
                e => return Err(scan_error("PROC_PIDFDVNODEPATHINFO", pid, e)),
            }
        }
        // The calling process's own event-only descriptors (FSEvents opens
        // the watch root and its ancestors `O_EVTONLY` for
        // `kFSEventStreamCreateFlagWatchRoot`): they never keep a file busy
        // and belong to the daemon's own Trackers, which it drops itself
        // [实现时补的决定].
        let evt = info.pfi.fi_openflags & libc::O_EVTONLY as u32 != 0;
        if pid == own_pid() && evt && !with_own_evt {
            continue;
        }
        let p = vip_path(&info.pvip);
        if !p.is_empty() && keep(p) {
            out.push((p.to_vec(), Hold::Fd, evt));
        }
    }

    // Mapped regions, from address 0 upward. A non-positive return is the end.
    let mut addr: u64 = 0;
    loop {
        let mut r: ProcRegionWithPathInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ProcRegionWithPathInfo>() as i32;
        // SAFETY: buffer is a proc_regionwithpathinfo.
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDREGIONPATHINFO2, addr, &mut r as *mut _ as *mut libc::c_void, size)
        };
        if n < size {
            break;
        }
        let p = vip_path(&r.prp_vip);
        if !p.is_empty() && keep(p) {
            out.push((p.to_vec(), Hold::Mapped, false));
        }
        let next = r.prp_prinfo.pri_address.saturating_add(r.prp_prinfo.pri_size);
        if next <= addr {
            break;
        }
        addr = next;
    }
    Ok(Some(out))
}

/// `pid`'s parent, or `None` when it cannot be read.
fn ppid_of(pid: i32) -> Option<i32> {
    let mut bi: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: buffer is a proc_bsdinfo.
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut bi as *mut _ as *mut libc::c_void, size) };
    (n == size).then_some(bi.pbi_ppid as i32)
}

fn own_pid() -> i32 {
    std::process::id() as i32
}

fn command_of(pid: i32) -> String {
    let mut buf = [0u8; 2 * 16 + 1];
    // SAFETY: buffer of len bytes.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if n > 0 {
        let s = CStr::from_bytes_until_nul(&buf).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        if !s.is_empty() {
            return s;
        }
    }
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buffer of len bytes.
    let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr() as *mut libc::c_void, path.len() as u32) };
    if n > 0 {
        path.truncate(n as usize);
        if let Some(name) = Path::new(std::ffi::OsStr::from_bytes(&path)).file_name() {
            return name.to_string_lossy().into_owned();
        }
    }
    format!("pid {pid}")
}

/// Every open file under `dir` (§5.11).
pub fn open_files(dir: &Path) -> Result<Vec<OpenFile>> {
    scan(&[dir])?.pop().expect("one result per dir")
}

/// The batch form: one pass over the processes for several directories.
/// The outer error is an enumeration failure; inner ones are per directory
/// (an unusable directory or an injected failure).
pub(crate) fn scan(dirs: &[&Path]) -> Result<Vec<Result<Vec<OpenFile>>>> {
    let mut results: Vec<Result<Vec<OpenFile>>> = Vec::with_capacity(dirs.len());
    let mut prefixes: Vec<Option<Vec<u8>>> = Vec::with_capacity(dirs.len());
    for d in dirs {
        if let Some(e) = crate::seams::open_files_failure(d) {
            results.push(Err(e));
            prefixes.push(None);
            continue;
        }
        match canonical_dir(d) {
            Ok(c) => {
                results.push(Ok(Vec::new()));
                prefixes.push(c);
            }
            Err(e) => {
                results.push(Err(e));
                prefixes.push(None);
            }
        }
    }
    let active: Vec<(usize, &[u8])> =
        prefixes.iter().enumerate().filter_map(|(i, p)| p.as_deref().map(|p| (i, p))).collect();
    if active.is_empty() {
        return Ok(results);
    }
    let keep = |p: &[u8]| active.iter().any(|(_, c)| is_under(p, c));
    // Our own event-only paths, read once when a child turns up holding an
    // event-only fd (see below).
    let mut own_evt: Option<Vec<Vec<u8>>> = None;
    for pid in all_pids()? {
        let Some(mut holds) = holds_of(pid, &keep, false)? else { continue };
        // A child we are spawning right now briefly holds copies of all our
        // descriptors, `O_CLOEXEC` ones included, until its exec closes
        // them: our own FSEvents `O_EVTONLY` fds then show up under the
        // child's pid (measured: 11 of 150 scans while 4 threads spawn).
        // A direct child's event-only fd on a path we hold event-only
        // ourselves is that copy, and event-only fds never keep a file busy
        // anyway [实现时补的决定].
        if pid != own_pid() && holds.iter().any(|h| h.2) && ppid_of(pid) == Some(own_pid()) {
            if own_evt.is_none() {
                let mine = holds_of(own_pid(), &keep, true)?.unwrap_or_default();
                own_evt = Some(mine.into_iter().filter(|h| h.2).map(|h| h.0).collect());
            }
            let mine = own_evt.as_deref().unwrap_or_default();
            holds.retain(|h| !(h.2 && mine.contains(&h.0)));
        }
        if holds.is_empty() {
            continue;
        }
        let command = command_of(pid);
        for (path, hold, _) in holds {
            for (i, c) in &active {
                if is_under(&path, c)
                    && let Ok(v) = &mut results[*i]
                {
                    v.push(OpenFile {
                        pid,
                        command: command.clone(),
                        path: PathBuf::from(std::ffi::OsStr::from_bytes(&path)),
                        hold,
                    });
                }
            }
        }
    }
    for v in results.iter_mut().flatten() {
        v.sort_by(|a, b| (a.pid, &a.path, hold_rank(a.hold)).cmp(&(b.pid, &b.path, hold_rank(b.hold))));
        v.dedup_by(|a, b| a.pid == b.pid && a.path == b.path && a.hold == b.hold);
    }
    Ok(results)
}

fn hold_rank(h: Hold) -> u8 {
    match h {
        Hold::Fd => 0,
        Hold::Mapped => 1,
        Hold::Cwd => 2,
    }
}

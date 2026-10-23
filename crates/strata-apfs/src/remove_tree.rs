//! `remove_tree_rooted` (§5.4, addendum 8, §4.2 H): remove a tree under a root
//! without following symlinks, reporting what it leaves behind.

use std::ffi::{CStr, OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use serde_json::json;
use strata_types::{Code, RepoPath};
use unicode_normalization::UnicodeNormalization;

use crate::error::{IoSide, classify_errno, errno_error, with};
use crate::hooks as seams;
use crate::meta::{SF_APPEND, SF_IMMUTABLE, UF_APPEND, UF_IMMUTABLE, blocking_flags, blocking_flags_str};
use crate::rooted::{DIR_FLAGS, Fd, openat, resolve};
use crate::sys;
use crate::{MAX_REPORTED_STUCK, MAX_WALK_DEPTH, Result};

const UF_BLOCK: u32 = UF_IMMUTABLE | UF_APPEND;
const SF_BLOCK: u32 = SF_IMMUTABLE | SF_APPEND;

/// One entry that was not removed. Reasons follow §5.4's stuck-reason table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stuck {
    /// Path relative to the root (NFC).
    pub path: RepoPath,
    pub reason: StuckReason,
    /// The failing errno; 0 when no system call failed.
    pub errno: i32,
    /// Only for `Immutable`: the blocking flag names joined with `|`, or
    /// `hard_link` for a shared inode whose flags were left alone.
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckReason {
    /// File flags block removal and apfs did not or could not clear them.
    Immutable,
    /// EACCES / EPERM without blocking flags.
    NoPermission,
    /// A nested mount point: not entered.
    OtherDevice,
    /// EBUSY.
    Busy,
    /// Swapped between stat and open, or written to while being removed.
    Changed,
    /// A directory that could not be opened or read.
    Unreadable,
    /// Deeper than `MAX_WALK_DEPTH`.
    TooDeep,
    /// The unlink's outcome is unknown (EIO and friends).
    UnknownOutcome,
    /// Any other errno: the unlink certainly failed.
    Io,
}

impl StuckReason {
    /// The snake-case name.
    pub fn as_str(&self) -> &'static str {
        match self {
            StuckReason::Immutable => "immutable",
            StuckReason::NoPermission => "no_permission",
            StuckReason::OtherDevice => "other_device",
            StuckReason::Busy => "busy",
            StuckReason::Changed => "changed",
            StuckReason::Unreadable => "unreadable",
            StuckReason::TooDeep => "too_deep",
            StuckReason::UnknownOutcome => "unknown_outcome",
            StuckReason::Io => "io",
        }
    }
}

/// What `remove_tree_rooted` did and what it left.
///
/// Dropping the report is a compile error under `deny(unused_must_use)`:
///
/// ```compile_fail
/// #![deny(unused_must_use)]
/// use strata_types::RepoPath;
/// fn f(p: &RepoPath) {
///     strata_apfs::remove_tree_rooted(std::path::Path::new("/tmp"), p).unwrap();
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[must_use = "RemoveReport says what was left behind; check is_complete()"]
pub struct RemoveReport {
    /// Entries removed (each file, symlink, special file, directory, including the target).
    pub removed: u64,
    /// Entries whose UF_IMMUTABLE or UF_APPEND was cleared.
    pub flags_cleared: u64,
    /// Directories given u+rwx.
    pub made_writable: u64,
    /// Entries not removed, at most `MAX_REPORTED_STUCK`, in the order met.
    pub stuck: Vec<Stuck>,
    /// The exact number of entries not removed.
    pub stuck_total: u64,
}

impl RemoveReport {
    /// The whole tree, including the target, is gone.
    pub fn is_complete(&self) -> bool {
        self.stuck_total == 0
    }

    pub(crate) fn record(&mut self, s: Stuck) {
        self.stuck_total += 1;
        if self.stuck.len() < MAX_REPORTED_STUCK {
            self.stuck.push(s);
        }
    }
}

/// The stuck-reason table for a failed unlink/rmdir/open (pure).
pub(crate) fn stuck_reason(errno: i32, st_flags: u32, st_nlink: u64) -> (StuckReason, Option<String>) {
    match errno {
        libc::EPERM | libc::EACCES => {
            if st_flags & SF_BLOCK != 0 {
                (StuckReason::Immutable, Some(blocking_flags_str(st_flags)))
            } else if st_flags & UF_BLOCK != 0 {
                if st_nlink > 1 {
                    (StuckReason::Immutable, Some("hard_link".into()))
                } else {
                    (StuckReason::Immutable, Some(blocking_flags_str(st_flags)))
                }
            } else {
                (StuckReason::NoPermission, None)
            }
        }
        libc::EBUSY => (StuckReason::Busy, None),
        libc::ENOTEMPTY | libc::ENOTDIR | libc::ELOOP => (StuckReason::Changed, None),
        libc::EIO | libc::ETIMEDOUT | libc::ENXIO | libc::ENODEV | libc::EDEVERR => (StuckReason::UnknownOutcome, None),
        _ => (StuckReason::Io, None),
    }
}

/// The gate for the one name-based chmod (review M1): only when the owner
/// lacks the search bit and no blocking flag is set.
pub(crate) fn chmod_by_name_helps(st_mode: u32, st_flags: u32) -> bool {
    st_mode & (libc::S_IXUSR as u32) == 0 && blocking_flags(st_flags).is_empty()
}

struct Ctx {
    report: RemoveReport,
    tdev: i32,
    step: usize,
    uid: u32,
}

impl Ctx {
    fn stuck(&mut self, path: &RepoPath, reason: StuckReason, errno: i32, detail: Option<String>) -> bool {
        self.report.record(Stuck { path: path.clone(), reason, errno, detail });
        false
    }
}

fn child_path(parent: &RepoPath, name: &OsStr) -> RepoPath {
    let n: String = String::from_utf8_lossy(name.as_bytes()).nfc().collect();
    RepoPath::try_from(format!("{}/{}", parent.as_str(), n)).unwrap_or_else(|_| parent.clone())
}

fn same_obj(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

fn cname(name: &OsStr) -> std::ffi::CString {
    sys::cos(name).unwrap_or_default()
}

fn fchflags(fd: i32, flags: u32) -> std::result::Result<(), i32> {
    sys::retry(|| unsafe { libc::fchflags(fd, flags) }).map(|_| ())
}

fn fchmod(fd: i32, mode: u32) -> std::result::Result<(), i32> {
    sys::retry(|| unsafe { libc::fchmod(fd, mode as libc::mode_t) }).map(|_| ())
}

fn unlinkat(dir: i32, name: &OsStr, flags: libc::c_int) -> std::result::Result<(), i32> {
    let c = cname(name);
    sys::retry(|| unsafe { libc::unlinkat(dir, c.as_ptr(), flags) }).map(|_| ())
}

/// Names and d_type of a directory, read through a dup of `fd` and closed at once.
fn read_names(fd: i32) -> std::result::Result<Vec<(OsString, u8)>, i32> {
    let d = sys::retry(|| unsafe { libc::dup(fd) })?;
    let dir = unsafe { libc::fdopendir(d) };
    if dir.is_null() {
        let e = sys::errno();
        unsafe { libc::close(d) };
        return Err(e);
    }
    unsafe { libc::rewinddir(dir) };
    let mut out = Vec::new();
    let err;
    loop {
        unsafe { *libc::__error() = 0 };
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            err = sys::errno();
            break;
        }
        let ent = unsafe { &*ent };
        let name = unsafe { CStr::from_ptr(ent.d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        out.push((OsString::from_vec(name.to_vec()), ent.d_type));
    }
    unsafe { libc::closedir(dir) };
    if err != 0 { Err(err) } else { Ok(out) }
}

/// Remove the tree at `root/path` (any entry kind), never following a symlink
/// and never crossing a device. `Err` only for refusals before anything is
/// touched; once traversal starts the result is always `Ok(report)`.
pub fn remove_tree_rooted(root: &Path, path: &RepoPath) -> Result<RemoveReport> {
    // 1. Pin the parent.
    let r = resolve(root, path, IoSide::Write, 0)?;
    // 2. The target exists (any kind).
    let st = match sys::fstatat_nofollow(r.dir.0, &r.name) {
        Ok(st) => st,
        Err(e) => return Err(classify_errno(e, IoSide::Read, &r.shown)),
    };
    let pst = sys::fstat(r.dir.0).map_err(|e| classify_errno(e, IoSide::Read, &r.shown))?;
    // 3. A mount point target.
    if sys::is_dir(&st) && st.st_dev != pst.st_dev {
        let e = errno_error(Code::CrossVolume, libc::EBUSY, &r.shown, Some("mount_point"));
        let e = with(e, "src_dev", json!(pst.st_dev));
        return Err(with(e, "dst_dev", json!(st.st_dev)));
    }
    // 4. The parent must be writable and unflagged.
    if pst.st_flags & (UF_BLOCK | SF_BLOCK) != 0 {
        let e = errno_error(Code::ReadOnly, libc::EPERM, &r.shown, Some("immutable"));
        return Err(with(e, "flags", json!(blocking_flags_str(pst.st_flags))));
    }
    let dot = c".";
    if let Err(e) = sys::retry(|| unsafe {
        libc::faccessat(r.dir.0, dot.as_ptr(), libc::W_OK | libc::X_OK, libc::AT_EACCESS)
    }) {
        return Err(errno_error(Code::ReadOnly, e, &r.shown, Some("no_permission")));
    }
    // 5. Traverse.
    let mut ctx = Ctx { report: RemoveReport::default(), tdev: st.st_dev, step: r.step, uid: unsafe { libc::geteuid() } };
    visit(&mut ctx, r.dir.0, &r.name, path, 0);
    Ok(ctx.report)
}

/// Returns true when `name` (and everything under it) is gone.
fn visit(ctx: &mut Ctx, parent: i32, name: &OsStr, rel: &RepoPath, depth: u32) -> bool {
    // 1.
    let st = match sys::fstatat_nofollow(parent, name) {
        Ok(st) => st,
        Err(libc::ENOENT) => return true,
        Err(e) => {
            let (reason, _) = stuck_reason(e, 0, 1);
            return ctx.stuck(rel, reason, e, None);
        }
    };
    if !sys::is_dir(&st) {
        return unlink_leaf(ctx, parent, name, rel);
    }
    if st.st_dev != ctx.tdev {
        return ctx.stuck(rel, StuckReason::OtherDevice, 0, None);
    }
    if depth >= MAX_WALK_DEPTH {
        return ctx.stuck(rel, StuckReason::TooDeep, 0, None);
    }
    if st.st_flags & SF_BLOCK != 0 {
        return ctx.stuck(rel, StuckReason::Immutable, libc::EPERM, Some(blocking_flags_str(st.st_flags)));
    }
    // 2.
    seams::before_open(ctx.step, name);
    ctx.step += 1;
    let fd = match openat(parent, name, DIR_FLAGS, 0) {
        Ok(fd) => fd,
        Err(libc::ENOENT) => return true,
        Err(e @ (libc::ENOTDIR | libc::ELOOP)) => return ctx.stuck(rel, StuckReason::Changed, e, None),
        Err(libc::EACCES) => match open_unreadable(ctx, parent, name, rel, &st) {
            Some(fd) => fd,
            None => return false,
        },
        Err(e) => return ctx.stuck(rel, StuckReason::Unreadable, e, None),
    };
    // 3. Fresh fstat on the handle.
    let st2 = match sys::fstat(fd.0) {
        Ok(s) => s,
        Err(e) => return ctx.stuck(rel, StuckReason::Unreadable, e, None),
    };
    if !same_obj(&st, &st2) {
        return ctx.stuck(rel, StuckReason::Changed, 0, None);
    }
    // 4.
    if st2.st_flags & UF_BLOCK != 0 {
        if let Err(e) = fchflags(fd.0, st2.st_flags & !UF_BLOCK) {
            return ctx.stuck(rel, StuckReason::Immutable, e, Some(blocking_flags_str(st2.st_flags)));
        }
        ctx.report.flags_cleared += 1;
    }
    let mode = st2.st_mode as u32;
    if mode & 0o700 != 0o700 {
        if let Err(e) = fchmod(fd.0, (mode | 0o700) & 0o7777) {
            return ctx.stuck(rel, StuckReason::NoPermission, e, None);
        }
        ctx.report.made_writable += 1;
    }
    // 5.
    let names = match read_names(fd.0) {
        Ok(n) => n,
        Err(e) => return ctx.stuck(rel, StuckReason::Unreadable, e, None),
    };
    // 6.
    let mut clean = true;
    for (child, dtype) in names {
        let crel = child_path(rel, &child);
        let ok = if dtype == libc::DT_DIR || dtype == libc::DT_UNKNOWN {
            visit(ctx, fd.0, &child, &crel, depth + 1)
        } else {
            unlink_leaf(ctx, fd.0, &child, &crel)
        };
        clean &= ok;
    }
    // 7.
    drop(fd);
    if !clean {
        return false;
    }
    match unlinkat(parent, name, libc::AT_REMOVEDIR) {
        Ok(()) => {
            ctx.report.removed += 1;
            true
        }
        Err(libc::ENOENT) => true,
        Err(e @ (libc::EPERM | libc::EACCES)) => ctx.stuck(rel, StuckReason::NoPermission, e, None),
        Err(e) => {
            let (reason, _) = stuck_reason(e, 0, 1);
            ctx.stuck(rel, reason, e, None)
        }
    }
}

/// Step 2's EACCES branch: a directory the owner cannot read.
fn open_unreadable(ctx: &mut Ctx, parent: i32, name: &OsStr, rel: &RepoPath, st: &libc::stat) -> Option<Fd> {
    if st.st_uid != ctx.uid {
        ctx.stuck(rel, StuckReason::NoPermission, libc::EACCES, None);
        return None;
    }
    let exec_flags = libc::O_EXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    match openat(parent, name, exec_flags, 0) {
        Ok(xfd) => {
            // Through the handle.
            let xst = match sys::fstat(xfd.0) {
                Ok(s) => s,
                Err(e) => {
                    ctx.stuck(rel, StuckReason::Unreadable, e, None);
                    return None;
                }
            };
            if !same_obj(st, &xst) {
                ctx.stuck(rel, StuckReason::Changed, 0, None);
                return None;
            }
            if xst.st_flags & UF_BLOCK != 0 {
                if let Err(e) = fchflags(xfd.0, xst.st_flags & !UF_BLOCK) {
                    ctx.stuck(rel, StuckReason::Immutable, e, Some(blocking_flags_str(xst.st_flags)));
                    return None;
                }
                ctx.report.flags_cleared += 1;
            }
            if let Err(e) = fchmod(xfd.0, (xst.st_mode as u32 | 0o700) & 0o7777) {
                ctx.stuck(rel, StuckReason::NoPermission, e, None);
                return None;
            }
            ctx.report.made_writable += 1;
            drop(xfd);
            match openat(parent, name, DIR_FLAGS, 0) {
                Ok(fd) => Some(fd),
                Err(e) => {
                    ctx.stuck(rel, StuckReason::Unreadable, e, None);
                    None
                }
            }
        }
        Err(libc::EACCES) => {
            let mode = st.st_mode as u32;
            if st.st_flags & (UF_BLOCK | SF_BLOCK) != 0 {
                ctx.stuck(rel, StuckReason::Immutable, libc::EPERM, Some(blocking_flags_str(st.st_flags)));
                return None;
            }
            if !chmod_by_name_helps(mode, st.st_flags) {
                ctx.stuck(rel, StuckReason::NoPermission, libc::EACCES, None);
                return None;
            }
            // The one name-based mode change (§4.2 H residual risk).
            let c = cname(name);
            let m = ((mode | 0o700) & 0o7777) as libc::mode_t;
            if let Err(e) = sys::retry(|| unsafe { libc::fchmodat(parent, c.as_ptr(), m, libc::AT_SYMLINK_NOFOLLOW) }) {
                ctx.stuck(rel, StuckReason::NoPermission, e, None);
                return None;
            }
            ctx.report.made_writable += 1;
            match openat(parent, name, DIR_FLAGS, 0) {
                Ok(fd) => match sys::fstat(fd.0) {
                    Ok(s) if same_obj(st, &s) => Some(fd),
                    Ok(_) => {
                        ctx.stuck(rel, StuckReason::Changed, 0, None);
                        None
                    }
                    Err(e) => {
                        ctx.stuck(rel, StuckReason::Unreadable, e, None);
                        None
                    }
                },
                Err(e) => {
                    ctx.stuck(rel, StuckReason::Unreadable, e, None);
                    None
                }
            }
        }
        Err(e) => {
            ctx.stuck(rel, StuckReason::Unreadable, e, None);
            None
        }
    }
}

/// Step 8: unlink one non-directory, clearing UF flags on an unshared inode.
fn unlink_leaf(ctx: &mut Ctx, dir: i32, name: &OsStr, rel: &RepoPath) -> bool {
    let e = match unlinkat(dir, name, 0) {
        Ok(()) => {
            ctx.report.removed += 1;
            return true;
        }
        Err(libc::ENOENT) => return true,
        Err(e) => e,
    };
    if e != libc::EPERM && e != libc::EACCES {
        let (reason, _) = stuck_reason(e, 0, 1);
        return ctx.stuck(rel, reason, e, None);
    }
    let st = match sys::fstatat_nofollow(dir, name) {
        Ok(st) => st,
        Err(libc::ENOENT) => return true,
        Err(_) => return ctx.stuck(rel, StuckReason::NoPermission, e, None),
    };
    let flags = st.st_flags;
    if flags & SF_BLOCK != 0 || flags & UF_BLOCK == 0 || (st.st_nlink as u64) > 1 {
        let (reason, detail) = stuck_reason(e, flags, st.st_nlink as u64);
        return ctx.stuck(rel, reason, e, detail);
    }
    // UF flags on an unshared inode: clear them through a handle.
    let fd = if sys::is_lnk(&st) {
        openat(dir, name, libc::O_RDONLY | libc::O_SYMLINK | libc::O_NONBLOCK | libc::O_CLOEXEC, 0)
    } else {
        match openat(dir, name, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0) {
            Err(libc::EACCES) => {
                openat(dir, name, libc::O_EXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0)
            }
            other => other,
        }
    };
    let fd = match fd {
        Ok(fd) => fd,
        Err(libc::ENOENT) => return true,
        Err(he) => return ctx.stuck(rel, StuckReason::Immutable, he, Some(blocking_flags_str(flags))),
    };
    let hst = match sys::fstat(fd.0) {
        Ok(s) => s,
        Err(he) => return ctx.stuck(rel, StuckReason::Immutable, he, Some(blocking_flags_str(flags))),
    };
    if !same_obj(&st, &hst) {
        return ctx.stuck(rel, StuckReason::Changed, 0, None);
    }
    if hst.st_nlink > 1 {
        return ctx.stuck(rel, StuckReason::Immutable, libc::EPERM, Some("hard_link".into()));
    }
    if let Err(fe) = fchflags(fd.0, hst.st_flags & !UF_BLOCK) {
        return ctx.stuck(rel, StuckReason::Immutable, fe, Some(blocking_flags_str(hst.st_flags)));
    }
    ctx.report.flags_cleared += 1;
    drop(fd);
    match unlinkat(dir, name, 0) {
        Ok(()) => {
            ctx.report.removed += 1;
            true
        }
        Err(libc::ENOENT) => true,
        Err(e2) => {
            let (reason, _) = stuck_reason(e2, 0, 1);
            ctx.stuck(rel, reason, e2, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::UF_COMPRESSED;

    const UF_HIDDEN: u32 = 0x8000;

    fn stuck_at(n: usize) -> Stuck {
        Stuck {
            path: RepoPath::try_from(format!("d/{n}")).unwrap(),
            reason: StuckReason::Io,
            errno: libc::ENOSPC,
            detail: None,
        }
    }

    #[test]
    fn remove_report_counts_and_completeness() {
        let r = RemoveReport::default();
        assert_eq!((r.removed, r.flags_cleared, r.made_writable, r.stuck_total), (0, 0, 0, 0));
        assert!(r.stuck.is_empty());
        assert!(r.is_complete());
        let mut r = RemoveReport::default();
        for n in 0..MAX_REPORTED_STUCK + 5 {
            r.record(stuck_at(n));
        }
        assert!(!r.is_complete());
        assert_eq!(r.stuck.len(), MAX_REPORTED_STUCK);
        assert_eq!(r.stuck_total, (MAX_REPORTED_STUCK + 5) as u64);
        for (i, s) in r.stuck.iter().enumerate() {
            assert_eq!(s, &stuck_at(i));
        }
        // Truncated vector, honest total.
        let r = RemoveReport { stuck_total: 3, ..Default::default() };
        assert!(!r.is_complete());
    }

    #[test]
    fn stuck_reason_classification_and_names() {
        use StuckReason::*;
        let s = |x: &str| Some(x.to_string());
        let rows: &[(i32, u32, u64, StuckReason, Option<String>)] = &[
            (libc::EACCES, 0, 1, NoPermission, None),
            (libc::EPERM, 0, 1, NoPermission, None),
            (libc::EPERM, UF_HIDDEN | UF_COMPRESSED, 1, NoPermission, None),
            (libc::EPERM, UF_IMMUTABLE, 1, Immutable, s("UF_IMMUTABLE")),
            (libc::EPERM, UF_APPEND, 1, Immutable, s("UF_APPEND")),
            (libc::EPERM, UF_IMMUTABLE | UF_APPEND, 1, Immutable, s("UF_IMMUTABLE|UF_APPEND")),
            (libc::EPERM, UF_IMMUTABLE, 2, Immutable, s("hard_link")),
            (libc::EPERM, UF_APPEND, 3, Immutable, s("hard_link")),
            (libc::EPERM, SF_IMMUTABLE, 1, Immutable, s("SF_IMMUTABLE")),
            (libc::EPERM, SF_APPEND, 2, Immutable, s("SF_APPEND")),
            (libc::EPERM, SF_IMMUTABLE | UF_IMMUTABLE, 2, Immutable, s("UF_IMMUTABLE|SF_IMMUTABLE")),
            (libc::EACCES, SF_APPEND | UF_APPEND, 1, Immutable, s("UF_APPEND|SF_APPEND")),
            (libc::EBUSY, 0, 1, Busy, None),
            (libc::ENOTEMPTY, 0, 1, Changed, None),
            (libc::ENOTDIR, 0, 1, Changed, None),
            (libc::ELOOP, 0, 1, Changed, None),
            (libc::EIO, 0, 1, UnknownOutcome, None),
            (libc::ETIMEDOUT, 0, 1, UnknownOutcome, None),
            (libc::ENXIO, 0, 1, UnknownOutcome, None),
            (libc::ENODEV, 0, 1, UnknownOutcome, None),
            (libc::EDEVERR, 0, 1, UnknownOutcome, None),
            (libc::ENOSPC, 0, 1, Io, None),
            (libc::EDQUOT, 0, 1, Io, None),
            (libc::EINVAL, 0, 1, Io, None),
            (libc::EFBIG, 0, 1, Io, None),
        ];
        for (errno, flags, nlink, reason, detail) in rows {
            assert_eq!(stuck_reason(*errno, *flags, *nlink), (*reason, detail.clone()), "errno {errno} flags {flags:#x} nlink {nlink}");
        }
        let all = [Immutable, NoPermission, OtherDevice, Busy, Changed, Unreadable, TooDeep, UnknownOutcome, Io];
        let names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            ["immutable", "no_permission", "other_device", "busy", "changed", "unreadable", "too_deep", "unknown_outcome", "io"]
        );
        let mut d = names.clone();
        d.sort();
        d.dedup();
        assert_eq!(d.len(), 9);
        // The name-based chmod gate (review M1).
        let gate: &[(u32, u32, bool)] = &[
            (0o000, 0, true),
            (0o200, 0, true),
            (0o600, 0, true),
            (0o000, UF_HIDDEN, true),
            (0o100, 0, false),
            (0o700, 0, false),
            (0o000, UF_IMMUTABLE, false),
            (0o000, UF_APPEND, false),
            (0o000, SF_IMMUTABLE, false),
            (0o000, SF_APPEND, false),
        ];
        for (mode, flags, want) in gate {
            assert_eq!(chmod_by_name_helps(*mode, *flags), *want, "mode {mode:o} flags {flags:#x}");
        }
    }
}

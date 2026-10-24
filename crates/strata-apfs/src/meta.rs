//! Metadata changes through a handle (§5.4 "`make_removable(file)` 与
//! `set_file_meta(file, meta)`"): `make_removable`, `set_file_meta`,
//! `FileMeta`, and the shared pure `blocking_flags`.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use strata_types::{Code, StrataError};

use crate::Result;
use crate::error::{IoSide, classify_errno, errno_error, refusal, set_detail};
use crate::hooks::{self, MetaChange};
use crate::sys;

pub(crate) const UF_IMMUTABLE: u32 = 0x0000_0002;
pub(crate) const UF_APPEND: u32 = 0x0000_0004;
pub(crate) const UF_COMPRESSED: u32 = 0x0000_0020;
pub(crate) const UF_SETTABLE: u32 = 0x0000_ffff;
pub(crate) const SF_IMMUTABLE: u32 = 0x0002_0000;
pub(crate) const SF_APPEND: u32 = 0x0004_0000;
pub(crate) const BLOCKING: u32 = UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_APPEND;

/// APFS timestamp range: i64 nanoseconds since the epoch (§4.2 H).
const NS_MIN: i128 = i64::MIN as i128;
const NS_MAX: i128 = i64::MAX as i128;

/// What `set_file_meta` should change. `FileMeta::default()` changes nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMeta {
    /// Clear every `UF_*` flag except `UF_COMPRESSED`. `SF_*` are never cleared.
    pub clear_flags: bool,
    /// Permission bits become `m & 0o777`.
    pub mode: Option<u32>,
    /// mtime becomes `t` (clamped to the APFS range); atime is untouched.
    pub mtime: Option<SystemTime>,
}

/// Names of the set flags that block removal, in the fixed order
/// `UF_IMMUTABLE`, `UF_APPEND`, `SF_IMMUTABLE`, `SF_APPEND`.
pub(crate) fn blocking_flags(st_flags: u32) -> Vec<&'static str> {
    [
        (UF_IMMUTABLE, "UF_IMMUTABLE"),
        (UF_APPEND, "UF_APPEND"),
        (SF_IMMUTABLE, "SF_IMMUTABLE"),
        (SF_APPEND, "SF_APPEND"),
    ]
    .iter()
    .filter(|(b, _)| st_flags & b != 0)
    .map(|(_, n)| *n)
    .collect()
}

/// `blocking_flags` joined with `|`, the form used in `detail.flags`.
pub(crate) fn blocking_flags_str(st_flags: u32) -> String {
    blocking_flags(st_flags).join("|")
}

/// One system call the plan wants to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Flags(u32),
    Mode(u32),
    /// Nanoseconds since the epoch, already clamped.
    Mtime(i128),
}

impl Step {
    fn change(self) -> MetaChange {
        match self {
            Step::Flags(_) => MetaChange::Flags,
            Step::Mode(_) => MetaChange::Mode,
            Step::Mtime(_) => MetaChange::Mtime,
        }
    }
}

/// A named refusal decided before anything changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    NotFile(&'static str),
    HardLink,
    Immutable(String),
}

/// The part of `fstat` the rules look at.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Facts {
    pub(crate) fmt: u32,
    pub(crate) mode: u32,
    pub(crate) flags: u32,
    pub(crate) nlink: u64,
    pub(crate) mtime_ns: i128,
}

impl Facts {
    pub(crate) fn of(st: &libc::stat) -> Facts {
        Facts {
            fmt: (st.st_mode & libc::S_IFMT) as u32,
            mode: st.st_mode as u32 & 0o7777,
            flags: st.st_flags,
            nlink: st.st_nlink as u64,
            mtime_ns: st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128,
        }
    }
    fn is_dir(&self) -> bool {
        self.fmt == libc::S_IFDIR as u32
    }
}

fn shared_checks(f: &Facts) -> std::result::Result<(), Refusal> {
    if !f.is_dir() && f.nlink > 1 {
        return Err(Refusal::HardLink);
    }
    if f.flags & (SF_IMMUTABLE | SF_APPEND) != 0 {
        return Err(Refusal::Immutable(blocking_flags_str(f.flags)));
    }
    Ok(())
}

/// Pure plan for `make_removable`.
pub(crate) fn plan_make_removable(f: &Facts) -> std::result::Result<Vec<Step>, Refusal> {
    shared_checks(f)?;
    let mut steps = Vec::new();
    if f.flags & (UF_IMMUTABLE | UF_APPEND) != 0 {
        steps.push(Step::Flags(f.flags & !(UF_IMMUTABLE | UF_APPEND)));
    }
    if f.mode & 0o200 == 0 {
        steps.push(Step::Mode(f.mode | 0o200));
    }
    Ok(steps)
}

/// Clamp a `SystemTime` to the APFS range, in nanoseconds since the epoch.
pub(crate) fn clamp_ns(t: SystemTime) -> i128 {
    let ns = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    ns.clamp(NS_MIN, NS_MAX)
}

/// Pure plan for `set_file_meta`: kind, `st_nlink` (non-directories only),
/// `SF_*`, then the steps that actually change something.
pub(crate) fn plan_set_file_meta(f: &Facts, meta: &FileMeta) -> std::result::Result<Vec<Step>, Refusal> {
    let fmt = f.fmt;
    if fmt != libc::S_IFREG as u32 && fmt != libc::S_IFDIR as u32 {
        let reason = match fmt as libc::mode_t {
            libc::S_IFIFO => "fifo",
            libc::S_IFSOCK => "socket",
            libc::S_IFLNK => "symlink",
            _ => "device",
        };
        return Err(Refusal::NotFile(reason));
    }
    shared_checks(f)?;
    let mut steps = Vec::new();
    if meta.clear_flags && f.flags & UF_SETTABLE & !UF_COMPRESSED != 0 {
        steps.push(Step::Flags(f.flags & (UF_COMPRESSED | !UF_SETTABLE)));
    }
    if let Some(m) = meta.mode
        && f.mode != m & 0o777
    {
        steps.push(Step::Mode(m & 0o777));
    }
    if let Some(t) = meta.mtime {
        let ns = clamp_ns(t);
        if ns != f.mtime_ns {
            steps.push(Step::Mtime(ns));
        }
    }
    Ok(steps)
}

fn refusal_error(r: Refusal, path: &Path) -> StrataError {
    match r {
        Refusal::NotFile(reason) => refusal(Code::NotFile, path, reason, "not a regular file or directory"),
        Refusal::HardLink => refusal(
            Code::ReadOnly,
            path,
            "hard_link",
            "the inode has more than one name; changing it would change the other names too",
        ),
        Refusal::Immutable(flags) => {
            let mut e = refusal(Code::ReadOnly, path, "immutable", "system flags block the change");
            set_detail(&mut e, "flags", json!(flags));
            e
        }
    }
}

fn apply(fd: i32, step: Step) -> std::result::Result<(), i32> {
    match step {
        Step::Flags(f) => sys::retry(|| unsafe { libc::fchflags(fd, f) }).map(|_| ()),
        Step::Mode(m) => sys::retry(|| unsafe { libc::fchmod(fd, m as libc::mode_t) }).map(|_| ()),
        Step::Mtime(ns) => {
            let ts = [
                libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
                libc::timespec {
                    tv_sec: ns.div_euclid(1_000_000_000) as libc::time_t,
                    tv_nsec: ns.rem_euclid(1_000_000_000) as libc::c_long,
                },
            ];
            sys::retry(|| unsafe { libc::futimens(fd, ts.as_ptr()) }).map(|_| ())
        }
    }
}

fn run(file: &File, plan: impl Fn(&Facts) -> std::result::Result<Vec<Step>, Refusal>) -> Result<()> {
    let fd = file.as_raw_fd();
    let path = sys::fd_path(fd);
    let st = sys::fstat(fd).map_err(|e| classify_errno(e, IoSide::Read, &path))?;
    let facts = Facts::of(&st);
    let steps = plan(&facts).map_err(|r| refusal_error(r, &path))?;
    for (i, step) in steps.iter().enumerate() {
        hooks::before_meta(i, step.change());
        if let Err(e) = apply(fd, *step) {
            if i > 0 {
                return Err(classify_errno(e, IoSide::AfterWrite, &path));
            }
            return Err(write_side_error(e, &path, facts.flags));
        }
    }
    Ok(())
}

/// Write-side classification with the `immutable` reason for flag refusals.
fn write_side_error(e: i32, path: &Path, flags: u32) -> StrataError {
    if (e == libc::EPERM || e == libc::EACCES) && flags & BLOCKING != 0 {
        let mut err = errno_error(Code::ReadOnly, e, path, Some("immutable"));
        set_detail(&mut err, "flags", json!(blocking_flags_str(flags)));
        return err;
    }
    classify_errno(e, IoSide::Write, path)
}

/// Make a sealed directory (0555, or with `uchg` / `uappnd`) renamable and
/// removable: clear `UF_IMMUTABLE` and `UF_APPEND` first, then add u+w.
pub fn make_removable(file: &File) -> Result<()> {
    run(file, plan_make_removable)
}

/// Change a regular file's or a directory's metadata through its handle.
pub fn set_file_meta(file: &File, meta: &FileMeta) -> Result<()> {
    run(file, |f| plan_set_file_meta(f, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UF_HIDDEN: u32 = 0x8000;
    const UF_NODUMP: u32 = 0x1;
    const UF_OPAQUE: u32 = 0x8;
    const DIR: u32 = libc::S_IFDIR as u32;
    const REG: u32 = libc::S_IFREG as u32;

    fn facts(fmt: u32, mode: u32, flags: u32, nlink: u64) -> Facts {
        Facts { fmt, mode, flags, nlink, mtime_ns: 1_000_000_000 }
    }

    #[test]
    fn blocking_flags_lists_the_flags_that_block_removal() {
        assert!(blocking_flags(0).is_empty());
        assert_eq!(blocking_flags(UF_IMMUTABLE), ["UF_IMMUTABLE"]);
        assert_eq!(blocking_flags(UF_APPEND), ["UF_APPEND"]);
        assert_eq!(blocking_flags(SF_IMMUTABLE), ["SF_IMMUTABLE"]);
        assert_eq!(blocking_flags(SF_APPEND), ["SF_APPEND"]);
        assert_eq!(
            blocking_flags_str(SF_APPEND | UF_APPEND | SF_IMMUTABLE | UF_IMMUTABLE),
            "UF_IMMUTABLE|UF_APPEND|SF_IMMUTABLE|SF_APPEND"
        );
        for other in [UF_HIDDEN, UF_NODUMP, UF_COMPRESSED, UF_OPAQUE] {
            assert!(blocking_flags(other).is_empty(), "{other:#x}");
            assert_eq!(blocking_flags(other | UF_APPEND), ["UF_APPEND"]);
            assert_eq!(blocking_flags(other | SF_IMMUTABLE | UF_IMMUTABLE), ["UF_IMMUTABLE", "SF_IMMUTABLE"]);
        }
    }

    #[test]
    fn make_removable_and_set_file_meta_refuse_by_rule() {
        // make_removable
        for sf in [SF_IMMUTABLE, SF_APPEND] {
            assert_eq!(
                plan_make_removable(&facts(DIR, 0o555, sf, 2)),
                Err(Refusal::Immutable(blocking_flags_str(sf)))
            );
        }
        assert_eq!(
            plan_make_removable(&facts(DIR, 0o555, UF_IMMUTABLE | SF_APPEND, 2)),
            Err(Refusal::Immutable("UF_IMMUTABLE|SF_APPEND".into()))
        );
        assert_eq!(plan_make_removable(&facts(REG, 0o400, 0, 2)), Err(Refusal::HardLink));
        assert_eq!(plan_make_removable(&facts(DIR, 0o755, 0, 5)), Ok(vec![]));
        assert_eq!(plan_make_removable(&facts(DIR, 0o555, 0, 2)), Ok(vec![Step::Mode(0o755)]));
        assert_eq!(
            plan_make_removable(&facts(DIR, 0o755, UF_IMMUTABLE | UF_HIDDEN, 2)),
            Ok(vec![Step::Flags(UF_HIDDEN)])
        );
        assert_eq!(
            plan_make_removable(&facts(DIR, 0o555, UF_IMMUTABLE, 2)),
            Ok(vec![Step::Flags(0), Step::Mode(0o755)])
        );

        // set_file_meta: kind first.
        let m = FileMeta::default();
        for (fmt, reason) in [
            (libc::S_IFIFO, "fifo"),
            (libc::S_IFSOCK, "socket"),
            (libc::S_IFCHR, "device"),
            (libc::S_IFBLK, "device"),
            (libc::S_IFLNK, "symlink"),
        ] {
            // Even with nlink > 1 and SF_*: the kind check comes first.
            assert_eq!(
                plan_set_file_meta(&facts(fmt as u32, 0o644, SF_IMMUTABLE, 2), &m),
                Err(Refusal::NotFile(reason))
            );
        }
        // Directories pass kind and nlink; only SF_* remains.
        assert_eq!(plan_set_file_meta(&facts(DIR, 0o755, 0, 7), &m), Ok(vec![]));
        assert_eq!(
            plan_set_file_meta(&facts(DIR, 0o755, SF_APPEND, 7), &m),
            Err(Refusal::Immutable("SF_APPEND".into()))
        );
        // nlink before SF_* for non-directories.
        assert_eq!(plan_set_file_meta(&facts(REG, 0o644, SF_IMMUTABLE, 2), &m), Err(Refusal::HardLink));
        assert_eq!(
            plan_set_file_meta(&facts(REG, 0o644, SF_IMMUTABLE, 1), &m),
            Err(Refusal::Immutable("SF_IMMUTABLE".into()))
        );

        // Steps: nothing when everything is already the target.
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1);
        let all = FileMeta { clear_flags: true, mode: Some(0o644), mtime: Some(t) };
        assert_eq!(plan_set_file_meta(&facts(REG, 0o644, UF_COMPRESSED, 1), &all), Ok(vec![]));
        let t2 = UNIX_EPOCH + std::time::Duration::from_secs(2);
        let all2 = FileMeta { clear_flags: true, mode: Some(0o600), mtime: Some(t2) };
        assert_eq!(
            plan_set_file_meta(&facts(REG, 0o644, UF_COMPRESSED | UF_IMMUTABLE | UF_HIDDEN, 1), &all2),
            Ok(vec![Step::Flags(UF_COMPRESSED), Step::Mode(0o600), Step::Mtime(2_000_000_000)])
        );
        assert_eq!(
            plan_set_file_meta(&facts(DIR, 0o755, 0, 3), &FileMeta { mode: Some(0o555), ..Default::default() }),
            Ok(vec![Step::Mode(0o555)])
        );
        // setuid / setgid / sticky are masked, and present ones are cleared.
        assert_eq!(
            plan_set_file_meta(&facts(REG, 0o4755, 0, 1), &FileMeta { mode: Some(0o4755), ..Default::default() }),
            Ok(vec![Step::Mode(0o755)])
        );
    }

    #[test]
    fn clamp_ns_matches_the_apfs_range() {
        let far = UNIX_EPOCH + std::time::Duration::from_secs(32_503_680_000); // year 3000
        assert_eq!(clamp_ns(far), i64::MAX as i128);
        let past = UNIX_EPOCH - std::time::Duration::from_secs(9_223_372_037);
        assert_eq!(clamp_ns(past), i64::MIN as i128);
    }
}

//! Destination variants under a root (§5.4 "目的地变体"): the parent is pinned
//! by `resolve`, and one `*at` call with the last name lands the write.

use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::{Code, RepoPath, StrataError};

use crate::Result;
use crate::clone::{CLONE_FLAGS, check_devs, clone_errno};
use crate::error::{IoSide, classify_errno, errno_error, refusal, set_detail};
use crate::hooks;
use crate::meta::{FileMeta, set_file_meta};
use crate::rooted::{DIR_FLAGS, Resolved, components, dir_open_error, open_final, open_root_fd, openat, resolve};
use crate::sys;

/// Map the errno of a landing `*at` call (write side).
pub(crate) fn dest_error(e: i32, shown: &Path) -> StrataError {
    match e {
        libc::EEXIST => {
            let mut err = errno_error(Code::Exists, e, shown, None);
            set_detail(&mut err, "dst_exists", json!(true));
            err
        }
        libc::ENAMETOOLONG => errno_error(Code::InvalidInput, e, shown, Some("name_too_long")),
        _ => clone_errno(e, shown),
    }
}

fn exists_at(r: &Resolved) -> bool {
    sys::fstatat_nofollow(r.dir.0, &r.name).is_ok()
}

fn cname(r: &Resolved) -> Result<std::ffi::CString> {
    sys::cos(&r.name).map_err(|e| classify_errno(e, IoSide::Write, &r.shown))
}

/// Fire the landing seam and advance the step.
fn landing_seam(r: &mut Resolved) {
    hooks::before_open(r.step, &r.name);
    r.step += 1;
}

fn src_dev_of(src: &Path, st: &libc::stat) -> Result<i32> {
    let probe = if sys::is_lnk(st) { src.parent().unwrap_or(Path::new("/")) } else { src };
    sys::real_dev_path(probe).map_err(|e| classify_errno(e, IoSide::Read, src))
}

fn same_volume_as_parent(src: &Path, st: &libc::stat, r: &Resolved) -> Result<()> {
    let sd = src_dev_of(src, st)?;
    let dd = sys::real_dev_fd(r.dir.0).map_err(|e| classify_errno(e, IoSide::Read, &r.shown))?;
    check_devs(sd, dd, src, &r.shown)
}

fn land_clone(src: &Path, r: &mut Resolved) -> Result<()> {
    let cs = sys::cpath(src).map_err(|e| classify_errno(e, IoSide::Read, src))?;
    let cn = cname(r)?;
    landing_seam(r);
    match sys::retry(|| unsafe { libc::clonefileat(libc::AT_FDCWD, cs.as_ptr(), r.dir.0, cn.as_ptr(), CLONE_FLAGS) }) {
        Ok(_) => Ok(()),
        Err(e) => {
            let mut err = dest_error(e, &r.shown);
            set_detail(&mut err, "dst_exists", json!(exists_at(r)));
            Err(err)
        }
    }
}

/// Clone the file or symlink `src` to `root/dst`; with `mode = Some(m)` set the
/// clone's permission bits to `m & 0o777` through a handle (regular files only).
pub fn clone_file_rooted(src: &Path, root: &Path, dst: &RepoPath, mode: Option<u32>) -> Result<()> {
    crate::rooted::check_root(root)?;
    let st = sys::lstat(src).map_err(|e| classify_errno(e, IoSide::Read, src))?;
    if sys::is_dir(&st) {
        return Err(refusal(Code::IsDirectory, src, "directory", "source is a directory"));
    }
    if !sys::is_reg(&st) && !sys::is_lnk(&st) {
        return Err(refusal(Code::NotFile, src, sys::special_reason(&st), "source is not a regular file or symlink"));
    }
    let mut r = resolve(root, dst, IoSide::Write, 0)?;
    same_volume_as_parent(src, &st, &r)?;
    land_clone(src, &mut r)?;
    let Some(m) = mode else { return Ok(()) };
    if !sys::is_reg(&st) {
        return Ok(());
    }
    let res = match open_final(&mut r) {
        Ok(fd) => set_file_meta(&fd.into_file(), &FileMeta { mode: Some(m & 0o777), ..Default::default() }),
        Err(e) => Err(classify_errno(e, IoSide::AfterWrite, &r.shown)),
    };
    res.map_err(|mut e| {
        e.code = Code::Unknown;
        set_detail(&mut e, "dst_exists", json!(exists_at(&r)));
        set_detail(&mut e, "path", json!(r.shown.to_string_lossy()));
        e
    })
}

/// Clone the directory tree `src` to `root/dst` (atomic `clonefileat`).
pub fn clone_tree_rooted(src: &Path, root: &Path, dst: &RepoPath) -> Result<()> {
    crate::rooted::check_root(root)?;
    let st = sys::lstat(src).map_err(|e| classify_errno(e, IoSide::Read, src))?;
    if sys::is_lnk(&st) {
        return Err(refusal(Code::NotDirectory, src, "symlink", "source is a symlink, not a directory"));
    }
    if !sys::is_dir(&st) {
        return Err(refusal(Code::NotDirectory, src, sys::special_reason(&st), "source is not a directory"));
    }
    let mut r = resolve(root, dst, IoSide::Write, 0)?;
    if let Ok(rs) = std::fs::canonicalize(src) {
        let parent: PathBuf = sys::fd_path(r.dir.0);
        if parent.starts_with(&rs) {
            return Err(refusal(Code::InvalidInput, &r.shown, "dst_inside_src", "destination is inside the source"));
        }
    }
    same_volume_as_parent(src, &st, &r)?;
    land_clone(src, &mut r)
}

/// Create a new empty file (`O_CREAT | O_EXCL | O_NOFOLLOW`), returning a
/// writable handle. `mode` is subject to the umask.
pub fn create_file_rooted(root: &Path, dst: &RepoPath, mode: u32) -> Result<File> {
    let mut r = resolve(root, dst, IoSide::Write, 0)?;
    landing_seam(&mut r);
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    match openat(r.dir.0, &r.name, flags, mode as libc::c_uint) {
        Ok(fd) => Ok(fd.into_file()),
        Err(e) => Err(dest_error(e, &r.shown)),
    }
}

/// Create a symlink whose target is the raw bytes of `target`.
pub fn symlink_rooted(root: &Path, dst: &RepoPath, target: &OsStr) -> Result<()> {
    crate::rooted::check_root(root)?;
    let shown = root.join(dst.as_str());
    if target.is_empty() {
        return Err(refusal(Code::InvalidInput, &shown, "empty_target", "symlink target is empty"));
    }
    let ct = sys::cos(target).map_err(|e| classify_errno(e, IoSide::Write, &shown))?;
    let mut r = resolve(root, dst, IoSide::Write, 0)?;
    let cn = cname(&r)?;
    landing_seam(&mut r);
    match sys::retry(|| unsafe { libc::symlinkat(ct.as_ptr(), r.dir.0, cn.as_ptr()) }) {
        Ok(_) => Ok(()),
        Err(e) => Err(dest_error(e, &r.shown)),
    }
}

fn mkdirat(dir: i32, name: &OsStr, mode: u32, shown: &Path) -> std::result::Result<(), i32> {
    let c = sys::cos(name).map_err(|_| libc::EINVAL)?;
    let _ = shown;
    sys::retry(|| unsafe { libc::mkdirat(dir, c.as_ptr(), mode as libc::mode_t) }).map(|_| ())
}

/// Create a directory; with `parents = true` like `mkdir -p`.
pub fn mkdir_rooted(root: &Path, dst: &RepoPath, mode: u32, parents: bool) -> Result<()> {
    if !parents {
        let mut r = resolve(root, dst, IoSide::Write, 0)?;
        landing_seam(&mut r);
        return mkdirat(r.dir.0, &r.name, mode, &r.shown).map_err(|e| dest_error(e, &r.shown));
    }
    let mut dir = open_root_fd(root, IoSide::Write)?;
    let comps = components(dst);
    let mut created: Vec<String> = Vec::new();
    let mut walked = root.to_path_buf();
    let mut prefix = String::new();
    let fail = |mut e: StrataError, created: &Vec<String>| {
        if !created.is_empty() {
            e.code = Code::Unknown;
            set_detail(&mut e, "created", json!(created));
        }
        e
    };
    for (i, comp) in comps.iter().enumerate() {
        let leaf = i + 1 == comps.len();
        walked.push(comp);
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(&String::from_utf8_lossy(comp.as_bytes()));
        hooks::before_open(i, comp);
        let mut tried_mkdir = false;
        loop {
            match openat(dir.0, comp, DIR_FLAGS, 0) {
                Ok(next) => {
                    dir = next;
                    break;
                }
                Err(libc::ENOENT) if !tried_mkdir => {
                    tried_mkdir = true;
                    match mkdirat(dir.0, comp, mode, &walked) {
                        Ok(()) => created.push(prefix.clone()),
                        Err(libc::EEXIST) => {}
                        Err(e) => return Err(fail(dest_error(e, &walked), &created)),
                    }
                }
                Err(e @ (libc::ENOTDIR | libc::ELOOP)) if leaf => {
                    let _ = e;
                    let mut err = errno_error(Code::Exists, libc::EEXIST, &walked, None);
                    set_detail(&mut err, "dst_exists", json!(true));
                    return Err(fail(err, &created));
                }
                Err(e) => return Err(fail(dir_open_error(e, Some(dir.0), comp, IoSide::Write, &walked), &created)),
            }
        }
    }
    Ok(())
}

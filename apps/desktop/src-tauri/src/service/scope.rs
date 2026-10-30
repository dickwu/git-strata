//! cli.md D38 for the GUI (daemon-stop-scope review C5): the app starts
//! the launch agent only when HOME is the login user's home, and only an
//! agent that launchd loaded from that home's plist. Checked before any
//! launchctl verb that changes anything.
//!
//! The desktop may not depend on strata-daemon (gui.md §2.3), so these
//! mirror `strata_daemon::plist::{login_home, anchored, lifecycle_scope}`
//! and `git_strata::launch::loaded_path`; keep them in step.

use std::ffi::{CStr, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::Code;

use crate::error::GuiError;

/// The login user's home from the password database (`getpwuid_r` for the
/// real uid), never `$HOME`. `None` when there is no entry, the lookup
/// fails or `pw_dir` is empty.
pub fn login_home() -> Option<PathBuf> {
    // SAFETY: getuid/sysconf have no failure mode that matters here.
    let uid = unsafe { libc::getuid() };
    const MAX: usize = 1 << 20;
    let mut size = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        n if n > 0 => (n as usize).min(MAX),
        _ => 4096,
    };
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        // SAFETY: passwd is plain data; getpwuid_r fills it with pointers
        // into `buf`, which outlives every read below.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), size, &mut out) };
        if rc == libc::ERANGE && size < MAX {
            size = (size * 2).min(MAX);
            continue;
        }
        if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        // SAFETY: pw_dir points into `buf`, NUL-terminated by getpwuid_r.
        let dir = unsafe { CStr::from_ptr(pw.pw_dir) }.to_bytes();
        if dir.is_empty() {
            return None;
        }
        return Some(PathBuf::from(OsStr::from_bytes(dir)));
    }
}

/// Canonicalise the longest existing ancestor of `p` and re-append the
/// rest, so a since-deleted plist under a symlinked alias still compares
/// equal to its canonical spelling.
pub fn anchored(p: &Path) -> PathBuf {
    let mut base = p.to_path_buf();
    let mut tail = Vec::new();
    loop {
        if let Ok(mut c) = fs::canonicalize(&base) {
            for t in tail.iter().rev() {
                c.push(t);
            }
            return c;
        }
        match (base.file_name().map(|n| n.to_os_string()), base.parent()) {
            (Some(n), Some(parent)) => {
                tail.push(n);
                base = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// The canonical home when `home` and `login` resolve to the same
/// directory; otherwise `foreign_home` and nothing reaches launchd.
pub fn lifecycle_scope(home: &Path, login: Option<&Path>) -> Result<PathBuf, GuiError> {
    let h = fs::canonicalize(home).ok();
    let l = login.and_then(|l| fs::canonicalize(l).ok());
    if let (Some(h), Some(l)) = (&h, &l)
        && h == l
    {
        return Ok(h.clone());
    }
    Err(GuiError::foreign_home(home, login))
}

/// The top-level `path = <p>` of a `launchctl print` service dump: the
/// plist launchd loaded the agent from. Nested dicts and `stdout path` /
/// `stderr path` do not count. `None` when absent (format change).
pub fn loaded_path(text: &str) -> Option<String> {
    let mut depth = 0i32;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('}') {
            depth -= 1;
            continue;
        }
        if depth == 1
            && let Some((k, v)) = t.split_once(" = ")
            && k.trim() == "path"
        {
            let v = v.trim();
            if !v.is_empty() && !v.ends_with('{') {
                return Some(v.to_string());
            }
        }
        if t.ends_with('{') {
            depth += 1;
        }
    }
    None
}

/// `Err(foreign_agent)` when launchd's dump names a plist other than
/// `expected`; no `path` line means it is ours (as in the cli).
pub fn owned(print: &str, expected: &Path) -> Result<(), GuiError> {
    match loaded_path(print) {
        Some(lp) if anchored(Path::new(&lp)) != anchored(expected) => {
            Err(GuiError::foreign_agent(&lp, expected))
        }
        _ => Ok(()),
    }
}

impl GuiError {
    pub fn foreign_home(home: &Path, login: Option<&Path>) -> GuiError {
        let msg = match login {
            Some(l) => format!(
                "the app manages only the login user's launch agent; HOME={} is not the login home {}. Nothing was done.",
                home.display(),
                l.display()
            ),
            None => format!(
                "the app manages only the login user's launch agent and the login home could not be resolved (HOME={}). Nothing was done.",
                home.display()
            ),
        };
        GuiError::new(
            Code::InvalidInput,
            msg,
            json!({
                "reason": "foreign_home",
                "home": home.to_string_lossy(),
                "login_home": login.map(|l| l.to_string_lossy().into_owned()),
            }),
        )
    }

    pub fn foreign_agent(loaded: &str, expected: &Path) -> GuiError {
        GuiError::new(
            Code::InvalidInput,
            format!(
                "the loaded launch agent comes from {loaded}, not this home's plist {}. Nothing was done.",
                expected.display()
            ),
            json!({
                "reason": "foreign_agent",
                "loaded_path": loaded,
                "expected": expected.to_string_lossy(),
            }),
        )
    }
}

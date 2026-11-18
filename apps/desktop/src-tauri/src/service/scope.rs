//! cli.md D38 for the GUI: the app changes the service only from the
//! login user's home and checks that the loaded job came from that home's
//! plist before starting it or changing the update fence.
//!
//! The desktop implements the login-home check locally (gui.md §2.3).
//! Start keeps the CLI's fallback for a missing path line and checks the
//! absolute plist path when present. Fence mutations require a complete
//! dump and positive ownership.

use std::ffi::{CStr, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::Code;

use crate::error::GuiError;

pub(super) fn env_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

pub(super) fn current_login_home() -> Result<PathBuf, GuiError> {
    let home = env_home().ok_or_else(GuiError::no_home)?;
    lifecycle_scope(&home, login_home().as_deref())
}

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
    for line in text.lines().map(str::trim) {
        if line == "}" {
            depth -= 1;
            continue;
        }
        if depth == 1
            && let Some((key, value)) = line.split_once('=')
            && key.trim_end() == "path"
            && !value.starts_with('>')
        {
            let path = value.trim();
            if !path.is_empty() && path != "{" {
                return Some(path.to_owned());
            }
        }
        if block_key(line).is_some() {
            depth += 1;
        }
    }
    None
}

fn block_key(line: &str) -> Option<&str> {
    let (key, value) = line.split_once('=')?;
    matches!(value.trim(), "{" | "> {").then(|| key.trim_end())
}

#[derive(Default)]
pub(super) struct JobFields<'a> {
    pub(super) path: Option<&'a str>,
    pub(super) program: Option<&'a str>,
    pub(super) pid: Option<u32>,
    pub(super) exit_timeout_s: Option<u64>,
}

/// Read the body after the service heading, including its closing brace.
/// Launchd indents its sections and prints arguments and environment values
/// verbatim. A brace in either list is data unless it closes the section at
/// the indentation of the opener.
pub(super) fn job_fields<'a>(lines: impl Iterator<Item = &'a str>) -> Option<JobFields<'a>> {
    let mut fields = JobFields::default();
    let mut blocks = vec![(0, false)];
    let mut state = None;

    for raw in lines.filter(|line| !line.trim().is_empty()) {
        let line = raw.trim_start();
        let indent = raw.len() - line.len();
        let (opener, values_only) = *blocks.last()?;
        if line.trim_end() == "}" && indent == opener {
            blocks.pop();
            continue;
        }
        if indent <= opener {
            return None;
        }
        if values_only {
            continue;
        }
        let line = line.trim_end();
        if line == "}" {
            return None;
        }
        if blocks.len() == 1
            && let Some((key, value)) = line.split_once('=')
        {
            let value = value.trim();
            match key.trim_end() {
                "path" => {
                    if value.is_empty() || value == "{" || fields.path.replace(value).is_some() {
                        return None;
                    }
                }
                "program" => {
                    if value.is_empty() || value == "{" || fields.program.replace(value).is_some() {
                        return None;
                    }
                }
                "pid" => {
                    let pid = value.parse::<u32>().ok()?;
                    if pid == 0 || fields.pid.replace(pid).is_some() {
                        return None;
                    }
                }
                "exit timeout" => {
                    let timeout = value.parse().ok()?;
                    if fields.exit_timeout_s.replace(timeout).is_some() {
                        return None;
                    }
                }
                "state" if !value.is_empty() && state.is_none() => state = Some(value),
                "state" => return None,
                _ => {}
            }
        }
        if let Some(key) = block_key(line) {
            let values_only = matches!(
                key,
                "arguments" | "inherited environment" | "default environment" | "environment"
            );
            blocks.push((indent, values_only));
        }
    }

    // A job waiting out its respawn throttle can have a scheduled spawn
    // and no current process. Bootout still requires the caller's fence.
    let no_process = matches!(state, Some("not running" | "spawn scheduled"));
    (blocks.is_empty() && (fields.pid.is_some() || no_process)).then_some(fields)
}

/// `Err(foreign_agent)` when launchd's dump names a plist other than
/// `expected`; no `path` line means it is ours (as in the cli).
pub fn owned(print: &str, expected: &Path) -> Result<(), GuiError> {
    if let Some(path) = loaded_path(print) {
        owned_path(&path, expected)?;
    }
    Ok(())
}

pub(super) fn owned_path(loaded: &str, expected: &Path) -> Result<(), GuiError> {
    let path = Path::new(loaded);
    if !path.is_absolute() || anchored(path) != anchored(expected) {
        return Err(GuiError::foreign_agent(loaded, expected));
    }
    Ok(())
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

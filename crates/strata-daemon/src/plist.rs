//! The launchd agent (daemon.md §3): `install_agent` writes
//! `~/Library/LaunchAgents/com.gwd.git-stratad.plist` and bootstraps it into
//! the user's GUI domain. The launchctl calls go through [`Launchd`] so tests
//! write into a temp HOME and record the calls instead of loading anything.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;
use strata_apfs::{IoSide, classify_io_error};
use strata_types::{Code, StrataError};

use crate::{AGENT_LABEL, Result};

/// `launchctl bootstrap` / `bootout` for one plist.
pub trait Launchd {
    fn bootstrap(&self, plist: &Path) -> Result<()>;
    fn bootout(&self, plist: &Path) -> Result<()>;
}

/// The real `launchctl`, in `gui/<uid>`.
pub struct Launchctl;

impl Launchctl {
    fn run(&self, verb: &str, plist: &Path) -> Result<()> {
        // SAFETY: getuid has no failure mode.
        let uid = unsafe { libc::getuid() };
        let out = Command::new("/bin/launchctl")
            .arg(verb)
            .arg(format!("gui/{uid}"))
            .arg(plist)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| classify_io_error(&e, IoSide::Read, Path::new("/bin/launchctl")))?;
        if !out.status.success() {
            return Err(StrataError::new(Code::InvalidInput, format!("launchctl {verb} failed"))
                .with_detail(json!({ "reason": "launchctl", "verb": verb, "stderr": String::from_utf8_lossy(&out.stderr) })));
        }
        Ok(())
    }
}

impl Launchd for Launchctl {
    fn bootstrap(&self, plist: &Path) -> Result<()> {
        self.run("bootstrap", plist)
    }
    fn bootout(&self, plist: &Path) -> Result<()> {
        self.run("bootout", plist)
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The plist text for `bin` (absolute) and `home` (absolute).
/// Review P1-5: launchd's SIGTERM → SIGKILL grace. Above the longest
/// bounded git call (attach, 600 s) so a stop drains in-flight writes
/// instead of killing a ref transaction (launchd's default is about 20 s).
pub const EXIT_TIMEOUT_SECS: u64 = 900;

pub fn plist_text(bin: &Path, home: &Path) -> String {
    let err = crate::err_log_path(&crate::config_dir_for_home(home));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
  </array>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>ExitTimeOut</key>
  <integer>{exit_timeout}</integer>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>SoftResourceLimits</key>
  <dict>
    <key>NumberOfFiles</key>
    <integer>{nofile}</integer>
  </dict>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>StandardErrorPath</key>
  <string>{err}</string>
</dict>
</plist>
"#,
        label = AGENT_LABEL,
        exit_timeout = EXIT_TIMEOUT_SECS,
        nofile = crate::startup::FD_LIMIT,
        bin = xml(&bin.to_string_lossy()),
        path = xml(&crate::install_path_for_home(home)),
        err = xml(&err.to_string_lossy()),
    )
}

/// `<home>/Library/LaunchAgents/<label>.plist`.
pub fn agent_plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{AGENT_LABEL}.plist"))
}

/// The login user's home from the password database (`getpwuid_r` for the
/// real uid), never `$HOME`. `None` when there is no entry, the lookup fails
/// or `pw_dir` is empty (cli.md D38).
pub fn login_home() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getuid/sysconf have no failure mode that matters here.
    let uid = unsafe { libc::getuid() };
    let mut size = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        n if n > 0 => n as usize,
        _ => 4096,
    };
    loop {
        let mut buf = vec![0 as libc::c_char; size];
        // SAFETY: passwd is plain data; getpwuid_r fills it with pointers
        // into `buf`, which outlives every read below.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), size, &mut out) };
        if rc == libc::ERANGE && size < (1 << 20) {
            size *= 2;
            continue;
        }
        if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        let dir = unsafe { CStr::from_ptr(pw.pw_dir) }.to_bytes();
        if dir.is_empty() {
            return None;
        }
        return Some(PathBuf::from(OsStr::from_bytes(dir)));
    }
}

/// Canonicalise the longest existing ancestor of `p` and re-append the
/// remaining components, so a since-deleted file under a symlinked alias
/// still compares equal to its canonical spelling. A tail that cannot be
/// split (`..`, empty) leaves `p` as given.
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

/// D38: agent lifecycle (`daemon start/stop`, `install_agent`) acts only
/// when `home` resolves to the login home. Returns the canonical home;
/// otherwise `InvalidInput`/`foreign_home` before anything touches launchd.
pub fn lifecycle_scope(home: &Path, login: Option<&Path>) -> Result<PathBuf> {
    let h = fs::canonicalize(home).ok();
    let l = login.and_then(|l| fs::canonicalize(l).ok());
    if let (Some(h), Some(l)) = (&h, &l)
        && h == l
    {
        return Ok(h.clone());
    }
    let msg = match login {
        Some(l) => format!(
            "daemon start/stop manage only the login user's LaunchAgent; HOME={} is not the login home {}. Nothing was done.",
            home.display(),
            l.display()
        ),
        None => format!(
            "daemon start/stop manage only the login user's LaunchAgent and the login home could not be resolved (HOME={}). Nothing was done.",
            home.display()
        ),
    };
    Err(StrataError::new(Code::InvalidInput, msg).with_detail(json!({
        "reason": "foreign_home",
        "home": home.to_string_lossy(),
        "login_home": login.map(|l| l.to_string_lossy().into_owned()),
    })))
}

fn absolute(what: &str, p: &Path) -> Result<()> {
    if !p.is_absolute() {
        return Err(StrataError::new(Code::InvalidInput, format!("{what} must be an absolute path"))
            .with_detail(json!({ "reason": "not_absolute", "field": what, "path": p })));
    }
    Ok(())
}

/// Write the plist under `home` (atomically, 0644) and bootstrap it.
pub fn install_agent_with(bin: &Path, home: &Path, launchd: &dyn Launchd) -> Result<PathBuf> {
    absolute("bin", bin)?;
    absolute("home", home)?;
    let path = agent_plist_path(home);
    let dir = path.parent().unwrap_or(home);
    fs::create_dir_all(dir).map_err(|e| classify_io_error(&e, IoSide::Write, dir))?;
    fs::create_dir_all(crate::config_dir_for_home(home)).map_err(|e| classify_io_error(&e, IoSide::Write, home))?;
    let tmp = dir.join(format!(".{AGENT_LABEL}.plist.tmp"));
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)
            .map_err(|e| classify_io_error(&e, IoSide::Write, &tmp))?;
        f.write_all(plist_text(bin, home).as_bytes()).map_err(|e| classify_io_error(&e, IoSide::Write, &tmp))?;
        f.sync_all().map_err(|e| classify_io_error(&e, IoSide::Write, &tmp))?;
    }
    fs::rename(&tmp, &path).map_err(|e| classify_io_error(&e, IoSide::Write, &path))?;
    launchd.bootstrap(&path)?;
    Ok(path)
}

/// `bootout`, then remove the plist.
pub fn uninstall_agent_with(home: &Path, launchd: &dyn Launchd) -> Result<()> {
    let path = agent_plist_path(home);
    launchd.bootout(&path)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(classify_io_error(&e, IoSide::Write, &path)),
    }
}

/// Production: the user's HOME and the real `launchctl`. Called by the CLI
/// in its own process when no daemon runs; never by a test.
pub fn install_agent(bin: &Path) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| StrataError::new(Code::InvalidInput, "HOME is not set"))?;
    let home = lifecycle_scope(Path::new(&home), login_home().as_deref())?;
    install_agent_with(bin, &home, &Launchctl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_home_is_an_absolute_existing_dir() {
        let h = login_home().expect("the test user has a passwd entry");
        assert!(h.is_absolute() && h.is_dir(), "{}", h.display());
    }

    #[test]
    fn scope_accepts_the_login_home_with_aliases_and_trailing_slash() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        fs::create_dir(&real).unwrap();
        let alias = t.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let canon = fs::canonicalize(&real).unwrap();
        for (home, login) in [(&real, &real), (&alias, &real), (&real, &alias), (&alias, &alias)] {
            assert_eq!(lifecycle_scope(home, Some(login)).unwrap(), canon);
        }
        let slash = PathBuf::from(format!("{}/", alias.display()));
        assert_eq!(lifecycle_scope(&slash, Some(&real)).unwrap(), canon);
    }

    #[test]
    fn scope_refuses_a_foreign_or_unresolvable_home() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let reason = |r: Result<PathBuf>| r.unwrap_err().detail.unwrap()["reason"].clone();
        assert_eq!(reason(lifecycle_scope(&a, Some(&b))), json!("foreign_home"));
        assert_eq!(reason(lifecycle_scope(&a, None)), json!("foreign_home"));
        assert_eq!(reason(lifecycle_scope(&a, Some(&t.path().join("missing")))), json!("foreign_home"));
        assert_eq!(reason(lifecycle_scope(&t.path().join("missing"), Some(&a))), json!("foreign_home"));
        assert_eq!(reason(lifecycle_scope(&a, Some(Path::new("")))), json!("foreign_home"));
        let e = lifecycle_scope(&a, None).unwrap_err();
        assert_eq!(e.detail.unwrap()["login_home"], serde_json::Value::Null);
    }

    #[test]
    fn anchored_keeps_a_deleted_tail_under_an_alias() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        fs::create_dir(&real).unwrap();
        let alias = t.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let canon = fs::canonicalize(&real).unwrap();
        assert_eq!(anchored(&alias.join("Library/LaunchAgents/x.plist")), canon.join("Library/LaunchAgents/x.plist"));
        assert_eq!(anchored(&canon.join("gone")), canon.join("gone"));
    }
}

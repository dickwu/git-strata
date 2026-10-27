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
    install_agent_with(bin, Path::new(&home), &Launchctl)
}

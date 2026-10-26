//! `daemon start` / `daemon stop` (cli.md §3.2, D15, D16): `launchctl` in
//! the per-user `gui/<uid>` domain with the agent plist at
//! `~/Library/LaunchAgents/com.gwd.git-stratad.plist`. Neither touches the
//! socket. `launchctl` is looked up on `PATH` (tests put a recorder first).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use strata_types::{Code, StrataError};

use crate::Result;
use crate::envelope::Envelope;

/// How long `stop` waits for launchd to drop the service after bootout.
const STOP_WAIT: Duration = Duration::from_secs(10);

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn target() -> String {
    format!("gui/{}/{}", uid(), strata_daemon::AGENT_LABEL)
}

fn launchctl(args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| StrataError::new(Code::Unknown, format!("cannot run launchctl: {e}")).with_detail(json!({ "reason": "launchctl" })))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

fn plist(home: &Path) -> Result<PathBuf> {
    let p = strata_daemon::plist::agent_plist_path(home);
    if !p.is_file() {
        return Err(StrataError::new(Code::InvalidInput, format!("no agent plist at {}", p.display()))
            .with_detail(json!({ "reason": "no_plist", "path": p.to_string_lossy() })));
    }
    Ok(p)
}

fn loaded() -> Result<bool> {
    Ok(launchctl(&["print", &target()])?.0)
}

fn failed(what: &str, text: &str) -> StrataError {
    StrataError::new(Code::Unknown, format!("launchctl {what} failed: {}", text.trim()))
        .with_detail(json!({ "reason": "launchctl", "step": what }))
}

pub fn start(home: &Path) -> Result<Envelope> {
    let p = plist(home)?;
    if loaded()? {
        return Ok(Envelope::ok(json!({ "already": true })));
    }
    let (ok, text) = launchctl(&["bootstrap", &format!("gui/{}", uid()), &p.to_string_lossy()])?;
    if !ok {
        return Err(failed("bootstrap", &text));
    }
    Ok(Envelope::ok(json!({})))
}

/// No plist needed: an agent loaded from a since-deleted plist still stops.
pub fn stop() -> Result<Envelope> {
    if !loaded()? {
        return Ok(Envelope::ok(json!({ "already": true })));
    }
    let (ok, text) = launchctl(&["bootout", &target()])?;
    if !ok {
        return Err(failed("bootout", &text));
    }
    // bootout can return while launchd still lists the service; a `start`
    // right after would then answer `already` and leave nothing running.
    // Return only once it is gone (D24).
    let deadline = Instant::now() + STOP_WAIT;
    while loaded()? {
        if Instant::now() >= deadline {
            return Err(StrataError::new(Code::Unknown, "launchctl bootout returned but the agent is still loaded")
                .with_detail(json!({ "reason": "launchctl", "step": "bootout_wait" })));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Envelope::ok(json!({})))
}

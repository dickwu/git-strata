//! `daemon start` / `daemon stop` (cli.md §3.2, D15, D16, D38): `launchctl`
//! in the per-user `gui/<uid>` domain with the agent plist at
//! `~/Library/LaunchAgents/com.gwd.git-stratad.plist`. Neither touches the
//! socket.
//!
//! D38: both act only when the resolved HOME is the login home
//! (`plist::lifecycle_scope`), checked before any `launchctl`, and only on
//! an agent loaded from this home's plist. The `gui/<uid>` domain is per
//! user, not per HOME, so a temp HOME must never reach it.
//!
//! `launchctl` is looked up on `PATH` here on purpose (strata-daemon's
//! installer runs `/bin/launchctl`): the integration tests run the binary
//! with a recorder-only PATH. Unit tests inject [`Launchctl`] through the
//! `_with` variants; [`start`]/[`stop`] themselves are never called by a test.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use strata_daemon::plist::{agent_plist_path, anchored, lifecycle_scope, login_home};
use strata_types::{Code, StrataError};

use crate::Result;
use crate::envelope::Envelope;

/// How long `stop` waits for launchd to drop the service after bootout.
const STOP_WAIT: Duration = Duration::from_secs(10);
const STOP_POLL: Duration = Duration::from_millis(100);

/// The one place a `launchctl` process is spawned.
pub trait Launchctl {
    /// The exit code (none on signal) and combined stdout and stderr.
    fn run(&self, args: &[&str]) -> Result<(Option<i32>, String)>;
}

/// Production: `launchctl` from `PATH`.
pub struct PathLaunchctl;

impl Launchctl for PathLaunchctl {
    fn run(&self, args: &[&str]) -> Result<(Option<i32>, String)> {
        let out = Command::new("launchctl")
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| StrataError::new(Code::Unknown, format!("cannot run launchctl: {e}")).with_detail(json!({ "reason": "launchctl" })))?;
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        Ok((out.status.code(), text))
    }
}

fn uid() -> u32 {
    // SAFETY: getuid has no failure mode.
    unsafe { libc::getuid() }
}

fn target() -> String {
    format!("gui/{}/{}", uid(), strata_daemon::AGENT_LABEL)
}

fn failed(what: &str, text: &str) -> StrataError {
    StrataError::new(Code::Unknown, format!("launchctl {what} failed: {}", text.trim()))
        .with_detail(json!({ "reason": "launchctl", "step": what }))
}

/// The top-level `path = <p>` of a `launchctl print` service dump: the plist
/// launchd loaded the agent from. Nested dicts and `stdout path` /
/// `stderr path` do not count. `None` when absent (format change).
pub(crate) fn loaded_path(text: &str) -> Option<String> {
    service_field(text, "path").map(str::to_owned)
}

fn service_field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let mut depth = 0i32;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('}') {
            depth -= 1;
            continue;
        }
        if depth == 1
            && let Some((k, v)) = t.split_once(" = ")
            && k.trim() == field
        {
            let v = v.trim();
            if !v.is_empty() && !v.ends_with('{') {
                return Some(v);
            }
        }
        if t.ends_with('{') {
            depth += 1;
        }
    }
    None
}

/// Only launchd's explicit service-not-found reply proves it is unloaded.
fn print_agent(lc: &dyn Launchctl) -> Result<Option<String>> {
    let (status, text) = lc.run(&["print", &target()])?;
    if status == Some(0) {
        return Ok(Some(text));
    }
    if status == Some(113) {
        let missing = format!(
            "Could not find service \"{}\" in domain for user gui:",
            strata_daemon::AGENT_LABEL
        );
        if text.lines().any(|line| {
            line.trim()
                .strip_prefix(&missing)
                .and_then(|uid| uid.trim().trim_end_matches('.').parse::<u32>().ok())
                == Some(uid())
        }) {
            return Ok(None);
        }
    }
    Err(failed("print", &text))
}

/// `print`: is the agent loaded, and (D38) was it loaded from `home`'s plist?
fn loaded_owned(lc: &dyn Launchctl, home: &Path) -> Result<Option<String>> {
    let Some(text) = print_agent(lc)? else {
        return Ok(None);
    };
    let expected = agent_plist_path(home);
    if let Some(lp) = loaded_path(&text)
        && anchored(Path::new(&lp)) != anchored(&expected)
    {
        let t = target();
        return Err(StrataError::new(
            Code::InvalidInput,
            format!(
                "the loaded agent {t} comes from {lp}, not this home's plist {}; nothing was done. If that agent is stale, the human can remove it with `launchctl bootout {t}`.",
                expected.display()
            ),
        )
        .with_detail(json!({ "reason": "foreign_agent", "loaded_path": lp, "expected": expected.to_string_lossy() })));
    }
    Ok(Some(text))
}

fn check_enabled(lc: &dyn Launchctl, domain: &str) -> Result<()> {
    let (status, text) = lc.run(&["print-disabled", domain])?;
    if status != Some(0) {
        return Err(failed("print-disabled", &text));
    }
    let Some(entries) = text.trim_start().strip_prefix("disabled services = {") else {
        return Err(failed("print-disabled", &text));
    };
    let mut disabled = false;
    for line in entries
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line == "}" {
            // launchd can print other blocks after the disabled services.
            return if disabled {
                Err(StrataError::new(
                    Code::Unsupported,
                    "The daemon's launch agent is disabled (an app update may be in progress; open GIT STRATA)",
                )
                .with_detail(json!({ "reason": "agent_disabled" })))
            } else {
                Ok(())
            };
        }
        let Some((label, value)) = line.split_once("=>") else {
            return Err(failed("print-disabled", &text));
        };
        if label.trim().trim_matches('"') != strata_daemon::AGENT_LABEL {
            continue;
        }
        match value.trim() {
            "enabled" | "false" => {}
            "disabled" | "true" => disabled = true,
            _ => return Err(failed("print-disabled", &text)),
        }
    }
    Err(failed("print-disabled", &text))
}

pub fn start(home: &Path) -> Result<Envelope> {
    start_with(home, login_home().as_deref(), &PathLaunchctl).inspect_err(|e| {
        if e.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some("agent_disabled") {
            eprintln!("An app update is in progress; open GIT STRATA to finish it");
        }
    })
}

pub fn stop(home: &Path) -> Result<Envelope> {
    stop_with(home, login_home().as_deref(), &PathLaunchctl)
}

pub fn start_with(home: &Path, login: Option<&Path>, lc: &dyn Launchctl) -> Result<Envelope> {
    let home = lifecycle_scope(home, login)?;
    // The canonical spelling, so launchd records a canonical `path`.
    let p = agent_plist_path(&home);
    if !p.is_file() {
        return Err(StrataError::new(Code::InvalidInput, format!("no agent plist at {}", p.display()))
            .with_detail(json!({ "reason": "no_plist", "path": p.to_string_lossy() })));
    }
    let loaded = loaded_owned(lc, &home)?;
    let domain = format!("gui/{}", uid());
    check_enabled(lc, &domain)?;
    let service = target();
    let plist = p.to_string_lossy();
    let bootstrap = ["bootstrap", domain.as_str(), plist.as_ref()];
    let kickstart = ["kickstart", service.as_str()];
    let args: &[&str] = match loaded {
        None => &bootstrap,
        Some(text) => {
            if service_field(&text, "pid")
                .is_some_and(|pid| pid.parse::<u32>().is_ok_and(|pid| pid > 0))
            {
                return Ok(Envelope::ok(json!({ "already": true })));
            }
            if loaded_path(&text).is_none() {
                return Err(failed(
                    "print",
                    "could not confirm the loaded agent's plist path",
                ));
            }
            &kickstart
        }
    };
    let (status, text) = lc.run(args)?;
    if status != Some(0) {
        return Err(failed(args[0], &text));
    }
    Ok(Envelope::ok(json!({})))
}

/// No plist needed: an agent loaded from a since-deleted plist still stops.
pub fn stop_with(home: &Path, login: Option<&Path>, lc: &dyn Launchctl) -> Result<Envelope> {
    stop_wait(home, login, lc, STOP_WAIT)
}

fn stop_wait(home: &Path, login: Option<&Path>, lc: &dyn Launchctl, wait: Duration) -> Result<Envelope> {
    let home = lifecycle_scope(home, login)?;
    if loaded_owned(lc, &home)?.is_none() {
        return Ok(Envelope::ok(json!({ "already": true })));
    }
    let (status, text) = lc.run(&["bootout", &target()])?;
    if status != Some(0) {
        return Err(failed("bootout", &text));
    }
    // bootout can return before launchd drops the service (D24).
    let deadline = Instant::now() + wait;
    loop {
        let message = match print_agent(lc) {
            Ok(None) => return Ok(Envelope::ok(json!({}))),
            Ok(Some(_)) => "launchctl bootout returned but the agent is still loaded".into(),
            Err(e) => format!(
                "launchctl bootout returned but unloading could not be confirmed: {}",
                e.message
            ),
        };
        if Instant::now() >= deadline {
            return Err(StrataError::new(Code::Unknown, message)
                .with_detail(json!({ "reason": "launchctl", "step": "bootout_wait" })));
        }
        std::thread::sleep(STOP_POLL);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;

    /// Records every call; `reply(verb, n)` answers the n-th call (0-based).
    struct Mock<F: Fn(&str, usize) -> (i32, String)> {
        log: RefCell<Vec<String>>,
        reply: F,
    }

    impl<F: Fn(&str, usize) -> (i32, String)> Launchctl for Mock<F> {
        fn run(&self, args: &[&str]) -> Result<(Option<i32>, String)> {
            let n = self.log.borrow().len();
            self.log.borrow_mut().push(args.join(" "));
            let (status, text) = (self.reply)(args[0], n);
            Ok((Some(status), text))
        }
    }

    fn mock<F: Fn(&str, usize) -> (i32, String)>(reply: F) -> Mock<F> {
        Mock { log: RefCell::new(Vec::new()), reply }
    }

    fn not_found() -> (i32, String) {
        (
            113,
            format!(
                "Could not find service \"{}\" in domain for user gui: {}",
                strata_daemon::AGENT_LABEL,
                uid(),
            ),
        )
    }

    fn disabled_list() -> (i32, String) {
        (0, "disabled services = {\n}\n".into())
    }

    fn dump(path: &Path) -> String {
        format!(
            "{t} = {{\n\tactive count = 1\n\tpath = {p}\n\ttype = LaunchAgent\n\tpid = 42\n\tstdout path = /dev/null\n\tstderr path = /tmp/x.log\n\tenvironment = {{\n\t\tPATH => /usr/bin\n\t}}\n}}\n",
            t = target(),
            p = path.display()
        )
    }

    struct H {
        _t: tempfile::TempDir,
        home: PathBuf,
        alias: PathBuf,
        other: PathBuf,
    }

    fn homes() -> H {
        let t = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(t.path()).unwrap().join("home");
        let other = fs::canonicalize(t.path()).unwrap().join("other");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&other).unwrap();
        let alias = t.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        H { _t: t, home, alias, other }
    }

    fn with_plist(home: &Path) -> PathBuf {
        let p = agent_plist_path(home);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "<plist/>\n").unwrap();
        p
    }

    fn reason(r: Result<Envelope>) -> Value {
        match r {
            Err(e) => e.detail.expect("detail")["reason"].clone(),
            Ok(env) => panic!("expected an error, got {}", env.data),
        }
    }

    fn calls<F: Fn(&str, usize) -> (i32, String)>(m: &Mock<F>) -> Vec<String> {
        m.log.borrow().clone()
    }

    // U1: the incident. A foreign HOME refuses start and stop with zero calls.
    #[test]
    fn foreign_home_refuses_before_any_launchctl() {
        let h = homes();
        with_plist(&h.other);
        let m = mock(|_, _| (0, String::new()));
        assert_eq!(reason(stop_with(&h.other, Some(&h.home), &m)), json!("foreign_home"));
        assert_eq!(reason(start_with(&h.other, Some(&h.home), &m)), json!("foreign_home"));
        assert!(calls(&m).is_empty());
    }

    // U14: login home unresolvable (no entry, empty, missing dir).
    #[test]
    fn unresolvable_login_home_refuses_with_zero_calls() {
        let h = homes();
        with_plist(&h.home);
        let m = mock(|_, _| (0, String::new()));
        for login in [None, Some(PathBuf::new()), Some(h.home.join("missing"))] {
            assert_eq!(reason(stop_with(&h.home, login.as_deref(), &m)), json!("foreign_home"));
            assert_eq!(reason(start_with(&h.home, login.as_deref(), &m)), json!("foreign_home"));
        }
        let e = stop_with(&h.home, None, &m).err().unwrap();
        assert_eq!(e.detail.unwrap()["login_home"], Value::Null);
        assert!(calls(&m).is_empty());
    }

    // U4: HOME empty or a path that does not exist: refused with zero calls.
    #[test]
    fn missing_home_refuses_with_zero_calls() {
        let h = homes();
        with_plist(&h.home);
        let m = mock(|_, _| (0, String::new()));
        for home in [PathBuf::new(), h.home.join("missing")] {
            assert_eq!(reason(stop_with(&home, Some(&h.home), &m)), json!("foreign_home"));
            assert_eq!(reason(start_with(&home, Some(&h.home), &m)), json!("foreign_home"));
        }
        assert!(calls(&m).is_empty());
    }

    // U2/U5 (was daemon_stop_waits_until_launchd_drops_the_agent): owned
    // agent, still listed three times after bootout, then gone (D24).
    #[test]
    fn stop_waits_until_launchd_drops_the_agent() {
        let h = homes();
        let p = agent_plist_path(&h.home);
        let m = mock(|verb, n| match verb {
            "print" if n < 5 => (0, dump(&p)),
            "print" => not_found(),
            _ => (0, String::new()),
        });
        assert_eq!(stop_with(&h.home, Some(&h.home), &m).unwrap().data, json!({}));
        let t = target();
        let mut want = vec![format!("print {t}"), format!("bootout {t}")];
        want.extend(std::iter::repeat_n(format!("print {t}"), 4));
        assert_eq!(calls(&m), want);
    }

    #[test]
    fn stop_reports_an_agent_that_never_leaves() {
        let h = homes();
        let m = mock(|_, _| (0, String::new()));
        let r = stop_wait(&h.home, Some(&h.home), &m, Duration::from_millis(1));
        assert_eq!(r.err().unwrap().detail.unwrap()["step"], json!("bootout_wait"));
    }

    // U3/U8 (was daemon_start_stop_do_not_use_the_socket): the full cycle,
    // D16 `already` both ways, and no_plist before launchctl.
    #[test]
    fn start_stop_cycle_and_already() {
        let h = homes();
        let p = with_plist(&h.home);
        let state = RefCell::new(false);
        let m = mock(|verb, _| match verb {
            "print" if *state.borrow() => (0, dump(&p)),
            "print" => not_found(),
            "print-disabled" => disabled_list(),
            "bootstrap" => {
                *state.borrow_mut() = true;
                (0, String::new())
            }
            "bootout" => {
                *state.borrow_mut() = false;
                (0, String::new())
            }
            _ => panic!("unexpected verb {verb}"),
        });
        assert_eq!(start_with(&h.home, Some(&h.home), &m).unwrap().data, json!({}));
        assert_eq!(start_with(&h.home, Some(&h.home), &m).unwrap().data, json!({ "already": true }));
        assert_eq!(stop_with(&h.home, Some(&h.home), &m).unwrap().data, json!({}));
        assert_eq!(stop_with(&h.home, Some(&h.home), &m).unwrap().data, json!({ "already": true }));
        let (uid, t) = (uid(), target());
        let want = vec![
            format!("print {t}"),
            format!("print-disabled gui/{uid}"),
            format!("bootstrap gui/{uid} {}", p.display()),
            format!("print {t}"),
            format!("print-disabled gui/{uid}"),
            format!("print {t}"),
            format!("bootout {t}"),
            format!("print {t}"),
            format!("print {t}"),
        ];
        assert_eq!(calls(&m), want);
        fs::remove_file(&p).unwrap();
        assert_eq!(reason(start_with(&h.home, Some(&h.home), &m)), json!("no_plist"));
        assert_eq!(calls(&m).len(), want.len());
    }

    // Claude r2 C2: an aliased HOME bootstraps the canonical plist path.
    #[test]
    fn start_under_an_alias_bootstraps_the_canonical_plist() {
        let h = homes();
        let p = with_plist(&h.home);
        let m = mock(|verb, _| match verb {
            "print" => not_found(),
            "print-disabled" => disabled_list(),
            "bootstrap" => (0, String::new()),
            _ => panic!("unexpected verb {verb}"),
        });
        assert_eq!(
            start_with(&h.alias, Some(&h.home), &m).unwrap().data,
            json!({})
        );
        assert_eq!(
            calls(&m)[2],
            format!("bootstrap gui/{} {}", uid(), p.display())
        );
    }

    // U6/U7: an agent loaded from another plist is neither stopped nor
    // reported `already`; the message names the manual bootout.
    #[test]
    fn foreign_agent_is_left_alone() {
        let h = homes();
        with_plist(&h.home);
        let foreign = agent_plist_path(&h.other);
        let m = mock(|_, _| (0, dump(&foreign)));
        let e = stop_with(&h.home, Some(&h.home), &m).err().unwrap();
        assert!(e.message.contains(&format!("launchctl bootout {}", target())), "{}", e.message);
        let d = e.detail.unwrap();
        assert_eq!(d["reason"], json!("foreign_agent"));
        assert_eq!(d["loaded_path"], json!(foreign.to_string_lossy()));
        assert_eq!(reason(start_with(&h.home, Some(&h.home), &m)), json!("foreign_agent"));
        assert_eq!(calls(&m), vec![format!("print {}", target()); 2]);
    }

    // U12 (r1 Codex B2): HOME is an alias, launchd reports the canonical
    // path, the plist was deleted: still ours, so print, bootout, poll.
    #[test]
    fn alias_home_with_deleted_plist_still_stops() {
        let h = homes();
        let canon = agent_plist_path(&h.home);
        let m = mock(|verb, n| match verb {
            "print" if n == 0 => (0, dump(&canon)),
            "print" => not_found(),
            _ => (0, String::new()),
        });
        assert_eq!(stop_with(&h.alias, Some(&h.home), &m).unwrap().data, json!({}));
        let t = target();
        assert_eq!(calls(&m), vec![format!("print {t}"), format!("bootout {t}"), format!("print {t}")]);
        // And the reverse: canonical HOME, launchd loaded it via the alias.
        let aliased = agent_plist_path(&h.alias);
        let m = mock(|verb, n| match verb {
            "print" if n == 0 => (0, dump(&aliased)),
            "print" => not_found(),
            _ => (0, String::new()),
        });
        assert_eq!(stop_with(&h.home, Some(&h.alias), &m).unwrap().data, json!({}));
    }

    // U13: trailing slash on HOME.
    #[test]
    fn trailing_slash_home_is_the_login_home() {
        let h = homes();
        let slash = PathBuf::from(format!("{}/", h.home.display()));
        let m = mock(|_, _| not_found());
        assert_eq!(stop_with(&slash, Some(&h.home), &m).unwrap().data, json!({ "already": true }));
    }

    // No top-level `path` line (format change): proceed, the home guard
    // already scoped the call (D38).
    #[test]
    fn missing_path_line_proceeds() {
        let h = homes();
        let m = mock(|verb, n| match verb {
            "print" if n == 0 => (0, "something = {\n\tstderr path = /x\n}\n".to_string()),
            "print" => not_found(),
            _ => (0, String::new()),
        });
        assert_eq!(stop_with(&h.home, Some(&h.home), &m).unwrap().data, json!({}));
    }

    // U11/U16: parser.
    #[test]
    fn loaded_path_takes_only_the_top_level_key() {
        let p = Path::new("/Users/Jane Doe/Library/LaunchAgents/com.gwd.git-stratad.plist");
        assert_eq!(loaded_path(&dump(p)).as_deref(), Some(p.to_str().unwrap()));
        let nested = "svc = {\n\tstderr path = /a\n\tsub = {\n\t\tpath = /nested\n\t}\n\tpath = /top  \n}\n";
        assert_eq!(loaded_path(nested).as_deref(), Some("/top"));
        assert_eq!(loaded_path("svc = {\n\tstdout path = /a\n\tstderr path = /b\n}\n"), None);
        assert_eq!(loaded_path("path = /no/header\n"), None);
        assert_eq!(loaded_path(""), None);
    }
}

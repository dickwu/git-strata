//! dev-ports: `session ports` / `session exec` against the in-process
//! daemon (leases from `new`, reassign, the availability check, exec's env
//! and cwd), read-only use with the daemon down, an old daemon.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::live::{Live, write};
use support::{FakeDaemon, Run};

fn files() -> Vec<(&'static str, &'static [u8])> {
    vec![("a.txt", b"one\n")]
}

fn real(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap()
}

fn lease(r: &Run) -> Value {
    let d = r.ok();
    let ls = d["leases"].as_array().unwrap_or_else(|| panic!("{r:?}"));
    assert_eq!(ls.len(), 1, "{d}");
    ls[0].clone()
}

fn web(l: &Value) -> u16 {
    l["ports"]["web"].as_u64().unwrap() as u16
}

/// A listener on `port` whose cwd is `dir` (a child we own and kill).
struct Listening(Child);

impl Listening {
    fn start(dir: &Path, port: u16) -> Listening {
        let code = format!(
            "import socket,time\ns=socket.socket()\ns.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\ns.bind(('127.0.0.1',{port}))\ns.listen()\nprint('up',flush=True)\ntime.sleep(60)\n"
        );
        let mut c = Command::new("python3").args(["-c", &code]).current_dir(dir).stdout(Stdio::piped()).spawn().expect("python3");
        let mut out = c.stdout.take().unwrap();
        let mut buf = [0u8; 3];
        std::io::Read::read_exact(&mut out, &mut buf).expect("listener up");
        Listening(c)
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_free(port: u16) {
    let t = Instant::now();
    while std::net::TcpListener::bind(("127.0.0.1", port)).is_err() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

const SHOW: &str = "printf '%s|%s|%s|%s|%s' \"$PORT\" \"$STRATA_SESSION\" \"$STRATA_REPO\" \"$(pwd -P)\" \"$TAURI_CONNECTOR_PROJECT\"";

#[test]
fn ports_are_leased_at_new_read_without_the_daemon_and_exec_runs_with_them() {
    let lv = Live::new(&["tool"], &files());
    let (s, ws, l) = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = real(&lv.ws(&s, "tool"));
        let l = lease(&lv.run_in(&ws, &["session", "ports"]));
        (s, ws, l)
    });
    assert_eq!(l["session"], s.as_str());
    assert_eq!(l["repo"], "tool");
    assert_eq!(l["path"], ws.to_str().unwrap());
    assert_eq!(l["tauri_app_root"], Value::Null);
    let w = web(&l);
    assert!((20000..=29996).contains(&w) && w.is_multiple_of(4), "{l}");
    assert_eq!(l["ports"], json!({ "web": w, "connector": w + 1, "connector_mcp": w + 2, "aux": w + 3 }));
    let env = l["env"].as_object().unwrap();
    assert_eq!(env.len(), 9, "{l}");
    assert_eq!(env["PORT"], w.to_string());
    assert_eq!(env["STRATA_CONNECTOR_MCP_PORT"], (w + 2).to_string());
    assert_eq!(env["TAURI_CONNECTOR_PROJECT"], ws.to_str().unwrap());
    assert_eq!(env["STRATA_TAURI_DEV_CONFIG"], format!(r#"{{"build":{{"devUrl":"http://localhost:{w}"}}}}"#));

    // Daemon down: a leased workspace still answers (store, read-only).
    assert_eq!(lease(&lv.run_in(&ws, &["session", "ports"])), l);
    assert_eq!(lease(&lv.run(&["session", "ports", "--session", s.as_str()])), l);
    let all = lv.run(&["session", "ports", "--all", "--check"]).ok();
    assert_eq!(all["leases"][0]["check"]["web"]["state"], "free", "{all}");

    // exec from a subdirectory keeps it; an inherited PORT is overridden.
    fs::create_dir_all(ws.join("sub/src-tauri")).unwrap();
    write(&ws, "sub/src-tauri/tauri.conf.json", b"{}");
    let sub = ws.join("sub/src-tauri");
    let r = Run::from(lv.home.cmd(&["session", "exec", "--", "sh", "-c", SHOW]).current_dir(&sub).env("PORT", "3000").output().unwrap());
    assert_eq!(r.code, 0, "{r:?}");
    assert_eq!(r.stdout, format!("{w}|{s}|tool|{}|{}", sub.display(), ws.join("sub").display()));
    // From outside every workspace: the workspace root.
    let r = lv.run(&["session", "exec", "--session", s.as_str(), "--", "sh", "-c", SHOW]);
    assert_eq!(r.stdout, format!("{w}|{s}|tool|{}|{}", ws.display(), ws.display()), "{r:?}");
    // The command's exit status is exec's.
    assert_eq!(lv.run_in(&ws, &["session", "exec", "--", "sh", "-c", "exit 7"]).code, 7);
    // A missing command: NoSuchPath envelope.
    let r = lv.run_in(&ws, &["session", "exec", "--", "no-such-command-xyz"]);
    assert_eq!(r.err_code(), "NoSuchPath", "{r:?}");
}

#[test]
fn selection_errors() {
    let lv = Live::new(&["tool", "front"], &files());
    let (a, b) = lv.serve(|_| (lv.new_session("T-1", "tool,front"), lv.new_session("T-2", "tool")));
    let r = lv.run(&["session", "exec", "--session", a.as_str(), "--", "true"]);
    assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("InvalidInput", "ambiguous_repo"), "{r:?}");
    let two = lv.run(&["session", "ports", "--session", a.as_str()]).ok();
    assert_eq!(two["leases"].as_array().unwrap().len(), 2, "{two}");
    let wb = lv.ws(&b, "tool");
    let r = lv.run_in(&wb, &["session", "exec", "--session", a.as_str(), "--repo", "tool", "--", "true"]);
    assert_eq!(r.reason(), "cwd_mismatch", "{r:?}");
    let r = lv.run(&["session"]);
    assert_eq!(r.reason(), "unknown_command", "{r:?}");
    assert_eq!(r.env()["error"]["detail"]["group"], "session");
    assert_eq!(r.env()["error"]["detail"]["commands"], json!(["exec", "ports"]));
    let r = lv.run(&["session", "ports", "--all", "--session", a.as_str()]);
    assert_eq!(r.reason(), "both_selectors", "{r:?}");
    let r = lv.run(&["session", "ports", "--all", "--repo", "tool"]);
    assert_eq!(r.reason(), "both_selectors", "{r:?}");
    let r = lv.run_in(&wb, &["session", "exec", "--tauri", "--", "sh", "-c", "tauri dev"]);
    assert_eq!(r.reason(), "tauri_launcher", "{r:?}");
    assert!(r.env()["error"]["detail"]["fix"].as_str().unwrap().contains("STRATA_TAURI_DEV_CONFIG"));
}

#[test]
fn tauri_gets_the_dev_url_patch_and_scripts_are_checked() {
    let lv = Live::new(&["tool"], &files());
    let (s, ws, w) = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = real(&lv.ws(&s, "tool"));
        let w = web(&lease(&lv.run_in(&ws, &["session", "ports"])));
        (s, ws, w)
    });
    let _ = s;
    let bin = lv.sb.join("tbin");
    fs::create_dir_all(&bin).unwrap();
    let tauri = bin.join("tauri");
    fs::write(&tauri, "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done\n").unwrap();
    fs::set_permissions(&tauri, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
    let run = |args: &[&str]| Run::from(lv.home.cmd(args).current_dir(&ws).env("PATH", &path).output().unwrap());
    let patch = format!(r#"{{"build":{{"devUrl":"http://localhost:{w}"}}}}"#);
    let r = run(&["session", "exec", "--tauri", "--", "tauri", "dev", "--", "--app-arg"]);
    assert_eq!(r.stdout, format!("[dev][--config][{patch}][--][--app-arg]"), "{r:?}");
    write(&ws, "package.json", br#"{"scripts":{"tauri:dev":"tauri dev $X","dev":"next dev"}}"#);
    for script in ["tauri:dev", "dev"] {
        let r = run(&["session", "exec", "--tauri", "--", "npm", "run", script]);
        assert_eq!(r.reason(), "tauri_launcher", "{r:?}");
    }
}

#[test]
fn the_check_reassign_and_an_own_server() {
    let lv = Live::new(&["tool"], &files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = real(&lv.ws(&s, "tool"));
        let l = lease(&lv.run_in(&ws, &["session", "ports"]));
        let w = web(&l);
        let fix = format!("git-strata session ports --reassign --session {s} --repo tool");

        // A foreign listener (cwd outside the workspace) on the connector port.
        let foreign = Listening::start(&lv.sb, w + 1);
        let r = lv.run_in(&ws, &["session", "exec", "--", "true"]);
        assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("PortInUse", "foreign"), "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["fix"], fix.as_str());
        let c = lease(&lv.run_in(&ws, &["session", "ports", "--check"]));
        assert_eq!(c["check"]["connector"]["state"], "foreign", "{c}");
        assert_eq!(lv.run_in(&ws, &["session", "exec", "--no-check", "--", "true"]).code, 0);
        // Reassign is the remedy.
        let moved = lease(&lv.run_in(&ws, &["session", "ports", "--reassign"]));
        let w2 = web(&moved);
        assert_ne!(w2, w);
        assert_eq!(lease(&lv.run_in(&ws, &["session", "ports"]))["ports"], moved["ports"]);
        assert_eq!(lv.run_in(&ws, &["session", "exec", "--", "true"]).code, 0);
        drop(foreign);

        // This workspace's own server: exec and reassign refuse.
        let own = Listening::start(&ws, w2);
        let r = lv.run_in(&ws, &["session", "exec", "--", "true"]);
        assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("PortInUse", "already_running"), "{r:?}");
        let r = lv.run_in(&ws, &["session", "ports", "--reassign"]);
        assert_eq!(r.reason(), "already_running", "{r:?}");
        drop(own);
        wait_free(w2);
        assert_eq!(lv.run_in(&ws, &["session", "exec", "--", "true"]).code, 0);
    });
}

#[test]
fn an_old_daemon_cannot_reassign() {
    fn old(req: &Value) -> Value {
        json!({ "id": req["id"], "ok": false, "error": { "rpc": "UnknownMethod", "message": "unknown method" } })
    }
    let lv = Live::new(&["tool"], &files());
    let (s, ws, w) = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        let w = web(&lease(&lv.run_in(&ws, &["session", "ports"])));
        (s, ws, w)
    });
    let _ = fs::remove_file(lv.home.socket());
    let d = FakeDaemon::start(&lv.home.socket(), old);
    let r = lv.run_in(&ws, &["session", "ports", "--reassign"]);
    assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("Unsupported", "daemon_too_old"), "{r:?}");
    assert_eq!(d.requests()[0]["method"], "ports_reassign");
    // the base the cli checked, so the daemon can refuse a stale check
    assert_eq!(d.requests()[0]["params"], json!({ "session": s.as_str(), "repo": "tool", "checked": w }));
}

/// A range with one block: the second workspace of a session gets none.
/// `ports` lists it (`ports: null, reason: ports_exhausted`) beside the
/// leased one; `exec` there refuses.
#[test]
fn an_exhausted_workspace_is_an_entry_not_a_failed_call() {
    let lv = Live::new(&["tool", "front"], &files());
    let lo = 40000 + (std::process::id() % 2000) as u16 * 4;
    let cfg = lv.home.config_dir().join("config.toml");
    let mut t = fs::read_to_string(&cfg).unwrap();
    t.push_str(&format!("\n[dev_ports]\nrange = \"{lo}-{}\"\n", lo + 3));
    fs::write(&cfg, t).unwrap();
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool,front");
        let all = lv.run(&["session", "ports", "--session", s.as_str()]);
        let ls = all.ok()["leases"].as_array().unwrap().clone();
        assert_eq!(ls.len(), 2, "{all:?}");
        let (leased, none): (Vec<&Value>, Vec<&Value>) = ls.iter().partition(|l| !l["ports"].is_null());
        assert_eq!((leased.len(), none.len()), (1, 1), "{all:?}");
        assert_eq!(web(leased[0]), lo);
        assert_eq!(none[0]["reason"], "ports_exhausted", "{all:?}");
        let repo = none[0]["repo"].as_str().unwrap();
        let r = lv.run_in(&lv.ws(&s, repo), &["session", "exec", "--", "true"]);
        assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("Busy", "ports_exhausted"), "{r:?}");
    });
}

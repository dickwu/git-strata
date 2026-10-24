//! daemon.md §8 Task 3: single instance, recover, then listen (§5.1, §5.9).

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_daemon::DaemonWarning;
use strata_daemon::startup::{self, Daemon};
use strata_store::PublishPhase;
use strata_types::Code;
use support::*;

fn calls_mention(core: &startup::Core, needle: &str) -> Vec<String> {
    let mut out = Vec::new();
    for g in core.gits.values() {
        for c in g.take_git_calls() {
            let line = format!(
                "{} :: {}",
                c.cwd.display(),
                c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
            );
            if line.contains(needle) {
                out.push(line);
            }
        }
    }
    out
}

fn push_calls(core: &startup::Core) -> Vec<String> {
    let mut out = Vec::new();
    for g in core.gits.values() {
        for c in g.take_git_calls() {
            if c.args.iter().any(|a| a == "push" || a == "send-pack") {
                out.push(format!("{:?}", c.args));
            }
        }
    }
    out
}

fn status(d: &Daemon<'_>) -> Value {
    d.daemon_status(false).unwrap()
}

fn unknown_ids(v: &Value) -> Vec<u64> {
    let mut ids: Vec<u64> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.get("UnknownPublish").map(|u| u["id"].as_u64().unwrap()))
        .collect();
    ids.sort();
    ids
}

/// Serve `d` on its socket in a scoped thread, run `f`, stop.
fn serving<R>(d: &Daemon<'_>, f: impl FnOnce() -> R) -> R {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| d.serve(&listener, &ext, &|req| d.handle(req)));
        // Stop even when `f` panics, or the scope would wait forever.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn call(socket: &Path, line: &str) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut r = BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap()
}

#[test]
fn unknown_publish_reported_on_restart_not_replayed() {
    let fx = Dfx::new();
    let (unknown, running) = {
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let id = d.sessions.new(None, &[repo_name("tool")]).unwrap().manifest.id;
        fx.sb.write(&ws(&core, &id, "tool"), "src/a.ts", b"a\n");
        let cp = checkpoint(&core, &id, "tool");
        let now = SystemTime::now();
        let unknown = core.store.record_publish(&group(vec![batch(&id, "tool", cp.id, &["src/a.ts"])]), now).unwrap();
        assert!(core.store.set_publish_phase(unknown, PublishPhase::Running, PublishPhase::Unknown, None, now).unwrap());
        let running = core.store.record_publish(&group(vec![batch(&id, "tool", cp.id, &["src/a.ts"])]), now).unwrap();
        drop(d);
        core.close().unwrap();
        (unknown, running)
    };
    let origin = fx.sb.path("tool-origin.git");
    let before = fx.sb.git(&origin, &["for-each-ref"]);

    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let mut want = vec![unknown, running];
    want.sort();
    assert_eq!(unknown_ids(&status(&d)), want);
    for id in [unknown, running] {
        assert_eq!(core.store.publish_record(id).unwrap().unwrap().phase, PublishPhase::Unknown);
    }
    assert!(core.store.open_publishes().unwrap().iter().all(|r| r.phase == PublishPhase::Unknown));
    assert_eq!(push_calls(&core), Vec::<String>::new());
    assert_eq!(fx.sb.git(&origin, &["for-each-ref"]), before, "origin untouched");
    // Over the socket too.
    let v = serving(&d, || call(&fx.socket(), r#"{"id":1,"method":"daemon_status","params":{}}"#));
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(unknown_ids(&v["data"]), want);
    drop(d);
    core.close().unwrap();

    // A third start: still reported, still not replayed.
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    assert_eq!(unknown_ids(&status(&d)), want);
    assert_eq!(push_calls(&core), Vec::<String>::new());
    drop(d);
    core.close().unwrap();
}

#[test]
fn second_instance_refused_then_reacquired() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    // O_CLOEXEC: no child inherits the lock.
    let flags = unsafe { libc::fcntl(core.instance_lock().raw_fd(), libc::F_GETFD) };
    assert!(flags & libc::FD_CLOEXEC != 0);
    let d = Daemon::start(&core).unwrap();

    // A second instance (its own open file description) gets Exists, not Busy,
    // before it reads config or opens the store.
    let e = fx.prepare().err().expect("second instance refused");
    assert_eq!(e.code, Code::Exists);
    assert_eq!(reason(&e), Some("daemon_running"));
    let e = startup::InstanceLock::take(&strata_daemon::lock_path(&fx.config_dir)).err().unwrap();
    assert_eq!(e.code, Code::Exists);

    serving(&d, || assert!(strata_daemon::rpc::ready(&fx.socket(), Duration::from_secs(5))));
    drop(d);
    let stopped = Instant::now();
    core.close().unwrap();
    assert!(!fx.socket().exists(), "socket unlinked on shutdown");

    // Within a second a new instance takes the lock and binds.
    let core = fx.prepare().unwrap();
    let l = startup::bind(&core.opts.socket).unwrap();
    assert!(stopped.elapsed() < Duration::from_secs(1));
    let mode = fs::metadata(fx.socket()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    drop(l);
    core.close().unwrap();
}

#[test]
fn quarantine_is_an_alert() {
    let fx = Dfx::new();
    // A real session so the shadow repository exists.
    let (id, shadow) = {
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let id = d.sessions.new(None, &[repo_name("tool")]).unwrap().manifest.id;
        drop(d);
        core.close().unwrap();
        let shadow = fx.config.paths.root.join(format!("sessions/.repos/{id}/tool.git"));
        assert!(shadow.is_dir(), "shadow at {}", shadow.display());
        (id, shadow)
    };
    let q = shadow.join("strata-quarantine");
    fs::create_dir_all(&q).unwrap();
    let entry = q.join("main-0123abcd");
    std::os::unix::fs::symlink("/nonexistent/target", &entry).unwrap();

    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let w = status(&d)["warnings"].clone();
    assert!(
        w.as_array().unwrap().contains(&serde_json::to_value(DaemonWarning::Quarantine { path: entry.clone() }).unwrap()),
        "{w}"
    );
    let alerts = fx.alerts();
    assert_eq!(alerts.iter().filter(|l| l.contains("quarantine")).count(), 1, "{alerts:?}");
    assert!(alerts[0].contains(entry.to_str().unwrap()));
    // No gc, worktree prune or commit ran in that shadow repository.
    let touched = calls_mention(&core, shadow.to_str().unwrap());
    for t in &touched {
        assert!(!t.contains(" gc") && !t.contains("prune") && !t.contains("commit"), "{t}");
    }
    assert!(fs::symlink_metadata(&entry).is_ok(), "quarantine entry left alone");
    // A second status does not print again.
    let _ = status(&d);
    assert_eq!(fx.alerts().len(), alerts.len());
    drop(d);
    core.close().unwrap();
    let _ = id;
}

#[test]
fn quarantine_scan_reports_non_directory_worktrees() {
    let root = tempfile::tempdir().unwrap();
    let shadow = root.path().join("sessions/.repos/s1/tool.git");
    fs::create_dir_all(&shadow).unwrap();
    fs::write(shadow.join("worktrees"), b"x").unwrap();
    let other = root.path().join("sessions/.repos/s2/web.git/worktrees");
    fs::create_dir_all(&other).unwrap();
    std::os::unix::fs::symlink("/tmp", other.join("ws")).unwrap();
    fs::create_dir_all(other.join("fine")).unwrap();
    let got = startup::quarantine_entries(root.path());
    assert_eq!(got, vec![shadow.join("worktrees"), other.join("ws")]);
}

#[test]
fn release_daemon_has_no_test_seams() {
    // This test build has the seams on: the refusal is in force.
    const { assert!(strata_confirm::TEST_SEAMS) };
    assert_eq!(startup::refuse_test_seams().unwrap_err().code, Code::InvalidInput);
    // The binary cargo built for these tests is a seams build: it refuses
    // before it takes the lock, reads config or listens.
    let home = tempfile::Builder::new().prefix("sd-").tempdir_in("/tmp").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_git-stratad"))
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let line: Value = serde_json::from_slice(out.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(line["ok"], json!(false));
    assert_eq!(line["error"]["detail"]["reason"], json!("test_seams"));
    let cfg = strata_daemon::config_dir_for_home(home.path());
    assert!(!cfg.exists(), "nothing created under HOME: no lock, no socket");
}

#[test]
fn daemon_does_not_idle_exit() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    serving(&d, || {
        assert!(strata_daemon::rpc::ready(&fx.socket(), Duration::from_secs(5)));
        // An idle connection and an idle daemon: both still there later.
        let idle = UnixStream::connect(fx.socket()).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!d.stopping());
        let v = call(&fx.socket(), r#"{"id":"x","method":"daemon_status","params":{}}"#);
        assert_eq!(v["ok"], json!(true));
        drop(idle);
    });
    drop(d);
    core.close().unwrap();
    // No idle timer and no "a workspace keeps it alive" relation in the source.
    let src = concat!(include_str!("../src/startup.rs"), include_str!("../src/main.rs"), include_str!("../src/lib.rs"));
    for bad in ["idle_timeout", "IDLE_", "exit_when_idle", "keep_alive_while"] {
        assert!(!src.contains(bad), "{bad}");
    }
}

#[test]
fn idle_close_is_not_ours() {
    // Closing the connection is the client's business: the daemon keeps
    // serving, and a restart is §5.1 recovery, not reclaiming a container.
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    serving(&d, || {
        for _ in 0..3 {
            let s = UnixStream::connect(fx.socket()).unwrap();
            drop(s);
        }
        let v = call(&fx.socket(), r#"{"id":2,"method":"daemon_status","params":{"verify":true}}"#);
        assert_eq!(v["ok"], json!(true), "{v}");
        assert_eq!(v["data"]["verify"]["ok"], json!(true));
    });
    drop(d);
    core.close().unwrap();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    assert!(d.recovered.errors.is_empty());
    drop(d);
    core.close().unwrap();
}

#[test]
fn no_linux_install_path() {
    let src = concat!(
        include_str!("../src/startup.rs"),
        include_str!("../src/main.rs"),
        include_str!("../src/lib.rs"),
        include_str!("../src/locks.rs"),
        include_str!("../src/rpc.rs"),
        include_str!("../Cargo.toml"),
    );
    for bad in ["systemd", "/usr/local/bin/git-stratad", "/etc/", "install_binary", "setuid", "chown("] {
        assert!(!src.contains(bad), "{bad}");
    }
    // The daemon does not run installs at startup: a session's deps are
    // installed by `new`, never by start/recover.
    let fx = Dfx::new();
    let before = fx.sb.install_count();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    drop(d);
    core.close().unwrap();
    assert_eq!(fx.sb.install_count(), before);
}

#[test]
fn config_and_realpath_refusals() {
    let fx = Dfx::new();
    fx.write_config_text("[paths\nroot = 1");
    let e = fx.prepare().err().unwrap();
    assert_eq!(e.code, Code::InvalidInput);
    // A repo path that is not its realpath: refused with the realpath.
    let mut cfg = fx.config.clone();
    let main = fx.main("tool");
    fs::create_dir_all(fx.sb.path("links")).unwrap();
    let link = fx.sb.path("links/tool");
    std::os::unix::fs::symlink(&main, &link).unwrap();
    cfg.repos.get_mut(&repo_name("tool")).unwrap().path = link.clone();
    fx.write_config_text(&toml::to_string(&cfg).unwrap());
    let e = fx.prepare().err().unwrap();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("not_realpath"), "{e:?}");
    assert_eq!(e.detail.as_ref().unwrap()["realpath"], json!(main));
    // A failed start released the lock: the good config starts.
    fx.write_config();
    fx.prepare().unwrap().close().unwrap();
}

#[test]
fn two_names_for_one_repository_is_name_taken() {
    // A linked worktree of `tool` named `tool2`: its own realpath, the same
    // common dir, so the same RepoId.
    let fx = Dfx::new();
    let wt = fx.sb.path("wt/tool2");
    fx.sb.git(&fx.main("tool"), &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
    let mut cfg = fx.config.clone();
    let mut rc = cfg.repos[&repo_name("tool")].clone();
    rc.path = wt;
    cfg.repos.insert(repo_name("tool2"), rc);
    fx.write_config_text(&toml::to_string(&cfg).unwrap());
    let e = fx.prepare().err().expect("same repository under two names refused");
    assert_eq!(e.code, Code::NameTaken, "{e:?}");
    assert_eq!(reason(&e), Some("repo_id"));
}

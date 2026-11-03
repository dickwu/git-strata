mod support;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use support::{FakeDaemon, Home, Run};
fn slow(q: &Value) -> Value {
    let data = if q["method"] == "daemon_status" {
        json!({"holds":[],"queues":[{"session":"s-abcdefgh","holder":"old_tree","queued":["sync"]}]})
    } else {
        std::thread::sleep(Duration::from_millis(250));
        if q["method"] == "status" { json!({"sessions":[]}) } else { json!({"finished":true}) }
    };
    json!({"id":q["id"],"ok":true,"data":data})
}
#[test]
fn timeouts_are_not_local_fallbacks_and_writes_are_not_retried() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), slow);
    for args in [vec!["status"], vec!["status", "--brief"], vec!["sync", "--session", "s-abcdefgh"]] {
        let mut args = args;
        args.extend(["--timeout", "0.05"]);
        let run = h.run(&args);
        assert_eq!(run.reason(), "client_timeout", "{run:?}");
        assert_eq!(run.err_code(), if args[0] == "sync" { "Unknown" } else { "Busy" });
        assert!(run.env()["error"]["detail"]["queues"].is_array());
        if args[0] == "sync" {
            assert_eq!(run.env()["error"]["detail"]["outcome"], "in_progress");
            assert_eq!(run.env()["error"]["detail"]["session"]["id"], "s-abcdefgh");
            assert_eq!(run.env()["error"]["detail"]["session"]["in_flight"], "old_tree");
            assert!(run.stdout.contains("may still run"));
            assert!(run.stdout.contains("status --session s-abcdefgh"));
        }
        assert!(run.stderr.is_empty());
    }
    assert_eq!(d.requests().iter().filter(|r| r["method"] == "sync").count(), 1);
    // Closing the timed-out peer doesn't cancel the fixture or prevent its next reply.
    assert!(h.run(&["sync", "--timeout", "0"]).ok()["finished"].as_bool().unwrap());
}
#[test]
fn flag_overrides_env_and_zero_waits() {
    let h = Home::new();
    let _d = FakeDaemon::start(&h.socket(), slow);
    let out: Run = h.cmd(&["sync", "--timeout", "0"]).env("GIT_STRATA_TIMEOUT", "0.01").output().unwrap().into();
    assert_eq!(out.ok()["finished"], true);
    let out: Run = h.cmd(&["sync"]).env("GIT_STRATA_TIMEOUT", "0.01").output().unwrap().into();
    assert_eq!(out.reason(), "client_timeout");
    let out: Run = h.cmd(&["status"]).env("GIT_STRATA_TIMEOUT", "NaN").output().unwrap().into();
    assert_eq!(out.reason(), "timeout");
}
#[test]
fn stale_socket_sync_is_down_without_waiting_for_its_budget() {
    let h = Home::new();
    assert_eq!(h.run(&["--version"]).code, 0);
    drop(std::os::unix::net::UnixListener::bind(h.socket()).unwrap());
    let start = Instant::now();
    let run = h.run(&["sync", "--timeout", "30"]);
    assert!(start.elapsed() < Duration::from_millis(300), "{:?}", start.elapsed());
    assert_eq!(run.err_code(), "Busy");
    assert_eq!(run.reason(), "daemon_down");
    assert_eq!(run.env()["error"]["detail"]["socket_exists"], true);
}
#[test]
fn old_daemon_sync_rejects_cwd_locally_before_resend() {
    let h = Home::new();
    h.write_config(&[
        support::RepoSpec { name: "tool", push: false, confirm: false, deploy_on_push: false },
        support::RepoSpec { name: "tool-old", push: false, confirm: false, deploy_on_push: false },
    ]);
    let ws = h.canon().join("root/sessions/s-abcdefgh/tool");
    std::fs::create_dir_all(&ws).unwrap();
    let d = FakeDaemon::start(&h.socket(), |q| {
        if q["params"].get("caller_pids").is_some() {
            json!({"id":q["id"],"ok":false,"error":{"rpc":"Failed","message":"unknown field `caller_pids`"}})
        } else {
            json!({"id":q["id"],"ok":true,"data":{}})
        }
    });
    let run: Run = h.cmd(&["sync"]).current_dir(&ws).output().unwrap().into();
    assert_eq!(run.reason(), "caller_cwd");
    assert_eq!(d.requests().len(), 1);
    let run: Run = h.cmd(&["sync", "--allow-cwd"]).current_dir(&ws).output().unwrap().into();
    run.ok();
    assert_eq!(d.requests().len(), 3);
    assert!(d.requests()[2]["params"].get("caller_pids").is_none());
    let staging = ws.with_file_name(".strata-tmp-move-tool-old-01234567");
    std::fs::create_dir_all(&staging).unwrap();
    let run: Run = h.cmd(&["sync", "--session", "s-abcdefgh", "--repo", "tool"])
        .current_dir(&staging).output().unwrap().into();
    run.ok(); // The staged sibling is not the selected repo, despite its prefix.

}

#[test]
fn real_daemon_finishes_sync_after_client_timeout_and_survives_closed_reply() {
    use support::live::Live;
    let live = Live::new(&["tool"], &[("a.txt", b"base\n")]);
    live.serve(|core| {
        let id = live.new_session("deadline", "tool");
        let main = live.main("tool");
        std::fs::write(main.join("later.txt"), b"upstream\n").unwrap();
        support::git(&main, live.home.path(), &["add", "later.txt"]);
        support::git(&main, live.home.path(), &["commit", "-qm", "upstream"]);
        support::git(&main, live.home.path(), &["push", "-q", "origin", "main"]);
        let (tx, rx) = std::sync::mpsc::channel();
        live.on_request.lock().unwrap().push((
            "sync".into(),
            Box::new(move |_| {
                rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }),
        ));
        let run = live.run(&["sync", "--session", id.as_str(), "--timeout", "0.1"]);
        // Release even if an assertion fails, so the fixture cannot strand its server.
        tx.send(()).unwrap();
        assert_eq!(run.err_code(), "Unknown");
        assert_eq!(run.reason(), "client_timeout");
        let start = Instant::now();
        loop {
            let rec = core.store.session(&id).unwrap().unwrap();
            if rec.manifest.repos.values().any(|ws| ws.path.join("later.txt").exists()) {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(15));
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = live.run(&["status", "--brief", "--session", id.as_str(), "--timeout", "5"]);
        assert_eq!(status.code, 0, "{status:?}");
        assert_eq!(live.requests.lock().unwrap().iter().filter(|m| m.as_str() == "sync").count(), 1);
    });
}

#[test]
fn stale_socket_status_falls_back_to_a_v5_store_and_old_probes_may_omit_queues() {
    let h = Home::new();
    h.write_config(&[support::RepoSpec { name: "tool", push: false, confirm: false, deploy_on_push: false }]);
    let root = h.canon().join("root");
    let store = strata_store::Store::open(&root, &[]).unwrap();
    drop(store);
    drop(std::os::unix::net::UnixListener::bind(h.socket()).unwrap());
    let start = Instant::now();
    let status = h.run(&["status", "--brief", "--all", "--timeout", "30"]);
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(status.ok()["source"], "local");
    std::fs::remove_file(h.socket()).unwrap();
    let _d = FakeDaemon::start(&h.socket(), |q| {
        if q["method"] != "daemon_status" {
            std::thread::sleep(Duration::from_millis(150));
        }
        json!({"id":q["id"],"ok":true,"data":{"holds":[]}})
    });
    let out = h.run(&["sync", "--timeout", "0.02"]);
    assert_eq!(out.err_code(), "Unknown");
    assert!(out.env()["error"]["detail"].get("queues").is_none());
}

mod support;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use support::{FakeDaemon, Home, Run};

fn data(behind: Option<u32>, unpublished: bool) -> Value {
    json!({"session":"s-abcdefgh","upstream":{"clean":false,"repos":{"tool":{
        "following":true,"behind":behind,"unchecked":behind.is_none(),"capped":false,"diverged":false,
        "base_head":"1111111111111111111111111111111111111111","unpublished":unpublished
    }}}})
}
fn hook(home: &Home, active: bool) -> Run {
    warm_executable(home);
    let start = Instant::now();
    let mut child = home
        .cmd(&["sync", "--check", "--hook"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.take().unwrap(),
        "{}",
        json!({"cwd":home.path(),"stop_hook_active":active})
    )
    .unwrap();
    hook_output_within_deadline(child, start)
}

fn hook_output_within_deadline(mut child: Child, start: Instant) -> Run {
    // Five-second hook contract, plus process scheduling/loader margin.
    let limit = Duration::from_millis(5750);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("hook exceeded {limit:?}; killed and reaped child: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        start.elapsed() < limit,
        "hook deadline exceeded: {:?}",
        start.elapsed()
    );
    child.wait_with_output().unwrap().into()
}
fn silent(run: Run) {
    assert_eq!(run.code, 0, "{run:?}");
    assert!(run.stdout.is_empty(), "{run:?}");
    assert!(run.stderr.is_empty(), "{run:?}");
}

// macOS can spend over a second validating a freshly linked executable
// before main runs. Time the hook after that one-time loader work.
fn warm_executable(home: &Home) {
    static WARM: std::sync::Once = std::sync::Once::new();
    WARM.call_once(|| {
        assert_eq!(home.run(&["--version"]).code, 0);
    });
}

#[test]
fn old_daemon_unknown_method_mapping() {
    let home = Home::new();
    let fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":false,"error":{"rpc":"UnknownMethod","message":"unknown"}}),
    );
    let r = home.run(&["sync", "--check"]);
    assert_eq!(r.err_code(), "Unsupported");
    assert_eq!(r.reason(), "daemon_too_old");
    assert_eq!(fake.requests()[0]["method"], "sync");
}

#[test]
fn sync_forwards_all_flags_and_noncheck_to_daemon() {
    let home = Home::new();
    let fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":true,"data":q["params"]}),
    );
    let out = home
        .run(&[
            "sync",
            "--session",
            "s-abcdefgh",
            "--repo",
            "tool,front",
            "--no-fetch",
            "--resolved",
            "a",
            "--keep",
            "b",
            "--target",
            "1111111111111111111111111111111111111111",
            "--generation",
            "4",
            "--accept-diverged",
        ])
        .ok();
    assert_eq!(out["check"], false);
    assert_eq!(out["fetch"], false);
    assert_eq!(out["repos"], json!(["tool", "front"]));
    assert_eq!(out["resolved"], json!(["a"]));
    assert_eq!(out["keep"], json!(["b"]));
    assert_eq!(out["generation"], 4);
    assert_eq!(out["accept_diverged"], true);
    assert_eq!(fake.requests().len(), 1);
}

/// The daemon decides whether a resolution needs target/generation: a saved
/// conflict report requires both; named legacy acknowledgements (including
/// behind/diverged workspaces without a report and non-following repos) do not.
#[test]
fn resolution_binding_is_left_to_the_daemon() {
    let home = Home::new();
    let fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":true,"data":q["params"]}),
    );
    for flag in ["--keep", "--resolved"] {
        let out = home.run(&["sync", flag, "a"]).ok();
        assert_eq!(out["target"], Value::Null);
        assert_eq!(out["generation"], Value::Null);
        let r = home.run(&[
            "sync",
            flag,
            "a",
            "--target",
            "1111111111111111111111111111111111111111",
        ]);
        assert_eq!(r.err_code(), "InvalidInput");
    }
    assert_eq!(fake.requests().len(), 2);
}

#[test]
fn hook_blocks_once_with_unpublished_and_behind() {
    let home = Home::new();
    let fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":true,"data":data(Some(3),true)}),
    );
    let out = hook(&home, false);
    assert_eq!(out.code, 0);
    assert!(out.stderr.is_empty());
    let block: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(block["decision"], "block");
    assert!(
        block["reason"]
            .as_str()
            .unwrap()
            .contains("main moved 3 commits since your base in tool")
    );
    assert_eq!(fake.requests()[0]["params"]["hook"], true);
    assert_eq!(fake.requests()[0]["params"]["check"], true);
    assert_eq!(fake.requests()[0]["params"]["fetch"], false);
    silent(hook(&home, true));
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn hook_cold_cache_does_not_block() {
    let home = Home::new();
    let _fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":true,"data":data(None,true)}),
    );
    silent(hook(&home, false));
}

#[test]
fn hook_clean_behind_does_not_block() {
    let home = Home::new();
    let _fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":true,"data":data(Some(3),false)}),
    );
    silent(hook(&home, false));
}

#[test]
fn hook_silent_outside_workspace_or_daemon_down() {
    let home = Home::new();
    silent(hook(&home, false));
    let _fake = FakeDaemon::start(
        &home.socket(),
        |q| json!({"id":q["id"],"ok":false,"error":{"code":"NoSuchSession","message":"outside"}}),
    );
    silent(hook(&home, false));
    silent(home.run(&["sync", "--hook"]));
}

fn stalled(trickle: bool) {
    let home = Home::new();
    warm_executable(&home);
    let listener = UnixListener::bind(home.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (done, until_done) = mpsc::channel();
    let t = std::thread::spawn(move || {
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if until_done.recv_timeout(Duration::from_millis(10))
                        != Err(RecvTimeoutError::Timeout)
                    {
                        return false;
                    }
                }
                Err(e) => panic!("accept fake daemon connection: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(!line.is_empty(), "hook did not send a request");
        // Keep the peer open until the test ends. Closing near five seconds
        // would let a hook with no deadline pass merely because it saw EOF.
        while until_done.recv_timeout(Duration::from_millis(100)) == Err(RecvTimeoutError::Timeout)
        {
            if trickle && stream.write_all(b" ").is_err() {
                break;
            }
        }
        true
    });
    let out = hook(&home, false);
    let _ = done.send(());
    assert!(
        t.join().unwrap(),
        "hook never connected to the stalled daemon"
    );
    silent(out);
}
#[test]
fn hook_exits_0_on_accepting_silent_daemon() {
    stalled(false);
}
#[test]
fn hook_exits_0_on_trickling_reply() {
    stalled(true);
}

#[test]
fn hook_process_deadline_covers_stalled_stdin() {
    let home = Home::new();
    warm_executable(&home);
    let start = Instant::now();
    let mut child = home
        .cmd(&["sync", "--check", "--hook"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    silent(hook_output_within_deadline(child, start));
    drop(input);
}

#[test]
fn hook_deps_conflicts_and_following_rules() {
    let mut v = data(Some(0), true)["upstream"].clone();
    v["repos"]["tool"]["deps"] = json!({"proposal":1,"mode":"apply","state":"installing","from":"a".repeat(64),"to":"b".repeat(64),"summary":"react 18.2.0→18.3.1"});
    let reason =
        |v: &Value| git_strata::sync::block_reason(&serde_json::from_value(v.clone()).unwrap());
    assert!(
        reason(&v)
            .unwrap()
            .contains("main's dependencies changed in tool (react 18.2.0→18.3.1)")
    );
    v["repos"]["tool"]["unpublished"] = json!(false);
    assert!(reason(&v).is_none());
    v["repos"]["tool"]["auto"] = json!({"source":"main","state":"deferred","reason":"conflicts","since":"2026-11-02T00:00:00Z","attempts":1});
    assert!(reason(&v).is_some());
    v["repos"]["tool"].as_object_mut().unwrap().remove("auto");
    v["repos"]["tool"]["conflicts"] = json!(["src/a"]);
    assert!(reason(&v).is_some());
    v["repos"]["tool"]["following"] = json!(false);
    assert!(reason(&v).is_none());
}

#[test]
fn stuck_hook_blocks_clean_workspace_once_and_does_not_offer_sync_when_not_following() {
    let home=Home::new();
    let _fake=FakeDaemon::start(&home.socket(),|q| {
        let mut d=data(Some(0),false);
        let u=&mut d["upstream"]["repos"]["tool"];
        u["following"]=json!(false);
        u["auto"]=json!({"source":"main","state":"deferred","reason":"open_files","since":"2026-11-03T12:00:00Z","attempts":3,"escalated_at":"2026-11-03T12:06:00Z","escalation_new":true,"detail":{"holders":[{"pid":123,"command":"node","hold":"cwd","path":"."}],"count":1}});
        json!({"id":q["id"],"ok":true,"data":d})
    });
    let r=hook(&home,false); let block:Value=serde_json::from_str(&r.stdout).unwrap();
    let reason=block["reason"].as_str().unwrap();
    assert!(reason.contains("ask the user") && reason.contains("node[123] cwd"));
    assert!(!reason.contains("git-strata sync"));
    silent(hook(&home,true));
}

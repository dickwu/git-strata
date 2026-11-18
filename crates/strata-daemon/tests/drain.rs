//! Drain integration tests use temporary stores and in-process daemons.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{self, Server, Trackers};
use strata_daemon::rpc::parse_request;
use strata_daemon::seams::Verdict;
use strata_daemon::startup::{self, Core, Daemon};
use strata_types::{SessionId, serde_rfc3339};
use support::{Dfx, checkpoint, repo_name, ws};

const WAIT: Duration = Duration::from_secs(10);

struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn request(method: &str, params: Value) -> String {
    format!(
        "{}\n",
        json!({ "id": 1, "method": method, "params": params })
    )
}

fn call(server: &Server<'_, '_>, method: &str, params: Value) -> Value {
    server
        .handle(parse_request(request(method, params).as_bytes()).unwrap())
        .0
}

fn rpc(socket: &Path, method: &str, params: Value) -> Value {
    let mut socket = UnixStream::connect(socket).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    socket
        .write_all(request(method, params).as_bytes())
        .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("invalid reply {line:?}: {e}"))
}

fn data(reply: Value) -> Value {
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"].clone()
}

fn refused(reply: Value, code: &str, reason: &str) {
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["error"]["code"], code, "{reply}");
    assert_eq!(reply["error"]["detail"]["reason"], reason, "{reply}");
}

fn until<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + WAIT;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < end, "condition did not become true");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn status(server: &Server<'_, '_>) -> Value {
    data(call(server, "daemon_status", json!({})))
}

fn lease(server: &Server<'_, '_>, params: Value) -> Value {
    data(call(server, "drain", params))["lease_id"].clone()
}

fn new_session(server: &Server<'_, '_>) -> Value {
    data(call(
        server,
        "new",
        json!({ "ticket": null, "repos": ["tool"] }),
    ))["session"]["manifest"]["id"]
        .clone()
}

/// The external stop flag stands in for the executable's signal handler.
fn serve(
    daemon: &Daemon<'_>,
    test: impl FnOnce(&Path, &AtomicBool, &Receiver<strata_daemon::Result<()>>),
) {
    let path = &daemon.core.opts.socket;
    let listener = startup::bind(path).unwrap();
    let external = AtomicBool::new(false);
    let (done, ended) = mpsc::channel();
    std::thread::scope(|scope| {
        let stop = &external;
        let listener = &listener;
        scope.spawn(move || {
            let _ = done.send(handlers::serve(daemon, listener, stop));
        });
        let _stop = Stop(stop);
        test(path, stop, &ended);
    });
}

#[test]
fn lease_expiry_renewal_and_read_only_calls() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);

    let initial = status(&server);
    assert_eq!(initial["store_schema"], strata_types::STORE_SCHEMA);
    assert_eq!(initial.get("draining"), Some(&Value::Null));
    assert_eq!(initial["inflight"], json!([]));
    assert!(
        initial["features"]
            .as_array()
            .unwrap()
            .contains(&json!("daemon.drain"))
    );

    let id = lease(&server, json!({ "on": true }));
    let first = status(&server);
    assert_eq!(first["draining"]["lease_id"], id);
    assert_eq!(first["draining"]["sealed"], false);
    assert_eq!(first["draining"]["then"], Value::Null);
    let expiry =
        |s: &Value| serde_rfc3339::parse(s["draining"]["expires_at"].as_str().unwrap()).unwrap();
    assert!(
        (119..=120).contains(
            &expiry(&first)
                .duration_since(SystemTime::now())
                .unwrap()
                .as_secs()
        )
    );

    let renewed = lease(
        &server,
        json!({ "on": true, "lease_id": id, "ttl_s": 1000 }),
    );
    assert_eq!(renewed, id);
    let later = status(&server);
    assert!(expiry(&later) > expiry(&first));
    assert!(
        (599..=600).contains(
            &expiry(&later)
                .duration_since(SystemTime::now())
                .unwrap()
                .as_secs()
        )
    );
    assert_eq!(later["inflight"], json!([]));
    let rejected = call(&server, "new", json!({ "ticket": null, "repos": ["tool"] }));
    refused(rejected, "Busy", "draining");
    data(call(
        &server,
        "status",
        json!({ "cwd": null, "session": null, "all": true }),
    ));
    data(call(
        &server,
        "gc",
        json!({ "repo": null, "store": true, "dry_run": true }),
    ));

    assert_eq!(
        lease(&server, json!({ "on": true, "lease_id": id, "ttl_s": 0 })),
        id
    );
    assert_eq!(status(&server)["draining"], Value::Null);
    refused(
        call(
            &server,
            "drain",
            json!({ "seal": true, "lease_id": id, "then": "stop" }),
        ),
        "Busy",
        "lease_lost",
    );
    let next = lease(&server, json!({ "on": true }));
    assert_ne!(next, id);
    refused(
        call(&server, "drain", json!({ "on": false, "lease_id": id })),
        "Busy",
        "lease_lost",
    );
    data(call(&server, "drain", json!({ "on": false })));
    let session = new_session(&server);
    assert!(session.is_string());
    data(call(&server, "drain", json!({ "on": true })));
    data(call(
        &server,
        "sync",
        json!({ "session": session, "check": true }),
    ));
    assert_eq!(status(&server)["inflight"], json!([]));
}

#[test]
fn lease_less_client_cannot_renew_an_existing_drain() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    serve(&daemon, |socket, _, _| {
        // rpc opens a separate connection per call.
        let owner = data(rpc(socket, "drain", json!({ "on": true })));
        let id = &owner["lease_id"];
        assert!(id.is_string());
        let check_other_client = || {
            let before = data(rpc(socket, "daemon_status", json!({})))["draining"].clone();
            let denied = rpc(socket, "drain", json!({ "on": true, "ttl_s": 600 }));
            refused(denied.clone(), "Busy", "draining");
            assert!(denied.get("data").is_none());
            assert_eq!(denied["error"]["detail"], json!({ "reason": "draining" }));
            assert_eq!(
                data(rpc(socket, "daemon_status", json!({})))["draining"],
                before,
            );
        };
        check_other_client();
        seal_when_idle(socket, id, "hold");
        check_other_client();
    });
}

#[test]
fn competing_write_is_either_queued_in_inflight_or_refused() {
    let mut fx = Dfx::new();
    fx.lock_wait = WAIT;
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let session = new_session(&server);
    let sid: strata_types::SessionId = serde_json::from_value(session.clone()).unwrap();

    for _ in 0..8 {
        let start = Barrier::new(2);
        std::thread::scope(|scope| {
            let (locked, ready) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let locks = &core.locks;
            let sid = &sid;
            let holder = scope.spawn(move || {
                locks
                    .with_sessions(std::slice::from_ref(sid), || {
                        locked.send(()).unwrap();
                        // A failed assertion drops the sender and unwinds the test.
                        let _ = released.recv_timeout(WAIT);
                        Ok(())
                    })
                    .unwrap();
            });
            ready.recv_timeout(WAIT).unwrap();
            let start_write = &start;
            let server = &server;
            let session = &session;
            let writer = scope.spawn(move || {
                start_write.wait();
                call(
                    server,
                    "mark",
                    json!({ "session": session, "state": "done" }),
                )
            });
            start.wait();
            data(call(server, "drain", json!({ "on": true })));
            let listed = until(|| {
                let row = status(server)["inflight"][0].clone();
                if row["phase"] == "queued" {
                    return Some(Some(row));
                }
                writer.is_finished().then_some(None)
            });
            release.send(()).unwrap();
            holder.join().unwrap();
            let reply = writer.join().unwrap();
            if let Some(row) = listed {
                assert_eq!(row["method"], "mark");
                assert_eq!(row["session"], *session);
                serde_rfc3339::parse(row["started_at"].as_str().unwrap()).unwrap();
                data(reply);
            } else {
                refused(reply, "Busy", "draining");
            }
            assert_eq!(status(server)["inflight"], json!([]));
            data(call(server, "drain", json!({ "on": false })));
        });
    }
}

#[test]
fn cached_publish_preview_stays_readable_but_ref_refresh_requires_admission() {
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    let main = fx.main("tool");
    fx.sb.git(&main, &["push", "origin", "HEAD:release"]);
    let cfg = fx.config.repos.get_mut(&repo).unwrap();
    cfg.deps_dirs.clear();
    cfg.install = None;
    cfg.install_inputs.clear();
    cfg.seed_caches.clear();
    cfg.publish.branch = "release".into();
    cfg.publish.push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let session: SessionId = serde_json::from_value(new_session(&server)).unwrap();
    fs::write(ws(&core, &session, "tool").join("mine"), b"mine").unwrap();
    let cp = checkpoint(&core, &session, "tool");
    let preview = || {
        call(
            &server,
            "publish_plan",
            json!({ "group": { "id": 0, "batches": [
            { "session": session, "repo": repo, "checkpoint": cp.id, "paths": ["mine"] }
        ] } }),
        )
    };
    let tracking_ref = "refs/strata/remotes/origin/release";
    let read_ref = || core.gits[&repo].read_ref(tracking_ref).unwrap();
    assert!(read_ref().is_none());

    let id = lease(&server, json!({ "on": true }));
    refused(preview(), "Busy", "draining");
    assert!(read_ref().is_none());
    data(call(
        &server,
        "drain",
        json!({ "on": false, "lease_id": id }),
    ));

    let upload = fx.sb.path("paused-upload-pack");
    fs::write(
        &upload,
        r#"#!/bin/sh
: > "$0.entered"
i=0
while [ ! -f "$0.release" ]; do
    [ "$i" -lt 500 ] || exit 1
    i=$((i + 1))
    sleep 0.02
done
exec git upload-pack "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&upload, fs::Permissions::from_mode(0o700)).unwrap();
    fx.sb.git(
        &main,
        &[
            "config",
            "remote.origin.uploadpack",
            upload.to_str().unwrap(),
        ],
    );

    let id = std::thread::scope(|scope| {
        let refresh = scope.spawn(preview);
        until(|| {
            fx.sb
                .path("paused-upload-pack.entered")
                .exists()
                .then_some(())
        });
        let id = lease(&server, json!({ "on": true }));
        let current = status(&server);
        assert_eq!(current["inflight"].as_array().unwrap().len(), 1);
        assert_eq!(current["inflight"][0]["method"], "publish_plan");
        assert_eq!(current["inflight"][0]["repo"], "tool");
        refused(
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            ),
            "Busy",
            "not_idle",
        );
        assert!(read_ref().is_none());
        fs::write(fx.sb.path("paused-upload-pack.release"), b"").unwrap();
        data(refresh.join().unwrap());
        id
    });

    assert!(read_ref().is_some());
    until(|| {
        let reply = call(
            &server,
            "drain",
            json!({ "seal": true, "lease_id": id, "then": "hold" }),
        );
        if reply["ok"] == true {
            return Some(());
        }
        refused(reply, "Busy", "not_idle");
        None
    });
    assert_eq!(data(preview())["repos"], json!(["tool"]));
    assert!(core.store.publishes().unwrap().is_empty());
}

#[test]
fn admitted_reload_can_finish_during_drain_even_after_pending_reply() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let repo = repo_name("tool");
    fx.config
        .repos
        .get_mut(&repo)
        .unwrap()
        .checks
        .push("true".into());
    fx.write_config();

    // Park the worker by starting it after the drain. The first caller
    // already gave up waiting, so its admission can only be held by the ticket.
    assert_eq!(
        core.request_reload(Duration::ZERO).unwrap().outcome,
        "pending"
    );
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stop = Stop(&stop);
        let reload = scope.spawn(|| call(&server, "config_reload", json!({})));
        until(|| (core.reload.requested_for_test() == 2).then_some(()));
        let id = lease(&server, json!({ "on": true }));
        let current = status(&server);
        let inflight = current["inflight"].as_array().unwrap();
        assert_eq!(inflight.len(), 2);
        assert!(inflight.iter().all(|row| row["method"] == "config_reload"));
        refused(
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            ),
            "Busy",
            "not_idle",
        );

        scope.spawn(|| strata_daemon::reload::run(&core, &|| stop.load(Ordering::SeqCst)));
        until(|| (core.reload.status()["outcome"] == "applied").then_some(()));
        assert_eq!(data(reload.join().unwrap())["outcome"], "applied");
        until(|| (status(&server)["inflight"] == json!([])).then_some(()));
        assert_eq!(
            core.catalog.current().config.repos[&repo]
                .checks
                .last()
                .unwrap(),
            "true"
        );
        refused(
            call(&server, "config_reload", json!({})),
            "Busy",
            "draining",
        );
    });
}

fn record_pending_dependency_change(fx: &Dfx, core: &Core, session: &SessionId) {
    let repo = repo_name("tool");
    let row = core.store.session(session).unwrap().unwrap();
    let workspace = &row.manifest.repos[&repo];
    let set = core
        .store
        .deps_set(&repo, &workspace.base.deps)
        .unwrap()
        .unwrap()
        .set;
    let target = strata_types::CommitOid::try_from(fx.sb.commit(
        &fx.main("tool"),
        &[("bun.lock", b"lock v2\n")],
        "dependency update",
    ))
    .unwrap();
    let tree = core.gits[&repo].tree_of(&target).unwrap();
    core.store
        .record_proposal(
            &strata_store::NewProposal {
                repo: repo.clone(),
                origin: None,
                from: workspace.base.deps.clone(),
                from_inputs: set.inputs_hash,
                from_head: Some(workspace.head.clone()),
                to: strata_types::DepsSetId::try_from("b".repeat(64)).unwrap(),
                to_inputs: "b".repeat(64),
                tree: Some(tree),
                head: Some(target),
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap();
}

#[test]
fn drain_reads_cached_dependency_summaries_and_skips_new_computations() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let session: SessionId = serde_json::from_value(new_session(&server)).unwrap();
    record_pending_dependency_change(&fx, &core, &session);
    let repo = repo_name("tool");

    let proposals =
        || data(call(&server, "deps_list", json!({ "repo": repo })))["proposals"].clone();
    let summary = || {
        data(call(
            &server,
            "sync",
            json!({ "session": session, "check": true }),
        ))["upstream"]["repos"]["tool"]["deps"]["summary"]
            .clone()
    };
    let id = lease(&server, json!({ "on": true }));
    let cold = proposals();
    assert_eq!(cold.as_array().unwrap().len(), 1);
    assert!(cold[0].get("changes").is_none());
    assert!(summary().is_null());
    data(call(
        &server,
        "drain",
        json!({ "on": false, "lease_id": id }),
    ));

    until(|| proposals()[0]["changes"].is_object().then_some(()));
    let warm = until(|| summary().as_str().map(str::to_owned));
    lease(&server, json!({ "on": true }));
    assert!(proposals()[0].get("changes").is_none());
    assert_eq!(summary(), warm);
}

#[test]
fn dependency_summary_blocks_seal_after_its_rpc_returns() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let session = serde_json::from_value(new_session(&server)).unwrap();
    record_pending_dependency_change(&fx, &core, &session);
    assert!(daemon.du.wait_idle(WAIT));

    for (method, params, summary) in [
        (
            "deps_list",
            json!({ "repo": "tool" }),
            "/proposals/0/changes",
        ),
        (
            "sync",
            json!({ "session": session, "check": true }),
            "/upstream/repos/tool/deps/summary",
        ),
    ] {
        let (parked, entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        strata_deps::before_changes_worker(move || {
            parked.send(()).unwrap();
            let _ = released.recv_timeout(WAIT);
        });
        // Another test can briefly own the parser slot. Once this worker
        // parks, the RPC must return before we can open the release channel.
        let reply = until(|| {
            let reply = data(call(&server, method, params.clone()));
            entered.try_recv().ok().map(|()| reply)
        });
        assert!(
            reply.pointer(summary).is_none_or(Value::is_null),
            "{method}"
        );

        let id = lease(&server, json!({ "on": true }));
        let seal = || {
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            )
        };
        assert_eq!(status(&server)["inflight"], json!([]), "{method}");
        refused(seal(), "Busy", "not_idle");
        release.send(()).unwrap();
        until(|| {
            let reply = seal();
            if reply["ok"] == true {
                return Some(());
            }
            refused(reply, "Busy", "not_idle");
            None
        });
        data(call(
            &server,
            "drain",
            json!({ "on": false, "lease_id": id }),
        ));
    }
}

#[test]
fn confirming_request_blocks_a_seal() {
    let fx = Dfx::new();
    let (confirmer, prompts, _) = fx.confirmer(vec![Verdict::Hang]);
    let core = fx.prepare_with(confirmer).unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let sid = new_session(&server);
    std::thread::scope(|scope| {
        let rm = scope.spawn(|| call(&server, "rm", json!({ "session": sid, "discard": true })));
        until(|| (!prompts.shown().is_empty()).then_some(()));
        let id = lease(&server, json!({ "on": true }));
        let state = status(&server);
        assert_eq!(state["inflight"].as_array().unwrap().len(), 1);
        let row = &state["inflight"][0];
        assert_eq!(row["method"], "rm");
        assert_eq!(row["phase"], "confirming");
        assert_eq!(row["session"], sid);
        refused(
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            ),
            "Busy",
            "not_idle",
        );
        assert_eq!(rm.join().unwrap()["error"]["code"], "NotConfirmed");
    });
    assert_eq!(status(&server)["inflight"], json!([]));
}

#[test]
fn config_relax_leaves_confirming_before_post_prompt_work() {
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    fx.write_config();
    let (confirmer, _, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = false;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    let shown = core.catalog.current().file_flags[&repo].clone();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let (entered, ready) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = std::sync::Mutex::new(released);
    let root = &core.config.paths.root;
    strata_daemon::seams::set_after_confirm(
        root,
        Some(std::sync::Arc::new(move || {
            entered.send(()).unwrap();
            let _ = released.lock().unwrap().recv_timeout(WAIT);
        })),
    );
    std::thread::scope(|scope| {
        let relax = scope.spawn(|| {
            call(
                &server,
                "config_relax",
                json!({ "repo": repo, "shown": shown }),
            )
        });
        ready.recv_timeout(WAIT).unwrap();
        let id = lease(&server, json!({ "on": true }));
        let current = status(&server);
        assert_eq!(current["inflight"][0]["method"], "config_relax");
        assert_eq!(current["inflight"][0]["phase"], "running");
        refused(
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            ),
            "Busy",
            "not_idle",
        );
        release.send(()).unwrap();
        data(relax.join().unwrap());
    });
    strata_daemon::seams::set_after_confirm(root, None);
    assert!(!core.catalog.current().pins[&repo].confirm);
}

#[test]
fn chain_restore_leaves_confirming_before_post_prompt_work() {
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    fx.write_config();
    let main = fx.main("tool");
    let tip = fx
        .sb
        .commit(&main, &[("restore.txt", b"chain tip\n")], "chain tip");
    // A populated chain without an anchor is classified as rewound.
    fx.sb
        .git(&main, &["update-ref", "refs/strata/publish/main", &tip]);
    let (confirmer, _, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    core.gits[&repo].fetch("origin", "main").unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let root = &core.config.paths.root;

    std::thread::scope(|scope| {
        let (entered, ready) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let released = std::sync::Mutex::new(released);
        strata_daemon::seams::set_after_confirm(
            root,
            Some(std::sync::Arc::new(move || {
                entered.send(()).unwrap();
                let _ = released.lock().unwrap().recv_timeout(WAIT);
            })),
        );
        let restore = scope.spawn(|| {
            call(
                &server,
                "chain_resolve",
                json!({ "repo": repo, "choice": "Restore", "shown": tip }),
            )
        });
        ready.recv_timeout(WAIT).unwrap();
        strata_daemon::seams::set_after_confirm(root, None);
        let id = lease(&server, json!({ "on": true }));
        let current = status(&server);
        assert_eq!(current["inflight"].as_array().unwrap().len(), 1);
        assert_eq!(current["inflight"][0]["method"], "chain_resolve");
        assert_eq!(current["inflight"][0]["phase"], "running");
        refused(
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            ),
            "Busy",
            "not_idle",
        );
        release.send(()).unwrap();
        assert_eq!(data(restore.join().unwrap())["pushed"], true);
    });
    assert_eq!(status(&server)["inflight"], json!([]));
}

#[test]
fn background_holder_blocks_seal_and_lease_pauses_new_passes() {
    let mut fx = Dfx::new();
    fx.reconcile_every = Duration::from_millis(35);
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let (entered, inside) = mpsc::channel();
    let (release, released) = mpsc::channel();
    std::thread::scope(|scope| {
        let core = daemon.core;
        let holder = scope.spawn(move || {
            core.locks
                .with_repo(&repo_name("tool"), || {
                    entered.send(()).unwrap();
                    let _ = released.recv_timeout(WAIT);
                    Ok(())
                })
                .unwrap();
        });
        inside.recv_timeout(WAIT).unwrap();
        let id = lease(&server, json!({ "on": true }));
        assert_eq!(status(&server)["inflight"], json!([]));
        refused(
            call(
                &server,
                "drain",
                json!({ "seal": true, "lease_id": id, "then": "hold" }),
            ),
            "Busy",
            "not_idle",
        );
        release.send(()).unwrap();
        holder.join().unwrap();

        let config = core.opts.config_file.clone();
        let changed = fs::read_to_string(&config).unwrap() + "\n# A new config revision\n";
        serve(&daemon, |socket, _, _| {
            fs::write(config, changed).unwrap();
            data(rpc(socket, "du", json!({ "refresh": true })));
            std::thread::sleep(Duration::from_millis(170));
            let before = data(rpc(socket, "daemon_status", json!({})));
            assert_eq!(before["reconcile"]["runs"], 0);
            assert_eq!(before["config"]["generation"], 1);
            assert_eq!(before["scan"]["running"], false);
            assert_eq!(before["scan"]["queued"], 0);
            assert!(handlers::catch_up_pass(&daemon, SystemTime::now()).is_empty());
            data(rpc(socket, "drain", json!({ "on": false })));
            until(|| {
                let state = data(rpc(socket, "daemon_status", json!({})));
                (state["reconcile"]["runs"].as_u64().unwrap() > 0
                    && state["config"]["generation"].as_u64().unwrap() > 1)
                    .then_some(())
            });
        });
    });
}

#[test]
fn lease_pauses_periodic_fetch_and_automatic_dependency_install() {
    let fx = Dfx::new();
    let mut core = fx.prepare().unwrap();
    core.opts.background_main_sync = true;
    core.opts.catch_up_every = Duration::from_millis(250);
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    new_session(&server);

    let repo = repo_name("tool");
    let main = fx.main("tool");
    fx.sb
        .commit(&main, &[("bun.lock", b"lock v2\n")], "dependency update");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo].fetch("origin", "main").unwrap();
    strata_daemon::main_sync::reconcile(&core, &repo).unwrap();
    let proposal = strata_deps::open_proposals(&core.store, Some(&repo))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    let stale = SystemTime::UNIX_EPOCH;
    core.store.record_fetch(&repo, true, None, stale).unwrap();
    let id = lease(&server, json!({ "on": true }));

    serve(&daemon, |socket, _, _| {
        until(|| (core.reload.loops_started.load(Ordering::SeqCst) == 2).then_some(()));
        // Give the fetch loop its one-second retry interval and the
        // installer several chances to pick up the pending proposal.
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(core.store.repo_fetch(&repo).unwrap().unwrap().at, stale);
        assert!(core.store.deps_auto(proposal.id).unwrap().is_none());

        data(rpc(socket, "drain", json!({ "on": false, "lease_id": id })));
        let deadline = Instant::now() + WAIT;
        loop {
            let fetch = core.store.repo_fetch(&repo).unwrap().unwrap();
            let install = core.store.deps_auto(proposal.id).unwrap();
            let ready = install.as_ref().is_some_and(|p| {
                matches!(
                    p.state,
                    strata_store::DepsAutoState::Ready | strata_store::DepsAutoState::Applied
                )
            });
            // A fetch can lose its final try-lock to the installer or
            // catch-up. The timestamp records that the fetch was started.
            if fetch.at > stale && ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "main-sync did not resume after release: fetch {fetch:?}, install {install:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    });
}

#[test]
fn admitted_background_pass_blocks_seal_before_taking_locks() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    daemon.sessions.requeue_old_tree(
        strata_session::OldTreeKey {
            session: "s-abcdefgh".to_string().try_into().unwrap(),
            repo: repo_name("tool"),
            n: 999,
        },
        fx.sb.path("already-missing"),
        1,
    );
    let stop = AtomicBool::new(false);
    let (entered, ready) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = std::sync::Mutex::new(released);
    let park = || {
        entered.send(()).unwrap();
        let _ = released.lock().unwrap().recv_timeout(WAIT);
    };
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            handlers::test_old_tree_loop(
                &daemon,
                &|| stop.load(Ordering::SeqCst),
                Duration::from_millis(50),
                &park,
            );
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ready.recv_timeout(WAIT).unwrap();
            let id = lease(&server, json!({ "on": true }));
            let visible = status(&server);
            assert_eq!(visible["holds"], json!([]));
            assert_eq!(visible["queues"], json!([]));
            assert_eq!(visible["inflight"], json!([]));
            refused(
                call(
                    &server,
                    "drain",
                    json!({ "seal": true, "lease_id": id, "then": "hold" }),
                ),
                "Busy",
                "not_idle",
            );
        }));
        stop.store(true, Ordering::SeqCst);
        drop(release);
        worker.join().unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    });
}

fn seal(socket: &Path, id: &Value, then: &str) -> Value {
    rpc(
        socket,
        "drain",
        json!({ "seal": true, "lease_id": id, "then": then }),
    )
}

fn seal_when_idle(socket: &Path, id: &Value, then: &str) {
    until(|| {
        let reply = seal(socket, id, then);
        if reply["ok"] == true {
            assert_eq!(reply["data"]["lease_id"], *id);
            return Some(());
        }
        refused(reply, "Busy", "not_idle");
        None
    });
}

fn shutdown_record(fx: &Dfx) -> Value {
    let bytes = fs::read(fx.config_dir.join("last_shutdown.json")).unwrap();
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    let epoch = record["epoch"].as_str().unwrap();
    assert_eq!(epoch.len(), 32);
    assert!(u128::from_str_radix(epoch, 16).is_ok());
    serde_rfc3339::parse(record["at"].as_str().unwrap()).unwrap();
    record
}

#[test]
fn hold_is_permanent_until_matching_cancel_or_signal() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    serve(&daemon, |socket, stop, ended| {
        let id = data(rpc(socket, "drain", json!({ "on": true, "ttl_s": 2 })))["lease_id"].clone();
        seal_when_idle(socket, &id, "hold");
        let held = json!({ "sealed": true, "then": "hold", "lease_id": id, "expires_at": null });
        std::thread::sleep(Duration::from_millis(2050));
        assert!(!daemon.stopping());
        assert!(ended.try_recv().is_err());
        let status = data(rpc(socket, "daemon_status", json!({})));
        assert_eq!(status["draining"], held);
        assert_eq!(status["inflight"], json!([]));
        refused(
            rpc(socket, "new", json!({ "ticket": null, "repos": ["tool"] })),
            "Busy",
            "draining",
        );
        assert_eq!(
            data(rpc(
                socket,
                "drain",
                json!({ "on": true, "lease_id": id, "ttl_s": 0 })
            ))["lease_id"],
            id
        );
        assert_eq!(
            data(rpc(socket, "daemon_status", json!({})))["draining"],
            held
        );
        refused(
            rpc(socket, "drain", json!({ "on": false })),
            "Busy",
            "lease_lost",
        );
        refused(
            rpc(socket, "drain", json!({ "on": false, "lease_id": "wrong" })),
            "Busy",
            "lease_lost",
        );
        refused(seal(socket, &id, "restart"), "InvalidInput", "sealed");
        data(rpc(socket, "drain", json!({ "on": false, "lease_id": id })));
        assert_eq!(
            data(rpc(socket, "daemon_status", json!({})))["draining"],
            Value::Null
        );
        data(rpc(
            socket,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        let second = data(rpc(socket, "drain", json!({ "on": true })))["lease_id"].clone();
        assert_ne!(second, id);
        seal_when_idle(socket, &second, "hold");
        stop.store(true, Ordering::SeqCst);
        ended.recv_timeout(WAIT).unwrap().unwrap();
    });
    drop(daemon);
    assert_eq!(core.close_with_exit_code().unwrap(), 0);
    assert_eq!(shutdown_record(&fx)["mode"], "hold");
}

#[test]
fn terminal_seal_replies_and_exits_with_a_stalled_reader() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    serve(&daemon, |socket, _, ended| {
        let mut stalled = UnixStream::connect(socket).unwrap();
        stalled.set_read_timeout(Some(WAIT)).unwrap();
        stalled.set_write_timeout(Some(WAIT)).unwrap();
        // The reply echoes this ID. A reply larger than the socket buffer
        // stays in write_all after the peer has stopped reading.
        let query =
            json!({ "id": "x".repeat(2 * 1024 * 1024), "method": "daemon_status", "params": {} });
        writeln!(stalled, "{query}").unwrap();
        let mut first = [0];
        stalled.read_exact(&mut first).unwrap();
        assert_eq!(first, [b'{']);

        let id = data(rpc(socket, "drain", json!({ "on": true })))["lease_id"].clone();
        seal_when_idle(socket, &id, "restart");
        ended.recv_timeout(WAIT).unwrap().unwrap();
        drop(stalled);
    });
    drop(daemon);
    assert_eq!(core.close_with_exit_code().unwrap(), 75);
    assert_eq!(shutdown_record(&fx)["mode"], "restart");
}

#[test]
fn terminal_seal_cannot_be_cancelled() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let id = lease(&server, json!({ "on": true }));
    until(|| {
        let reply = call(
            &server,
            "drain",
            json!({ "seal": true, "lease_id": id, "then": "stop" }),
        );
        if reply["ok"] == true {
            return Some(());
        }
        refused(reply, "Busy", "not_idle");
        None
    });
    assert!(daemon.stopping());
    refused(
        call(&server, "drain", json!({ "on": false })),
        "InvalidInput",
        "sealed",
    );
    refused(
        call(&server, "drain", json!({ "on": false, "lease_id": id })),
        "InvalidInput",
        "sealed",
    );
    refused(
        call(
            &server,
            "drain",
            json!({ "on": false, "lease_id": "wrong" }),
        ),
        "Busy",
        "lease_lost",
    );
}

#[test]
fn seal_exit_codes_and_shutdown_diagnostic_survive_close_error() {
    for (mode, expected) in [("stop", 0), ("restart", 75), ("sigterm", 0)] {
        let fx = Dfx::new();
        let core = fx.prepare().unwrap();
        let daemon = Daemon::start(&core).unwrap();
        serve(&daemon, |socket, stop, ended| {
            if mode == "sigterm" {
                stop.store(true, Ordering::SeqCst);
            } else {
                let id = data(rpc(socket, "drain", json!({ "on": true })))["lease_id"].clone();
                // The real socket delivers the seal reply before serving ends.
                seal_when_idle(socket, &id, mode);
            }
            ended.recv_timeout(WAIT).unwrap().unwrap();
        });
        drop(daemon);
        // Reproduce the close error without touching the live SQLite files.
        let borrowed = core.store.clone();
        assert_eq!(core.close_with_exit_code().unwrap(), expected);
        drop(borrowed);
        let record = shutdown_record(&fx);
        assert_eq!(record["mode"], mode);
        assert_eq!(record["close_error"]["code"], "Busy");
        assert!(
            fx.alerts()
                .iter()
                .any(|line| line.contains("store is still borrowed"))
        );
        assert!(!fx.socket().exists());
    }
}

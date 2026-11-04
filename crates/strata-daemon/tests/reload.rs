//! Reload tests use only temporary repositories, config directories and stores.
mod support;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
use strata_daemon::{
    handlers,
    startup::{self, Daemon},
};
use strata_types::Code;
use support::*;

fn call(d: &Daemon<'_>, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(&d.core.opts.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({"id":1,"method":method,"params":params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn serving(d: &Daemon<'_>, f: impl FnOnce()) {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(d, &listener, &stop));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        result.unwrap();
    });
}
fn add_repo(fx: &mut Dfx, name: &str) {
    let path = fx.sb.init_project(name);
    fx.sb.bare_origin(&path);
    fx.config.repos.insert(repo_name(name), fx.sb.config(&path));
    fx.write_config();
}

struct ReleaseSession<'a, 'b>(&'a strata_session::Sessions<'b>);
impl Drop for ReleaseSession<'_, '_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn wait_until(message: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn checkpoint_group(
    d: &Daemon<'_>,
    repo: &str,
) -> (strata_types::SessionId, strata_types::ReleaseGroup) {
    let new = call(d, "new", json!({"ticket":null,"repos":[repo],"only":true}));
    assert_eq!(new["ok"], true, "{new}");
    let id = serde_json::from_value(new["data"]["session"]["manifest"]["id"].clone()).unwrap();
    fs::write(
        ws(d.core, &id, repo).join("src/index.ts"),
        "export const v = 42;\n",
    )
    .unwrap();
    let cp = call(
        d,
        "checkpoint",
        json!({"session":id,"repo":repo,"reason":{"kind":"Manual","message":"reload test"},"paths":[]}),
    );
    assert_eq!(cp["ok"], true, "{cp}");
    let cp_id = serde_json::from_value(cp["data"]["checkpoint"]["id"].clone()).unwrap();
    let group = group(vec![batch(&id, repo, cp_id, &["src/index.ts"])]);
    (id, group)
}

fn assert_done(core: &strata_daemon::startup::Core, reply: &Value) {
    assert_eq!(reply["ok"], true, "{reply}");
    let id = reply["data"]["published"][0]["id"]
        .as_u64()
        .expect("publish id");
    assert_eq!(
        core.store.publish_record(id).unwrap().unwrap().phase,
        strata_store::PublishPhase::Done
    );
    assert!(
        core.store.open_publishes().unwrap().is_empty(),
        "landed row left open"
    );
}

#[test]
fn explicit_reload_enables_new_and_search_and_strict_params() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let daemon = Daemon::start(&core).unwrap();
    serving(&daemon, || {
        add_repo(&mut fx, "front");
        let reply = call(&daemon, "config_reload", json!({}));
        assert_eq!(reply["ok"], true, "{reply}");
        assert!(
            reply["data"]["repos"]
                .as_array()
                .unwrap()
                .contains(&json!("front"))
        );
        let new = call(
            &daemon,
            "new",
            json!({"ticket":null,"repos":["front"],"only":true,"base":{"front":"HEAD"}}),
        );
        assert_eq!(new["ok"], true, "{new}");
        let id = new["data"]["session"]["manifest"]["id"].clone();
        let files = call(
            &daemon,
            "files",
            json!({"session":id,"repo":"front","glob":"src/*"}),
        );
        assert_eq!(files["ok"], true, "{files}");
        assert!(!files["data"]["paths"].as_array().unwrap().is_empty());
        let old_cli = call(&daemon, "config_relax", json!({"repo":"front"}));
        assert_eq!(old_cli["error"]["detail"]["reason"], "shown_required");
        let extra = call(&daemon, "config_reload", json!({"extra":true}));
        assert_eq!(extra["error"]["rpc"], "Failed");

        let again = call(&daemon, "config_reload", json!({}));
        assert_eq!(again["data"]["outcome"], "unchanged");
        assert!(
            again["data"]["repos"]
                .as_array()
                .unwrap()
                .contains(&json!("front"))
        );
        assert_eq!(again["data"]["added"], json!([]));
        let status = call(&daemon, "daemon_status", json!({}));
        assert_eq!(
            status["data"]["config"]["last_attempt"]["outcome"],
            "unchanged"
        );
        assert_eq!(status["data"]["config"]["restart_required"], json!([]));
    });
}

#[test]
fn hot_reload_preserves_pin_audit_history_and_records_changes() {
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    fx.write_config();
    let core = fx
        .prepare_with(fx.confirmer(vec![strata_confirm::seams::Verdict::pass()]).0)
        .unwrap();
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = false;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    core.config_relax(&repo).unwrap();
    let relaxed_at = core.pins.metadata()["tool"]["relaxed_at"].clone();
    assert!(relaxed_at.is_string());

    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    add_repo(&mut fx, "front");
    assert_eq!(core.reload_for_test().outcome, "applied");
    let metadata = core.pins.metadata();
    assert_eq!(metadata["tool"]["relaxed_at"], relaxed_at);
    assert!(metadata["tool"]["pinned_at"].is_string());
    assert!(metadata["front"]["pinned_at"].is_string());
    let audit = core.pins.path().with_file_name("audit.jsonl");
    let entries: Vec<Value> = fs::read_to_string(&audit)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (event, repo) in [("config_relax", "tool"), ("tighten", "tool"), ("pin_new", "front")] {
        assert!(entries.iter().any(|e| e["event"] == event && e["repo"] == repo));
    }
    let pins: Value = serde_json::from_slice(&fs::read(core.pins.path()).unwrap()).unwrap();
    assert_eq!(pins.as_object().unwrap().len(), 1);
    assert!(pins["repos"]["tool"].get("relaxed_at").is_none());

    fs::remove_file(&audit).unwrap();
    fs::create_dir(&audit).unwrap();
    fx.config.repos.get_mut(&repo).unwrap().publish.push = false;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    assert!(!core.catalog.current().pins[&repo].push);
    assert!(core.pins.audit_error(&repo).is_some());
    assert_eq!(core.pins.metadata()["tool"]["relaxed_at"], relaxed_at);
}

#[test]
fn invalid_and_restart_changes_keep_catalog_and_pins() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let pins = fs::read(core.pins.path()).unwrap();
    fx.write_config_text("not toml [");
    assert_eq!(core.reload_for_test().outcome, "rejected");
    assert_eq!(core.catalog.current().generation, 1);
    assert_eq!(core.reload.status()["reason"], "parse");
    assert_eq!(core.reload.status()["reasons"][0]["detail"]["rule"], "toml");
    assert!(core.reload.status()["reasons"][0]["detail"]["message"].is_string());
    fx.config.status.parallel += 1;
    fx.write_config();
    let result = core.reload_for_test();
    assert_eq!(result.restart_required, ["status.parallel"]);
    assert_eq!(fs::read(core.pins.path()).unwrap(), pins);
    fx.config.status.parallel -= 1;
    fx.config.repos.remove(&repo_name("tool"));
    add_repo(&mut fx, "front");
    let result = core.reload_for_test();
    assert_eq!(result.outcome, "rejected");
    assert!(core.catalog.current().gits.contains_key(&repo_name("tool")));
    assert!(
        !core
            .catalog
            .current()
            .gits
            .contains_key(&repo_name("front"))
    );
}

#[test]
fn tighten_fences_old_and_missing_bindings_but_not_unchanged_repo_or_cleanup() {
    use strata_deps::RepoWriteLock;
    let mut fx = Dfx::with(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let old = core.catalog.enter(|c| c.generation);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    core.locks.with_repo(&repo_name("tool"), || Ok(())).unwrap();
    let e = core
        .locks
        .with_repo::<()>(&repo_name("front"), || panic!("fenced closure ran"))
        .unwrap_err();
    assert!(strata_deps::pins_tightened(&e));
    core.locks
        .with_cleanup(&repo_name("front"), &mut || Ok(()))
        .unwrap()
        .unwrap();
    drop(old);
    assert!(strata_deps::pins_tightened(
        &core
            .locks
            .with_repo(&repo_name("front"), || Ok(()))
            .unwrap_err()
    ));
    let _new = core.admit();
    core.locks
        .with_repo(&repo_name("front"), || Ok(()))
        .unwrap();
}

#[test]
fn partial_spawn_and_real_pins_write_failure_cancel_parked_loops() {
    let mut fx = Dfx::new();
    fx.config_poll_every = Duration::from_secs(3600);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let pins = fs::read(core.pins.path()).unwrap();
    serving(&d, || {
        wait_until("startup loops did not start", || {
            core.reload.loops_started.load(Ordering::SeqCst) == 2
        });
        add_repo(&mut fx, "front");
        core.reload.fail_spawn.store(true, Ordering::SeqCst);
        let failed = call(&d, "config_reload", json!({}));
        core.reload.fail_spawn.store(false, Ordering::SeqCst);
        assert_eq!(failed["data"]["outcome"], "rejected", "{failed}");
        assert_eq!(
            failed["data"]["reasons"][0]["detail"]["reason"],
            "spawn_failed"
        );
        wait_until("partial spawn was not cancelled", || {
            core.reload.parked_cancelled.load(Ordering::SeqCst) == 1
        });
        assert_eq!(core.reload.parked_spawned.load(Ordering::SeqCst), 3);
        assert_eq!(core.catalog.current().generation, 1);
        assert_eq!(fs::read(core.pins.path()).unwrap(), pins);
        assert_eq!(
            core.locks
                .with_repo(&repo_name("front"), || Ok(()))
                .unwrap_err()
                .code,
            Code::NoSuchRepo
        );

        // An actual OpenOptions failure in Pins::write, after both loops park.
        let temp_pin = fx
            .config_dir
            .join(format!(".pins.json.{}.tmp", std::process::id()));
        fs::create_dir(&temp_pin).unwrap();
        let failed = call(&d, "config_reload", json!({}));
        fs::remove_dir(&temp_pin).unwrap();
        assert_eq!(failed["data"]["outcome"], "rejected", "{failed}");
        assert_eq!(
            failed["data"]["reasons"][0]["detail"]["path"],
            json!(temp_pin)
        );
        wait_until("pins failure did not cancel both loops", || {
            core.reload.parked_cancelled.load(Ordering::SeqCst) == 3
        });
        assert_eq!(core.reload.parked_spawned.load(Ordering::SeqCst), 5);
        assert_eq!(core.reload.loops_started.load(Ordering::SeqCst), 2);
        assert_eq!(core.catalog.current().generation, 1);
        assert_eq!(fs::read(core.pins.path()).unwrap(), pins);
        assert_eq!(
            core.locks
                .with_repo(&repo_name("front"), || Ok(()))
                .unwrap_err()
                .code,
            Code::NoSuchRepo
        );
        assert_eq!(
            call(&d, "config_reload", json!({}))["data"]["outcome"],
            "applied"
        );
        wait_until("new loops did not start", || {
            core.reload.loops_started.load(Ordering::SeqCst) == 4
        });
    });
}

#[test]
fn queued_waiter_checks_membership_after_add() {
    let mut fx = Dfx::with(&["tool", "front"]);
    fx.lock_wait = Duration::from_secs(30);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let _pin = core.admit();
    let new = d
        .sessions
        .new_with(None, &[repo_name("tool")], strata_session::Layout::Only);
    let id = new.unwrap().manifest.id;
    std::thread::scope(|s| {
        core.locks
            .with_sessions(std::slice::from_ref(&id), || {
                let (tx, rx) = std::sync::mpsc::channel();
                let core_ref = &core;
                let id_ref = &id;
                let h = s.spawn(move || {
                    let core = core_ref;
                    let id = id_ref;
                    let _old = core.admit();
                    tx.send(()).unwrap();
                    core.locks
                        .with_sessions(std::slice::from_ref(id), || Ok(()))
                });
                rx.recv().unwrap();
                wait_until("session waiter did not queue", || {
                    core.locks.queued(&id) > 0
                });
                d.sessions.add(&id, &[repo_name("front")])?;
                fx.config
                    .repos
                    .get_mut(&repo_name("front"))
                    .unwrap()
                    .publish
                    .push = false;
                fx.write_config();
                assert_eq!(core.reload_for_test().outcome, "applied");
                assert!(strata_deps::pins_tightened(&h.join().unwrap().unwrap_err()));
                Ok(())
            })
            .unwrap();
    });
    let refused = core
        .locks
        .with_sessions::<()>(std::slice::from_ref(&id), || {
            panic!("stale session queue grant ran its closure")
        })
        .unwrap_err();
    assert!(strata_deps::pins_tightened(&refused));
    drop(_pin);
    let _fresh = core.admit();
    core.locks
        .with_sessions(std::slice::from_ref(&id), || Ok(()))
        .unwrap();
}

#[test]
fn new_rolls_back_after_pin_is_fenced() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    d.sessions.arm(strata_session::SessionSeam::PauseAt {
        phase: "new.pin".into(),
    });
    std::thread::scope(|s| {
        let h = s.spawn(|| {
            let _pin = core.admit();
            d.sessions.new(None, &[repo_name("tool")])
        });
        let _release = ReleaseSession(&d.sessions);
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(10)),
            "session seam was not reached"
        );
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .publish
            .push = false;
        fx.write_config();
        let reloaded = core.reload_for_test();
        d.sessions.release();
        assert_eq!(reloaded.outcome, "applied");
        let e = h.join().unwrap().unwrap_err();
        assert!(strata_deps::pins_tightened(&e), "{e:?}");
        let rows = core.store.sessions().unwrap();
        assert_eq!(rows.len(), 1, "rollback must have created a session row");
        assert!(
            rows.iter()
                .all(|r| r.lifecycle == strata_store::Lifecycle::Removed)
        );
    });
}

#[test]
fn shutdown_wakes_an_already_waiting_reload() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    std::thread::scope(|s| {
        let h = s.spawn(|| core.request_reload(Duration::from_secs(15)));
        wait_until("reload did not register its ticket", || {
            core.reload.requested_for_test() == 1
        });
        core.locks.shutdown();
        assert_eq!(
            h.join().unwrap().unwrap_err().detail.unwrap()["reason"],
            "shutting_down"
        );
    });
}

#[test]
fn landed_publish_base_registration_fence_defers_and_closes_done() {
    landed_fence(false);
}

#[test]
fn landed_publish_under_locks_and_pending_attempt_fences_defer_then_catch_up() {
    landed_fence(true);
}

fn landed_fence(cached: bool) {
    let mut fx = Dfx::new();
    fx.config_poll_every = Duration::from_secs(3600);
    fx.catch_up_every = Duration::from_secs(3600);
    let repo = repo_name("tool");
    let rc = fx.config.repos.get_mut(&repo).unwrap();
    rc.publish.push = false;
    rc.rebase.quiet_secs = 0;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    serving(&d, || {
        let (id, group) = checkpoint_group(&d, "tool");
        let old = core.admit();
        let git = &core.snapshot().unwrap().gits[&repo];
        std::thread::scope(|scope| {
            let gap = if cached {
                Some(strata_publish::seams::pause(
                    git,
                    strata_publish::PublishFault::PauseBeforeRebase,
                ))
            } else {
                d.sessions.arm(strata_session::SessionSeam::PauseAt {
                    phase: "base.register".into(),
                });
                None
            };
            let h = scope.spawn(|| call(&d, "publish", json!({"group":group})));
            let _release = ReleaseSession(&d.sessions);
            if let Some(gap) = &gap {
                gap.wait_until_paused();
                // Ensure rebase's base_for is a cache hit, so the refusal must
                // come from under_locks, not from base registration.
                let head = git
                    .read_ref(&strata_publish::chain_ref("main"))
                    .unwrap()
                    .unwrap();
                let tree = git.tree_of(&head).unwrap();
                let deps = core.store.session(&id).unwrap().unwrap().manifest.repos[&repo]
                    .base
                    .deps
                    .clone();
                d.sessions.ensure_base_for(&repo, &tree, &deps).unwrap();
            } else {
                assert!(
                    d.sessions.wait_paused_timeout(Duration::from_secs(10)),
                    "session seam was not reached"
                );
            }
            fx.config.repos.get_mut(&repo).unwrap().sync.deps = strata_types::SyncDeps::Confirm;
            fx.write_config();
            let reloaded = core.reload_for_test();
            if let Some(gap) = gap {
                gap.release();
            } else {
                d.sessions.release();
            }
            assert_eq!(reloaded.outcome, "applied");
            let reply = h.join().unwrap();
            assert_done(&core, &reply);
            assert_eq!(
                reply["data"]["published"][0]["rebases"][0]["report"]["reason"], "pins_tightened",
                "{reply}"
            );
            let row = core.store.rebase_pending_of(&id, &repo).unwrap().unwrap();
            assert_eq!(row.reason, "pins_tightened");
            if cached {
                let cfg = &core.snapshot().unwrap().config.repos[&repo];
                let deps = strata_deps::Ctx {
                    store: &core.store,
                    paths: &core.config.paths,
                    repo: &repo,
                    cfg,
                    git,
                    env: &core.env,
                    locks: &core.locks,
                };
                let ctx = strata_publish::Ctx {
                    store: &core.store,
                    git,
                    cfg,
                    repo: &repo,
                    deps: &deps,
                    locks: &core.locks,
                    sessions: &d.sessions,
                };
                let retry =
                    strata_publish::pending::attempt(&ctx, &row, strata_publish::MovePolicy::Auto)
                        .unwrap()
                        .unwrap();
                assert!(
                    matches!(retry, strata_publish::RebaseReport::Deferred { reason, .. } if reason == "pins_tightened")
                );
                let deferred = core.store.rebase_pending_of(&id, &repo).unwrap().unwrap();
                assert_eq!(deferred.reason, "pins_tightened");
                assert_eq!(deferred.attempts, row.attempts + 1);
                assert_done(&core, &reply);
            }
        });
        drop(old);
        let passes = handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
        let rows = passes
            .into_iter()
            .find(|(r, _)| *r == repo)
            .unwrap()
            .1
            .unwrap();
        assert!(
            matches!(
                &rows[..],
                [(
                    _,
                    Ok(strata_publish::pending::CatchUp::Attempted(
                        strata_publish::RebaseReport::Moved { .. }
                    ))
                )]
            ),
            "{rows:?}"
        );
        assert!(core.store.rebase_pending_of(&id, &repo).unwrap().is_none());
    });
}

#[test]
fn rm_tightened_while_holding_queue_refuses_before_removing() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let id = {
        let _pin = core.admit();
        d.sessions
            .new(None, &[repo_name("tool")])
            .unwrap()
            .manifest
            .id
    };
    d.sessions.arm(strata_session::SessionSeam::PauseAt {
        phase: "remove.before_removing".into(),
    });
    std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            let _pin = core.admit();
            core.locks.with_sessions(std::slice::from_ref(&id), || {
                d.sessions.remove(&id, false, None)
            })
        });
        let _release = ReleaseSession(&d.sessions);
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(10)),
            "session seam was not reached"
        );
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .publish
            .push = false;
        fx.write_config();
        let reloaded = core.reload_for_test();
        d.sessions.release();
        assert_eq!(reloaded.outcome, "applied");
        let e = h.join().unwrap().unwrap_err();
        assert!(strata_deps::pins_tightened(&e), "{e:?}");
        assert_eq!(
            core.store.session(&id).unwrap().unwrap().lifecycle,
            strata_store::Lifecycle::Active
        );
        assert!(ws(&core, &id, "tool").exists());
    });
}

#[test]
fn hot_add_does_not_wait_for_existing_holder() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let core_ref = &core;
        let h = scope.spawn(move || {
            let core = core_ref;
            let _old = core.admit();
            core.locks.with_repo(&repo_name("tool"), || {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        ready.recv().unwrap();
        add_repo(&mut fx, "front");
        let result = core.reload_for_test();
        // Always release the fixture holder before asserting to avoid hiding a
        // failure behind a scoped-thread join.
        release.send(()).unwrap();
        h.join().unwrap().unwrap();
        assert_eq!(result.outcome, "applied");
        let _new = core.admit();
        core.locks
            .with_repo(&repo_name("front"), || Ok(()))
            .unwrap();
    });
}

#[test]
fn relax_rejects_a_changed_file_and_preserves_pin_bytes() {
    let mut fx = Dfx::new();
    let (confirmer, _, _) = fx.confirmer(vec![strata_daemon::seams::Verdict::pass()]);
    let core = fx.prepare_unseeded(confirmer).unwrap();
    let before = fs::read(core.pins.path()).unwrap();
    let repo = repo_name("tool");
    fx.config
        .repos
        .get_mut(&repo)
        .unwrap()
        .checks
        .push("true".into());
    let e = core
        .config_relax_with(&repo, &|| fx.write_config())
        .unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed);
    assert_eq!(e.detail.unwrap()["reason"], "action_changed");
    assert_eq!(fs::read(core.pins.path()).unwrap(), before);
    let e = core.config_relax(&repo).unwrap_err();
    assert_eq!(e.detail.unwrap()["reason"], "config_not_applied");
}

#[test]
fn deploy_flag_does_not_fence() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let _old = core.admit();
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .deploy_on_push = true;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    core.locks.with_repo(&repo_name("tool"), || Ok(())).unwrap();
}

#[test]
fn old_operation_keeps_its_dev_port_policy() {
    let mut fx = Dfx::new();
    fx.config.dev_ports.range = "20000-20999".into();
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let old = core.admit();
    fx.config.dev_ports.range = "30000-30999".into();
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    let first = d.sessions.new(None, &[repo_name("tool")]).unwrap();
    let leases = core
        .store
        .dev_port_leases(Some(&first.manifest.id))
        .unwrap()
        .unwrap();
    assert!(!leases.is_empty());
    assert!(leases.iter().all(|l| (20000..21000).contains(&l.base)));
    drop(old);
    let _new = core.admit();
    let next = d.sessions.new(None, &[repo_name("tool")]).unwrap();
    let leases = core
        .store
        .dev_port_leases(Some(&next.manifest.id))
        .unwrap()
        .unwrap();
    assert!(!leases.is_empty());
    assert!(leases.iter().all(|l| (30000..31000).contains(&l.base)));
}

#[test]
fn fetch_revalidates_changed_config_with_a_successful_staged_control() {
    for change in [false, true] {
        let mut fx = Dfx::new();
        let core = fx.prepare().unwrap();
        let repo = repo_name("tool");
        let main = fx.main("tool");
        let before = core.catalog.current().gits[&repo]
            .read_ref(&strata_gitio::remote_ref("origin", "main"))
            .unwrap();
        let head = strata_types::CommitOid::try_from(fx.sb.commit(
            &main,
            &[("upstream.txt", b"new upstream")],
            "upstream",
        ))
        .unwrap();
        fx.sb.git(&main, &["push", "-q", "origin", "main"]);
        let step = strata_daemon::periodic_fetch::fetch_due_with(
            &core,
            &repo,
            SystemTime::now(),
            &mut |git, branch| {
                let stage = strata_daemon::periodic_fetch::staging_ref(branch);
                git.update_ref(&stage, &head, None).unwrap();
                if change {
                    fx.config
                        .repos
                        .get_mut(&repo)
                        .unwrap()
                        .checks
                        .push("true".into());
                    fx.write_config();
                    assert_eq!(core.reload_for_test().outcome, "applied");
                }
                Ok(head.clone())
            },
        )
        .unwrap();
        let git = &core.catalog.current().gits[&repo];
        if change {
            assert_eq!(step, strata_daemon::periodic_fetch::FetchStep::Skipped);
            assert!(!core.store.repo_fetch(&repo).unwrap().unwrap().ok);
            assert_eq!(
                git.read_ref(&strata_gitio::remote_ref("origin", "main"))
                    .unwrap(),
                before
            );
        } else {
            assert_eq!(step, strata_daemon::periodic_fetch::FetchStep::Fetched);
            assert!(core.store.repo_fetch(&repo).unwrap().unwrap().ok);
            assert_eq!(
                git.read_ref(&strata_gitio::remote_ref("origin", "main"))
                    .unwrap(),
                Some(head)
            );
        }
    }
}

#[test]
fn poll_applies_a_file_changed_between_prepare_and_serve() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    add_repo(&mut fx, "front");
    serving(&d, || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !core
            .catalog
            .current()
            .config
            .repos
            .contains_key(&repo_name("front"))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "poll lost a pre-serve config change"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let reply = call(&d, "config_reload", json!({}));
        assert_eq!(reply["data"]["outcome"], "unchanged");
        assert!(
            reply["data"]["repos"]
                .as_array()
                .unwrap()
                .contains(&json!("front"))
        );
    });
}

#[test]
fn hot_added_repo_syncs_publishes_moves_and_removes() {
    let mut fx = Dfx::new();
    fx.config_poll_every = Duration::from_secs(3600);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    serving(&d, || {
        add_repo(&mut fx, "front");
        fx.config
            .repos
            .get_mut(&repo_name("front"))
            .unwrap()
            .publish
            .push = false;
        fx.write_config();
        assert_eq!(
            call(&d, "config_reload", json!({}))["data"]["outcome"],
            "applied"
        );
        let pin = &core.catalog.current().pins[&repo_name("front")];
        assert!(pin.confirm, "a new name keeps conservative pins");
        assert!(!pin.push);
        let (id, _) = checkpoint_group(&d, "front");
        let main = fx.main("front");
        fx.sb
            .commit(&main, &[("upstream.txt", b"remote advance")], "upstream");
        fx.sb.git(&main, &["push", "-q", "origin", "main"]);
        let sync = call(
            &d,
            "sync",
            json!({"session":id,"repos":["front"],"check":false,"fetch":true}),
        );
        assert_eq!(sync["ok"], true, "{sync}");
        assert_eq!(sync["data"]["repos"]["front"]["result"], "moved", "{sync}");
        assert_eq!(
            fs::read(ws(&core, &id, "front").join("upstream.txt")).unwrap(),
            b"remote advance"
        );
        let cp = call(
            &d,
            "checkpoint",
            json!({"session":id,"repo":"front","reason":{"kind":"Manual","message":"after sync"},"paths":[]}),
        );
        assert_eq!(cp["ok"], true, "{cp}");
        let cp_id = serde_json::from_value(cp["data"]["checkpoint"]["id"].clone()).unwrap();
        let group = group(vec![batch(&id, "front", cp_id, &["src/index.ts"])]);
        let reply = call(&d, "publish", json!({"group":group}));
        assert_done(&core, &reply);
        assert_eq!(
            reply["data"]["published"][0]["rebases"][0]["report"]["kind"], "Moved",
            "{reply}"
        );
        let sync = call(
            &d,
            "sync",
            json!({"session":id,"repos":["front"],"check":false,"fetch":false}),
        );
        assert_eq!(sync["ok"], true, "{sync}");
        let status = call(&d, "status", json!({"session":id,"cwd":null,"all":false}));
        assert_eq!(status["ok"], true, "{status}");
        assert_eq!(
            call(&d, "rm", json!({"session":id,"discard":false}))["ok"],
            true
        );
    });
}

#[test]
fn hot_add_refuses_real_shadow_quarantine_without_relational_rows() {
    for symlink_directory in [false, true] {
        let mut fx = Dfx::new();
        let core = fx.prepare().unwrap();
        let pins = fs::read(core.pins.path()).unwrap();
        add_repo(&mut fx, "front");
        let shadow = fx
            .config
            .paths
            .root
            .join("sessions/.repos/retained/front.git");
        fs::create_dir_all(&shadow).unwrap();
        let quarantine = shadow.join("strata-quarantine");
        let entry = if symlink_directory {
            std::os::unix::fs::symlink("missing-target", &quarantine).unwrap();
            quarantine.clone()
        } else {
            fs::create_dir(&quarantine).unwrap();
            let entry = quarantine.join("retired-worktree");
            std::os::unix::fs::symlink("missing-target", &entry).unwrap();
            entry
        };
        assert!(!core.store.repo_has_state(&repo_name("front")).unwrap());
        assert_eq!(
            startup::quarantine_entries(&fx.config.paths.root),
            vec![entry.clone()]
        );
        let result = core.reload_for_test();
        assert_eq!(result.outcome, "rejected");
        assert_eq!(result.restart_required, ["repos.front (stale_rows)"]);
        assert_eq!(result.reasons[0]["detail"]["reason"], "stale_rows");
        assert_eq!(core.catalog.current().generation, 1);
        assert_eq!(fs::read(core.pins.path()).unwrap(), pins);
        assert!(
            fs::symlink_metadata(&entry)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // The same orphan belongs only to front, not another newly added name.
        fx.config.repos.remove(&repo_name("front"));
        add_repo(&mut fx, "other");
        assert_eq!(core.reload_for_test().outcome, "applied");
    }
}

#[test]
fn add_fenced_after_first_build_keeps_paths_and_cleans_unrecorded_pins() {
    let mut fx = Dfx::with(&["tool", "front", "zlast"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let _old = core.admit();
    let id = d
        .sessions
        .new_with(None, &[repo_name("tool")], strata_session::Layout::Only)
        .unwrap()
        .manifest
        .id;
    let root = ws(&core, &id, "tool").parent().unwrap().to_path_buf();
    d.sessions.arm(strata_session::SessionSeam::PauseAt {
        phase: "add.build@zlast".into(),
    });
    std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            let _old = core.admit();
            d.sessions
                .add(&id, &[repo_name("front"), repo_name("zlast")])
        });
        let _release = ReleaseSession(&d.sessions);
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(10)),
            "session seam was not reached"
        );
        // The first worker can finish its build while the second is paused.
        wait_until("first added workspace was not built", || {
            root.join("front").is_dir()
        });
        fx.config
            .repos
            .get_mut(&repo_name("zlast"))
            .unwrap()
            .publish
            .push = false;
        fx.write_config();
        let result = core.reload_for_test();
        d.sessions.release();
        assert_eq!(result.outcome, "applied");
        let error = h.join().unwrap().unwrap_err();
        assert!(strata_deps::pins_tightened(&error), "{error:?}");
        assert!(
            error.detail.as_ref().unwrap()["kept"]
                .as_array()
                .unwrap()
                .contains(&json!(root.join("front")))
        );
        let rec = core.store.session(&id).unwrap().unwrap();
        assert_eq!(
            rec.manifest.repos.keys().collect::<Vec<_>>(),
            [&repo_name("tool")]
        );
        assert!(root.join("front").is_dir());
        for repo in ["front", "zlast"] {
            assert_eq!(
                core.snapshot().unwrap().gits[&repo_name(repo)]
                    .read_ref(&format!("refs/strata/pins/{id}"))
                    .unwrap(),
                None
            );
        }
    });
}

#[test]
fn config_relax_not_applied_on_reload_then_confirmed_without_restart() {
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    fx.write_config();
    let (confirmer, log, _) = fx.confirmer(vec![strata_daemon::seams::Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    let before = fs::read(core.pins.path()).unwrap();
    let old = core.admit();
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = false;
    fx.write_config();
    let report = core.reload_for_test();
    assert_eq!(report.outcome, "applied");
    assert_eq!(report.pending_relax, std::slice::from_ref(&repo));
    assert!(
        core.warnings
            .snapshot()
            .contains(&strata_daemon::DaemonWarning::ConfigRelaxPending { repo: repo.clone() })
    );
    assert_eq!(fs::read(core.pins.path()).unwrap(), before);
    assert!(log.shown().is_empty());
    assert_eq!(log.probes(), 0);
    assert!(core.catalog.current().pins[&repo].confirm);
    core.locks.with_repo(&repo, || Ok(())).unwrap();
    drop(old);
    // A concurrent reload during confirmation preserves its unrelated change.
    fx.config
        .repos
        .get_mut(&repo)
        .unwrap()
        .checks
        .push("true".into());
    core.config_relax_with(&repo, &|| {
        fx.write_config();
        assert_eq!(core.reload_for_test().outcome, "applied");
    })
    .unwrap();
    assert!(!core.catalog.current().pins[&repo].confirm);
    assert_eq!(
        core.catalog.current().source.repos[&repo]
            .checks
            .last()
            .unwrap(),
        "true"
    );
    assert_eq!(log.shown().len(), 1);
    assert!(core.catalog.current().pending_relax.is_empty());
    core.locks.with_repo(&repo, || Ok(())).unwrap();
    assert_eq!(core.reload_for_test().outcome, "unchanged");
    assert!(!core.catalog.current().pins[&repo].confirm);
    assert_ne!(fs::read(core.pins.path()).unwrap(), before);
}

#[test]
fn reload_tightening_during_confirmation_is_not_undone_by_relax() {
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    fx.write_config();
    let (confirmer, log, _) = fx.confirmer(vec![strata_daemon::seams::Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = false;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    let before = fs::read(core.pins.path()).unwrap();
    fx.config.repos.get_mut(&repo).unwrap().publish.confirm = true;
    let e = core
        .config_relax_with(&repo, &|| {
            fx.write_config();
            assert_eq!(core.reload_for_test().outcome, "applied");
        })
        .unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed);
    assert_eq!(reason(&e), Some("action_changed"));
    assert_eq!(log.shown().len(), 1);
    assert!(core.catalog.current().pins[&repo].confirm);
    assert_eq!(fs::read(core.pins.path()).unwrap(), before);
}

#[test]
fn catalog_growth_warning_replaces_the_previous_count() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    for _ in 0..1000 {
        let mut next = core.catalog.current().clone();
        next.generation += 1;
        core.catalog.publish(next);
    }
    for _ in 0..2 {
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .checks
            .push("true".into());
        fx.write_config();
        assert_eq!(core.reload_for_test().outcome, "applied");
        let counts: Vec<_> = core
            .warnings
            .snapshot()
            .into_iter()
            .filter_map(|w| match w {
                strata_daemon::DaemonWarning::CatalogGrowth { generations } => Some(generations),
                _ => None,
            })
            .collect();
        assert_eq!(counts, [core.catalog.len()]);
    }
    assert_eq!(
        fx.alerts()
            .iter()
            .filter(|a| a.contains("1000 catalogs"))
            .count(),
        1
    );
}

#[test]
fn reconcile_matches_startup_for_tighten_relax_kept_and_missing_pins() {
    use std::collections::BTreeMap;
    use strata_daemon::confirm_flow::{self, Pins};
    let mut fx = Dfx::new();
    let repo = repo_name("tool");
    for (old_confirm, old_push, new_confirm, new_push) in [
        (false, true, true, false),
        (true, false, false, true),
        (false, true, false, true),
    ] {
        let cfg = fx.config.repos.get_mut(&repo).unwrap();
        cfg.publish.confirm = old_confirm;
        cfg.publish.push = old_push;
        let flags = confirm_flow::flags_of(cfg);
        let mut old = BTreeMap::from([(repo.clone(), flags.clone()), (repo_name("kept"), flags)]);
        for missing in [false, true] {
            if missing {
                old.remove(&repo);
            }
            let cfg = fx.config.repos.get_mut(&repo).unwrap();
            cfg.publish.confirm = new_confirm;
            cfg.publish.push = new_push;
            let file = BTreeMap::from([(repo.clone(), confirm_flow::flags_of(cfg))]);
            let (expected, pending) = confirm_flow::reconcile(&old, &file);
            let path = fx.config_dir.join("comparison-pins.json");
            fs::write(&path, serde_json::to_vec(&json!({"repos":old})).unwrap()).unwrap();
            let startup = Pins::load(&path, &fx.config).unwrap();
            assert_eq!(startup.pinned(&repo), Some(expected[&repo].clone()));
            assert_eq!(
                startup.pinned(&repo_name("kept")),
                Some(expected[&repo_name("kept")].clone())
            );
            assert_eq!(startup.pending(), pending);
        }
    }
}

#[test]
fn old_tree_worker_requeues_a_fenced_take_with_a_fresh_catalog() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let repo = repo_name("tool");
    let id = {
        let _pin = core.admit();
        d.sessions.new(None, std::slice::from_ref(&repo)).unwrap().manifest.id
    };
    // The missing intent is dropped only after admission and both lock takes.
    d.sessions.requeue_old_tree(
        strata_session::OldTreeKey { session: id, repo: repo.clone(), n: 999 },
        fx.sb.path("old-tree-no-longer-present"),
        1,
    );
    let stop = AtomicBool::new(false);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let (admitted, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = std::sync::Mutex::new(released);
    let after_admit = || {
        let generation = core.snapshot().unwrap().generation;
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        if attempt != 0 {
            // The fresh retry can take the previously fenced repo.
            core.locks.with_repo(&repo, || Ok(())).unwrap();
        }
        admitted.send(generation).unwrap();
        if attempt == 0 {
            let _ = released.lock().unwrap().recv_timeout(Duration::from_secs(10));
        }
    };
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            handlers::test_old_tree_loop(
                &d,
                &|| stop.load(Ordering::SeqCst),
                Duration::from_millis(50),
                &after_admit,
            );
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let old = ready.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(old, core.catalog.current().generation);
            fx.config.repos.get_mut(&repo).unwrap().publish.push = false;
            fx.write_config();
            assert_eq!(core.reload_for_test().outcome, "applied");
            let fresh = core.catalog.current().generation;
            assert!(fresh > old);
            release.send(()).unwrap();
            // No lock is contended: only the old admission's fence can cause
            // this item to be retried instead of dropped as no_intent.
            assert_eq!(ready.recv_timeout(Duration::from_secs(10)).unwrap(), fresh);
        }));
        stop.store(true, Ordering::SeqCst);
        drop(release);
        worker.join().unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    });
}

#[test]
fn quarantine_enumeration_does_not_follow_directory_links() {
    let fx = Dfx::new();
    let root = &fx.config.paths.root;
    let outside = fx.sb.path("outside");
    fs::create_dir_all(outside.join("front.git/strata-quarantine/entry")).unwrap();
    let repos = root.join("sessions/.repos");
    fs::create_dir_all(repos.join("real-session")).unwrap();
    std::os::unix::fs::symlink(&outside, repos.join("linked-session")).unwrap();
    let linked_shadow = repos.join("real-session/front.git");
    std::os::unix::fs::symlink(outside.join("front.git"), &linked_shadow).unwrap();
    // The shadow link itself is suspicious; no target entry is traversed.
    assert_eq!(startup::quarantine_entries(root), [linked_shadow]);
}

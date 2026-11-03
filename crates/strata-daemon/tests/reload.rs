//! Reload tests use only temporary repositories, config directories and stores.
mod support;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
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

#[test]
fn reload_reply_then_new_and_search_and_poll_unchanged() {
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
            json!({"ticket":null,"repos":["front"],"only":true}),
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
fn invalid_and_restart_changes_keep_catalog_and_pins() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let pins = fs::read(core.pins.path()).unwrap();
    fx.write_config_text("not toml [");
    assert_eq!(core.reload_for_test().outcome, "rejected");
    assert_eq!(core.catalog.current().generation, 1);
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
fn tighten_fences_old_missing_and_sibling_takes_but_not_new_or_cleanup() {
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
fn failed_spawn_or_pins_write_inserts_no_repo() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    add_repo(&mut fx, "front");
    for spawn in [true, false] {
        core.reload.fail_spawn.store(spawn, Ordering::SeqCst);
        core.reload.fail_pins.store(!spawn, Ordering::SeqCst);
        assert_eq!(core.reload_for_test().outcome, "rejected");
        assert_eq!(core.catalog.current().generation, 1);
        assert_eq!(
            core.locks
                .with_repo(&repo_name("front"), || Ok(()))
                .unwrap_err()
                .code,
            Code::NoSuchRepo
        );
    }
    core.reload.fail_pins.store(false, Ordering::SeqCst);
    assert_eq!(core.reload_for_test().outcome, "applied");
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
                while core.locks.queued(&id) == 0 {
                    std::thread::yield_now();
                }
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
        d.sessions.wait_paused();
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .publish
            .push = false;
        fx.write_config();
        assert_eq!(core.reload_for_test().outcome, "applied");
        d.sessions.release();
        let e = h.join().unwrap().unwrap_err();
        assert!(strata_deps::pins_tightened(&e), "{e:?}");
        assert!(
            core.store
                .sessions()
                .unwrap()
                .iter()
                .all(|r| r.lifecycle == strata_store::Lifecycle::Removed)
        );
    });
}

#[test]
fn shutdown_wakes_pending_reload_and_empty_params_are_strict() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    std::thread::scope(|s| {
        let h = s.spawn(|| core.request_reload(Duration::from_secs(15)));
        core.locks.shutdown();
        assert_eq!(
            h.join().unwrap().unwrap_err().detail.unwrap()["reason"],
            "shutting_down"
        );
    });
}

#[test]
fn landed_publish_base_registration_fence_defers_and_closes_done() {
    let mut fx = Dfx::new();
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    serving(&d, || {
        let new = call(&d, "new", json!({"ticket":null,"repos":["tool"]}));
        assert_eq!(new["ok"], true, "{new}");
        let id: strata_types::SessionId =
            serde_json::from_value(new["data"]["session"]["manifest"]["id"].clone()).unwrap();
        fs::write(
            ws(&core, &id, "tool").join("src/index.ts"),
            "export const v = 42;\n",
        )
        .unwrap();
        let cp = call(
            &d,
            "checkpoint",
            json!({"session":id,"repo":"tool","reason":{"kind":"Manual","message":"reload test"},"paths":[]}),
        );
        assert_eq!(cp["ok"], true, "{cp}");
        let cp_id = serde_json::from_value(cp["data"]["checkpoint"]["id"].clone()).unwrap();
        let group = group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]);
        d.sessions.arm(strata_session::SessionSeam::PauseAt {
            phase: "base.register".into(),
        });
        std::thread::scope(|scope| {
            let h = scope.spawn(|| call(&d, "publish", json!({"group":group})));
            d.sessions.wait_paused();
            fx.config
                .repos
                .get_mut(&repo_name("tool"))
                .unwrap()
                .sync
                .deps = strata_types::SyncDeps::Confirm;
            fx.write_config();
            assert_eq!(core.reload_for_test().outcome, "applied");
            d.sessions.release();
            let reply = h.join().unwrap();
            assert_eq!(reply["ok"], true, "{reply}");
            assert_eq!(
                reply["data"]["published"][0]["rebases"][0]["report"]["reason"], "pins_tightened",
                "{reply}"
            );
            assert!(
                core.store.open_publishes().unwrap().is_empty(),
                "landed row left open"
            );
            assert_eq!(
                core.store
                    .rebase_pending_of(&id, &repo_name("tool"))
                    .unwrap()
                    .unwrap()
                    .reason,
                "pins_tightened"
            );
        });
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
        d.sessions.wait_paused();
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .publish
            .push = false;
        fx.write_config();
        assert_eq!(core.reload_for_test().outcome, "applied");
        d.sessions.release();
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
fn hot_add_does_not_wait_for_existing_holder_and_reuses_fifo() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
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
fn deploy_flag_does_not_fence_and_relax_uses_committer() {
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
fn fetch_discards_a_result_prepared_under_changed_config() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let repo = repo_name("tool");
    let head =
        strata_types::CommitOid::try_from(fx.sb.head(&fx.main("tool")).trim().to_owned()).unwrap();
    let result = strata_daemon::periodic_fetch::fetch_due_with(
        &core,
        &repo,
        std::time::SystemTime::now(),
        &mut |_git, _branch| {
            fx.config
                .repos
                .get_mut(&repo)
                .unwrap()
                .checks
                .push("true".into());
            fx.write_config();
            assert_eq!(core.reload_for_test().outcome, "applied");
            Ok(head.clone())
        },
    )
    .unwrap();
    assert_eq!(result, strata_daemon::periodic_fetch::FetchStep::Skipped);
    assert!(!core.store.repo_fetch(&repo).unwrap().unwrap().ok);
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

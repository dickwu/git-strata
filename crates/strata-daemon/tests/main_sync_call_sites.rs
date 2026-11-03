//! T4 entry points use isolated stores and in-process daemon harnesses.
mod support;

use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};
use strata_confirm::seams::Verdict;
use strata_daemon::{
    handlers::{self, Server, Trackers},
    rpc::{Method, Params, Request},
    startup::{self, Core, Daemon},
};
use strata_store::PendingSource;
use strata_types::{CommitOid, SessionId, SyncDeps};
use support::*;

fn fixture() -> Dfx {
    let mut fx = Dfx::new();
    let cfg = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    cfg.rebase.quiet_secs = 3600;
    cfg.sync.fetch_secs = 0;
    fx.write_config();
    fx
}

fn call(server: &Server<'_, '_>, method: &str, params: Value) -> Value {
    let method = Method::from_name(method).unwrap();
    server
        .handle(Request {
            id: json!(1),
            method,
            params: Params::parse(method, params).unwrap(),
        })
        .0
}

fn ok(reply: Value) -> Value {
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"].clone()
}

fn new(server: &Server<'_, '_>) -> SessionId {
    serde_json::from_value(
        ok(call(
            server,
            "new",
            json!({"ticket":null,"repos":["tool"],"only":true}),
        ))["session"]["manifest"]["id"]
            .clone(),
    )
    .unwrap()
}

fn advance(fx: &Dfx, core: &Core) -> CommitOid {
    let main = fx.main("tool");
    let head = fx
        .sb
        .commit(&main, &[("src/moved.ts", b"upstream\n")], "upstream");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo_name("tool")]
        .fetch("origin", "main")
        .unwrap();
    CommitOid::try_from(head).unwrap()
}

fn assert_enrolled(core: &Core, session: &SessionId, head: &CommitOid) {
    let row = core
        .store
        .rebase_pending_of(session, &repo_name("tool"))
        .unwrap()
        .expect("T4 must enroll the existing workspace");
    assert_eq!(row.source, PendingSource::Main);
    assert_eq!(row.head.as_deref(), Some(head.as_str()));
}

#[test]
fn upstream_not_following_has_no_sync_fix() {
    for split in [false, true] {
        let mut fx = fixture();
        let rn = repo_name("tool");
        let cfg = fx.config.repos.get_mut(&rn).unwrap();
        cfg.sync.deps = SyncDeps::Confirm;
        if split {
            cfg.publish.branch = "work".into();
            cfg.deps_dirs.clear();
            cfg.seed_caches.clear();
            cfg.install_inputs.clear();
            cfg.install = None;
        } else {
            cfg.sync.follow_main = Some(false);
        }
        fx.write_config();
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let trackers = Trackers::new(&core);
        let server = Server::new(&d, &trackers);
        let s = new(&server);
        advance(&fx, &core);
        let reply = ok(call(&server, "sync", json!({"session":s,"check":true})));
        let upstream = &reply["upstream"]["repos"]["tool"];
        assert_eq!(upstream["following"], false);
        assert_eq!(upstream["behind"], 1);
        assert!(upstream["fix"].is_null(), "{reply}");
    }
}

#[test]
fn new_runs_main_follow_for_existing_sessions() {
    let fx = fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let old = new(&server);
    let head = advance(&fx, &core);
    assert!(
        core.store
            .rebase_pending_of(&old, &repo_name("tool"))
            .unwrap()
            .is_none()
    );
    new(&server);
    assert_enrolled(&core, &old, &head);
}

#[test]
fn sync_runs_main_follow_for_other_sessions() {
    let fx = fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let old = new(&server);
    let syncing = new(&server);
    let head = advance(&fx, &core);
    assert!(
        core.store
            .rebase_pending_of(&old, &repo_name("tool"))
            .unwrap()
            .is_none()
    );
    ok(call(
        &server,
        "sync",
        json!({"session":syncing,"fetch":false}),
    ));
    assert_enrolled(&core, &old, &head);
}

#[test]
fn startup_and_config_restart_reconcile_existing_sessions() {
    for enable_on_restart in [false, true] {
        let mut fx = fixture();
        let rn = repo_name("tool");
        if enable_on_restart {
            let cfg = fx.config.repos.get_mut(&rn).unwrap();
            cfg.sync.follow_main = Some(false);
            cfg.sync.deps = SyncDeps::Confirm;
            fx.write_config();
        }
        let core = fx.prepare().unwrap();
        let s = {
            let d = Daemon::start(&core).unwrap();
            d.sessions
                .new(None, std::slice::from_ref(&rn))
                .unwrap()
                .manifest
                .id
        };
        let head = advance(&fx, &core);
        assert!(core.store.rebase_pending_of(&s, &rn).unwrap().is_none());
        core.close().unwrap();
        if enable_on_restart {
            fx.config.repos.get_mut(&rn).unwrap().sync.follow_main = Some(true);
            fx.write_config();
        }
        // A restart reads the new config; serve's first T2 pass runs T4.
        // Periodic fetch/install are disabled, so they cannot enroll the row.
        let core = fx.prepare().unwrap();
        assert!(core.config.repos[&rn].follow_main());
        let d = Daemon::start(&core).unwrap();
        assert!(core.store.rebase_pending_of(&s, &rn).unwrap().is_none());
        let listener = startup::bind(&fx.socket()).unwrap();
        let external = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| handlers::serve(&d, &listener, &external));
            let deadline = Instant::now() + Duration::from_secs(15);
            while core.store.rebase_pending_of(&s, &rn).unwrap().is_none()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            d.request_stop();
            serving.join().unwrap().unwrap();
        });
        assert_enrolled(&core, &s, &head);
    }
}

#[test]
fn config_relax_enrolls_deps_on_next_catch_up() {
    let mut fx = fixture();
    let rn = repo_name("tool");
    fx.config.repos.get_mut(&rn).unwrap().sync.deps = SyncDeps::Confirm;
    fx.write_config();
    fx.prepare().unwrap().close().unwrap();
    fx.config.repos.get_mut(&rn).unwrap().sync.deps = SyncDeps::Apply;
    fx.write_config();
    let core = fx
        .prepare_with(fx.confirmer(vec![Verdict::pass()]).0)
        .unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let main = fx.main("tool");
    fx.sb.commit(&main, &[("bun.lock", b"new lock\n")], "deps");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    let v = ok(call(&server, "sync", json!({"session":s})));
    assert_eq!(v["repos"]["tool"]["result"], "source_only");
    assert!(core.store.rebase_pending_of(&s, &rn).unwrap().is_none());
    ok(call(&server, "config_relax", json!({"repo":rn})));
    assert_eq!(core.effective_sync_deps(&rn), SyncDeps::Apply);
    handlers::catch_up_pass(&d, SystemTime::now());
    let pending = core
        .store
        .rebase_pending_of(&s, &rn)
        .unwrap()
        .expect("the next T2 pass must enroll the dependency-only move");
    assert_eq!(pending.source, PendingSource::Main);
    assert!(pending.to_deps.is_some());
}

#[test]
fn push_runs_main_follow_for_existing_sessions() {
    let fx = fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let rn = repo_name("tool");
    let head = advance(&fx, &core);
    let git = &core.gits[&rn];
    let pushed = git
        .commit_tree(&git.tree_of(&head).unwrap(), &head, "push\n")
        .unwrap();
    git.ref_transaction(&[
        strata_gitio::RefUpdate::set(
            "refs/strata/publish/main",
            pushed.clone(),
            git.read_ref("refs/strata/publish/main").unwrap(),
        ),
        strata_gitio::RefUpdate::set(
            "refs/strata/anchor/main",
            head.clone(),
            git.read_ref("refs/strata/anchor/main").unwrap(),
        ),
    ])
    .unwrap();
    assert!(core.store.rebase_pending_of(&s, &rn).unwrap().is_none());
    ok(call(&server, "push", json!({"repo":rn})));
    assert_enrolled(&core, &s, &pushed);
}

#[test]
fn chain_resolve_runs_main_follow_for_existing_sessions() {
    let fx = fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let rn = repo_name("tool");
    let ws = core.store.session(&s).unwrap().unwrap().manifest.repos[&rn].clone();
    let git = &core.gits[&rn];
    let fork = git.commit_tree(&ws.base.tree, &ws.head, "fork\n").unwrap();
    git.ref_transaction(&[
        strata_gitio::RefUpdate::set(
            "refs/strata/publish/main",
            fork,
            git.read_ref("refs/strata/publish/main").unwrap(),
        ),
        strata_gitio::RefUpdate::set(
            "refs/strata/anchor/main",
            ws.head,
            git.read_ref("refs/strata/anchor/main").unwrap(),
        ),
    ])
    .unwrap();
    let head = advance(&fx, &core);
    assert!(core.store.rebase_pending_of(&s, &rn).unwrap().is_none());
    ok(call(
        &server,
        "chain_resolve",
        json!({"repo":rn,"choice":"AcceptOriginDrop"}),
    ));
    assert_enrolled(&core, &s, &head);
}

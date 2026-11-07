//! Handler-level sibling/deferred-push regressions from auto-close §10 row 15.
//! Each fixture owns temporary repositories; no background T2 is allowed to
//! erase a deliberately deferred target before the handler can judge it.
mod support;

use serde_json::{Value, json};
use std::fs;
use std::time::Duration;
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_store::{Lifecycle, PendingSource, PublishPhase};
use strata_types::{MarkByKind, SessionId};
use support::close::{self, call, mark_of, ok};
use support::*;

fn fixture(split: bool, push: bool) -> Dfx {
    let mut fx = close::opted_in(&["front", "tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    // Row 15 is about landing proofs, not dependency installation. Keep the
    // sibling publish interleavings independent of fake-bun version timeouts
    // when the other slices run subprocess-heavy tests on this same machine.
    for cfg in fx.config.repos.values_mut() {
        cfg.deps_dirs.clear();
        cfg.seed_caches.clear();
        cfg.install_inputs.clear();
        cfg.install = None;
    }

    if split {
        let main = fx.main("front");
        fx.sb.git(&main, &["checkout", "-q", "-b", "release"]);
        fx.sb.commit(&main, &[("REL", b"release\n")], "release tip");
        fx.sb.git(&main, &["push", "-q", "origin", "release"]);
        fx.sb.git(&main, &["checkout", "-q", "main"]);
    }
    let cfg = fx.config.repos.get_mut(&repo_name("front")).unwrap();
    cfg.publish.push = push;
    cfg.rebase.quiet_secs = 86400;
    if split {
        cfg.publish.branch = "release".into();
        cfg.deps_dirs.clear();
        cfg.seed_caches.clear();
        cfg.install_inputs.clear();
        cfg.install = None;
    }
    fx.write_config();
    fx
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn assert_pins(core: &Core, s: &SessionId, present: bool) {
    for repo in ["front", "tool"] {
        assert_eq!(
            core.gits[&repo_name(repo)]
                .read_ref(&format!("refs/strata/pins/{s}"))
                .unwrap()
                .is_some(),
            present,
            "{s}: {repo} pin"
        );
    }
}

fn assert_active(core: &Core, s: &SessionId) {
    assert_eq!(
        core.store.session(s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    assert_pins(core, s, true);
    assert_eq!(mark_of(core, s).unwrap().by.kind, MarkByKind::Daemon);
}

fn assert_closed(core: &Core, out: &Value, s: &SessionId) {
    assert_eq!(entry(out, s)["closed"], true, "{out}");
    assert_eq!(
        core.store.session(s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert_pins(core, s, false);
}

fn write_publish(
    server: &Server<'_, '_>,
    core: &Core,
    s: &SessionId,
    repo: &str,
    path: &str,
) -> Value {
    fs::write(ws(core, s, repo).join(path), format!("{s} {path}\n")).unwrap();
    ok(close::publish(server, core, s, repo, &[path]))
}

fn restore(core: &Core, s: &SessionId) {
    // The publish was deferred; the workspace still has base B. Remove the
    // just-published addition to make its overlay empty without consuming C.
    fs::remove_file(ws(core, s, "front").join("new.txt")).unwrap();
}

fn pending_head(core: &Core, s: &SessionId) -> String {
    core.store
        .rebase_pending_of(s, &repo_name("front"))
        .unwrap()
        .unwrap()
        .head
        .unwrap()
        .as_str()
        .to_owned()
}

fn push_front(server: &Server<'_, '_>) -> Value {
    ok(call(server, "push", json!({"repo": "front"})))
}

fn assert_publish_hold(out: &Value, s: &SessionId, commit: &str, split: bool) {
    let e = entry(out, s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(
        e["reasons"],
        json!([if split { "not_merged" } else { "not_pushed" }]),
        "{out}"
    );
    let detail = &e["detail"];
    assert_eq!(detail["repo"], "front", "{out}");
    assert_eq!(detail["what"], "publish", "{out}");
    assert_eq!(detail["commit"], commit, "{out}");
    if split {
        assert_eq!(detail["publish_branch"], "release", "{out}");
        assert_eq!(detail["default_branch"], "main", "{out}");
        assert_eq!(detail["ref"], "refs/strata/remotes/origin/main", "{out}");
    } else {
        assert_eq!(detail["on_chain"], true, "{out}");
        assert_eq!(detail["chain"], "leads", "{out}");
    }
}

fn earlier_sibling(split: bool, rejudge: bool) {
    let fx = fixture(split, split);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let t = close::new_only(&server, &["tool"]);
    let main_ref = "refs/strata/remotes/origin/main";
    let before = core.gits[&repo_name("front")].read_ref(main_ref).unwrap();
    let first = write_publish(&server, &core, &s, "front", "new.txt");
    let commit = first["published"][0]["commit"].as_str().unwrap().to_owned();
    if split {
        assert_eq!(first["published"][0]["push"]["kind"], "Pushed", "{first}");
    } else {
        assert!(first["published"][0]["push"].is_null(), "{first}");
    }
    let report = &first["published"][0]["rebases"][0]["report"];
    assert_eq!(report["kind"], "Deferred", "{first}");
    assert_publish_hold(&first, &s, &commit, split);
    assert_eq!(
        entry(&first, &s)["detail"]["pending"]["source"],
        "publish",
        "{first}"
    );
    assert_eq!(
        entry(&first, &s)["detail"]["pending"]["reason"],
        "recent_writes",
        "{first}"
    );
    restore(&core, &s);
    let publisher = if rejudge { &t } else { &s };
    let out = write_publish(&server, &core, publisher, "tool", "x.txt");
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    assert_publish_hold(&out, &s, &commit, split);
    assert_eq!(
        out["closed"], rejudge,
        "only the publishing group controls top-level closed: {out}"
    );
    if rejudge {
        assert_eq!(out["auto_close"]["rejudged"], json!([s]), "{out}");
        assert_eq!(entry(&out, &s)["in_group"], false, "{out}");
    }
    assert_active(&core, &s);
    assert_eq!(pending_head(&core, &s), commit);
    assert_eq!(
        core.gits[&repo_name("front")].read_ref(main_ref).unwrap(),
        before
    );
    if split {
        // Land release without a second front publish: a later unrelated
        // tool publish must rejudge the daemon mark and close the sibling.
        let main = fx.main("front");
        fx.sb.git(&main, &["fetch", "origin", "release"]);
        fx.sb.git(&main, &["merge", "--no-edit", "origin/release"]);
        fx.sb.git(&main, &["push", "-q", "origin", "main"]);
        core.gits[&repo_name("front")]
            .fetch("origin", "main")
            .unwrap();
        let u = close::new_only(&server, &["tool"]);
        let landed = write_publish(&server, &core, &u, "tool", "landed.txt");
        assert_closed(&core, &landed, &s);
    } else {
        let pushed = push_front(&server);
        assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
        assert_eq!(pushed["closed"], true, "{pushed}");
        assert_closed(&core, &pushed, &s);
    }
}

#[test]
fn earlier_deferred_push_false_sibling_holds_the_close_on_own_publish() {
    earlier_sibling(false, false);
}
#[test]
fn earlier_deferred_push_false_sibling_holds_the_close_on_a_rejudge() {
    earlier_sibling(false, true);
}
#[test]
fn earlier_split_branch_sibling_holds_the_close_on_own_publish() {
    earlier_sibling(true, false);
}
#[test]
fn earlier_split_branch_sibling_holds_the_close_on_a_rejudge() {
    earlier_sibling(true, true);
}

#[test]
fn unknown_publish_of_a_sibling_holds_the_close() {
    let fx = fixture(false, true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    fs::write(ws(&core, &s, "front").join("new.txt"), "mine\n").unwrap();
    strata_publish::seams::fail_once(
        &core.gits[&repo_name("front")],
        strata_publish::seams::FailPoint::BeforePush,
    );
    let failed = close::publish(&server, &core, &s, "front", &["new.txt"]);
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(
        mark_of(&core, &s).is_none(),
        "failed publish must not mark done"
    );
    let rows = core.store.publishes().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].phase, PublishPhase::Unknown);
    let commit = pending_head(&core, &s);
    restore(&core, &s);
    let out = write_publish(&server, &core, &s, "tool", "x.txt");
    assert_publish_hold(&out, &s, &commit, false);
    assert_eq!(entry(&out, &s)["detail"]["phase"], "unknown", "{out}");
    assert_eq!(
        entry(&out, &s)["detail"]["pending"]["reason"],
        "publish_unfinished",
        "{out}"
    );
    assert_active(&core, &s);
    let pushed = push_front(&server);
    assert_closed(&core, &pushed, &s);
    assert_eq!(
        core.store
            .publishes()
            .unwrap()
            .iter()
            .find(|r| r.group.id == rows[0].group.id)
            .unwrap()
            .phase,
        PublishPhase::Unknown
    );
}

// Advance the sandbox origin independently of the private publish chain.
fn diverge_front(fx: &Dfx, core: &Core) {
    let main = fx.main("front");
    fx.sb.commit(
        &main,
        &[("origin-only.txt", b"origin change\n")],
        "origin O",
    );
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo_name("front")]
        .fetch("origin", "main")
        .unwrap();
}

#[test]
fn a_record_sync_consumed_does_not_hide_an_unlanded_publish() {
    let fx = fixture(false, false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let first = write_publish(&server, &core, &s, "front", "new.txt");
    let c = first["published"][0]["commit"].as_str().unwrap();
    diverge_front(&fx, &core);
    restore(&core, &s);
    let synced = ok(call(
        &server,
        "sync",
        json!({"session": s, "repos": ["front"], "fetch": false}),
    ));
    assert_eq!(synced["repos"]["front"]["result"], "moved", "{synced}");
    assert!(
        core.store
            .rebase_pending_of(&s, &repo_name("front"))
            .unwrap()
            .is_none()
    );
    let out = write_publish(&server, &core, &s, "tool", "x.txt");
    let detail = &entry(&out, &s)["detail"];
    assert_eq!(entry(&out, &s)["reasons"], json!(["not_pushed"]), "{out}");
    assert_eq!(detail["what"], "publish", "{out}");
    assert_eq!(detail["commit"], c, "{out}");
    assert!(detail.get("pending").is_none(), "{out}");
    assert_eq!(detail["on_chain"], true, "{out}");
    assert_eq!(detail["chain"], "diverged", "{out}");
    assert_active(&core, &s);
    let pushed = push_front(&server);
    assert_eq!(pushed["replayed"]["commits"], 1, "{pushed}");
    assert_closed(&core, &pushed, &s);
}

#[test]
fn replay_adopted_target_holds_the_close_until_it_lands() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture(false, false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let first = write_publish(&server, &core, &s, "front", "new.txt");
    let c = first["published"][0]["commit"].as_str().unwrap();
    diverge_front(&fx, &core);
    let hook = fx.sb.path("front-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let failed = call(&server, "push", json!({"repo": "front"}));
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "PushRefused", "{failed}");
    assert!(
        failed["error"]["detail"].get("replayed").is_some(),
        "{failed}"
    );
    let pending = core
        .store
        .rebase_pending_of(&s, &repo_name("front"))
        .unwrap()
        .unwrap();
    let copy = pending.head.unwrap();
    assert_ne!(copy.as_str(), c);
    assert_eq!(pending.source, PendingSource::Replay);
    assert!(pending.unpushed);
    restore(&core, &s);
    let out = write_publish(&server, &core, &s, "tool", "x.txt");
    assert_publish_hold(&out, &s, c, false);
    let detail = &entry(&out, &s)["detail"];
    assert_eq!(detail["copy"], copy.as_str(), "{out}");
    assert_eq!(detail["pending"]["source"], "replay", "{out}");
    assert_eq!(detail["pending"]["unpushed"], true, "{out}");
    assert_eq!(pending_head(&core, &s), copy.as_str());
    assert_active(&core, &s);
    fs::remove_file(hook).unwrap();
    let pushed = push_front(&server);
    assert_closed(&core, &pushed, &s);
}

#[test]
fn chain_drop_names_the_agents_rm() {
    let fx = fixture(false, false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let first = write_publish(&server, &core, &s, "front", "new.txt");
    let c = first["published"][0]["commit"].as_str().unwrap();
    diverge_front(&fx, &core);
    restore(&core, &s);
    ok(call(
        &server,
        "chain_resolve",
        json!({"repo": "front", "choice": "AcceptOriginDrop"}),
    ));
    let out = write_publish(&server, &core, &s, "tool", "x.txt");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["not_pushed"]), "{out}");
    assert_eq!(e["detail"]["commit"], c, "{out}");
    assert_eq!(e["detail"]["on_chain"], false, "{out}");
    assert_eq!(e["detail"]["chain"], "equal", "{out}");
    assert_active(&core, &s);
    let note = mark_of(&core, &s).unwrap().note.unwrap();
    assert!(
        note.contains(&format!("git-strata rm --session {s}")),
        "{note}"
    );
    ok(call(&server, "rm", json!({"session": s, "discard": false})));
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert_pins(&core, &s, false);
}

#[test]
fn main_target_on_an_unpushed_chain_holds_the_session() {
    let fx = fixture(false, false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let u = close::new_only(&server, &["front"]);
    let first = write_publish(&server, &core, &u, "front", "new.txt");
    let c = first["published"][0]["commit"].as_str().unwrap();
    let pending = core
        .store
        .rebase_pending_of(&s, &repo_name("front"))
        .unwrap()
        .unwrap();
    assert_eq!(pending.source, PendingSource::Main);
    assert_eq!(pending.head.as_ref().unwrap().as_str(), c);
    let out = write_publish(&server, &core, &s, "tool", "x.txt");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["not_pushed"]), "{out}");
    assert_eq!(e["detail"]["repo"], "front", "{out}");
    assert_eq!(e["detail"]["what"], "pending", "{out}");
    assert_eq!(e["detail"]["pending"]["source"], "main", "{out}");
    assert_eq!(e["detail"]["head"], c, "{out}");
    assert_active(&core, &s);
    let pushed = push_front(&server);
    assert_closed(&core, &pushed, &s);
    assert_eq!(entry(&pushed, &u)["closed"], true, "{pushed}");
    assert_eq!(
        core.store.session(&u).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
}

#[test]
fn group_mate_is_not_held_by_a_commit_it_did_not_publish() {
    let fx = fixture(false, false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let u = close::new_only(&server, &["front"]);
    fs::write(ws(&core, &u, "front").join("new.txt"), "front\n").unwrap();
    fs::write(ws(&core, &s, "tool").join("x.txt"), "tool\n").unwrap();
    let front = checkpoint(&core, &u, "front");
    let tool = checkpoint(&core, &s, "tool");
    let out = ok(call(
        &server,
        "publish",
        json!({"group": group(vec![
        batch(&u, "front", front.id, &["new.txt"]), batch(&s, "tool", tool.id, &["x.txt"])
    ]), "shown": {}}),
    ));
    assert_eq!(out["closed"], false, "{out}");
    assert_closed(&core, &out, &s);
    assert_eq!(entry(&out, &u)["reasons"], json!(["not_pushed"]), "{out}");
    assert_eq!(
        core.store.session(&u).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
}

#[test]
fn landed_replay_with_a_stale_row_refuses_until_t2() {
    use strata_confirm::seams::Verdict;
    let mut fx = fixture(false, false);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .publish
        .confirm = true;
    fx.write_config();
    let (confirmer, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let first = write_publish(&server, &core, &s, "front", "new.txt");
    let c = first["published"][0]["commit"].as_str().unwrap();
    assert_eq!(pending_head(&core, &s), c);
    assert!(log.shown().is_empty());
    diverge_front(&fx, &core);
    ok(call(
        &server,
        "chain_resolve",
        json!({"repo": "front", "choice": "AcceptOriginReplay"}),
    ));
    restore(&core, &s);
    let shown = core.gits[&repo_name("front")]
        .read_ref(&strata_publish::chain_ref("main"))
        .unwrap()
        .unwrap();
    assert_ne!(shown.as_str(), c);
    let pushed = ok(call(
        &server,
        "push",
        json!({"repo": "front", "shown": shown}),
    ));
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert_eq!(log.shown().len(), 1);
    let e = entry(&pushed, &s);
    assert_eq!(e["closed"], false, "{pushed}");
    assert_eq!(e["reasons"], json!(["not_pushed"]), "{pushed}");
    assert_eq!(e["detail"]["what"], "pending", "{pushed}");
    assert_eq!(e["detail"]["head"], c, "{pushed}");
    assert_eq!(e["detail"]["on_chain"], false, "{pushed}");
    assert_eq!(pending_head(&core, &s), c);
    assert_active(&core, &s);
    let pass = strata_daemon::handlers::catch_up_pass(
        &d,
        std::time::SystemTime::now() + Duration::from_secs(86401),
    );
    let after = core
        .store
        .rebase_pending_of(&s, &repo_name("front"))
        .unwrap();
    // T2 reconstruct discards stale C, then main-follow can enroll the landed
    // C′ again during the same pass. Prove the stale target is gone, rather
    // than forbidding that valid replacement row.
    if let Some(after) = after {
        assert_ne!(
            after.head.as_deref(),
            Some(c),
            "T2: {pass:?}; pending: {after:?}"
        );
        assert_eq!(
            after.head.as_deref(),
            Some(shown.as_str()),
            "T2: {pass:?}; pending: {after:?}"
        );
        assert_eq!(after.source, PendingSource::Main);
        assert!(!after.unpushed);
    }
    let t = close::new_only(&server, &["tool"]);
    let out = write_publish(&server, &core, &t, "tool", "t.txt");
    assert_closed(&core, &out, &s);
}

// Acknowledged cwd holder with RAII cleanup, including on assertion failure.
struct Worker(std::process::Child);
impl Worker {
    fn in_dir(path: &std::path::Path) -> Self {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf 'ready\\n'; exec /bin/sleep 300"])
            .current_dir(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        Self(child)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn landed_replay_on_a_moved_session_refuses_on_its_old_head() {
    let mut fx = fixture(false, false);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .rebase
        .quiet_secs = 0;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front", "tool"]);
    let first = write_publish(&server, &core, &s, "front", "new.txt");
    let c = first["published"][0]["commit"].as_str().unwrap();
    assert_eq!(
        first["published"][0]["rebases"][0]["report"]["kind"], "Moved",
        "{first}"
    );
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name("front")]
            .head
            .as_str(),
        c
    );
    let holder = Worker::in_dir(&ws(&core, &s, "front"));
    diverge_front(&fx, &core);
    let pushed = push_front(&server);
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert_eq!(pushed["replayed"]["commits"], 1, "{pushed}");
    let pending = core
        .store
        .rebase_pending_of(&s, &repo_name("front"))
        .unwrap()
        .unwrap();
    assert_eq!(pending.source, PendingSource::Replay);
    assert_eq!(pending.reason, "open_files");
    let copy = pending.head.unwrap();
    assert_ne!(copy.as_str(), c);
    let e = entry(&pushed, &s);
    assert_eq!(e["closed"], false, "{pushed}");
    assert_eq!(e["reasons"], json!(["not_pushed", "ahead"]), "{pushed}");
    assert_eq!(e["detail"]["what"], "base", "{pushed}");
    assert_eq!(e["detail"]["head"], c, "{pushed}");
    assert_eq!(e["detail"]["on_chain"], false, "{pushed}");
    assert_active(&core, &s);
    drop(holder);
    strata_daemon::handlers::catch_up_pass(
        &d,
        std::time::SystemTime::now() + Duration::from_secs(86401),
    );
    assert!(
        core.store
            .rebase_pending_of(&s, &repo_name("front"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name("front")]
            .head
            .as_str(),
        copy.as_str()
    );
    ok(call(&server, "rm", json!({"session": s, "discard": false})));
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert_pins(&core, &s, false);
}

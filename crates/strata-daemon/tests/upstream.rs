//! Main-sync §5: isolated repositories/stores and in-process handlers only.
mod support;
use serde_json::{Value, json};
use std::fs;
use std::time::SystemTime;
use strata_daemon::{
    handlers::{Server, Trackers},
    rpc::{Method, Params, Request},
    startup::{Core, Daemon},
    upstream::ReadMode,
};
use strata_types::{CommitOid, SessionId, Upstream};
use support::*;

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
    serde_json::from_value(ok(call(server, "new", json!({"ticket":null,"repos":["tool"]})))["session"]["manifest"]["id"].clone()).unwrap()
}
fn check(server: &Server<'_, '_>, s: &SessionId, hook: bool) -> Upstream {
    serde_json::from_value(
        ok(call(
            server,
            "sync",
            json!({"session":s,"check":true,"hook":hook}),
        ))["upstream"]
            .clone(),
    )
    .unwrap()
}
fn check_with_summary(server: &Server<'_, '_>, s: &SessionId, hook: bool) -> Upstream {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let up = check(server, s, hook);
        if up.repos[&repo_name("tool")]
            .deps
            .as_ref()
            .is_some_and(|d| d.summary.is_some())
        {
            return up;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "summary stayed cold: {up:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
fn advance(fx: &Dfx, core: &Core, message: &str) -> CommitOid {
    let main = fx.main("tool");
    let head = CommitOid::try_from(fx.sb.commit(&main, &[], message)).unwrap();
    let git = &core.gits[&repo_name("tool")];
    let name = strata_gitio::remote_ref("origin", "main");
    git.ref_transaction(&[strata_gitio::RefUpdate::set(
        &name,
        head.clone(),
        git.read_ref(&name).unwrap(),
    )])
    .unwrap();
    head
}

#[test]
fn behind_capped_at_1000() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let git = &core.gits[&repo_name("tool")];
    let row = core.store.session(&s).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo_name("tool")];
    let mut head = ws.head.clone();
    for i in 0..1002 {
        head = git
            .commit_tree(&ws.base.tree, &head, &format!("commit {i}\n"))
            .unwrap();
    }
    let name = strata_gitio::remote_ref("origin", "main");
    git.ref_transaction(&[strata_gitio::RefUpdate::set(
        &name,
        head,
        git.read_ref(&name).unwrap(),
    )])
    .unwrap();
    let up = check(&server, &s, false);
    let r = &up.repos[&repo_name("tool")];
    assert_eq!(r.behind, Some(1000));
    assert!(r.capped);
    assert!(!r.diverged);
    assert_eq!(r.commits.len(), 20);
    assert!(!up.clean);
}

#[test]
fn diverged_head_not_enrolled_and_reported_with_base_only() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let original = fx.sb.head(&fx.main("tool"));
    advance(&fx, &core, "base-only");
    let s = new(&server);
    // new fetches origin, so give the session a branch through origin first.
    let git = &core.gits[&repo_name("tool")];
    let mut row = core.store.session(&s).unwrap().unwrap();
    let ws = row.manifest.repos.get_mut(&repo_name("tool")).unwrap();
    ws.head = git
        .commit_tree(
            &ws.base.tree,
            &CommitOid::try_from(original.clone()).unwrap(),
            "ours\n",
        )
        .unwrap();
    let ours = ws.head.clone();
    let tip = git
        .commit_tree(
            &ws.base.tree,
            &CommitOid::try_from(original).unwrap(),
            "theirs\n",
        )
        .unwrap();
    let name = strata_gitio::remote_ref("origin", "main");
    git.ref_transaction(&[strata_gitio::RefUpdate::set(
        &name,
        tip,
        git.read_ref(&name).unwrap(),
    )])
    .unwrap();
    let up = core
        .upstream
        .session(&core, &row, ReadMode::Full, |_, _, _| Default::default())
        .unwrap();
    let r = &up.repos[&repo_name("tool")];
    assert!(r.diverged);
    assert_eq!(r.behind, Some(1));
    assert_eq!(r.base_only, vec![ours]);
    assert!(core.store.rebase_pending(Some(&s)).unwrap().is_empty());
}

#[test]
fn ahead_only_is_not_diverged() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let mut row = core.store.session(&s).unwrap().unwrap();
    let ws = row.manifest.repos.get_mut(&repo_name("tool")).unwrap();
    ws.head = core.gits[&repo_name("tool")]
        .commit_tree(&ws.base.tree, &ws.head, "ahead\n")
        .unwrap();
    let up = core
        .upstream
        .session(&core, &row, ReadMode::Full, |_, _, _| Default::default())
        .unwrap();
    let r = &up.repos[&repo_name("tool")];
    assert!(!r.diverged);
    assert_eq!(r.behind, Some(0));
    assert!(up.clean);
}

#[test]
fn upstream_clean_definition() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let up = check(&server, &s, false);
    assert!(up.clean);
    assert!(up.as_of.is_some());
    let mut repos = up.repos.clone();
    repos.get_mut(&repo_name("tool")).unwrap().unpublished = Some(true);
    assert!(strata_daemon::upstream::is_clean(&repos));
    repos.get_mut(&repo_name("tool")).unwrap().behind = None;
    assert!(!strata_daemon::upstream::is_clean(&repos));
    let mut repos = up.repos;
    repos.get_mut(&repo_name("tool")).unwrap().conflicts = vec![rp("a")];
    assert!(!strata_daemon::upstream::is_clean(&repos));
}

#[test]
fn ls_reads_cache_only() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let git = &core.gits[&repo_name("tool")];
    git.take_git_calls();
    for workspaces in [false, true] {
        let data = ok(call(
            &server,
            "status",
            json!({"cwd":null,"session":s,"all":false,"summary":true,"workspaces":workspaces}),
        ));
        assert_eq!(
            data["sessions"][0]["behind"]["tool"],
            json!({"behind":null,"unchecked":true})
        );
        if workspaces {
            assert_eq!(data["sessions"][0]["workspaces"][0]["unchecked"], true);
        }
    }
    assert!(git.take_git_calls().is_empty());
    advance(&fx, &core, "other");
    core.upstream.warm(&core);
    git.take_git_calls();
    let data = ok(call(
        &server,
        "status",
        json!({"cwd":null,"session":s,"all":false,"summary":true}),
    ));
    assert_eq!(data["sessions"][0]["behind"]["tool"]["behind"], 1);
    let hook = check(&server, &s, true);
    assert_eq!(hook.repos[&repo_name("tool")].behind, Some(1));
    assert!(git.take_git_calls().is_empty());
}

#[test]
fn sync_check_never_fetches_or_writes() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    advance(&fx, &core, "upstream");
    let git = &core.gits[&repo_name("tool")];
    git.take_git_calls();
    let before = core.store.sync_counts();
    let row = core.store.session(&s).unwrap();
    core.store.arm(strata_store::Seam::CommitSyncFails);
    assert_eq!(
        check(&server, &s, false).repos[&repo_name("tool")].behind,
        Some(1)
    );
    assert_eq!(core.store.sync_counts(), before);
    assert_eq!(core.store.session(&s).unwrap(), row);
    assert!(core.store.rebase_pending(Some(&s)).unwrap().is_empty());
    assert!(
        git.take_git_calls()
            .iter()
            .all(|c| !c.args.iter().any(|a| a == "fetch"))
    );
}

#[test]
fn unpublished_cold_reads_true() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    checkpoint(&core, &s, "tool");
    assert_eq!(
        check(&server, &s, false).repos[&repo_name("tool")].unpublished,
        Some(true)
    );
}

#[test]
fn unpublished_true_after_empty_checkpoint_then_edit_and_background_reconcile() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    checkpoint(&core, &s, "tool");
    trackers.reconcile_once();
    // An unavailable FSEvents service is conservatively unpublished too;
    // the deterministic Tracker test covers the clean-to-dirty transition.
    assert!(
        core.store
            .latest_checkpoint(&s, &repo_name("tool"))
            .unwrap()
            .unwrap()
            .entries
            .is_empty()
    );
    fs::write(ws(&core, &s, "tool").join("src/index.ts"), "changed\n").unwrap();
    trackers.reconcile_once();
    assert_eq!(
        check(&server, &s, true).repos[&repo_name("tool")].unpublished,
        Some(true)
    );
    trackers.clear();
}

#[test]
fn unpublished_from_tracker_no_walk() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    checkpoint(&core, &s, "tool");
    trackers.reconcile_once();
    check(&server, &s, false);
    let git = &core.gits[&repo_name("tool")];
    git.take_git_calls();
    let count = core.store.sync_counts();
    check(&server, &s, true);
    assert!(git.take_git_calls().is_empty());
    assert_eq!(count, core.store.sync_counts());
    trackers.clear();
}

#[test]
fn commits_use_publish_trailer_and_are_capped() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    advance(
        &fx,
        &core,
        &format!("own\n\nsessions: s-deadbeef, {s}\npaths: 1\n"),
    );
    advance(&fx, &core, "other");
    let up = check(&server, &s, false);
    let commits = &up.repos[&repo_name("tool")].commits;
    assert_eq!(commits.len(), 2);
    assert!(!commits[0].own);
    assert!(commits[1].own);
    assert_eq!(commits[1].author, "Strata Test");
}

#[test]
fn rm_upstream_captured_before_teardown() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    advance(&fx, &core, "upstream");
    let out = ok(call(&server, "rm", json!({"session":s,"discard":false})));
    assert_eq!(out["upstream"]["repos"]["tool"]["behind"], 1);
    assert_eq!(out["upstream"]["repos"]["tool"]["unpublished"], false);
    assert_eq!(out["upstream"]["as_of_rm"], "before");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        strata_store::Lifecycle::Removed
    );
}

#[test]
fn rm_dirty_detail_upstream() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    advance(&fx, &core, "upstream");
    fs::write(ws(&core, &s, "tool").join("dirty.txt"), "mine").unwrap();
    let out = call(&server, "rm", json!({"session":s,"discard":false}));
    assert_eq!(out["error"]["code"], "Dirty");
    assert_eq!(
        out["error"]["detail"]["upstream"]["repos"]["tool"]["behind"],
        1
    );
}

#[test]
fn publish_reply_upstream_after_rebase() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let out = ok(call(
        &server,
        "publish",
        json!({"group":group(vec![batch(&s,"tool",cp.id,&["new.txt"])]),"shown":{}}),
    ));
    assert_eq!(
        out["upstream"][s.as_str()]["repos"]["tool"]["base_head"],
        out["published"][0]["commit"]
    );
    assert_eq!(out["upstream"][s.as_str()]["repos"]["tool"]["behind"], 0);
}

#[test]
fn publish_upstream_keyed_by_session_multi_session_group() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let a = new(&server);
    let b = new(&server);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a").unwrap();
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b").unwrap();
    let acp = checkpoint(&core, &a, "tool");
    let bcp = checkpoint(&core, &b, "tool");
    let out = ok(call(
        &server,
        "publish",
        json!({"group":group(vec![batch(&a,"tool",acp.id,&["a.txt"]),batch(&b,"tool",bcp.id,&["b.txt"])]),"shown":{}}),
    ));
    for s in [&a, &b] {
        assert_eq!(out["upstream"][s.as_str()]["repos"]["tool"]["behind"], 0);
    }
    assert_eq!(out["upstream"].as_object().unwrap().len(), 2);
}

#[test]
fn fetch_failure_retains_success_as_of() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let before = check(&server, &s, false).as_of;
    core.store
        .record_fetch(
            &repo_name("tool"),
            false,
            Some("offline"),
            SystemTime::now(),
        )
        .unwrap();
    assert!(before.is_some());
    assert_eq!(check(&server, &s, false).as_of, before);
}

#[test]
fn upstream_failure_does_not_change_outcome() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    core.upstream.fail_for_test(true);
    let dirty = call(&server, "rm", json!({"session":s,"discard":false}));
    assert_eq!(dirty["error"]["code"], "Dirty");
    assert!(dirty["error"]["detail"].get("upstream").is_none());
    let published = ok(call(
        &server,
        "publish",
        json!({"group":group(vec![batch(&s,"tool",cp.id,&["new.txt"])]),"shown":{}}),
    ));
    assert!(published.get("upstream").is_none());
    assert!(published["published"][0]["commit"].is_string());
    let removed = ok(call(&server, "rm", json!({"session":s,"discard":false})));
    assert!(removed.get("upstream").is_none());
}

#[test]
fn unpublished_true_after_failed_publish() {
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().checks = vec!["exit 1".into()];
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let result = call(
        &server,
        "publish",
        json!({"group":group(vec![batch(&s,"tool",cp.id,&["new.txt"])]),"shown":{}}),
    );
    assert_eq!(result["ok"], false);
    assert!(
        core.store
            .publishes()
            .unwrap()
            .iter()
            .any(|r| r.phase == strata_store::PublishPhase::Done)
    );
    trackers.reconcile_once();
    assert_eq!(
        check(&server, &s, true).repos[&repo_name("tool")].unpublished,
        Some(true)
    );
    trackers.clear();
}

#[test]
fn unpublished_true_after_partial_publish() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    for p in ["a.txt", "b.txt"] {
        fs::write(ws(&core, &s, "tool").join(p), p).unwrap();
    }
    let cp = checkpoint(&core, &s, "tool");
    ok(call(
        &server,
        "publish",
        json!({"group":group(vec![batch(&s,"tool",cp.id,&["a.txt"])]),"shown":{}}),
    ));
    trackers.reconcile_once();
    assert_eq!(
        check(&server, &s, true).repos[&repo_name("tool")].unpublished,
        Some(true)
    );
    trackers.clear();
}

#[test]
fn status_and_t2_warm_upstream() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let target = advance(&fx, &core, "main");
    strata_daemon::handlers::catch_up_pass(&d, SystemTime::now());
    // The follow part may move this idle workspace during T2. Judge its
    // resulting base, and read the cache BEFORE full status can fill it.
    let after = core.store.session(&s).unwrap().unwrap();
    let expected = u32::from(after.manifest.repos[&repo_name("tool")].head != target);
    let cached = check(&server, &s, true);
    assert!(!cached.repos[&repo_name("tool")].unchecked);
    assert_eq!(cached.repos[&repo_name("tool")].behind, Some(expected));
    let out = ok(call(
        &server,
        "status",
        json!({"session":s,"cwd":null,"all":false}),
    ));
    assert_eq!(
        out["sessions"][0]["status"]["upstream"]["repos"]["tool"]["behind"],
        expected
    );
    assert_eq!(
        out["sessions"][0]["status"]["workspaces"]["tool"]["upstream"]["behind"],
        expected
    );
    assert!(strata_daemon::FEATURES.contains(&"status.upstream"));
}

#[test]
fn features_lists_sync() {
    assert!(strata_daemon::FEATURES.contains(&"sync"));
}

#[test]
fn deps_and_auto_fields_follow_store_and_cached_summary() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let repo = repo_name("tool");
    let row = core.store.session(&s).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    let set = core
        .store
        .deps_set(&repo, &ws.base.deps)
        .unwrap()
        .unwrap()
        .set;
    let target = advance(&fx, &core, "deps update");
    let tree = core.gits[&repo].tree_of(&target).unwrap();
    let p = core
        .store
        .record_proposal(
            &strata_store::NewProposal {
                repo: repo.clone(),
                origin: None,
                from: ws.base.deps.clone(),
                from_inputs: set.inputs_hash,
                from_head: Some(ws.head.clone()),
                to: strata_types::DepsSetId::try_from("b".repeat(64)).unwrap(),
                to_inputs: "b".repeat(64),
                tree: Some(tree.clone()),
                head: Some(target.clone()),
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap();
    core.store
        .enroll_main(&s, &repo, &ws.base, &target, &tree, None, SystemTime::now())
        .unwrap();
    let pending = core.store.rebase_pending_of(&s, &repo).unwrap().unwrap();
    core.store
        .defer_pending(
            &s,
            &repo,
            pending.rev,
            "conflicts",
            &json!({"paths":["src/a"]}),
            SystemTime::now(),
        )
        .unwrap();
    for (state, want) in [
        (strata_store::DepsAutoState::Installing, "installing"),
        (strata_store::DepsAutoState::Ready, "ready"),
        (strata_store::DepsAutoState::InstallFailed, "install_failed"),
    ] {
        core.store
            .set_deps_auto(p, state, None, SystemTime::now())
            .unwrap();
        let up = check_with_summary(&server, &s, false);
        let r = &up.repos[&repo];
        assert_eq!(r.deps.as_ref().unwrap().state, want);
        assert_eq!(r.deps.as_ref().unwrap().mode, strata_types::SyncDeps::Apply);
        assert!(r.deps.as_ref().unwrap().summary.is_some());
        assert!(!up.clean);
        let auto = r.auto.as_ref().unwrap();
        assert_eq!(auto.source, "main");
        assert_eq!(auto.state, "deferred");
        assert_eq!(auto.reason.as_deref(), Some("conflicts"));
        assert_eq!(auto.attempts, 1);
        assert_eq!(auto.target.as_ref().unwrap().head, target);
        assert_eq!(auto.detail.as_ref().unwrap()["paths"], json!(["src/a"]));
    }
    core.gits[&repo].take_git_calls();
    let up = check_with_summary(&server, &s, true);
    assert!(up.repos[&repo].deps.as_ref().unwrap().summary.is_some());
    assert!(core.gits[&repo].take_git_calls().is_empty());

    // Fail only the summary's tree reads; graph and store metadata stay available.
    use std::os::unix::fs::PermissionsExt;
    let git_wrapper = fx.sb.bin.join("git");
    fs::write(&git_wrapper, "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = ls-tree ]; then exit 1; fi\ndone\nexec /usr/bin/git \"$@\"\n").unwrap();
    fs::set_permissions(&git_wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(fx.sb.bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    core.gits[&repo].set_env("PATH", Some(&path));
    let good = up.repos[&repo].deps.as_ref().unwrap().summary.clone();
    let failed = check(&server, &s, false);
    assert_eq!(failed.repos[&repo].deps.as_ref().unwrap().summary, good);
    assert_eq!(
        check(&server, &s, true).repos[&repo]
            .deps
            .as_ref()
            .unwrap()
            .summary,
        good
    );
    // The same retention rule applies when the response deadline wins.
    fs::write(&git_wrapper, "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = ls-tree ]; then /bin/sleep 0.4; fi\ndone\nexec /usr/bin/git \"$@\"\n").unwrap();
    let timed_out = check(&server, &s, false);
    assert_eq!(timed_out.repos[&repo].deps.as_ref().unwrap().summary, good);

    let mut changed = row.clone();
    changed.workspaces.get_mut(&repo).unwrap().generation += 1;
    let cold = core
        .upstream
        .session(&core, &changed, ReadMode::Full, |_, _, _| {
            Default::default()
        })
        .unwrap();
    assert!(
        cold.repos[&repo].deps.as_ref().unwrap().summary.is_none(),
        "a new generation cannot reuse the previous summary"
    );
    let cached_cold = core
        .upstream
        .session(&core, &changed, ReadMode::Cached, |_, _, _| Default::default())
        .unwrap();
    assert!(
        cached_cold.repos[&repo].deps.as_ref().unwrap().summary.is_none(),
        "cached reads cannot reuse a summary for an obsolete key"
    );
    core.gits[&repo].set_env("PATH", None);
    let live = |found: String| strata_daemon::upstream::Live {
        unpublished: true,
        inputs: Some((found, "base inputs".into())),
    };
    let up = core
        .upstream
        .session(&core, &row, ReadMode::Cached, |_, _, _| {
            live("c".repeat(64))
        })
        .unwrap();
    assert_eq!(up.repos[&repo].deps.as_ref().unwrap().state, "drift");
    let up = core
        .upstream
        .session(&core, &row, ReadMode::Cached, |_, _, _| {
            live("b".repeat(64))
        })
        .unwrap();
    assert_eq!(
        up.repos[&repo].deps.as_ref().unwrap().state,
        "install_failed",
        "inputs already at target are not drift"
    );
    core.store
        .record_proposal(
            &strata_store::NewProposal {
                repo: repo.clone(),
                origin: Some(s.clone()),
                from: ws.base.deps.clone(),
                from_inputs: "a".repeat(64),
                from_head: Some(ws.head.clone()),
                to: strata_types::DepsSetId::try_from("d".repeat(64)).unwrap(),
                to_inputs: "d".repeat(64),
                tree: None,
                head: None,
            },
            None,
            SystemTime::now(),
        )
        .unwrap();
    let up = core
        .upstream
        .session(&core, &row, ReadMode::Cached, |_, _, _| {
            live("c".repeat(64))
        })
        .unwrap();
    assert_eq!(up.repos[&repo].deps.as_ref().unwrap().state, "own_change");
}

/// impl-r1 B2: `resolve_move` keeps the scene JSON as history. A resolved
/// scene must not hide the workspace from upstream (or the Stop hook) nor
/// block Agent sync; an unresolved one still does.
#[test]
fn resolved_scene_history_reported_and_syncable() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = new(&server);
    let rn = repo_name("tool");
    let ws = core.store.session(&s).unwrap().unwrap().manifest.repos[&rn].clone();
    let n = core
        .store
        .begin_move(&s, &rn, &ws.base, &ws.head, &ws.path.with_file_name(".scene-move"), SystemTime::now())
        .unwrap();
    core.store
        .set_move_state(
            &s,
            &rn,
            n,
            strata_store::MoveState::Preparing,
            strata_store::MoveState::Swapping,
            Some(&[]),
            SystemTime::now(),
        )
        .unwrap();
    core.store.fail_move(&s, &rn, n, &json!({"failed":"swap"}), SystemTime::now()).unwrap();
    let head = advance(&fx, &core, "upstream after scene");
    assert!(core.store.workspace_scene_live(&s, &rn, true).unwrap());
    assert!(!check(&server, &s, false).repos.contains_key(&rn), "an unresolved scene is the human's");
    core.store.resolve_move(&s, &rn, n, false, SystemTime::now()).unwrap();
    assert!(core.store.session(&s).unwrap().unwrap().workspaces[&rn].scene.is_some());
    assert!(!core.store.workspace_scene_live(&s, &rn, true).unwrap());
    for hook in [false, true] {
        let u = check(&server, &s, hook);
        assert_eq!(u.repos[&rn].behind, Some(1), "hook={hook}");
        assert!(!u.clean, "hook={hook}");
    }
    let v = ok(call(&server, "sync", json!({"session":s,"fetch":false})));
    assert_eq!(v["repos"]["tool"]["result"], "moved", "{v}");
    let now = core.store.session(&s).unwrap().unwrap();
    assert_eq!(now.manifest.repos[&rn].head, head);
}

#[test]
fn add_runs_main_follow_for_added_repos() {
    // T4 after `add`, as after `new`: other sessions on the repo learn of a
    // tracking-ref move the add observed.
    let fx = Dfx::with(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s1 = new(&server);
    let rn = repo_name("tool");
    let s2: SessionId = serde_json::from_value(
        ok(call(&server, "new", json!({"ticket":null,"repos":["front"],"only":true})))["session"]["manifest"]["id"].clone(),
    )
    .unwrap();
    let main = fx.main("tool");
    fx.sb.commit(&main, &[("src/moved.ts", b"moved\n")], "moved");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&rn].fetch("origin", "main").unwrap();
    assert!(core.store.rebase_pending_of(&s1, &rn).unwrap().is_none());
    ok(call(&server, "add", json!({"session":s2,"repos":["tool"]})));
    assert!(
        core.store.rebase_pending_of(&s1, &rn).unwrap().is_some(),
        "add reconciled the added repo"
    );
}

#[test]
fn stuck_rows_warn_and_hand_back_once_then_clear() {
    let fx=Dfx::new(); let core=fx.prepare().unwrap(); let d=Daemon::start(&core).unwrap();
    let trackers=Trackers::new(&core); let server=Server::new(&d,&trackers); let s=new(&server);
    let repo=repo_name("tool"); let rec=core.store.session(&s).unwrap().unwrap(); let w=&rec.manifest.repos[&repo];
    core.store.enroll_main(&s,&repo,&w.base,&w.head,&w.base.tree,None,SystemTime::now()).unwrap();
    let p=core.store.rebase_pending_of(&s,&repo).unwrap().unwrap();
    for _ in 0..3 { core.store.defer_pending(&s,&repo,p.rev,"open_files",&json!({"holders":[{"pid":123,"command":"shell","hold":"cwd","path":"."}],"count":1}),SystemTime::now()).unwrap(); }
    let status=d.daemon_status(false).unwrap();
    assert_eq!(status["warnings"].as_array().unwrap().iter().filter(|w| w.get("AutoRebaseStuck").is_some()).count(),1);
    let first=check(&server,&s,true); let second=check(&server,&s,true);
    assert!(first.repos[&repo].auto.as_ref().unwrap().escalation_new);
    assert!(!second.repos[&repo].auto.as_ref().unwrap().escalation_new);
    assert_eq!(first.repos[&repo].auto.as_ref().unwrap().escalated_at,second.repos[&repo].auto.as_ref().unwrap().escalated_at);
    core.store.set_pending_reason(&s,&repo,p.rev,"waiting_install").unwrap();
    assert!(!d.daemon_status(false).unwrap()["warnings"].as_array().unwrap().iter().any(|w|w.get("AutoRebaseStuck").is_some()));
    core.store.drop_pending(&s,&repo,p.rev).unwrap();
    assert!(check(&server,&s,true).repos[&repo].auto.is_none());
}

#[test]
fn whole_group_base_gate_precedes_every_prompt_and_commit() {
    let mut fx=Dfx::with(&["aaa","tool"]);
    for cfg in fx.config.repos.values_mut() { cfg.publish.confirm=true; cfg.publish.push=true; }
    fx.write_config();
    let (confirmer,log,_)=fx.confirmer(vec![strata_daemon::seams::Verdict::pass()]);
    let core=fx.prepare_with(confirmer).unwrap(); let d=Daemon::start(&core).unwrap(); let trackers=Trackers::new(&core); let server=Server::new(&d,&trackers);
    let first:SessionId=serde_json::from_value(ok(call(&server,"new",json!({"ticket":null,"repos":["aaa"],"only":true})))["session"]["manifest"]["id"].clone()).unwrap();
    let normal=new(&server);
    let main=core.config.repos[&repo_name("tool")].path.clone();
    let local=fx.sb.commit(&main,&[("base-only",b"base")],"parked");
    let parked:SessionId=serde_json::from_value(ok(call(&server,"new",json!({"ticket":null,"repos":["tool"],"only":true,"base":{"tool":local}})))["session"]["manifest"]["id"].clone()).unwrap();
    let mut batches=Vec::new();
    for (id,repo,path) in [(&first,"aaa","first"),(&normal,"tool","normal"),(&parked,"tool","parked")] {
        fs::write(ws(&core,id,repo).join(path),b"edit").unwrap();
        let cp=checkpoint(&core,id,repo);
        batches.push(json!({"session":id,"repo":repo,"checkpoint":cp.id,"paths":[path]}));
    }
    let params=json!({"group":{"id":0,"batches":batches}});
    for method in ["publish_plan","publish"] {
        let out=call(&server,method,params.clone());
        assert_eq!(out["error"]["detail"]["reason"],"base_not_on_main","{out}");
        assert!(out["error"]["detail"]["fix"].as_str().unwrap().starts_with("ask the human"));
    }
    assert!(log.shown().is_empty());
    assert!(core.store.publishes().unwrap().is_empty());
}

#[test]
fn parked_status_sync_and_rm_check_other_session_pins_before_warning() {
    let fx=Dfx::new(); let core=fx.prepare().unwrap(); let d=Daemon::start(&core).unwrap(); let trackers=Trackers::new(&core); let server=Server::new(&d,&trackers);
    let repo=repo_name("tool"); let main=core.config.repos[&repo].path.clone(); let initial=fx.sb.head(&main);
    let c=fx.sb.commit(&main,&[("only-c",b"C")],"local C");
    let make=|| -> SessionId { serde_json::from_value(ok(call(&server,"new",json!({"ticket":null,"repos":["tool"],"only":true,"base":{"tool":c}})))["session"]["manifest"]["id"].clone()).unwrap() };
    let a=make(); let b=make();
    let up=check(&server,&a,false); assert_eq!(up.repos[&repo].ahead,1); assert_eq!(up.repos[&repo].base_only[0].as_str(),c);
    let sync=ok(call(&server,"sync",json!({"session":a,"check":false,"cwd":null})));
    assert!(sync.to_string().contains("up_to_date")); assert!(sync.to_string().contains("\"ahead\":1"));
    fx.sb.git(&main,&["reset","--hard",&initial]);
    let first=ok(call(&server,"rm",json!({"session":a,"discard":false})));
    assert!(first.get("base_rev_unreferenced").is_none(),"the other session pin keeps C reachable: {first}");
    let second=ok(call(&server,"rm",json!({"session":b,"discard":false})));
    assert_eq!(second["base_rev_unreferenced"][0]["oid"],c);
}

#[test]
fn divergent_parked_base_requires_human_drop_before_publish() {
    let fx=Dfx::new(); let core=fx.prepare().unwrap(); let d=Daemon::start(&core).unwrap(); let trackers=Trackers::new(&core); let server=Server::new(&d,&trackers);
    let repo=repo_name("tool"); let main=fx.main("tool"); let initial=fx.sb.head(&main);
    let c=fx.sb.commit(&main,&[("only-c",b"C")],"local C");
    let s:SessionId=serde_json::from_value(ok(call(&server,"new",json!({"ticket":null,"repos":["tool"],"only":true,"base":{"tool":c}})))["session"]["manifest"]["id"].clone()).unwrap();
    fs::write(ws(&core,&s,"tool").join("mine"),b"mine").unwrap();
    fx.sb.git(&main,&["reset","--hard",&initial]);
    fx.sb.commit(&main,&[("only-d",b"D")],"sibling D"); fx.sb.git(&main,&["push","origin","HEAD:main"]); core.gits[&repo].fetch("origin","main").unwrap();
    let cp=checkpoint(&core,&s,"tool");
    let params=json!({"group":{"id":0,"batches":[{"session":s,"repo":"tool","checkpoint":cp.id,"paths":["mine"]}]}});
    let e=call(&server,"publish_plan",params); assert_eq!(e["error"]["detail"]["reason"],"base_not_on_main");
    assert!(e["error"]["detail"]["fix"].as_str().unwrap().contains("recover the publish chain first"));
    let e=call(&server,"sync",json!({"session":s,"check":false,"cwd":null,"fetch":true}));
    assert_eq!(e["error"]["detail"]["reason"],"base_diverged");
    ok(call(&server,"sync",json!({"session":s,"check":false,"cwd":null,"fetch":true,"accept_diverged":true})));
    assert!(!ws(&core,&s,"tool").join("only-c").exists());
    let cp=checkpoint(&core,&s,"tool");
    let out=ok(call(&server,"publish",json!({"group":{"id":0,"batches":[{"session":s,"repo":"tool","checkpoint":cp.id,"paths":["mine"]}]}})));
    assert_eq!(out["published"].as_array().unwrap().len(),1);
}

#[test]
fn publish_plan_fills_cold_split_target_and_refreshes_a_previously_parked_base() {
    let mut fx=Dfx::new(); let repo=repo_name("tool"); let main=fx.main("tool");
    fx.sb.git(&main,&["push","origin","HEAD:release"]);
    let cfg=fx.config.repos.get_mut(&repo).unwrap();
    cfg.deps_dirs.clear(); cfg.install=None; cfg.install_inputs.clear(); cfg.seed_caches.clear();
    cfg.publish.branch="release".into(); cfg.publish.push=false; fx.write_config();
    let core=fx.prepare().unwrap(); let d=Daemon::start(&core).unwrap(); let trackers=Trackers::new(&core); let server=Server::new(&d,&trackers);
    let normal=new(&server);
    fs::write(ws(&core,&normal,"tool").join("normal"),b"normal").unwrap();
    let cp=checkpoint(&core,&normal,"tool");
    let group=|s:&SessionId,cp:&strata_types::Checkpoint,path:&str| json!({"group":{"id":0,"batches":[{"session":s,"repo":"tool","checkpoint":cp.id,"paths":[path]}]}});
    assert!(core.gits[&repo].read_ref("refs/strata/remotes/origin/release").unwrap().is_none());
    ok(call(&server,"publish_plan",group(&normal,&cp,"normal")));
    assert!(core.gits[&repo].read_ref("refs/strata/remotes/origin/release").unwrap().is_some());
    assert!(core.gits[&repo].read_ref("refs/strata/publish/release").unwrap().is_none());
    assert!(core.store.publishes().unwrap().is_empty());
    ok(call(&server,"publish",group(&normal,&cp,"normal")));
    let c=fx.sb.commit(&main,&[("only-c",b"C")],"local C");
    let parked:SessionId=serde_json::from_value(ok(call(&server,"new",json!({"ticket":null,"repos":["tool"],"only":true,"base":{"tool":c}})))["session"]["manifest"]["id"].clone()).unwrap();
    fs::write(ws(&core,&parked,"tool").join("mine"),b"mine").unwrap(); let cp=checkpoint(&core,&parked,"tool");
    let params=group(&parked,&cp,"mine");
    fx.sb.git(&main,&["update-ref","-d","refs/strata/remotes/origin/release"]);
    assert!(check(&server,&parked,false).repos[&repo].unchecked, "an unknown publish target cannot prove ahead");
    let e=call(&server,"publish_plan",params.clone()); assert_eq!(e["error"]["detail"]["reason"],"base_not_on_main");
    fx.sb.git(&main,&["push","origin","HEAD:main"]);
    // No explicit fetch and no following sync exists on this split repo.
    ok(call(&server,"publish_plan",params));
}

mod support;
use std::time::{Duration, Instant, SystemTime};
use strata_daemon::{
    main_sync,
    periodic_fetch::{self, FetchStep},
    startup::{Core, Daemon},
};
use strata_gitio::RefUpdate;
use strata_store::DepsAutoState;
use strata_types::{Code, CommitOid, SessionId, StrataError};
use support::*;

fn session(core: &Core) -> SessionId {
    Daemon::start(core)
        .unwrap()
        .sessions
        .new(None, &[repo_name("tool")])
        .unwrap()
        .manifest
        .id
}
fn pending(core: &Core) -> strata_deps::Proposal {
    strata_deps::open_proposals(&core.store, Some(&repo_name("tool")))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap()
}
fn advance(f: &Dfx, core: &Core) -> CommitOid {
    let h = f.sb.commit(
        &f.main("tool"),
        &[
            ("bun.lock", b"lock v2\n"),
            ("src/next.ts", b"committed source\n"),
        ],
        "deps",
    );
    f.sb.git(&f.main("tool"), &["push", "-q", "origin", "main"]);
    core.gits[&repo_name("tool")]
        .fetch("origin", "main")
        .unwrap();
    main_sync::reconcile(core, &repo_name("tool")).unwrap();
    CommitOid::try_from(h).unwrap()
}
fn stage(git: &strata_gitio::Git, branch: &str, h: &CommitOid) -> strata_daemon::Result<CommitOid> {
    let r = periodic_fetch::staging_ref(branch);
    git.ref_transaction(&[RefUpdate::set(&r, h.clone(), git.read_ref(&r)?)])?;
    Ok(h.clone())
}
fn exports_empty(core: &Core) {
    let p = core.config.paths.root.join("daemon/main-follow");
    assert!(!p.exists() || std::fs::read_dir(p).unwrap().next().is_none());
}
#[test]
fn auto_worker_ensures_without_confirmer() {
    let f = Dfx::new();
    let (c, log, _) = f.confirmer(vec![]);
    let core = f.prepare_with(c).unwrap();
    let _s = session(&core);
    advance(&f, &core);
    let p = pending(&core);
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Ready
    );
    assert!(log.shown().is_empty());
    exports_empty(&core);
}
#[test]
fn worker_exports_tree_and_removes() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    f.sb.write(&f.main("tool"), "src/next.ts", b"uncommitted");
    let mut called = false;
    main_sync::deps_auto_pass_with(&core, &repo_name("tool"), &mut |ctx, id, dir| {
        called = true;
        assert!(core.locks.holds().is_empty());
        assert_eq!(
            std::fs::read(dir.join("src/next.ts")).unwrap(),
            b"committed source\n"
        );
        assert_eq!(
            core.store.deps_auto(id).unwrap().unwrap().state,
            DepsAutoState::Installing
        );
        strata_deps::ensure_target(ctx, id, dir)
    })
    .unwrap();
    assert!(called);
    exports_empty(&core);
}
#[test]
fn install_failure_not_retried_and_waiting() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    f.sb.set_install_script("exit 7\n");
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    let count = f.sb.install_count();
    for _ in 0..3 {
        main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    }
    assert_eq!(f.sb.install_count(), count);
    let a = core.store.deps_auto(p.id).unwrap().unwrap();
    assert_eq!(a.state, DepsAutoState::InstallFailed);
    let e = a.error.unwrap();
    assert_eq!(e["detail"]["reason"], "install_failed");
    assert!(std::path::Path::new(e["detail"]["staging"].as_str().unwrap()).exists());
    exports_empty(&core);
}
#[test]
fn previous_attempt_maps_to_install_failed() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    f.sb.set_install_script("exit 8\n");
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    core.store.delete_deps_auto(p.id).unwrap();
    let count = f.sb.install_count();
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    let a = core.store.deps_auto(p.id).unwrap().unwrap();
    assert_eq!(a.state, DepsAutoState::InstallFailed);
    assert_eq!(a.error.unwrap()["detail"]["reason"], "previous_attempt");
    assert_eq!(count, f.sb.install_count());
}
#[test]
fn transient_ensure_errors_not_terminal() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    let mut calls = 0;
    for code in [Code::Busy, Code::ReadOnly, Code::Busy] {
        main_sync::deps_auto_pass_with(&core, &repo_name("tool"), &mut |_, _, _| {
            calls += 1;
            Err(StrataError::new(code, "transient"))
        })
        .unwrap();
        assert!(core.store.deps_auto(p.id).unwrap().is_none());
        exports_empty(&core);
    }
    assert_eq!(calls, 3);
}
#[test]
fn no_such_proposal_deletes_auto_row() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    main_sync::deps_auto_pass_with(&core, &repo_name("tool"), &mut |_, _, _| {
        Err(StrataError::new(Code::NoSuchProposal, "superseded"))
    })
    .unwrap();
    assert!(core.store.deps_auto(p.id).unwrap().is_none());
}
#[test]
fn replaced_proposal_auto_applied() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    let s = session(&core);
    advance(&f, &core);
    let p = pending(&core);
    f.sb.set_bun_version("1.3.12");
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    let replacement = pending(&core);
    assert_ne!(p.id, replacement.id);
    assert!(core.store.deps_auto(p.id).unwrap().is_none());
    let d = Daemon::start(&core).unwrap();
    let out =
        strata_daemon::handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
    assert!(out.iter().all(|(_, r)| r.is_ok()), "{out:?}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name("tool")]
            .base
            .deps,
        replacement.to
    );
    assert_eq!(
        core.store.deps_auto(replacement.id).unwrap().unwrap().state,
        DepsAutoState::Applied
    );
}
#[test]
fn sealed_by_other_path_returns_install_failed_to_ready() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    core.store
        .set_deps_auto(p.id, DepsAutoState::InstallFailed, None, SystemTime::now())
        .unwrap();
    main_sync::deps_auto_pass_with(&core, &repo_name("tool"), &mut |_, _, _| {
        panic!("sealed set must skip installer")
    })
    .unwrap();
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Ready
    );
}
#[test]
fn auto_applied_only_when_no_session_differs() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    main_sync::deps_auto_pass(&core, &repo_name("tool")).unwrap();
    assert_eq!(
        strata_deps::get_proposal(&core.store, p.id).unwrap().state,
        strata_deps::ProposalState::Pending
    );
    let d = Daemon::start(&core).unwrap();
    strata_daemon::handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Applied
    );
}
#[test]
fn t4_identity_refresh_real_locks_no_reentrant_wait() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    f.sb.set_bun_version("1.3.13");
    let at = Instant::now();
    main_sync::reconcile(&core, &repo_name("tool")).unwrap();
    assert!(at.elapsed() < Duration::from_secs(3));
    let p = pending(&core);
    assert_eq!(
        core.store.rebase_pending(None).unwrap()[0].to_deps,
        Some(p.to)
    );
    exports_empty(&core);
}
/// Session creation fetches main and records it (upstream freshness), so the
/// first periodic fetch is due one interval after that.
/// (The fixture interval is 900 s; an hour past the recorded fetch is due.)
fn due(core: &Core) -> SystemTime {
    let at = core.store.repo_fetch(&repo_name("tool")).unwrap().map(|f| f.at);
    at.unwrap_or_else(SystemTime::now) + Duration::from_secs(3600)
}
#[test]
fn periodic_fetch_real_locks_observes_in_same_pass() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    let s = session(&core);
    let h = CommitOid::try_from(f.sb.commit(&f.main("tool"), &[("bun.lock", b"new\n")], "new"))
        .unwrap();
    let at = Instant::now();
    let r = periodic_fetch::fetch_due_with(
        &core,
        &repo_name("tool"),
        due(&core),
        &mut |git, b| {
            assert!(core.locks.holds().is_empty());
            stage(git, b, &h)
        },
    )
    .unwrap();
    assert_eq!(r, FetchStep::Fetched);
    assert!(at.elapsed() < Duration::from_secs(3));
    assert_eq!(pending(&core).head, Some(h.clone()));
    assert_eq!(
        core.store
            .rebase_pending_of(&s, &repo_name("tool"))
            .unwrap()
            .unwrap()
            .head
            .as_deref(),
        Some(h.as_str())
    );
    exports_empty(&core);
}
#[test]
fn periodic_fetch_respects_interval_and_failure() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    let now = due(&core);
    let mut calls = 0;
    assert!(
        periodic_fetch::fetch_due_with(&core, &repo_name("tool"), now, &mut |_, _| {
            calls += 1;
            Err(StrataError::new(Code::Unknown, "offline"))
        })
        .is_err()
    );
    for _ in 0..2 {
        assert_eq!(
            periodic_fetch::fetch_due_with(
                &core,
                &repo_name("tool"),
                now + Duration::from_secs(1),
                &mut |_, _| panic!("not due")
            )
            .unwrap(),
            FetchStep::Skipped
        );
    }
    let head = core.gits[&repo_name("tool")]
        .read_ref("refs/strata/remotes/origin/main")
        .unwrap()
        .unwrap();
    periodic_fetch::fetch_due_with(
        &core,
        &repo_name("tool"),
        now + Duration::from_secs(901),
        &mut |g, b| {
            calls += 1;
            stage(g, b, &head)
        },
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert!(
        core.store
            .repo_fetch(&repo_name("tool"))
            .unwrap()
            .unwrap()
            .ok
    );
    assert!(
        !core
            .warnings
            .snapshot()
            .iter()
            .any(|w| matches!(w, strata_daemon::DaemonWarning::FetchFailed { .. }))
    );
}
#[test]
fn periodic_fetch_cas_under_try_lock() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    let git = &core.gits[&repo_name("tool")];
    let old = git.read_ref("refs/strata/remotes/origin/main").unwrap();
    let a = CommitOid::try_from(f.sb.commit(&f.main("tool"), &[("a", b"a")], "a")).unwrap();
    let b = CommitOid::try_from(f.sb.commit(&f.main("tool"), &[("b", b"b")], "b")).unwrap();
    let e = periodic_fetch::fetch_due_with(
        &core,
        &repo_name("tool"),
        due(&core),
        &mut |g, branch| {
            core.locks
                .with_repo(&repo_name("tool"), || {
                    g.ref_transaction(&[RefUpdate::set(
                        "refs/strata/remotes/origin/main",
                        b.clone(),
                        old.clone(),
                    )])
                })
                .unwrap();
            stage(g, branch, &a)
        },
    )
    .unwrap_err();
    assert_eq!(e.code, Code::HeadMoved);
    assert_eq!(
        git.read_ref("refs/strata/remotes/origin/main").unwrap(),
        Some(b)
    );
}
#[test]
fn periodic_fetch_holds_no_lock() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    let head = core.gits[&repo_name("tool")]
        .read_ref("refs/strata/remotes/origin/main")
        .unwrap()
        .unwrap();
    periodic_fetch::fetch_due_with(&core, &repo_name("tool"), due(&core), &mut |g, b| {
        assert!(core.locks.holds().is_empty());
        assert_eq!(
            periodic_fetch::fetch_due_with(
                &core,
                &repo_name("tool"),
                due(&core),
                &mut |_, _| panic!("reentered")
            )
            .unwrap(),
            FetchStep::Skipped
        );
        stage(g, b, &head)
    })
    .unwrap();
}
#[test]
fn periodic_fetch_skips_busy_repo_and_rejects_rewind() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    let repo = repo_name("tool");
    core.locks
        .with_repo(&repo, || {
            assert_eq!(
                periodic_fetch::fetch_due_with(
                    &core,
                    &repo,
                    due(&core),
                    &mut |_, _| panic!("busy")
                )
                .unwrap(),
                FetchStep::Skipped
            );
            Ok(())
        })
        .unwrap();
    let old = core.gits[&repo]
        .read_ref("refs/strata/remotes/origin/main")
        .unwrap()
        .unwrap();
    advance(&f, &core);
    let e = periodic_fetch::fetch_due_with(&core, &repo, due(&core), &mut |g, b| {
        stage(g, b, &old)
    })
    .unwrap_err();
    assert_eq!(e.code, Code::HeadMoved);
}

#[test]
fn periodic_fetch_export_temp_removed() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    let h = f.sb.commit(
        &f.main("tool"),
        &[("bun.lock", b"new fetched deps\n")],
        "fetch",
    );
    f.sb.git(&f.main("tool"), &["push", "-q", "origin", "main"]);
    assert_eq!(
        periodic_fetch::fetch_due(&core, &repo_name("tool"), due(&core)).unwrap(),
        FetchStep::Fetched
    );
    assert_eq!(pending(&core).head.unwrap().as_str(), h);
    exports_empty(&core);
}
#[test]
fn periodic_fetch_try_lock_after_network_does_not_wait() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    let repo = repo_name("tool");
    let git = &core.gits[&repo];
    let old = git
        .read_ref("refs/strata/remotes/origin/main")
        .unwrap()
        .unwrap();
    let head = CommitOid::try_from(f.sb.commit(&f.main("tool"), &[("x", b"x")], "x")).unwrap();
    let entered = std::sync::Barrier::new(2);
    let release = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let mut holder = None;
        let e = periodic_fetch::fetch_due_with(&core, &repo, due(&core), &mut |g, b| {
            holder = Some(scope.spawn(|| {
                core.locks.with_repo(&repo, || {
                    entered.wait();
                    release.wait();
                    Ok(())
                })
            }));
            entered.wait();
            stage(g, b, &head)
        })
        .unwrap_err();
        release.wait();
        holder.unwrap().join().unwrap().unwrap();
        assert_eq!(e.code, Code::Busy);
    });
    assert_eq!(
        git.read_ref("refs/strata/remotes/origin/main").unwrap(),
        Some(old)
    );
    exports_empty(&core);
}
#[test]
fn toolchain_change_after_seal_installs_new_set_and_c3() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    let s = session(&core);
    let repo = repo_name("tool");
    let before = core.store.session(&s).unwrap().unwrap().manifest.repos[&repo].clone();
    f.sb.set_bun_version("1.3.14");
    main_sync::reconcile(&core, &repo).unwrap();
    let p = pending(&core);
    assert_eq!(p.head, Some(before.head.clone()));
    main_sync::deps_auto_pass(&core, &repo).unwrap();
    let d = Daemon::start(&core).unwrap();
    strata_daemon::handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
    let after = core.store.session(&s).unwrap().unwrap().manifest.repos[&repo].clone();
    assert_eq!(before.head, after.head);
    assert_eq!(before.base.tree, after.base.tree);
    assert_ne!(before.base.deps, after.base.deps);
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Applied
    );
}
struct Held(std::process::Child);
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn human_approve_after_install_failed_then_open_files_release_auto_moves() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    let s = session(&core);
    advance(&f, &core);
    let repo = repo_name("tool");
    let p = pending(&core);
    f.sb.set_install_script("exit 9\n");
    main_sync::deps_auto_pass(&core, &repo).unwrap();
    let a = core.store.deps_auto(p.id).unwrap().unwrap();
    let failure = a.error.unwrap();
    let staging = std::path::Path::new(failure["detail"]["staging"].as_str().unwrap());
    // Human recovery explicitly removes this fixture's failed install scene;
    // the worker never does. Exercise the approval's existing ensure_target.
    std::fs::remove_dir_all(staging).unwrap();
    f.sb.set_install_script(DEFAULT_INSTALL);
    let ctx = strata_deps::Ctx {
        store: &core.store,
        paths: &core.config.paths,
        repo: &repo,
        cfg: &core.config.repos[&repo],
        git: &core.gits[&repo],
        env: &core.env,
        locks: &core.locks,
    };
    let tmp = strata_publish::main_follow::TempTree::new(&core.config.paths.root).unwrap();
    ctx.git
        .export_tree(p.tree.as_ref().unwrap(), &tmp.path())
        .unwrap();
    strata_deps::ensure_target(&ctx, p.id, &tmp.path()).unwrap();
    let holder = Held(
        std::process::Command::new("/bin/sleep")
            .arg("300")
            .current_dir(ws(&core, &s, "tool").join("node_modules"))
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(100));
    let d = Daemon::start(&core).unwrap();
    strata_daemon::handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Ready
    );
    assert_eq!(
        core.store
            .rebase_pending_of(&s, &repo)
            .unwrap()
            .unwrap()
            .reason,
        "open_files"
    );
    drop(holder);
    strata_daemon::handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Applied
    );
}
#[test]
fn auto_worker_one_job_per_repo_and_d10_codes() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let repo = repo_name("tool");
    let p = pending(&core);
    for reason in ["timeout", "inputs_modified", "staging_failed"] {
        core.store.delete_deps_auto(p.id).unwrap();
        main_sync::deps_auto_pass_with(&core, &repo, &mut |_, _, _| {
            main_sync::deps_auto_pass_with(&core, &repo, &mut |_, _, _| panic!("duplicate worker"))
                .unwrap();
            Err(StrataError::new(Code::Unknown, "install failed")
                .with_detail(serde_json::json!({"reason":reason})))
        })
        .unwrap();
        assert_eq!(
            core.store.deps_auto(p.id).unwrap().unwrap().state,
            DepsAutoState::InstallFailed
        );
    }
}

#[test]
fn worker_completion_preserves_concurrent_applied_audit() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    main_sync::deps_auto_pass_with(&core, &repo_name("tool"), &mut |ctx, id, dir| {
        let result = strata_deps::ensure_target(ctx, id, dir)?;
        let d = Daemon::start(&core).unwrap();
        strata_daemon::handlers::catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600));
        assert_eq!(
            core.store.deps_auto(p.id).unwrap().unwrap().state,
            DepsAutoState::Applied
        );
        Ok(result)
    })
    .unwrap();
    assert_eq!(
        core.store.deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Applied
    );
}

#[test]
fn worker_skips_drafts_and_prepares_landed_session_origin() {
    let f = Dfx::new();
    let core = f.prepare().unwrap();
    let s = session(&core);
    let repo = repo_name("tool");
    advance(&f, &core);
    let p = pending(&core);
    core.store
        .decide_proposal(
            p.id,
            strata_store::ProposalDecision::Applied,
            SystemTime::now(),
        )
        .unwrap();
    let new = strata_store::NewProposal {
        repo: repo.clone(),
        origin: Some(s),
        from: p.from.clone(),
        from_inputs: p.from_inputs.clone(),
        from_head: p.from_head.clone(),
        to: p.to.clone(),
        to_inputs: p.to_inputs.clone(),
        tree: None,
        head: None,
    };
    let draft = core
        .locks
        .with_repo(&repo, || {
            core.store.record_proposal(&new, None, SystemTime::now())
        })
        .unwrap()
        .unwrap();
    main_sync::deps_auto_pass_with(&core, &repo, &mut |_, _, _| {
        panic!("draft must not install")
    })
    .unwrap();
    assert!(core.store.deps_auto(draft).unwrap().is_none());
    assert_eq!(
        strata_deps::get_proposal(&core.store, draft).unwrap().state,
        strata_deps::ProposalState::AwaitingPublish
    );
    core.locks
        .with_repo(&repo, || {
            core.store.promote_proposal(
                draft,
                p.tree.as_ref().unwrap(),
                p.head.as_ref().unwrap(),
                SystemTime::now(),
            )
        })
        .unwrap();
    let landed = draft;
    main_sync::deps_auto_pass(&core, &repo).unwrap();
    assert_eq!(
        core.store.deps_auto(landed).unwrap().unwrap().state,
        DepsAutoState::Ready
    );
}

#[test]
fn serving_schedules_worker_and_t2_until_applied() {
    let mut f = Dfx::new();
    f.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .sync
        .fetch_secs = 0;
    f.catch_up_every = Duration::from_millis(250);
    f.write_config();
    let mut core = f.prepare().unwrap();
    core.opts.background_main_sync = true;
    session(&core);
    advance(&f, &core);
    let p = pending(&core);
    let d = Daemon::start(&core).unwrap();
    let listener = strata_daemon::startup::bind(&f.socket()).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let server = scope.spawn(|| strata_daemon::handlers::serve(&d, &listener, &stop));
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut applied = false;
        while Instant::now() < deadline {
            applied = core
                .store
                .deps_auto(p.id)
                .unwrap()
                .is_some_and(|a| a.state == DepsAutoState::Applied);
            if applied {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        d.request_stop();
        server.join().unwrap().unwrap();
        assert!(
            applied,
            "the scheduled worker and T2 did not settle the proposal"
        );
    });
}

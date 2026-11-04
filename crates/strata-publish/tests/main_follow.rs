mod support;

use std::time::{Duration, SystemTime};
use strata_publish::{RebaseReport, main_follow, pending};
use strata_store::DepsAutoState;
use strata_types::{CommitOid, SyncDeps};
use support::*;

fn fx() -> Fx {
    Fx::with(&["tool"])
}
fn advance(f: &Fx, deps: bool) -> CommitOid {
    let files: &[(&str, &[u8])] = if deps {
        &[("bun.lock", b"lock v2\n")]
    } else {
        &[("src/next.ts", b"next\n")]
    };
    let h = f.sb.commit(&f.main("tool"), files, "next");
    f.sb.git(&f.main("tool"), &["push", "-q", "origin", "main"]);
    f.git_of("tool").fetch("origin", "main").unwrap();
    CommitOid::try_from(h).unwrap()
}
fn reconcile(f: &Fx) {
    let d = f.deps_ctx("tool");
    let ss = f.sessions();
    main_follow::reconcile(&f.ctx(&d, &ss)).unwrap();
}
fn pass(
    f: &Fx,
) -> Vec<(
    strata_types::SessionId,
    strata_publish::Result<pending::CatchUp>,
)> {
    let d = f.deps_ctx("tool");
    let ss = f.sessions();
    pending::catch_up(
        &f.ctx(&d, &ss),
        SystemTime::now() + Duration::from_secs(3600),
    )
    .unwrap()
}
#[test]
fn main_move_enrolls_idle_sessions() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let h = advance(&f, false);
    reconcile(&f);
    let r = f
        .store()
        .rebase_pending_of(&s, &f.rn("tool"))
        .unwrap()
        .unwrap();
    assert_eq!(r.head.as_deref(), Some(h.as_str()));
    assert_eq!(r.source, strata_store::PendingSource::Main);
    assert!(r.to_deps.is_none());
}
#[test]
fn reconcile_enrolls_session_after_scene_clears_same_tip() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, false);
    let ws = f.store().session(&s).unwrap().unwrap().manifest.repos[&f.rn("tool")].clone();
    let n = f
        .store()
        .begin_move(
            &s,
            &f.rn("tool"),
            &ws.base,
            &ws.head,
            &ws.path.with_file_name(".follow-scene"),
            SystemTime::now(),
        )
        .unwrap();
    f.store()
        .set_move_state(
            &s,
            &f.rn("tool"),
            n,
            strata_store::MoveState::Preparing,
            strata_store::MoveState::Swapping,
            Some(&[]),
            SystemTime::now(),
        )
        .unwrap();
    f.store()
        .fail_move(
            &s,
            &f.rn("tool"),
            n,
            &serde_json::json!({"scene":true}),
            SystemTime::now(),
        )
        .unwrap();
    reconcile(&f);
    assert!(
        f.store()
            .rebase_pending_of(&s, &f.rn("tool"))
            .unwrap()
            .is_none()
    );
    f.store()
        .resolve_move(&s, &f.rn("tool"), n, false, SystemTime::now())
        .unwrap();
    reconcile(&f);
    assert!(
        f.store()
            .rebase_pending_of(&s, &f.rn("tool"))
            .unwrap()
            .is_some()
    );
    let existing = row(&f, &s).unwrap();
    f.store()
        .drop_pending(&s, &f.rn("tool"), existing.rev)
        .unwrap();
    let n = f
        .store()
        .begin_move(
            &s,
            &f.rn("tool"),
            &ws.base,
            &ws.head,
            &ws.path.with_file_name(".follow-refused"),
            SystemTime::now(),
        )
        .unwrap();
    f.store()
        .set_move_state(
            &s,
            &f.rn("tool"),
            n,
            strata_store::MoveState::Preparing,
            strata_store::MoveState::Refused,
            None,
            SystemTime::now(),
        )
        .unwrap();
    reconcile(&f);
    assert!(
        row(&f, &s).is_some(),
        "a later pre-swap refusal cannot resurrect the resolved scene"
    );
}
#[test]
fn c2_then_install_ready_then_c3_without_new_main_commit() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let old = f.base_of(&s, "tool");
    advance(&f, true);
    let first = pass(&f);
    assert!(matches!(
        &first[0].1,
        Ok(pending::CatchUp::Attempted(RebaseReport::Moved {
            deps_pending: Some(_),
            ..
        }))
    ));
    assert_eq!(f.base_of(&s, "tool").deps, old.deps);
    reconcile(&f);
    let row = f
        .store()
        .rebase_pending_of(&s, &f.rn("tool"))
        .unwrap()
        .unwrap();
    assert!(row.to_deps.is_some());
    let d = f.deps_ctx("tool");
    let set = strata_deps::ensure(&d, &f.store().base_dir(&f.base_of(&s, "tool"))).unwrap();
    let out = pass(&f);
    assert!(
        matches!(
            &out[0].1,
            Ok(pending::CatchUp::Attempted(RebaseReport::Moved {
                deps_pending: None,
                ..
            }))
        ),
        "{out:?}"
    );
    assert_eq!(f.base_of(&s, "tool").deps, set.id);
    assert!(
        f.store()
            .rebase_pending_of(&s, &f.rn("tool"))
            .unwrap()
            .is_none()
    );
    assert!(
        f.store()
            .deps_auto_all(Some(&f.rn("tool")))
            .unwrap()
            .iter()
            .any(|a| a.state == DepsAutoState::Applied)
    );
}
#[test]
fn deps_only_row_unsealed_defers_with_backoff() {
    let mut f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    f.cfg_mut("tool").rebase.quiet_secs = 1;
    for n in 1..=3 {
        let out = pass(&f);
        assert!(
            matches!(&out[0].1,Ok(pending::CatchUp::Attempted(RebaseReport::Deferred{reason,..})) if reason=="waiting_install"),
            "{out:?}"
        );
        let row = f
            .store()
            .rebase_pending_of(&s, &f.rn("tool"))
            .unwrap()
            .unwrap();
        assert_eq!(row.attempts, n);
    }
}
#[test]
fn confirm_repo_never_auto_moves_deps() {
    let mut f = fx();
    f.cfg_mut("tool").sync.deps = SyncDeps::Confirm;
    let s = f.new_session(&["tool"]);
    let old = f.base_of(&s, "tool");
    advance(&f, true);
    pass(&f);
    reconcile(&f);
    assert_eq!(f.base_of(&s, "tool").deps, old.deps);
    assert!(
        f.store()
            .rebase_pending_of(&s, &f.rn("tool"))
            .unwrap()
            .is_none()
    );
}

fn row(f: &Fx, s: &strata_types::SessionId) -> Option<strata_store::RebasePending> {
    f.store().rebase_pending_of(s, &f.rn("tool")).unwrap()
}
fn seal_tip(f: &Fx) -> strata_types::DepsSetId {
    let d = f.deps_ctx("tool");
    let p = main_follow::prepare(&d).unwrap().unwrap();
    let tmp = main_follow::TempTree::new(&f.root()).unwrap();
    f.git_of("tool").export_tree(&p.tree, &tmp.path()).unwrap();
    strata_deps::ensure(&d, &tmp.path()).unwrap().id
}
#[test]
fn retarget_keeps_open_files_backoff() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, false);
    reconcile(&f);
    let r = row(&f, &s).unwrap();
    f.store()
        .defer_pending(
            &s,
            &f.rn("tool"),
            r.rev,
            "open_files",
            &serde_json::json!({"held":true}),
            SystemTime::now(),
        )
        .unwrap();
    let before = row(&f, &s).unwrap();
    let h = advance(&f, true);
    reconcile(&f);
    let after = row(&f, &s).unwrap();
    assert_eq!(after.head.as_deref(), Some(h.as_str()));
    assert_eq!(
        (after.attempts, after.last_at, after.reason),
        (before.attempts, before.last_at, before.reason)
    );
    assert_eq!(after.from_tree, f.base_of(&s, "tool").tree.as_str());
    assert_eq!(after.from_deps, f.base_of(&s, "tool").deps.as_str());
}
#[test]
fn deps_only_row_retargeted_to_newer_main_clears_to_deps() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    reconcile(&f);
    assert!(row(&f, &s).unwrap().to_deps.is_some());
    let h = advance(&f, false);
    reconcile(&f);
    let r = row(&f, &s).unwrap();
    assert_eq!(r.head.as_deref(), Some(h.as_str()));
    assert!(r.to_deps.is_none());
}
#[test]
fn key_cache_includes_toolchain_identity() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    reconcile(&f);
    let a = row(&f, &s).unwrap();
    f.sb.set_bun_version("1.3.9");
    reconcile(&f);
    let b = row(&f, &s).unwrap();
    assert_ne!(a.to_deps, b.to_deps);
    assert!(b.rev > a.rev);
    assert_eq!(b.attempts, 0);
    let p = strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    assert_eq!(b.to_deps, Some(p.to));
    assert_eq!(a.tree, b.tree);
}
#[test]
fn deps_only_row_follows_replaced_proposal_same_tree() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    reconcile(&f);
    let d = f.deps_ctx("tool");
    let old = strata_deps::open_proposals(f.store(), Some(d.repo))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    f.sb.set_bun_version("1.3.10");
    let dir = f.store().base_dir(&f.base_of(&s, "tool"));
    let target = strata_deps::ensure_target(&d, old.id, &dir).unwrap();
    let strata_deps::Target::Replaced { replacement, .. } = target else {
        panic!("{target:?}")
    };
    reconcile(&f);
    assert_eq!(row(&f, &s).unwrap().to_deps, Some(replacement.to));
}
#[test]
fn deps_only_never_takes_c2() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    let base = f.base_of(&s, "tool");
    for _ in 0..3 {
        let out = pass(&f);
        assert!(matches!(
            &out[0].1,
            Ok(pending::CatchUp::Attempted(RebaseReport::Deferred { .. }))
        ));
        assert_eq!(f.base_of(&s, "tool"), base);
    }
}
#[test]
fn already_based_keeps_row_with_unmet_to_deps() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    pass(&f);
    assert!(row(&f, &s).unwrap().to_deps.is_some());
    assert_eq!(row(&f, &s).unwrap().attempts, 1);
}
#[test]
fn deps_only_unsealed_records_propose_main() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    reconcile(&f);
    let p = strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    f.store()
        .decide_proposal(
            p.id,
            strata_store::ProposalDecision::Applied,
            SystemTime::now(),
        )
        .unwrap();
    let d = f.deps_ctx("tool");
    let ss = f.sessions();
    let ctx = f.ctx(&d, &ss);
    let out = pending::attempt(
        &ctx,
        &row(&f, &s).unwrap(),
        strata_publish::MovePolicy::Auto,
    )
    .unwrap();
    assert!(matches!(&out,Some(RebaseReport::Deferred{reason,..}) if reason=="waiting_install"), "{out:?}");
    assert!(
        strata_deps::open_proposals(f.store(), Some(d.repo))
            .unwrap()
            .iter()
            .any(|p| p.state == strata_deps::ProposalState::Pending)
    );
}
#[test]
fn deps_only_row_dropped_on_install_failed_confirm_or_drift() {
    for mode in ["failed", "confirm", "drift"] {
        let mut f = fx();
        let s = f.new_session(&["tool"]);
        advance(&f, true);
        pass(&f);
        reconcile(&f);
        match mode {
            "failed" => {
                let p = strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
                    .unwrap()
                    .into_iter()
                    .find(|p| p.state == strata_deps::ProposalState::Pending)
                    .unwrap();
                f.store()
                    .set_deps_auto(p.id, DepsAutoState::InstallFailed, None, SystemTime::now())
                    .unwrap();
            }
            "confirm" => f.cfg_mut("tool").sync.deps = SyncDeps::Confirm,
            _ => f.edit(&s, "tool", &[("bun.lock", b"own inputs\n")]),
        }
        for _ in 0..3 {
            pass(&f);
            assert!(row(&f, &s).is_none(), "{mode}");
        }
    }
}
#[test]
fn c3_full_move_when_set_sealed_and_apply() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    let set = seal_tip(&f);
    let out = pass(&f);
    assert!(
        matches!(
            &out[0].1,
            Ok(pending::CatchUp::Attempted(RebaseReport::Moved {
                deps_pending: None,
                ..
            }))
        ),
        "{out:?}"
    );
    assert_eq!(f.base_of(&s, "tool").deps, set);
}
#[test]
fn c3_builds_base_before_gates() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let old = f.base_of(&s, "tool");
    advance(&f, true);
    let set = seal_tip(&f);
    let p = main_follow::prepare(&f.deps_ctx("tool")).unwrap().unwrap();
    let throwaway = strata_types::BaseGen {
        repo: f.rn("tool"),
        tree: p.tree.clone(),
        deps: old.deps,
    };
    assert!(!f.store().has_base_gen(&throwaway).unwrap());
    pass(&f);
    assert_eq!(f.base_of(&s, "tool").deps, set);
    assert!(!f.store().has_base_gen(&throwaway).unwrap());
}
#[test]
fn c3_uses_rollout_classifier_not_status_drift() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    pass(&f);
    let d = f.deps_ctx("tool");
    let w = f.store().session(&s).unwrap().unwrap().manifest.repos[&f.rn("tool")].clone();
    let pinned = strata_deps::find_set(f.store(), d.repo, &w.base.deps)
        .unwrap()
        .unwrap();
    assert!(
        strata_deps::drift(d.cfg, d.git, &w.path, &pinned)
            .unwrap()
            .is_some()
    );
    let set = seal_tip(&f);
    pass(&f);
    assert_eq!(f.base_of(&s, "tool").deps, set);
}
#[test]
fn c3_falls_back_to_c2_while_installing() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let old = f.base_of(&s, "tool");
    advance(&f, true);
    reconcile(&f);
    let p = strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    f.store()
        .set_deps_auto(p.id, DepsAutoState::Installing, None, SystemTime::now())
        .unwrap();
    pass(&f);
    assert_eq!(f.base_of(&s, "tool").deps, old.deps);
    let out = pass(&f);
    assert!(
        matches!(&out[0].1,Ok(pending::CatchUp::Attempted(RebaseReport::Deferred{reason,..})) if reason=="installing")
    );
}
#[test]
fn apply_repo_skips_t2_prewarm() {
    let mut f = fx();
    f.cfg_mut("tool").rebase.deps_changed = strata_types::DepsChanged::Wait;
    let s = f.new_session(&["tool"]);
    let before = f.sb.install_count();
    let base = f.base_of(&s, "tool");
    advance(&f, true);
    let out = pass(&f);
    assert!(matches!(
        &out[0].1,
        Ok(pending::CatchUp::Attempted(RebaseReport::Waiting { .. }))
    ));
    assert_eq!(f.sb.install_count(), before);
    assert_eq!(base, f.base_of(&s, "tool"));
}
#[test]
fn split_branch_repo_not_followed_no_stale_loop() {
    let mut f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, false);
    f.cfg_mut("tool").publish.branch = "release".into();
    for _ in 0..3 {
        assert!(pass(&f).is_empty());
        assert!(row(&f, &s).is_none());
    }
}
#[test]
fn enroll_skips_untracked_scene_and_follow_main_off() {
    let mut f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, false);
    f.cfg_mut("tool").sync.follow_main = Some(false);
    f.cfg_mut("tool").sync.deps = SyncDeps::Confirm;
    reconcile(&f);
    assert!(row(&f, &s).is_none());
    // A scene is independently covered by the real failed move fixture above.
}
#[test]
fn reconcile_replaces_discarded_stale_target_same_pass() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let initial = f.sb.head(&f.main("tool"));
    let old = advance(&f, false);
    reconcile(&f);
    f.sb.git(&f.main("tool"), &["reset", "--hard", &initial]);
    let fork = f.sb.commit(&f.main("tool"), &[("other", b"fork")], "fork");
    f.sb.git(&f.main("tool"), &["push", "--force", "origin", "main"]);
    f.git_of("tool").fetch("origin", "main").unwrap();
    assert_ne!(old.as_str(), fork);
    let out = pass(&f);
    assert!(
        matches!(
            &out[0].1,
            Ok(pending::CatchUp::Attempted(RebaseReport::Moved { .. }))
        ),
        "{out:?}"
    );
    assert_eq!(
        f.store().session(&s).unwrap().unwrap().manifest.repos[&f.rn("tool")]
            .head
            .as_str(),
        fork
    );
}
#[test]
fn deps_only_enrolled_from_deps_main_after_settle() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let head = advance(&f, true);
    pass(&f);
    let p = strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    f.store()
        .decide_proposal(
            p.id,
            strata_store::ProposalDecision::Applied,
            SystemTime::now(),
        )
        .unwrap();
    reconcile(&f);
    assert!(row(&f, &s).unwrap().to_deps.is_some());
    assert!(
        strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
            .unwrap()
            .iter()
            .any(|q| q.state == strata_deps::ProposalState::Pending
                && Some(&q.to) == row(&f, &s).unwrap().to_deps.as_ref()
                && q.from_head.as_ref() == Some(&head))
    );
}

fn copy_session(
    f: &Fx,
    s: &strata_types::SessionId,
    name: &str,
    tracked: bool,
    unknown: bool,
) -> strata_types::SessionId {
    let mut m = f.store().session(s).unwrap().unwrap().manifest;
    m.id = strata_types::SessionId::try_from(name.to_owned()).unwrap();
    m.root = m.root.with_file_name(name);
    for (repo, w) in &mut m.repos {
        w.path = m.root.join(repo.as_str());
        w.tracked = tracked;
    }
    f.store().create_session(&m, SystemTime::now()).unwrap();
    let next = if unknown {
        strata_store::Lifecycle::Unknown
    } else {
        strata_store::Lifecycle::Active
    };
    let scene = unknown.then(|| serde_json::json!({"failed":"creation"}));
    f.store()
        .set_lifecycle(
            &m.id,
            strata_store::Lifecycle::Creating,
            next,
            scene.as_ref(),
            SystemTime::now(),
        )
        .unwrap();
    m.id
}
#[test]
fn settle_counts_unknown_sessions() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let unknown = copy_session(&f, &s, "s-unknownx", true, true);
    advance(&f, true);
    seal_tip(&f);
    pass(&f);
    let p = strata_deps::open_proposals(f.store(), Some(&f.rn("tool")))
        .unwrap()
        .into_iter()
        .find(|p| p.state == strata_deps::ProposalState::Pending)
        .unwrap();
    assert_eq!(f.base_of(&s, "tool").deps, p.to);
    assert_ne!(f.base_of(&unknown, "tool").deps, p.to);
    assert_ne!(
        f.store().deps_auto(p.id).unwrap().unwrap().state,
        DepsAutoState::Applied
    );
}
#[test]
fn enroll_skips_untracked_workspace() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let untracked = copy_session(&f, &s, "s-untrackx", false, false);
    advance(&f, false);
    reconcile(&f);
    assert!(row(&f, &s).is_some());
    assert!(row(&f, &untracked).is_none());
}
#[test]
fn c2_consumes_row_and_next_reconcile_adds_deps_only() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    let out = pass(&f);
    assert!(matches!(
        &out[0].1,
        Ok(pending::CatchUp::Attempted(RebaseReport::Moved { .. }))
    ));
    assert!(row(&f, &s).is_none());
    reconcile(&f);
    let r = row(&f, &s).unwrap();
    assert!(r.to_deps.is_some());
    assert_eq!(r.attempts, 0);
}
#[test]
fn own_change_gets_source_only_and_no_deps_only_row() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let d = f.deps_ctx("tool");
    let old = f.base_of(&s, "tool");
    f.edit(
        &s,
        "tool",
        &[(
            "package.json",
            br#"{"name":"tool","dependencies":{"mine":"1"}}"#,
        )],
    );
    let (inputs, to) = strata_deps::key_of(&d, &f.ws(&s, "tool")).unwrap();
    let from_inputs = strata_deps::find_set(f.store(), d.repo, &old.deps)
        .unwrap()
        .unwrap()
        .inputs_hash;
    f.lock_write(d.repo, || {
        strata_deps::propose(
            &d,
            strata_deps::ProposeArgs {
                origin: strata_deps::ProposalOrigin::Session { session: s.clone() },
                from: old.deps.clone(),
                from_inputs: from_inputs.clone(),
                from_head: None,
                to: to.clone(),
                to_inputs: inputs.clone(),
                tree: None,
                head: None,
            },
        )
        .unwrap();
    })
    .unwrap();
    advance(&f, true);
    seal_tip(&f);
    let out = pass(&f);
    assert!(
        matches!(
            &out[0].1,
            Ok(pending::CatchUp::Attempted(RebaseReport::Moved {
                deps_pending: Some(_),
                ..
            }))
        ),
        "{out:?}"
    );
    assert_eq!(f.base_of(&s, "tool").deps, old.deps);
    for _ in 0..3 {
        reconcile(&f);
        assert!(row(&f, &s).is_none());
    }
}
struct Held(std::process::Child);
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn c3_defers_on_open_deps_dir() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    advance(&f, true);
    seal_tip(&f);
    let holder = Held(
        std::process::Command::new("/bin/sleep")
            .arg("300")
            .current_dir(f.ws(&s, "tool").join("node_modules"))
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(100));
    let out = pass(&f);
    assert!(
        matches!(&out[0].1,Ok(pending::CatchUp::Attempted(RebaseReport::Deferred{reason,..})) if reason=="open_files"),
        "{out:?}"
    );
    drop(holder);
    let out = pass(&f);
    assert!(
        matches!(
            &out[0].1,
            Ok(pending::CatchUp::Attempted(RebaseReport::Moved { .. }))
        ),
        "{out:?}"
    );
}
fn chain_session(f: &Fx) -> (strata_types::SessionId, CommitOid) {
    let old = CommitOid::try_from(f.sb.head(&f.main("tool"))).unwrap();
    f.git_of("tool").fetch("origin", "main").unwrap();
    let top = CommitOid::try_from(f.sb.commit(
        &f.main("tool"),
        &[("base-only", b"must survive")],
        "local",
    ))
    .unwrap();
    f.git_of("tool")
        .ref_transaction(&[
            strata_gitio::RefUpdate::set("refs/strata/anchor/main", old.clone(), None),
            strata_gitio::RefUpdate::set("refs/strata/publish/main", top, None),
        ])
        .unwrap();
    let s = f.new_session(&["tool"]);
    f.sb.git(
        &f.main("tool"),
        &["update-ref", "-d", "refs/strata/publish/main"],
    );
    (s, old)
}
#[test]
fn diverged_head_not_enrolled_and_reported_with_base_only() {
    let f = fx();
    let (s, old) = chain_session(&f);
    f.sb.git(&f.main("tool"), &["reset", "--hard", old.as_str()]);
    advance(&f, true);
    for _ in 0..3 {
        reconcile(&f);
        assert!(row(&f, &s).is_none());
    }
    assert_eq!(
        std::fs::read(f.ws(&s, "tool").join("base-only")).unwrap(),
        b"must survive"
    );
}
#[test]
fn ahead_only_is_not_diverged() {
    let f = fx();
    let (s, old) = chain_session(&f);
    reconcile(&f);
    assert!(row(&f, &s).is_none());
    let ws = f.store().session(&s).unwrap().unwrap().manifest.repos[&f.rn("tool")].clone();
    assert!(f.git_of("tool").is_ancestor(&old, &ws.head).unwrap());
}
#[test]
fn enroll_insert_only_keeps_descendant() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    let a = advance(&f, false);
    let b = advance(&f, true);
    reconcile(&f);
    f.git_of("tool")
        .ref_transaction(&[strata_gitio::RefUpdate::set(
            "refs/strata/remotes/origin/main",
            a,
            Some(b.clone()),
        )])
        .unwrap();
    reconcile(&f);
    assert_eq!(row(&f, &s).unwrap().head.as_deref(), Some(b.as_str()));
}
fn published_row(f: &mut Fx) -> (strata_types::SessionId, CommitOid) {
    f.cfg_mut("tool").rebase.quiet_secs = 120;
    let s = f.new_session(&["tool"]);
    f.edit(&s, "tool", &[("published", b"work")]);
    let c = f.checkpoint(&s, "tool");
    let d = f.deps_ctx("tool");
    let ss = f.sessions();
    let p = strata_publish::publish(
        &f.ctx(&d, &ss),
        &group(vec![batch(&s, "tool", c.id, &["published"])]),
        None,
    )
    .unwrap();
    assert_eq!(
        row(f, &s).unwrap().source,
        strata_store::PendingSource::Publish
    );
    (s, p.commit)
}
#[test]
fn publish_row_retargeted_to_main_tip_one_move() {
    let mut f = fx();
    let (s, h) = published_row(&mut f);
    f.sb.git(&f.main("tool"), &["reset", "--hard", h.as_str()]);
    let target = advance(&f, false);
    reconcile(&f);
    assert_eq!(row(&f, &s).unwrap().head.as_deref(), Some(target.as_str()));
    f.cfg_mut("tool").rebase.quiet_secs = 0;
    let out = pass(&f);
    assert_eq!(out.len(), 1);
    assert!(row(&f, &s).is_none());
}
#[test]
fn unpushed_publish_row_not_retargeted() {
    let mut f = fx();
    let (s, h) = published_row(&mut f);
    let r = row(&f, &s).unwrap();
    f.store()
        .set_pending_reason(&s, &f.rn("tool"), r.rev, "publish_unfinished")
        .unwrap();
    f.sb.git(&f.main("tool"), &["reset", "--hard", h.as_str()]);
    advance(&f, false);
    reconcile(&f);
    assert_eq!(row(&f, &s).unwrap().head.as_deref(), Some(h.as_str()));
}

#[test]
fn main_key_matches_canonical_checkout_conversions() {
    let f = fx();
    let s = f.new_session(&["tool"]);
    f.sb.commit(
        &f.main("tool"),
        &[(".gitattributes", b"bun.lock text eol=crlf\n")],
        "checkout conversion",
    );
    f.sb.git(&f.main("tool"), &["push", "-q", "origin", "main"]);
    f.git_of("tool").fetch("origin", "main").unwrap();
    let d = f.deps_ctx("tool");
    let p = main_follow::prepare(&d).unwrap().unwrap();
    let tmp = main_follow::TempTree::new(&f.root()).unwrap();
    d.git.export_tree(&p.tree, &tmp.path()).unwrap();
    assert!(
        std::fs::read(tmp.path().join("bun.lock"))
            .unwrap()
            .windows(2)
            .any(|w| w == b"\r\n")
    );
    assert_eq!(p.deps, strata_deps::key_of(&d, &tmp.path()).unwrap().1);
    reconcile(&f);
    assert!(row(&f, &s).is_some());
}

#[test]
fn parked_base_does_not_keep_a_dependency_proposal_pending() {
    let f=fx(); let normal=f.new_session(&["tool"]); let initial=f.sb.head(&f.main("tool"));
    let c=f.sb.commit(&f.main("tool"),&[("only-c",b"C")],"local C");
    let s=f.sessions().new_at(None,&[repo_name("tool")],strata_session::Layout::Only,None,&std::collections::BTreeMap::from([(repo_name("tool"),c.clone())])).unwrap().manifest.id;
    f.sb.git(&f.main("tool"),&["reset","--hard",&initial]);
    advance(&f,true); let target=seal_tip(&f); pass(&f);
    assert_eq!(f.base_of(&normal,"tool").deps,target);
    assert_ne!(f.base_of(&s,"tool").deps,target);
    assert_eq!(f.store().session(&s).unwrap().unwrap().manifest.repos[&repo_name("tool")].head.as_str(),c);
    let d=f.deps_ctx("tool"); main_follow::settle_auto(&d).unwrap();
    assert!(!strata_deps::open_proposals(f.store(),Some(&repo_name("tool"))).unwrap().iter().any(|p|p.state==strata_deps::ProposalState::Pending && p.to==target));
}

#[test]
fn main_target_that_would_drop_a_parked_base_is_discarded() {
    let f = fx();
    let repo = repo_name("tool");
    let main = f.main("tool");
    let initial = f.sb.head(&main);
    let local =
        f.sb.commit(&main, &[("only-local", b"keep")], "parked base");
    let session = f
        .sessions()
        .new_at(
            None,
            std::slice::from_ref(&repo),
            strata_session::Layout::Only,
            None,
            &std::collections::BTreeMap::from([(repo.clone(), local.clone())]),
        )
        .unwrap()
        .manifest
        .id;
    f.sb.git(&main, &["reset", "--hard", &initial]);
    let target = advance(&f, false);
    let ws = f.store().session(&session).unwrap().unwrap().manifest.repos[&repo].clone();
    f.store()
        .enroll_main(
            &session,
            &repo,
            &ws.base,
            &target,
            &f.git_of("tool").tree_of(&target).unwrap(),
            None,
            SystemTime::now(),
        )
        .unwrap();
    let result = pass(&f);
    assert!(
        result.iter().any(|(s, result)| s == &session
            && matches!(result, Ok(pending::CatchUp::Dropped(reason)) if reason == "base_ahead")),
        "{result:?}"
    );
    assert!(row(&f, &session).is_none());
    assert_eq!(
        f.store().session(&session).unwrap().unwrap().manifest.repos[&repo]
            .head
            .as_str(),
        local
    );
    assert!(f.store().move_intents(&session).unwrap().is_empty());
}

#[test]
fn parked_local_base_follows_when_origin_contains_its_child() {
    let f = fx();
    let c = f.sb.commit(&f.main("tool"), &[("only-c", b"C")], "local C");
    let s = f
        .sessions()
        .new_at(
            None,
            &[repo_name("tool")],
            strata_session::Layout::Only,
            None,
            &std::collections::BTreeMap::from([(repo_name("tool"), c)]),
        )
        .unwrap()
        .manifest
        .id;
    let d = advance(&f, false);
    reconcile(&f);
    assert!(row(&f,&s).is_some(), "the parked base becomes eligible once origin contains it");
    pass(&f);
    assert_eq!(f.store().session(&s).unwrap().unwrap().manifest.repos[&repo_name("tool")].head,d);
    assert_eq!(std::fs::read(f.ws(&s,"tool").join("only-c")).unwrap(),b"C");
}

mod support;
use serde_json::json;
use strata_store::*;
use strata_types::{CommitOid, DepsSetId, ProposalId};
use support::world::*;
use support::*;

fn head() -> CommitOid {
    CommitOid::try_from("2".repeat(40)).unwrap()
}
fn proposal() -> NewProposal {
    NewProposal {
        repo: repo(),
        origin: None,
        from: gen1().deps,
        from_inputs: "a".repeat(64),
        from_head: Some(CommitOid::try_from("1".repeat(40)).unwrap()),
        to: DepsSetId::try_from("b".repeat(64)).unwrap(),
        to_inputs: "b".repeat(64),
        tree: Some(gen1().tree),
        head: Some(head()),
    }
}
fn report(s: &Store) -> SyncReportRow {
    SyncReportRow {
        target: head(),
        generation: 1,
        path: rp("src/a"),
        kind: "text".into(),
        ours_blob: Some(s.put_bytes(b"ours before editing").unwrap()),
        hunks: json!([{"theirs_start": 3, "theirs_len": 1_000_000, "ours_len": 5_000_000, "text": "conflict"}]),
    }
}
fn store_report(s: &Store) -> SyncReportRow {
    let row = report(s);
    s.put_sync_report(&sid(), &repo(), &head(), 1, std::slice::from_ref(&row))
        .unwrap();
    row
}

#[test]
fn proposal_from_head_recorded() {
    let env = env();
    let s = env.open();
    let p = proposal();
    let id = s.record_proposal(&p, None, t(10)).unwrap().unwrap();
    assert_eq!(s.proposal(id).unwrap().unwrap().from_head, p.from_head);
    assert_eq!(s.proposals(None, true).unwrap()[0].from_head, p.from_head);
    s.close().unwrap();
    let s = env.open();
    assert_eq!(s.proposal(id).unwrap().unwrap().from_head, p.from_head);
    s.close().unwrap();
}

#[test]
fn deps_auto_upsert_and_applied_transition() {
    let env = env();
    let s = env.open();
    let id = s
        .record_proposal(&proposal(), None, t(10))
        .unwrap()
        .unwrap();
    s.set_deps_auto(
        id,
        DepsAutoState::InstallFailed,
        Some(&json!({"reason": "previous_attempt"})),
        t(11),
    )
    .unwrap();
    assert_eq!(
        s.deps_auto(id).unwrap().unwrap().error.unwrap()["reason"],
        "previous_attempt"
    );
    s.set_deps_auto(id, DepsAutoState::Ready, None, t(12))
        .unwrap();
    assert_eq!(s.deps_auto_all(Some(&repo())).unwrap().len(), 1);
    assert!(
        s.deps_auto_all(Some(
            &strata_types::RepoName::try_from("other".to_string()).unwrap()
        ))
        .unwrap()
        .is_empty()
    );
    assert!(s.auto_applied(id, t(13)).unwrap());
    assert_eq!(
        s.proposal(id).unwrap().unwrap().status,
        ProposalStatus::Applied
    );
    let audit = s.deps_auto(id).unwrap().unwrap();
    assert_eq!(
        (audit.state, audit.error.clone(), audit.at),
        (DepsAutoState::Applied, None, t(13))
    );
    assert!(!s.auto_applied(id, t(14)).unwrap());
    assert_eq!(s.deps_auto(id).unwrap().unwrap(), audit);
    assert!(s.delete_deps_auto(id).unwrap());
    assert!(!s.delete_deps_auto(id).unwrap());
    assert!(
        s.auto_applied(ProposalId::try_from(100u64).unwrap(), t(15))
            .is_err()
    );
    let draft = NewProposal {
        origin: Some(sid()),
        tree: None,
        head: None,
        ..proposal()
    };
    let id = s.record_proposal(&draft, None, t(16)).unwrap().unwrap();
    s.set_deps_auto(id, DepsAutoState::Installing, None, t(17))
        .unwrap();
    assert!(!s.auto_applied(id, t(18)).unwrap());
    assert_eq!(
        s.proposal(id).unwrap().unwrap().status,
        ProposalStatus::AwaitingPublish
    );
    assert_eq!(
        s.deps_auto(id).unwrap().unwrap().state,
        DepsAutoState::Installing
    );
}

#[test]
fn fetch_failures_preserve_last_success() {
    let env = env();
    let s = env.open();
    s.record_fetch(&repo(), false, Some("offline"), t(10))
        .unwrap();
    assert_eq!(s.repo_fetch(&repo()).unwrap().unwrap().ok_at, None);
    s.record_fetch(&repo(), true, None, t(20)).unwrap();
    s.record_fetch(&repo(), false, Some("timeout"), t(30))
        .unwrap();
    assert_eq!(
        s.repo_fetch(&repo()).unwrap().unwrap(),
        RepoFetch {
            at: t(30),
            ok: false,
            error: Some("timeout".into()),
            ok_at: Some(t(20))
        }
    );
    s.record_fetch(&repo(), true, None, t(40)).unwrap();
    assert_eq!(s.repo_fetch(&repo()).unwrap().unwrap().ok_at, Some(t(40)));
}

#[test]
fn sync_report_replacement_is_atomic_and_hunks_are_uncapped() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let row = store_report(&s);
    assert_eq!(s.sync_report(&sid(), &repo()).unwrap(), vec![row.clone()]);
    let mut duplicate = row.clone();
    duplicate.kind = "binary".into();
    assert!(
        s.put_sync_report(
            &sid(),
            &repo(),
            &head(),
            1,
            &[duplicate.clone(), duplicate.clone()]
        )
        .is_err()
    );
    assert_eq!(s.sync_report(&sid(), &repo()).unwrap(), vec![row]);
    duplicate.ours_blob = None;
    s.put_sync_report(&sid(), &repo(), &head(), 1, &[duplicate.clone()])
        .unwrap();
    assert_eq!(s.sync_report(&sid(), &repo()).unwrap(), vec![duplicate]);
    s.delete_sync_report(&sid(), &repo()).unwrap();
    assert!(s.sync_report(&sid(), &repo()).unwrap().is_empty());
}

#[test]
fn successful_move_clears_sync_report_and_records_agent_policy() {
    for consuming in [false, true] {
        let env = env();
        let s = env.open();
        world(&env, &s);
        store_report(&s);
        let baseline = MoveBaseline {
            scope: Default::default(),
            m0_ns: 0,
            t0_ns: 0,
            root_names: vec![],
            entries: 0,
        };
        let n = s
            .begin_move_with_policy(
                &sid(),
                &repo(),
                &gen1(),
                &head(),
                &env.root.join("sessions").join(sid().as_str()).join(".move"),
                t(20),
                Some(&baseline),
                MoveActivityPolicy::Agent,
            )
            .unwrap();
        assert_eq!(
            s.move_activity(&sid(), &repo(), n).unwrap().unwrap().policy,
            MoveActivityPolicy::Agent
        );
        s.set_move_state(
            &sid(),
            &repo(),
            n,
            MoveState::Preparing,
            MoveState::Swapping,
            Some(&[]),
            t(21),
        )
        .unwrap();
        if consuming {
            s.finish_move_consuming(&sid(), &repo(), n, &[], t(22), Some(0))
                .unwrap();
        } else {
            s.finish_move(&sid(), &repo(), n, &[], t(22)).unwrap();
        }
        assert!(s.sync_report(&sid(), &repo()).unwrap().is_empty());
    }
}

#[test]
fn accepted_recovery_and_session_removal_clear_sync_report() {
    for accept in [false, true] {
        let env = env();
        let s = env.open();
        world(&env, &s);
        store_report(&s);
        let n = s
            .begin_move(
                &sid(),
                &repo(),
                &gen1(),
                &head(),
                &env.root.join("sessions").join(sid().as_str()).join(".move"),
                t(20),
            )
            .unwrap();
        s.set_move_state(
            &sid(),
            &repo(),
            n,
            MoveState::Preparing,
            MoveState::Swapping,
            Some(&[]),
            t(21),
        )
        .unwrap();
        s.fail_move(&sid(), &repo(), n, &json!({"reason":"test"}), t(22))
            .unwrap();
        assert_eq!(s.sync_report(&sid(), &repo()).unwrap().len(), 1);
        s.resolve_move(&sid(), &repo(), n, accept, t(23)).unwrap();
        assert_eq!(s.sync_report(&sid(), &repo()).unwrap().is_empty(), accept);
        store_report(&s);
        s.set_lifecycle(&sid(), Lifecycle::Active, Lifecycle::Removing, None, t(24))
            .unwrap();
        s.set_lifecycle(&sid(), Lifecycle::Removing, Lifecycle::Removed, None, t(25))
            .unwrap();
        assert!(s.sync_report(&sid(), &repo()).unwrap().is_empty());
    }
}

#[test]
fn clear_conflicts_is_atomic_and_keeps_other_paths() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let n = s
        .begin_move(
            &sid(),
            &repo(),
            &gen1(),
            &head(),
            &env.root.join("sessions").join(sid().as_str()).join(".move"),
            t(20),
        )
        .unwrap();
    let conflicts = vec![rp("a"), rp("b")];
    s.set_move_state(
        &sid(),
        &repo(),
        n,
        MoveState::Preparing,
        MoveState::Swapping,
        Some(&conflicts),
        t(21),
    )
    .unwrap();
    s.finish_move(&sid(), &repo(), n, &conflicts, t(22))
        .unwrap();
    assert!(
        s.clear_conflicts(&sid(), &repo(), &[rp("a"), rp("unrelated")])
            .is_err()
    );
    assert_eq!(
        s.session(&sid()).unwrap().unwrap().workspaces[&repo()].conflicts,
        conflicts
    );
    s.clear_conflicts(&sid(), &repo(), &[rp("a")]).unwrap();
    let rec = s.session(&sid()).unwrap().unwrap();
    assert_eq!(rec.workspaces[&repo()].conflicts, vec![rp("b")]);
    assert_eq!(rec.workspaces[&repo()].generation, n);
}

#[test]
fn sync_move_history_keeps_resolution_choices() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let n = s
        .begin_move(
            &sid(),
            &repo(),
            &gen1(),
            &head(),
            &env.root.join("sessions").join(sid().as_str()).join(".move"),
            t(20),
        )
        .unwrap();
    s.record_sync_resolution(
        &sid(),
        &repo(),
        n,
        &json!({"resolved":["a"],"kept":["b"],"whole_file":[]}),
    )
    .unwrap();
    s.set_move_state(
        &sid(),
        &repo(),
        n,
        MoveState::Preparing,
        MoveState::Swapping,
        Some(&[]),
        t(21),
    )
    .unwrap();
    s.finish_move(&sid(), &repo(), n, &[], t(22)).unwrap();
    let v = s.sync_move_history(&sid(), &repo()).unwrap();
    assert_eq!(v[0]["state"], "done");
    assert_eq!(v[0]["kept"], json!(["b"]));
    assert!(
        s.record_sync_resolution(&sid(), &repo(), n, &json!({}))
            .is_err()
    );
    s.close().unwrap();
    let s = env.open();
    assert_eq!(s.sync_move_history(&sid(), &repo()).unwrap(), v);
}

#[test]
fn activity_scope_roundtrips_and_corruption_checks_the_whole_tree() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let baseline = MoveBaseline {
        scope: strata_store::ActivityScope { fold: false, skip: vec!["node_modules".into()], exceptions: vec!["node_modules/.cache".into()] },
        m0_ns: 1, t0_ns: 2, root_names: vec!["node_modules".into()], entries: 3,
    };
    let n = s.begin_move_with_policy(&sid(), &repo(), &gen1(), &head(), &env.root.join("sessions").join(sid().as_str()).join(".move"), t(20), Some(&baseline), MoveActivityPolicy::Agent).unwrap();
    assert_eq!(s.move_activity(&sid(), &repo(), n).unwrap().unwrap().baseline, Some(baseline));
    let c = connect(&env.db());
    for value in [None, Some("not json"), Some(r#"{"skip":["../outside"],"exceptions":[]}"#)] {
        c.execute("UPDATE move_activity SET skip_json = ?1", [value]).unwrap();
        assert_eq!(s.move_activity(&sid(), &repo(), n).unwrap().unwrap().baseline.unwrap().scope, Default::default());
    }
}

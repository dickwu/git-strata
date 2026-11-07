//! rm-followups §1.6: the `rebase_pending` writers (plan, promote, defer,
//! discard, replay plan/settle) and consumption in `finish_move`.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use strata_store::*;
use strata_types::{
    Batch, BaseGen, BaseManifest, Checkpoint, CheckpointId, CheckpointReason, CommitOid, DepsSet, DepsSetId, EntryKind, OverlayEntry,
    ReleaseGroup, RepoName, RepoPath, SessionId, SessionManifest, TreeOid, WorkspaceState,
};
use support::*;

fn repo(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}
fn set_id(c: char) -> DepsSetId {
    DepsSetId::try_from(c.to_string().repeat(64)).unwrap()
}
fn tree(c: char) -> TreeOid {
    TreeOid::try_from(c.to_string().repeat(40)).unwrap()
}
fn head(c: char) -> CommitOid {
    CommitOid::try_from(c.to_string().repeat(40)).unwrap()
}
fn hex(c: char) -> String {
    c.to_string().repeat(40)
}
fn sess(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}
fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(secs * 1_000_000_000 + 7)
}
fn g(tr: char) -> BaseGen {
    BaseGen { repo: repo("app"), tree: tree(tr), deps: set_id('a') }
}

/// Store with app generations 1..=4 (deps a) and an active session whose
/// app workspace sits on generation 1 (`tracked` as given).
fn world(tracked: bool) -> (Env, Store, SessionId) {
    let env = env();
    let s = env.open();
    let set = DepsSet {
        id: set_id('a'),
        repo: repo("app"),
        inputs_hash: "0".repeat(64),
        platform: "darwin-arm64".into(),
        toolchain: "none".into(),
        dirs: vec![],
    };
    s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
    for c in ['1', '2', '3', '4'] {
        s.record_base_gen(&g(c), &BaseManifest { tree: tree(c), entries: BTreeMap::new() }, t(1)).unwrap();
    }
    let id = sess("aaaaaaaa");
    let root = env.root.join("sessions").join(id.as_str());
    let mut repos = BTreeMap::new();
    repos.insert(repo("app"), WorkspaceState { base: g('1'), head: head('1'), path: root.join("app"), tracked });
    s.create_session(&SessionManifest { id: id.clone(), ticket: None, agent: None, root, repos }, t(10)).unwrap();
    assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
    (env, s, id)
}

fn publish(s: &Store, id: &SessionId, at: u64) -> u64 {
    let blob = s.put_bytes(b"x").unwrap();
    let entry = OverlayEntry { path: RepoPath::try_from("f".to_string()).unwrap(), kind: EntryKind::Added { blob }, mode: 0o100644 };
    let cp = Checkpoint {
        id: CheckpointId::try_from(999u64).unwrap(),
        session: id.clone(),
        repo: repo("app"),
        base: g('1'),
        at: t(at),
        reason: CheckpointReason::Manual { message: "m".into() },
        entries: vec![entry],
    };
    let c = s.record_checkpoint(cp).unwrap();
    let b = Batch { session: id.clone(), repo: repo("app"), checkpoint: c, paths: vec![RepoPath::try_from("f".to_string()).unwrap()] };
    s.record_publish(&ReleaseGroup { id: 0, batches: vec![b] }, t(at)).unwrap()
}

fn row(s: &Store, id: &SessionId) -> Option<RebasePending> {
    s.rebase_pending_of(id, &repo("app")).unwrap()
}

/// Move the app workspace to generation `to` and finish with `consume`.
fn moved(env: &Env, s: &Store, id: &SessionId, to: char, consume: Option<u64>) {
    let stg: PathBuf = env.root.join("sessions").join(id.as_str()).join(format!(".m{to}"));
    let n = s.begin_move(id, &repo("app"), &g(to), &head(to), &stg, t(40)).unwrap();
    assert!(s.set_move_state(id, &repo("app"), n, MoveState::Preparing, MoveState::Swapping, Some(&[]), t(41)).unwrap());
    s.finish_move_consuming(id, &repo("app"), n, &[], t(42), consume).unwrap();
}

#[test]
fn record_publish_plans_and_promotion_commits_the_target() {
    let (_env, s, id) = world(true);
    let p1 = publish(&s, &id, 20);
    let r = row(&s, &id).expect("planned row");
    assert_eq!((r.planned_publish, r.head.as_deref(), r.rev, r.reason.as_str()), (Some(p1), None, 0, "planned"));
    assert_eq!((r.from_tree.as_str(), r.attempts), (hex('1').as_str(), 0));

    // promotion with a commit: target set, plan cleared, rev bumped
    assert!(s.promote_pending(&id, &repo("app"), 0, p1, Some((&hex('3'), &hex('3'))), "pending", t(25)).unwrap());
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head, r.tree, r.planned_publish, r.rev), (Some(hex('3')), Some(hex('3')), None, 1));
    assert_eq!(r.detail, Some(json!({ "publish": p1 })));
    assert!(!s.promote_pending(&id, &repo("app"), 0, p1, None, "pending", t(26)).unwrap(), "stale rev");

    // a second publish plans on top of the committed target
    let p2 = publish(&s, &id, 30);
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head.as_deref(), r.planned_publish, r.rev), (Some(hex('3').as_str()), Some(p2), 1));
    assert!(!s.clear_planned_publish(&id, &repo("app"), p1).unwrap(), "another publish's plan stays");
    assert!(s.clear_planned_publish(&id, &repo("app"), p2).unwrap());
    assert_eq!(row(&s, &id).unwrap().head, Some(hex('3')));
}

#[test]
fn a_plan_alone_is_dropped_when_cleared_or_promoted_without_a_commit() {
    let (_env, s, id) = world(true);
    let p = publish(&s, &id, 20);
    assert!(s.clear_planned_publish(&id, &repo("app"), p).unwrap());
    assert!(row(&s, &id).is_none());
    let p = publish(&s, &id, 21);
    assert!(s.promote_pending(&id, &repo("app"), 0, p, None, "pending", t(22)).unwrap());
    assert!(row(&s, &id).is_none());
}

#[test]
fn defer_and_discard_are_conditioned_on_rev() {
    let (_env, s, id) = world(true);
    assert!(!s.defer_pending(&id, &repo("app"), 0, "busy", &json!({}), t(20)).unwrap(), "no row, no insert");
    assert!(row(&s, &id).is_none());
    let p = publish(&s, &id, 20);
    assert!(!s.defer_pending(&id, &repo("app"), 0, "busy", &json!({}), t(21)).unwrap(), "no committed target");
    assert!(s.promote_pending(&id, &repo("app"), 0, p, Some((&hex('3'), &hex('3'))), "pending", t(22)).unwrap());
    assert!(!s.defer_pending(&id, &repo("app"), 0, "busy", &json!({}), t(23)).unwrap(), "stale rev");
    assert!(s.defer_pending(&id, &repo("app"), 1, "open_files", &json!({ "publish": p, "open": [] }), t(24)).unwrap());
    assert!(s.defer_pending(&id, &repo("app"), 1, "busy", &json!({ "publish": p }), t(25)).unwrap());
    let r = row(&s, &id).unwrap();
    assert_eq!((r.reason.as_str(), r.attempts, r.rev, r.last_at), ("busy", 2, 1, t(25)));
    assert!(s.set_pending_reason(&id, &repo("app"), 1, "publish_unfinished").unwrap());
    assert!(!s.discard_pending_target(&id, &repo("app"), 0, "stale_target", &json!({}), t(26)).unwrap());
    assert!(s.discard_pending_target(&id, &repo("app"), 1, "stale_target", &json!({}), t(26)).unwrap());
    assert!(row(&s, &id).is_none(), "nothing left on the row");
}

#[test]
fn finish_move_consumes_only_the_attempted_target() {
    let (env, s, id) = world(true);
    let p = publish(&s, &id, 20);
    assert!(s.promote_pending(&id, &repo("app"), 0, p, Some((&hex('3'), &hex('3'))), "pending", t(22)).unwrap());
    // a move with a stale rev keeps the target and rebases `from`
    moved(&env, &s, &id, '2', Some(7));
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head.as_deref(), r.from_tree.as_str()), (Some(hex('3').as_str()), hex('2').as_str()));
    // the attempted rev is consumed, and a plan alongside it survives
    let p2 = publish(&s, &id, 30);
    moved(&env, &s, &id, '3', Some(1));
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head, r.planned_publish, r.from_tree.as_str()), (None, Some(p2), hex('3').as_str()));
    // an unattributed move (None) consumes a target with the same head
    assert!(s.promote_pending(&id, &repo("app"), r.rev, p2, Some((&hex('4'), &hex('4'))), "pending", t(45)).unwrap());
    moved(&env, &s, &id, '4', None);
    assert!(row(&s, &id).is_none());
}

#[test]
fn replay_plans_are_adopted_or_cleared() {
    let (_env, s, id) = world(false);
    let plan = |h: char| vec![ReplayPlan { session: id.clone(), planned_head: hex(h), planned_tree: hex(h) }];
    s.plan_replay(&repo("app"), &plan('3'), t(20)).unwrap();
    let r = row(&s, &id).unwrap();
    assert_eq!((r.planned_head.as_deref(), r.head.as_deref(), r.reason.as_str()), (Some(hex('3').as_str()), None, "replay"));
    assert!(!s.settle_planned(&id, &repo("app"), &hex('4'), true, t(21)).unwrap(), "another plan");
    assert!(s.settle_planned(&id, &repo("app"), &hex('3'), true, t(21)).unwrap());
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head, r.planned_head, r.rev, r.reason.as_str()), (Some(hex('3')), None, 1, "pending"));
    // a failed replay clears its plan and leaves the committed target
    s.plan_replay(&repo("app"), &plan('4'), t(22)).unwrap();
    assert!(s.settle_planned(&id, &repo("app"), &hex('4'), false, t(23)).unwrap());
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head, r.planned_head), (Some(hex('3')), None));
}

/// Codex P2 B5: the tracking requirement is its own flag. It is set with
/// `publish_unfinished` and on an adopted replay copy, survives deferrals
/// that overwrite `reason`, and goes with the target (a clean promotion,
/// a consuming move, a discard).
#[test]
fn unpushed_survives_deferrals_and_goes_with_the_target() {
    let (env, s, id) = world(true);
    let p = publish(&s, &id, 20);
    assert!(s.promote_pending(&id, &repo("app"), 0, p, Some((&hex('3'), &hex('3'))), "publish_unfinished", t(22)).unwrap());
    assert!(row(&s, &id).unwrap().unpushed);
    assert!(s.defer_pending(&id, &repo("app"), 1, "busy", &json!({ "publish": p }), t(23)).unwrap());
    let r = row(&s, &id).unwrap();
    assert_eq!((r.reason.as_str(), r.unpushed), ("busy", true));
    // a clean later publish replaces the target and the requirement
    let p2 = publish(&s, &id, 30);
    assert!(s.promote_pending(&id, &repo("app"), 1, p2, Some((&hex('4'), &hex('4'))), "pending", t(31)).unwrap());
    assert!(!row(&s, &id).unwrap().unpushed);
    // set_pending_reason(publish_unfinished) sets it; another reason keeps it
    assert!(s.set_pending_reason(&id, &repo("app"), 2, "publish_unfinished").unwrap());
    assert!(s.set_pending_reason(&id, &repo("app"), 2, "pending").unwrap());
    assert!(row(&s, &id).unwrap().unpushed);
    // a replay's adopted copy must reach the tracking ref too
    let plan = vec![ReplayPlan { session: id.clone(), planned_head: hex('3'), planned_tree: hex('3') }];
    assert!(s.promote_pending(&id, &repo("app"), 2, p2, None, "pending", t(32)).is_ok());
    s.plan_replay(&repo("app"), &plan, t(40)).unwrap();
    assert!(s.settle_planned(&id, &repo("app"), &hex('3'), true, t(41)).unwrap());
    let r = row(&s, &id).unwrap();
    assert_eq!((r.head.as_deref(), r.reason.as_str(), r.unpushed), (Some(hex('3').as_str()), "pending", true));
    // a consuming move clears it with the target
    moved(&env, &s, &id, '3', Some(r.rev));
    assert!(row(&s, &id).is_none());
}

#[test]
fn main_pending_cas_rewrites_base_and_preserves_only_requested_backoff() {
    let (_env, s, id) = world(true);
    assert!(s.enroll_main(&id, &repo("app"), &g('1'), &head('2'), &tree('2'), Some(set_id('b')), t(10)).unwrap());
    let first = row(&s, &id).unwrap();
    assert_eq!(first.source, PendingSource::Main);
    assert!(!s.enroll_main(&id, &repo("app"), &g('1'), &head('3'), &tree('3'), None, t(11)).unwrap());
    assert_eq!(row(&s, &id).unwrap(), first);
    assert!(s.defer_pending(&id, &repo("app"), first.rev, "open_files", &json!({"holder": 1}), t(20)).unwrap());
    assert!(!s.retarget_pending(&id, &repo("app"), first.rev+1, &head('3'), &tree('3'), &g('2'), true, t(30)).unwrap());
    assert!(s.retarget_pending(&id, &repo("app"), first.rev, &head('3'), &tree('3'), &g('2'), true, t(30)).unwrap());
    let next = row(&s, &id).unwrap();
    assert_eq!(next.rev, first.rev + 1);
    assert!(next.to_deps.is_none());
    assert_eq!((next.from_tree, next.from_deps), (hex('2'), set_id('a').to_string()));
    assert_eq!((next.attempts, next.last_at, next.reason.as_str()), (1, t(20), "open_files"));
    assert!(!s.set_pending_to_deps(&id, &repo("app"), first.rev, Some(set_id('c')), t(40)).unwrap());
    assert!(s.set_pending_to_deps(&id, &repo("app"), next.rev, Some(set_id('c')), t(40)).unwrap());
    let next = row(&s, &id).unwrap();
    assert_eq!((next.to_deps, next.attempts, next.last_at), (Some(set_id('c')), 0, t(40)));
    assert!(s.defer_pending(&id, &repo("app"), next.rev, "conflicts", &json!({}), t(45)).unwrap());
    assert!(s.retarget_pending(&id, &repo("app"), next.rev, &head('4'), &tree('4'), &g('3'), false, t(50)).unwrap());
    let next = row(&s, &id).unwrap();
    assert_eq!((next.attempts, next.last_at), (0, t(50)));
    assert!(!s.drop_pending(&id, &repo("app"), first.rev).unwrap());
    assert!(s.drop_pending(&id, &repo("app"), next.rev).unwrap());
    assert!(row(&s, &id).is_none());
}

#[test]
fn to_deps_nulled_by_publish_promotion() {
    let (_env, s, id) = world(true);
    s.enroll_main(&id, &repo("app"), &g('1'), &head('2'), &tree('2'), Some(set_id('b')), t(10)).unwrap();
    let p = publish(&s, &id, 20);
    let rev = row(&s, &id).unwrap().rev;
    assert!(s.promote_pending(&id, &repo("app"), rev, p, Some((&hex('3'), &hex('3'))), "pending", t(30)).unwrap());
    let next = row(&s, &id).unwrap();
    assert!(next.to_deps.is_none());
    assert_eq!(next.source, PendingSource::Publish);
}

#[test]
fn replay_adoption_and_consumed_plans_clear_to_deps() {
    let (env, s, id) = world(true);
    s.enroll_main(&id, &repo("app"), &g('1'), &head('2'), &tree('2'), Some(set_id('b')), t(10)).unwrap();
    s.plan_replay(&repo("app"), &[ReplayPlan { session: id.clone(), planned_head: hex('3'), planned_tree: hex('3') }], t(11)).unwrap();
    s.settle_planned(&id, &repo("app"), &hex('3'), true, t(12)).unwrap();
    let r = row(&s, &id).unwrap();
    assert_eq!(r.source, PendingSource::Replay);
    assert!(r.to_deps.is_none());
    s.set_pending_to_deps(&id, &repo("app"), r.rev, Some(set_id('c')), t(13)).unwrap();
    let publish = publish(&s, &id, 14);
    let r = row(&s, &id).unwrap();
    moved(&env, &s, &id, '3', Some(r.rev));
    let r = row(&s, &id).unwrap();
    assert_eq!(r.planned_publish, Some(publish));
    assert!(r.head.is_none() && r.to_deps.is_none());
}

// ---- auto-close §3.2: the target writers and the guard on `Removing` ------

/// A second session of the same store, on `app` at generation 1, left
/// `Creating` until the caller moves it on.
fn creating(env: &Env, s: &Store, name: &str, tracked: bool) -> SessionId {
    let id = sess(name);
    let root = env.root.join("sessions").join(id.as_str());
    let mut repos = BTreeMap::new();
    repos.insert(repo("app"), WorkspaceState { base: g('1'), head: head('1'), path: root.join("app"), tracked });
    s.create_session(&SessionManifest { id: id.clone(), ticket: None, agent: None, root, repos }, t(10)).unwrap();
    id
}

fn second(env: &Env, s: &Store, name: &str) -> SessionId {
    let id = creating(env, s, name, true);
    assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
    id
}

fn replay_plan(id: &SessionId, c: char) -> ReplayPlan {
    ReplayPlan { session: id.clone(), planned_head: hex(c), planned_tree: hex(c) }
}

fn to_removing(s: &Store, id: &SessionId) {
    assert!(s.set_lifecycle(id, Lifecycle::Active, Lifecycle::Removing, None, t(30)).unwrap());
}

fn to_removed(s: &Store, id: &SessionId) {
    assert!(s.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Removed, None, t(31)).unwrap());
}

/// `enroll_main`, `retarget_pending`, `settle_planned(adopt)` and
/// `promote_pending(Some)` all answer `false` and write nothing; `plan_replay`
/// skips only the leaving session.
#[track_caller]
fn assert_writers_refuse(s: &Store, empty: &SessionId, full: &SessionId, other: &SessionId, rev: u64) {
    assert!(!s.enroll_main(empty, &repo("app"), &g('1'), &head('2'), &tree('2'), None, t(40)).unwrap());
    assert!(row(s, empty).is_none(), "no row for a session leaving for removal");
    let before = row(s, full);
    assert!(!s.retarget_pending(full, &repo("app"), rev, &head('4'), &tree('4'), &g('1'), false, t(41)).unwrap());
    assert!(!s.settle_planned(full, &repo("app"), &hex('3'), true, t(42)).unwrap());
    assert!(!s.promote_pending(full, &repo("app"), rev, 99, Some((&hex('4'), &hex('4'))), "pending", t(43)).unwrap());
    assert_eq!(row(s, full), before);
    s.plan_replay(&repo("app"), &[replay_plan(empty, '4'), replay_plan(other, '4')], t(44)).unwrap();
    assert!(row(s, empty).is_none(), "a leaving session's plan is skipped");
    assert_eq!(row(s, other).unwrap().planned_head, Some(hex('4')), "the other plan stands");
    assert!(s.settle_planned(other, &repo("app"), &hex('4'), false, t(45)).unwrap());
}

#[test]
fn target_writers_refuse_a_session_leaving_for_removal() {
    let (env, s, empty) = world(true);
    let full = second(&env, &s, "bbbbbbbb");
    let other = second(&env, &s, "cccccccc");
    s.enroll_main(&full, &repo("app"), &g('1'), &head('2'), &tree('2'), None, t(20)).unwrap();
    s.plan_replay(&repo("app"), &[replay_plan(&full, '3')], t(21)).unwrap();
    let rev = row(&s, &full).unwrap().rev;

    to_removing(&s, &empty);
    to_removing(&s, &full);
    assert_writers_refuse(&s, &empty, &full, &other, rev);
    assert_eq!(row(&s, &full).unwrap().head, Some(hex('2')));
    assert_eq!(row(&s, &full).unwrap().planned_head, Some(hex('3')));

    // What only clears is unchanged.
    assert!(s.promote_pending(&full, &repo("app"), rev, 99, None, "pending", t(46)).unwrap());
    assert!(s.settle_planned(&full, &repo("app"), &hex('3'), false, t(47)).unwrap());
    assert_eq!(row(&s, &full).unwrap().planned_head, None);

    to_removed(&s, &empty);
    to_removed(&s, &full);
    assert!(row(&s, &full).is_none(), "Removed deletes the row");
    assert_writers_refuse(&s, &empty, &full, &other, rev);
}

#[test]
fn replay_writers_keep_creating_and_unknown_main_writers_do_not() {
    let (env, s, _) = world(true);
    let scene = json!({ "op": "remove" });
    let unknown = second(&env, &s, "bbbbbbbb");
    to_removing(&s, &unknown);
    assert!(s.set_lifecycle(&unknown, Lifecycle::Removing, Lifecycle::Unknown, Some(&scene), t(31)).unwrap());
    let making = creating(&env, &s, "cccccccc", true);
    for id in [&unknown, &making] {
        assert!(!s.enroll_main(id, &repo("app"), &g('1'), &head('2'), &tree('2'), None, t(40)).unwrap());
        assert!(row(&s, id).is_none());
        s.plan_replay(&repo("app"), &[replay_plan(id, '3')], t(41)).unwrap();
        assert_eq!(row(&s, id).unwrap().planned_head, Some(hex('3')));
        assert!(s.settle_planned(id, &repo("app"), &hex('3'), true, t(42)).unwrap());
        let r = row(&s, id).unwrap();
        assert_eq!(r.head, Some(hex('3')));
        assert!(!s.retarget_pending(id, &repo("app"), r.rev, &head('4'), &tree('4'), &g('1'), false, t(43)).unwrap());
        assert_eq!(row(&s, id).unwrap().head, Some(hex('3')));
        assert!(s.promote_pending(id, &repo("app"), r.rev, 99, Some((&hex('4'), &hex('4'))), "pending", t(44)).unwrap());
        assert_eq!(row(&s, id).unwrap().head, Some(hex('4')));
    }
}

fn keys_of(s: &Store, id: &SessionId) -> Vec<PendingKey> {
    s.rebase_pending(Some(id)).unwrap().iter().map(PendingKey::of).collect()
}

fn lifecycle(s: &Store, id: &SessionId) -> Lifecycle {
    s.session(id).unwrap().unwrap().lifecycle
}

#[test]
fn set_removing_if_unchanged_compares_every_key() {
    let (_env, s, id) = world(true);
    let guard = |keys: &[PendingKey]| s.set_removing_if_unchanged(&id, Lifecycle::Active, keys, t(50)).unwrap();

    // A new row.
    let keys = keys_of(&s, &id);
    s.enroll_main(&id, &repo("app"), &g('1'), &head('2'), &tree('2'), None, t(20)).unwrap();
    assert_eq!(guard(&keys), IfUnchanged::PendingMoved);
    assert_eq!(lifecycle(&s, &id), Lifecycle::Active);

    // A moved `rev` and head.
    let keys = keys_of(&s, &id);
    let rev = row(&s, &id).unwrap().rev;
    assert!(s.retarget_pending(&id, &repo("app"), rev, &head('3'), &tree('3'), &g('1'), false, t(21)).unwrap());
    assert_eq!(guard(&keys), IfUnchanged::PendingMoved);

    // A set `planned_head`.
    let keys = keys_of(&s, &id);
    s.plan_replay(&repo("app"), &[replay_plan(&id, '4')], t(22)).unwrap();
    assert_eq!(guard(&keys), IfUnchanged::PendingMoved);

    // A set `planned_publish`.
    let keys = keys_of(&s, &id);
    publish(&s, &id, 23);
    assert_eq!(guard(&keys), IfUnchanged::PendingMoved);
    assert_eq!(lifecycle(&s, &id), Lifecycle::Active);

    // A deferral keeps the keys.
    let keys = keys_of(&s, &id);
    let rev = row(&s, &id).unwrap().rev;
    assert!(s.defer_pending(&id, &repo("app"), rev, "recent_writes", &json!({}), t(24)).unwrap());
    assert_eq!(keys_of(&s, &id), keys);

    // A moved lifecycle.
    assert_eq!(
        s.set_removing_if_unchanged(&id, Lifecycle::Unknown, &keys, t(50)).unwrap(),
        IfUnchanged::LifecycleMoved
    );
    assert_eq!(lifecycle(&s, &id), Lifecycle::Active);

    // Equal keys.
    assert_eq!(guard(&keys), IfUnchanged::Removing);
    assert_eq!(lifecycle(&s, &id), Lifecycle::Removing);
    assert_eq!(guard(&keys), IfUnchanged::LifecycleMoved, "only once");
}

/// A proposal for `app`: `origin` with no target is a draft; with `target`
/// it is `pending`.
fn proposal(origin: Option<&SessionId>, to: char, target: bool) -> NewProposal {
    NewProposal {
        repo: repo("app"),
        origin: origin.cloned(),
        from: set_id('a'),
        from_inputs: "a".repeat(64),
        from_head: None,
        to: set_id(to),
        to_inputs: to.to_string().repeat(64),
        tree: target.then(|| tree('5')),
        head: target.then(|| head('5')),
    }
}

/// The items of a refused guard over an unchanged row set.
#[track_caller]
fn reopened(s: &Store, id: &SessionId) -> Vec<serde_json::Value> {
    match s.set_removing_if_unchanged(id, Lifecycle::Active, &keys_of(s, id), t(50)).unwrap() {
        IfUnchanged::Reopened(items) => {
            assert_eq!(lifecycle(s, id), Lifecycle::Active, "nothing moved");
            items
        }
        other => panic!("expected Reopened, got {other:?}"),
    }
}

#[track_caller]
fn commits(s: &Store, id: &SessionId) {
    let got = s.set_removing_if_unchanged(id, Lifecycle::Active, &keys_of(s, id), t(50)).unwrap();
    assert_eq!(got, IfUnchanged::Removing);
    assert_eq!(lifecycle(s, id), Lifecycle::Removing);
}

#[test]
fn set_removing_if_unchanged_reads_clause_6_proposals_on_a_tracked_repo() {
    // A `pending` proposal to another set.
    let (_e, s, id) = world(true);
    let p = s.record_proposal(&proposal(None, 'b', true), None, t(20)).unwrap().unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["code"], "deps_pending");
    assert_eq!(items[0]["repo"], "app");
    assert_eq!(items[0]["proposal"], json!(p.get()));
    assert_eq!(items[0]["state"], "pending");
    assert_eq!(items[0]["own"], false);
    assert_eq!(items[0]["tracked"], true);
    assert!(s.proposal(p).unwrap().is_some());

    // One to the base's own set concerns nobody.
    let (_e, s, id) = world(true);
    let mut same = proposal(None, 'a', true);
    same.from = set_id('b');
    s.record_proposal(&same, None, t(20)).unwrap().unwrap();
    commits(&s, &id);

    // This session's draft refuses; another session's does not.
    let (_e, s, id) = world(true);
    s.record_proposal(&proposal(Some(&id), 'b', false), None, t(20)).unwrap().unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["state"], "awaiting_publish");
    assert_eq!(items[0]["own"], true);
    assert_eq!(items[0]["tracked"], true);

    let (env, s, id) = world(true);
    let other = second(&env, &s, "bbbbbbbb");
    s.record_proposal(&proposal(Some(&other), 'b', false), None, t(20)).unwrap().unwrap();
    commits(&s, &id);
}

#[test]
fn set_removing_if_unchanged_reads_clause_6_on_an_untracked_repo() {
    // Another origin's `pending` on a repo this session has untracked stays open
    // for the sessions it concerns.
    let (_e, s, id) = world(false);
    s.record_proposal(&proposal(None, 'b', true), None, t(20)).unwrap().unwrap();
    commits(&s, &id);

    // This session's own draft there would vanish with it.
    let (_e, s, id) = world(false);
    s.record_proposal(&proposal(Some(&id), 'b', false), None, t(20)).unwrap().unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["own"], true);
    assert_eq!(items[0]["tracked"], false);
    assert_eq!(items[0]["state"], "awaiting_publish");

    // Its own `pending` there refuses too.
    let (_e, s, id) = world(false);
    s.record_proposal(&proposal(Some(&id), 'b', true), None, t(20)).unwrap().unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items[0]["state"], "pending");
    assert_eq!(items[0]["own"], true);
    assert_eq!(items[0]["tracked"], false);

    // Its `applied` and `superseded` ones do not.
    let (_e, s, id) = world(false);
    let a = s.record_proposal(&proposal(Some(&id), 'b', true), None, t(20)).unwrap().unwrap();
    assert!(s.decide_proposal(a, ProposalDecision::Applied, t(21)).unwrap());
    commits(&s, &id);

    let (_e, s, id) = world(false);
    let a = s.record_proposal(&proposal(Some(&id), 'b', true), None, t(20)).unwrap().unwrap();
    s.record_proposal(&proposal(None, 'c', true), Some(a), t(21)).unwrap().unwrap();
    assert_eq!(s.proposal(a).unwrap().unwrap().status, ProposalStatus::Superseded);
    commits(&s, &id);
}

#[test]
fn a_proposal_found_by_both_reads_is_reported_once() {
    let (_e, s, id) = world(true);
    s.record_proposal(&proposal(Some(&id), 'b', true), None, t(20)).unwrap().unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["own"], true);
    assert_eq!(items[0]["tracked"], true);
}

#[test]
fn set_removing_if_unchanged_reads_clause_6_conflicts() {
    // A deferral whose reason is `conflicts` keeps the keys and still refuses.
    let (_e, s, id) = world(true);
    s.enroll_main(&id, &repo("app"), &g('1'), &head('2'), &tree('2'), None, t(20)).unwrap();
    let rev = row(&s, &id).unwrap().rev;
    let keys = keys_of(&s, &id);
    assert!(s.defer_pending(&id, &repo("app"), rev, "conflicts", &json!({}), t(21)).unwrap());
    assert_eq!(keys_of(&s, &id), keys);
    let items = reopened(&s, &id);
    assert_eq!(items[0]["code"], "conflicts");
    assert_eq!(items[0]["auto_reason"], "conflicts");
    assert_eq!(items[0]["repo"], "app");

    // A workspace row with conflicts.
    let (env, s, id) = world(true);
    let path = RepoPath::try_from("src/c.ts".to_string()).unwrap();
    let stg: PathBuf = env.root.join("sessions").join(id.as_str()).join(".m2");
    let n = s.begin_move(&id, &repo("app"), &g('2'), &head('2'), &stg, t(20)).unwrap();
    let conflicts = [path];
    assert!(s.set_move_state(&id, &repo("app"), n, MoveState::Preparing, MoveState::Swapping, Some(&conflicts), t(21)).unwrap());
    s.finish_move(&id, &repo("app"), n, &conflicts, t(22)).unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items[0]["code"], "conflicts");
    assert_eq!(items[0]["conflicts"], json!(["src/c.ts"]));
    assert!(items[0].get("report").is_none());

    // A saved `sync` report.
    let (_e, s, id) = world(true);
    let row = SyncReportRow {
        target: head('2'),
        generation: 1,
        path: RepoPath::try_from("src/p.ts".to_string()).unwrap(),
        kind: "content".into(),
        ours_blob: None,
        hunks: json!([]),
    };
    s.put_sync_report(&id, &repo("app"), &head('2'), 1, &[row]).unwrap();
    let items = reopened(&s, &id);
    assert_eq!(items[0]["code"], "conflicts");
    assert_eq!(items[0]["report"], true);
    assert_eq!(items[0]["conflicts"], json!(["src/p.ts"]));
}

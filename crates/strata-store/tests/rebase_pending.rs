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

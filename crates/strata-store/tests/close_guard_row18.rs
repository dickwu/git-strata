//! Auto-close §10 row 18 (store half): `set_removing_if_unchanged` reads
//! clause 6 in the transaction that commits `Removing`.

mod support;

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use strata_store::*;
use strata_types::{
    BaseGen, BaseManifest, CommitOid, DepsSet, DepsSetId, RepoName, RepoPath, SessionId, SessionManifest, TreeOid,
    WorkspaceState,
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
fn sess(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}
fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(secs * 1_000_000_000 + 7)
}
fn g(tr: char) -> BaseGen {
    BaseGen { repo: repo("app"), tree: tree(tr), deps: set_id('a') }
}

/// Two Active sessions, `aaaaaaaa` (the one closing) and `bbbbbbbb`, each
/// with an `app` workspace on generation 1 (`tracked` as given for the first).
fn world(tracked: bool) -> (Env, Store, SessionId, SessionId) {
    let env = env();
    let s = env.open();
    for c in ['a', 'c'] {
        let set = DepsSet {
            id: set_id(c),
            repo: repo("app"),
            inputs_hash: c.to_string().repeat(64),
            platform: "darwin-arm64".into(),
            toolchain: "none".into(),
            dirs: vec![],
        };
        s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
    }
    s.record_base_gen(&g('1'), &BaseManifest { tree: tree('1'), entries: BTreeMap::new() }, t(1)).unwrap();
    let mut ids = Vec::new();
    for (name, tr) in [("aaaaaaaa", tracked), ("bbbbbbbb", true)] {
        let id = sess(name);
        let root = env.root.join("sessions").join(id.as_str());
        let mut repos = BTreeMap::new();
        repos.insert(repo("app"), WorkspaceState { base: g('1'), head: head('1'), path: root.join("app"), tracked: tr });
        s.create_session(&SessionManifest { id: id.clone(), ticket: None, agent: None, root, repos }, t(10)).unwrap();
        assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
        ids.push(id);
    }
    let o = ids.pop().unwrap();
    (env, s, ids.pop().unwrap(), o)
}

fn proposal(s: &Store, origin: Option<&SessionId>, from: char, to: char, pending: bool) -> i64 {
    replacing(s, origin, from, to, pending, None)
}

fn replacing(s: &Store, origin: Option<&SessionId>, from: char, to: char, pending: bool, replaces: Option<i64>) -> i64 {
    let p = NewProposal {
        repo: repo("app"),
        origin: origin.cloned(),
        from: set_id(from),
        from_inputs: from.to_string().repeat(64),
        from_head: Some(head('1')),
        to: set_id(to),
        to_inputs: to.to_string().repeat(64),
        tree: pending.then(|| tree('2')),
        head: pending.then(|| head('2')),
    };
    let id = s.record_proposal(&p, replaces.map(|r| serde_json::from_value(json!(r)).unwrap()), t(20)).unwrap().unwrap();
    serde_json::to_value(id).unwrap().as_i64().unwrap()
}

fn keys(s: &Store, id: &SessionId) -> Vec<PendingKey> {
    s.rebase_pending_of(id, &repo("app")).unwrap().iter().map(PendingKey::of).collect()
}

fn judge(s: &Store, id: &SessionId) -> IfUnchanged {
    s.set_removing_if_unchanged(id, Lifecycle::Active, &keys(s, id), t(30)).unwrap()
}

fn lifecycle(s: &Store, id: &SessionId) -> Lifecycle {
    s.session(id).unwrap().unwrap().lifecycle
}

/// `Reopened` with exactly one item; that item. The lifecycle is unchanged.
fn reopened(s: &Store, id: &SessionId) -> Value {
    match judge(s, id) {
        IfUnchanged::Reopened(items) => {
            assert_eq!(lifecycle(s, id), Lifecycle::Active);
            assert_eq!(items.len(), 1, "{items:?}");
            items[0].clone()
        }
        other => panic!("expected Reopened, got {other:?}"),
    }
}

fn removing(s: &Store, id: &SessionId) {
    assert_eq!(judge(s, id), IfUnchanged::Removing);
    assert_eq!(lifecycle(s, id), Lifecycle::Removing);
}

fn decide(s: &Store, p: i64, to: ProposalDecision) {
    let pid = serde_json::from_value(json!(p)).unwrap();
    assert!(s.decide_proposal(pid, to, t(25)).unwrap());
}

#[test]
fn set_removing_if_unchanged_reads_clause_6() {
    // A `pending` proposal with another `to_set` on a tracked repo.
    let (_e, s, id, _o) = world(true);
    let p = proposal(&s, None, 'a', 'c', true);
    let it = reopened(&s, &id);
    assert_eq!((it["code"].as_str(), it["proposal"].as_i64(), it["state"].as_str()), (Some("deps_pending"), Some(p), Some("pending")));
    assert_eq!((it["own"].as_bool(), it["tracked"].as_bool()), (Some(false), Some(true)));

    // `to_set` equal to the base's deps.
    let (_e, s, id, _o) = world(true);
    proposal(&s, None, 'c', 'a', true);
    removing(&s, &id);

    // This session's own `awaiting_publish` draft.
    let (_e, s, id, _o) = world(true);
    let p = proposal(&s, Some(&id), 'a', 'c', false);
    let it = reopened(&s, &id);
    assert_eq!((it["code"].as_str(), it["proposal"].as_i64(), it["state"].as_str()), (Some("deps_pending"), Some(p), Some("awaiting_publish")));
    assert_eq!(it["own"], true);

    // Another session's draft.
    let (_e, s, id, o) = world(true);
    proposal(&s, Some(&o), 'a', 'c', false);
    removing(&s, &id);

    // r9: untracked repo, a `pending` of another origin → Removing.
    let (_e, s, id, o) = world(false);
    proposal(&s, Some(&o), 'a', 'c', true);
    removing(&s, &id);
    let (_e, s, id, _o) = world(false);
    proposal(&s, None, 'a', 'c', true);
    removing(&s, &id);

    // r9: untracked repo, this session's own `awaiting_publish` or `pending`.
    for pending in [false, true] {
        let (_e, s, id, _o) = world(false);
        let p = proposal(&s, Some(&id), 'a', 'c', pending);
        let it = reopened(&s, &id);
        assert_eq!((it["code"].as_str(), it["proposal"].as_i64()), (Some("deps_pending"), Some(p)), "{it}");
        assert_eq!(it["state"], if pending { "pending" } else { "awaiting_publish" });
        assert_eq!((it["own"].as_bool(), it["tracked"].as_bool()), (Some(true), Some(false)), "{it}");
    }

    // Its `applied` or `superseded` one → Removing.
    let (_e, s, id, _o) = world(false);
    let p = proposal(&s, Some(&id), 'a', 'c', true);
    decide(&s, p, ProposalDecision::Applied);
    removing(&s, &id);
    let (_e, s, id, _o) = world(false);
    let p = proposal(&s, Some(&id), 'a', 'c', true);
    let q = replacing(&s, None, 'a', 'c', true, Some(p));
    let old = format!("{:?}", s.proposal(serde_json::from_value(json!(p)).unwrap()).unwrap().unwrap());
    assert!(old.contains("Superseded"), "{old}");
    decide(&s, q, ProposalDecision::Applied);
    removing(&s, &id);

    // A row with `reason = 'conflicts'`.
    let (_e, s, id, _o) = world(true);
    assert!(s.enroll_main(&id, &repo("app"), &g('1'), &head('2'), &tree('2'), None, t(20)).unwrap());
    let rev = s.rebase_pending_of(&id, &repo("app")).unwrap().unwrap().rev;
    assert!(s.defer_pending(&id, &repo("app"), rev, "conflicts", &json!({"paths": ["f"]}), t(21)).unwrap());
    let it = reopened(&s, &id);
    assert_eq!((it["code"].as_str(), it["auto_reason"].as_str()), (Some("conflicts"), Some("conflicts")), "{it}");

    // A workspace with `conflicts`.
    let (env, s, id, _o) = world(true);
    let stg = env.root.join("sessions").join(id.as_str()).join(".m1");
    let n = s.begin_move(&id, &repo("app"), &g('1'), &head('1'), &stg, t(40)).unwrap();
    let path = RepoPath::try_from("src/c.ts".to_string()).unwrap();
    assert!(s.set_move_state(&id, &repo("app"), n, MoveState::Preparing, MoveState::Swapping, Some(std::slice::from_ref(&path)), t(41)).unwrap());
    s.finish_move(&id, &repo("app"), n, std::slice::from_ref(&path), t(42)).unwrap();
    let it = reopened(&s, &id);
    assert_eq!((it["code"].as_str(), &it["conflicts"]), (Some("conflicts"), &json!(["src/c.ts"])), "{it}");

    // r9: a `sync_report` row.
    let (_e, s, id, _o) = world(true);
    let row = SyncReportRow { target: head('2'), generation: 1, path: RepoPath::try_from("f".to_string()).unwrap(),
        kind: "text".into(), ours_blob: None, hunks: json!([]) };
    s.put_sync_report(&id, &repo("app"), &head('2'), 1, &[row]).unwrap();
    let it = reopened(&s, &id);
    assert_eq!((it["code"].as_str(), it["report"].as_bool()), (Some("conflicts"), Some(true)), "{it}");
}

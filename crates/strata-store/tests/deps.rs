//! store.md §7, Task 3: deps sets, attempts, proposals and main.

mod support;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use strata_store::*;
use strata_types::{Code, CommitOid, DepsSet, DepsSetId, ProposalId, RepoName, RepoPath, SessionId, StrataError, TreeOid};
use support::*;

fn repo(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}
fn set_id(c: char) -> DepsSetId {
    DepsSetId::try_from(c.to_string().repeat(64)).unwrap()
}
fn inputs(c: char) -> String {
    c.to_string().repeat(64)
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
    UNIX_EPOCH + Duration::from_nanos(secs * 1_000_000_000 + 123)
}
fn deps_set(r: &str, c: char, dirs: &[&str]) -> DepsSet {
    DepsSet {
        id: set_id(c),
        repo: repo(r),
        inputs_hash: inputs('9'),
        platform: "darwin-arm64".into(),
        toolchain: "node-22".into(),
        dirs: dirs.iter().map(|d| RepoPath::try_from(d.to_string()).unwrap()).collect(),
    }
}
fn interrupted() -> StrataError {
    StrataError {
        code: Code::Unknown,
        message: "builder died".into(),
        detail: Some(serde_json::json!({ "reason": "interrupted" })),
    }
}
fn pending(r: &str, from: char, to: char, t_: char) -> NewProposal {
    NewProposal {
        repo: repo(r),
        origin: None,
        from: set_id(from),
        from_inputs: inputs(from),
        to: set_id(to),
        to_inputs: inputs(to),
        tree: Some(tree(t_)),
        head: Some(head(t_)),
    }
}
fn awaiting(r: &str, origin: &str, from: char, to: char) -> NewProposal {
    NewProposal { origin: Some(sess(origin)), tree: None, head: None, ..pending(r, from, to, '0') }
}
fn pid(n: u64) -> ProposalId {
    ProposalId::try_from(n).unwrap()
}
fn count(env: &Env, sql: &str) -> i64 {
    let (c, _) = raw(&env.db(), false);
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn pendings_per_repo_ok(s: &Store) {
    let all = s.proposals(None, false).unwrap();
    let mut seen = std::collections::BTreeMap::new();
    for p in all.iter().filter(|p| p.status == ProposalStatus::Pending) {
        *seen.entry(p.repo.clone()).or_insert(0) += 1;
    }
    assert!(seen.values().all(|n| *n <= 1), "{seen:?}");
}

#[test]
fn deps_attempt_is_an_intent_row() {
    let env = env();
    let s = env.open();
    let (r, a) = (repo("app"), set_id('a'));
    let n = s.begin_deps_attempt(&r, &a, AttemptKind::Build, t(10)).unwrap();
    assert_eq!(n, 1);
    let rows = s.deps_attempts(Some(&r)).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!((row.id, row.state, row.kind, row.pid), (1, AttemptState::Running, AttemptKind::Build, None));
    assert_eq!(row.staging, s.deps_staging_dir(&r, 1));
    assert_eq!((row.started, row.ended, row.error.clone()), (t(10), None, None));

    let e = s.begin_deps_attempt(&r, &a, AttemptKind::Adopt, t(11)).unwrap_err();
    assert_code(&e, Code::Exists);
    assert_eq!(detail(&e, "attempt"), Some(&serde_json::json!(1)));

    // pid: positive, once
    assert_code(&s.set_deps_attempt_pid(1, 0).unwrap_err(), Code::InvalidInput);
    assert_code(&s.set_deps_attempt_pid(1, -5).unwrap_err(), Code::InvalidInput);
    assert_code(&s.set_deps_attempt_pid(99, 42).unwrap_err(), Code::InvalidInput);
    assert!(s.set_deps_attempt_pid(1, 4242).unwrap());
    assert_code(&s.set_deps_attempt_pid(1, 4243).unwrap_err(), Code::InvalidInput);
    assert_eq!(s.deps_attempts(None).unwrap()[0].pid, Some(4242));

    // end: only to Failed/Refused, CAS
    for bad in [AttemptState::Running, AttemptState::Sealed] {
        assert_code(&s.end_deps_attempt(1, bad, &interrupted(), t(12)).unwrap_err(), Code::InvalidInput);
    }
    assert_code(&s.end_deps_attempt(99, AttemptState::Failed, &interrupted(), t(12)).unwrap_err(), Code::InvalidInput);
    assert!(s.end_deps_attempt(1, AttemptState::Failed, &interrupted(), t(12)).unwrap());
    let row = &s.deps_attempts(Some(&r)).unwrap()[0];
    assert_eq!((row.state, row.ended, row.error.clone()), (AttemptState::Failed, Some(t(12)), Some(interrupted())));
    assert!(!s.end_deps_attempt(1, AttemptState::Refused, &interrupted(), t(13)).unwrap());
    assert!(!s.set_deps_attempt_pid(1, 77).unwrap());
    assert_eq!(s.deps_attempts(None).unwrap()[0].state, AttemptState::Failed);

    // a new attempt on the same set gets a new number
    let n2 = s.begin_deps_attempt(&r, &a, AttemptKind::Adopt, t(14)).unwrap();
    assert_eq!(n2, 2);
    let refused = StrataError { code: Code::InvalidInput, message: "nope".into(), detail: None };
    assert!(s.end_deps_attempt(2, AttemptState::Refused, &refused, t(15)).unwrap());
    let n3 = s.begin_deps_attempt(&repo("other"), &a, AttemptKind::Build, t(16)).unwrap();
    assert_eq!(n3, 3);

    let all = s.deps_attempts(None).unwrap();
    assert_eq!(all.iter().map(|a| a.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(all[1].error, Some(refused));
    assert_eq!(all[1].state, AttemptState::Refused);
    assert_eq!(s.deps_attempts(Some(&r)).unwrap().len(), 2);
    assert_eq!(all[2].staging, s.deps_staging_dir(&repo("other"), 3));
}

#[test]
fn deps_set_seals_its_attempt() {
    let env = env();
    let s = env.open();
    let r = repo("app");
    let built = deps_set("app", 'a', &["node_modules"]);
    let adopt_n = s.begin_deps_attempt(&r, &set_id('a'), AttemptKind::Adopt, t(1)).unwrap();
    let other_n = s.begin_deps_attempt(&r, &set_id('b'), AttemptKind::Build, t(1)).unwrap();
    let dead_n = s.begin_deps_attempt(&r, &set_id('c'), AttemptKind::Build, t(1)).unwrap();
    assert!(s.end_deps_attempt(dead_n, AttemptState::Failed, &interrupted(), t(2)).unwrap());

    let mismatches: Vec<(DepsSet, DepsOrigin, Option<u64>)> = vec![
        (deps_set("app", 'e', &[]), DepsOrigin::Empty, Some(adopt_n)),       // Empty with an attempt
        (deps_set("app", 'e', &["node_modules"]), DepsOrigin::Empty, None), // Empty with dirs
        (built.clone(), DepsOrigin::Build, Some(adopt_n)),                  // Build with an Adopt attempt
        (built.clone(), DepsOrigin::Adopt, None),                           // no attempt
        (built.clone(), DepsOrigin::Adopt, Some(other_n)),                  // another set's attempt
        (deps_set("app", 'c', &["node_modules"]), DepsOrigin::Build, Some(dead_n)), // attempt ended
        (deps_set("other", 'a', &["node_modules"]), DepsOrigin::Adopt, Some(adopt_n)), // other repo
        (built.clone(), DepsOrigin::Adopt, Some(99)),                       // no such attempt
    ];
    for (set, origin, attempt) in mismatches {
        let e = s.record_deps_set(&set, origin, attempt, t(3)).unwrap_err();
        assert_code(&e, Code::InvalidInput);
    }
    assert_eq!(count(&env, "SELECT count(*) FROM deps_sets"), 0);
    assert_eq!(s.deps_attempts(None).unwrap()[0].state, AttemptState::Running);

    s.record_deps_set(&built, DepsOrigin::Adopt, Some(adopt_n), t(4)).unwrap();
    let rec = s.deps_set(&r, &set_id('a')).unwrap().unwrap();
    assert_eq!(rec, DepsSetRecord { set: built.clone(), origin: DepsOrigin::Adopt, sealed: t(4) });
    let a = &s.deps_attempts(None).unwrap()[0];
    assert_eq!((a.state, a.ended, a.error.clone()), (AttemptState::Sealed, Some(t(4)), None));

    // already recorded → Exists, nothing written
    assert_code(&s.record_deps_set(&built, DepsOrigin::Adopt, Some(adopt_n), t(5)).unwrap_err(), Code::Exists);
    assert_code(&s.record_deps_set(&deps_set("app", 'a', &[]), DepsOrigin::Empty, None, t(5)).unwrap_err(), Code::Exists);
    assert_eq!(s.deps_set(&r, &set_id('a')).unwrap().unwrap().sealed, t(4));

    // Empty
    let empty = deps_set("app", 'e', &[]);
    s.record_deps_set(&empty, DepsOrigin::Empty, None, t(6)).unwrap();
    assert_eq!(s.deps_set(&r, &set_id('e')).unwrap().unwrap().origin, DepsOrigin::Empty);
    // same id in another repo is a different set
    assert!(s.deps_set(&repo("other"), &set_id('a')).unwrap().is_none());
    // Build seals a Build attempt
    let multi = deps_set("app", 'b', &["node_modules", "packages/x/node_modules"]);
    s.record_deps_set(&multi, DepsOrigin::Build, Some(other_n), t(7)).unwrap();
    assert_eq!(s.deps_set(&r, &set_id('b')).unwrap().unwrap().set.dirs, multi.dirs);
    assert_eq!(s.deps_attempts(None).unwrap()[1].state, AttemptState::Sealed);
}

#[test]
fn proposal_slots_supersede_in_one_transaction() {
    let env = env();
    let s = env.open();
    let r = repo("app");

    // validation
    let mut same = pending("app", 'a', 'a', '1');
    assert_code(&s.record_proposal(&same, None, t(1)).unwrap_err(), Code::InvalidInput);
    same.to = set_id('b');
    same.head = None;
    assert_code(&s.record_proposal(&same, None, t(1)).unwrap_err(), Code::InvalidInput);
    let mut no_target = pending("app", 'a', 'b', '1');
    no_target.tree = None;
    no_target.head = None;
    assert_code(&s.record_proposal(&no_target, None, t(1)).unwrap_err(), Code::InvalidInput);
    assert_eq!(count(&env, "SELECT count(*) FROM deps_proposals"), 0);

    // Pending slot
    let p1 = s.record_proposal(&pending("app", 'a', 'b', '1'), None, t(2)).unwrap().unwrap();
    assert_eq!(p1, pid(1));
    // stale replaces → None, nothing written
    assert_eq!(s.record_proposal(&pending("app", 'a', 'c', '2'), None, t(3)).unwrap(), None);
    assert_eq!(s.record_proposal(&pending("app", 'a', 'c', '2'), Some(pid(7)), t(3)).unwrap(), None);
    assert_eq!(count(&env, "SELECT count(*) FROM deps_proposals"), 1);
    let p2 = s.record_proposal(&pending("app", 'a', 'c', '2'), Some(p1), t(3)).unwrap().unwrap();
    assert_eq!(p2, pid(2));
    let old = s.proposal(p1).unwrap().unwrap();
    assert_eq!((old.status, old.superseded_by, old.updated, old.created), (ProposalStatus::Superseded, Some(p2), t(3), t(2)));
    let new = s.proposal(p2).unwrap().unwrap();
    assert_eq!((new.status, new.tree.clone(), new.head.clone()), (ProposalStatus::Pending, Some(tree('2')), Some(head('2'))));
    assert_eq!((new.created, new.updated, new.origin.clone()), (t(3), t(3), None));
    pendings_per_repo_ok(&s);

    // a Pending in another repo has its own slot
    let q = s.record_proposal(&pending("lib", 'a', 'b', '3'), None, t(4)).unwrap().unwrap();
    pendings_per_repo_ok(&s);

    // AwaitingPublish slots are per (repo, origin session)
    let w1 = s.record_proposal(&awaiting("app", "aaaaaaaa", 'a', 'd'), None, t(5)).unwrap().unwrap();
    let w2 = s.record_proposal(&awaiting("app", "bbbbbbbb", 'a', 'd'), None, t(5)).unwrap().unwrap();
    assert_eq!(s.record_proposal(&awaiting("app", "aaaaaaaa", 'a', 'e'), None, t(6)).unwrap(), None);
    assert_eq!(s.record_proposal(&awaiting("app", "aaaaaaaa", 'a', 'e'), Some(p2), t(6)).unwrap(), None);
    let w3 = s.record_proposal(&awaiting("app", "aaaaaaaa", 'a', 'e'), Some(w1), t(6)).unwrap().unwrap();
    assert_eq!(s.proposal(w1).unwrap().unwrap().superseded_by, Some(w3));
    assert_eq!(s.proposal(w2).unwrap().unwrap().status, ProposalStatus::AwaitingPublish);
    assert_eq!(s.proposal(p2).unwrap().unwrap().status, ProposalStatus::Pending);
    assert_eq!(s.proposal(q).unwrap().unwrap().status, ProposalStatus::Pending);
    let w = s.proposal(w3).unwrap().unwrap();
    assert_eq!((w.status, w.origin.clone(), w.tree.clone()), (ProposalStatus::AwaitingPublish, Some(sess("aaaaaaaa")), None));
    // a Pending with an origin session still takes the repo's Pending slot
    let mut with_origin = pending("app", 'a', 'f', '4');
    with_origin.origin = Some(sess("aaaaaaaa"));
    assert_eq!(s.record_proposal(&with_origin, Some(w3), t(7)).unwrap(), None);
    let p3 = s.record_proposal(&with_origin, Some(p2), t(7)).unwrap().unwrap();
    assert_eq!(s.proposal(w3).unwrap().unwrap().status, ProposalStatus::AwaitingPublish);
    assert_eq!(s.proposal(p2).unwrap().unwrap().superseded_by, Some(p3));
    pendings_per_repo_ok(&s);

    // listing: by id; repo filter; open_only
    let ids: Vec<u64> = s.proposals(None, false).unwrap().iter().map(|p| p.id.get()).collect();
    assert_eq!(ids, (1..=p3.get()).collect::<Vec<_>>());
    assert!(s.proposals(Some(&r), false).unwrap().iter().all(|p| p.repo == r));
    // only session aaaaaaaa exists (and is not removed): its AwaitingPublish is open, bbbbbbbb's is not
    let (c, _) = raw(&env.db(), false);
    c.execute(
        "INSERT INTO sessions (id, root, lifecycle, created_ns) VALUES ('s-aaaaaaaa', '/tmp/x', 'creating', 1)",
        [],
    )
    .unwrap();
    let open: Vec<ProposalId> = s.proposals(Some(&r), true).unwrap().iter().map(|p| p.id).collect();
    assert_eq!(open, vec![w3, p3]);
    let open_all: Vec<ProposalId> = s.proposals(None, true).unwrap().iter().map(|p| p.id).collect();
    assert_eq!(open_all, vec![q, w3, p3]);
    assert!(s.proposal(pid(999)).unwrap().is_none());
}

#[test]
fn promote_and_decide_are_compare_and_swap() {
    let env = env();
    let s = env.open();
    let p = s.record_proposal(&pending("app", 'a', 'b', '1'), None, t(1)).unwrap().unwrap();
    let w_same = s.record_proposal(&awaiting("app", "aaaaaaaa", 'a', 'c'), None, t(2)).unwrap().unwrap();
    let w_other_to = s.record_proposal(&awaiting("app", "bbbbbbbb", 'a', 'd'), None, t(2)).unwrap().unwrap();
    let w = s.record_proposal(&awaiting("app", "cccccccc", 'a', 'c'), None, t(3)).unwrap().unwrap();
    let other_repo = s.record_proposal(&awaiting("lib", "dddddddd", 'a', 'c'), None, t(3)).unwrap().unwrap();
    let lib_pending = s.record_proposal(&pending("lib", 'a', 'b', '1'), None, t(3)).unwrap().unwrap();

    assert_code(&s.promote_proposal(pid(99), &tree('5'), &head('5'), t(4)).unwrap_err(), Code::InvalidInput);
    assert!(!s.promote_proposal(p, &tree('5'), &head('5'), t(4)).unwrap());
    assert_eq!(s.proposal(p).unwrap().unwrap().tree, Some(tree('1')));

    assert!(s.promote_proposal(w, &tree('5'), &head('6'), t(5)).unwrap());
    let got = s.proposal(w).unwrap().unwrap();
    assert_eq!((got.status, got.tree, got.head, got.updated), (ProposalStatus::Pending, Some(tree('5')), Some(head('6')), t(5)));
    for (id, status, by) in [
        (p, ProposalStatus::Superseded, Some(w)),
        (w_same, ProposalStatus::Superseded, Some(w)),
        (w_other_to, ProposalStatus::AwaitingPublish, None),
        (other_repo, ProposalStatus::AwaitingPublish, None),
        (lib_pending, ProposalStatus::Pending, None),
    ] {
        let r = s.proposal(id).unwrap().unwrap();
        assert_eq!((r.status, r.superseded_by), (status, by), "proposal {id}");
    }
    assert_eq!(s.proposal(p).unwrap().unwrap().updated, t(5));
    pendings_per_repo_ok(&s);
    assert!(!s.promote_proposal(w, &tree('7'), &head('7'), t(6)).unwrap());

    // decide
    assert_code(&s.decide_proposal(pid(99), ProposalDecision::Applied, t(7)).unwrap_err(), Code::InvalidInput);
    assert_code(&s.decide_proposal(w, ProposalDecision::Superseded { by: w }, t(7)).unwrap_err(), Code::InvalidInput);
    assert_code(&s.decide_proposal(w, ProposalDecision::Superseded { by: pid(99) }, t(7)).unwrap_err(), Code::InvalidInput);
    assert!(!s.decide_proposal(w_other_to, ProposalDecision::Applied, t(7)).unwrap()); // AwaitingPublish
    assert!(!s.decide_proposal(p, ProposalDecision::Applied, t(7)).unwrap()); // Superseded
    assert!(s.decide_proposal(w, ProposalDecision::Applied, t(8)).unwrap());
    let got = s.proposal(w).unwrap().unwrap();
    assert_eq!((got.status, got.updated, got.superseded_by), (ProposalStatus::Applied, t(8), None));
    assert!(!s.decide_proposal(w, ProposalDecision::Applied, t(9)).unwrap());
    assert!(!s.decide_proposal(w, ProposalDecision::Superseded { by: p }, t(9)).unwrap());
    assert!(s.decide_proposal(lib_pending, ProposalDecision::Superseded { by: other_repo }, t(10)).unwrap());
    let got = s.proposal(lib_pending).unwrap().unwrap();
    assert_eq!((got.status, got.superseded_by), (ProposalStatus::Superseded, Some(other_repo)));
    assert!(!s.decide_proposal(lib_pending, ProposalDecision::Applied, t(11)).unwrap());
    assert_eq!(s.proposal(w).unwrap().unwrap().status, ProposalStatus::Applied);
    assert!(s.proposals(None, true).unwrap().iter().all(|p| p.status != ProposalStatus::Pending));
}

#[test]
fn proposal_ids_never_reused() {
    let env = env();
    let s = env.open();
    let a = s.record_proposal(&pending("a", 'a', 'b', '1'), None, t(1)).unwrap().unwrap();
    let b = s.record_proposal(&pending("b", 'a', 'b', '1'), None, t(1)).unwrap().unwrap();
    let c = s.record_proposal(&pending("c", 'a', 'b', '1'), None, t(1)).unwrap().unwrap();
    assert_eq!((a, b, c), (pid(1), pid(2), pid(3)));
    let n1 = s.begin_deps_attempt(&repo("a"), &set_id('a'), AttemptKind::Build, t(1)).unwrap();
    let n2 = s.begin_deps_attempt(&repo("a"), &set_id('b'), AttemptKind::Build, t(1)).unwrap();
    assert_eq!((n1, n2), (1, 2));

    let (raw_c, _) = raw(&env.db(), false);
    // outside gc the delete triggers refuse
    assert!(raw_c.execute("DELETE FROM deps_proposals WHERE id = 3", []).is_err());
    let (gc, _) = raw(&env.db(), true);
    assert_eq!(gc.execute("DELETE FROM deps_proposals WHERE id = 3", []).unwrap(), 1);
    assert_eq!(gc.execute("DELETE FROM deps_attempts WHERE id = 2", []).unwrap(), 1);
    drop(gc);

    let d = s.record_proposal(&pending("c", 'a', 'b', '1'), None, t(2)).unwrap().unwrap();
    assert_eq!(d, pid(4));
    let n3 = s.begin_deps_attempt(&repo("a"), &set_id('b'), AttemptKind::Build, t(2)).unwrap();
    assert_eq!(n3, 3);
    assert_eq!(s.deps_attempts(None).unwrap()[1].staging, s.deps_staging_dir(&repo("a"), 3));

    // the counters survive a reopen
    s.close().unwrap();
    let s = env.open();
    let e = s.record_proposal(&pending("e", 'a', 'b', '1'), None, t(3)).unwrap().unwrap();
    assert_eq!(e, pid(5));
    assert_eq!(s.begin_deps_attempt(&repo("x"), &set_id('a'), AttemptKind::Adopt, t(3)).unwrap(), 4);

    // at the top: TooLarge, nothing written
    let (raw_c, _) = raw(&env.db(), false);
    raw_c
        .execute("UPDATE sqlite_sequence SET seq = 9223372036854775807 WHERE name IN ('deps_proposals', 'deps_attempts')", [])
        .unwrap();
    let e = s.record_proposal(&pending("f", 'a', 'b', '1'), None, t(4)).unwrap_err();
    assert_code(&e, Code::TooLarge);
    assert_eq!(detail(&e, "limit"), Some(&serde_json::json!("proposal_id")));
    let e = s.begin_deps_attempt(&repo("y"), &set_id('a'), AttemptKind::Adopt, t(4)).unwrap_err();
    assert_code(&e, Code::TooLarge);
    assert_eq!(detail(&e, "limit"), Some(&serde_json::json!("attempt_id")));
    assert_eq!(s.proposals(None, false).unwrap().len(), 4);
    assert_eq!(s.deps_attempts(None).unwrap().len(), 3);
}

#[test]
fn deps_main_is_compare_and_swap() {
    let env = env();
    let s = env.open();
    let row = |t_: char, set: char, at: u64| DepsMain {
        repo: repo("app"),
        tree: tree(t_),
        head: head(t_),
        inputs_hash: inputs(set),
        set: set_id(set),
        observed: t(at),
    };
    let (r1, r2, r3) = (row('1', 'a', 1), row('2', 'b', 2), row('3', 'c', 3));
    assert_eq!(s.deps_main(&repo("app")).unwrap(), None);
    // set need not be recorded
    assert!(s.deps_set(&repo("app"), &set_id('a')).unwrap().is_none());
    assert_eq!(s.set_deps_main(&r1, None).unwrap(), (true, None));
    assert_eq!(s.deps_main(&repo("app")).unwrap(), Some(r1.clone()));
    assert_eq!(s.set_deps_main(&r2, Some(&r1)).unwrap(), (true, Some(r1.clone())));
    assert_eq!(s.set_deps_main(&r3, Some(&r1)).unwrap(), (false, Some(r2.clone())));
    assert_eq!(s.deps_main(&repo("app")).unwrap(), Some(r2.clone()));
    // differs only in observed
    let mut near = r2.clone();
    near.observed = r2.observed + Duration::from_nanos(1);
    assert_eq!(s.set_deps_main(&r3, Some(&near)).unwrap(), (false, Some(r2.clone())));
    // differs only in head
    let mut other_head = r2.clone();
    other_head.head = head('9');
    assert_eq!(s.set_deps_main(&r3, Some(&other_head)).unwrap(), (false, Some(r2.clone())));
    // expecting no row when one exists
    assert_eq!(s.set_deps_main(&r3, None).unwrap(), (false, Some(r2.clone())));
    assert_eq!(s.deps_main(&repo("app")).unwrap(), Some(r2.clone()));
    // expecting a row when there is none
    let mut lib = r1.clone();
    lib.repo = repo("lib");
    assert_eq!(s.set_deps_main(&lib, Some(&lib)).unwrap(), (false, None));
    assert_eq!(s.deps_main(&repo("lib")).unwrap(), None);
    // repo mismatch
    assert_code(&s.set_deps_main(&lib, Some(&r2)).unwrap_err(), Code::InvalidInput);
    assert_eq!(s.set_deps_main(&r3, Some(&r2)).unwrap(), (true, Some(r2)));
    assert_eq!(s.deps_main(&repo("app")).unwrap(), Some(r3));
    assert_eq!(s.set_deps_main(&lib, None).unwrap(), (true, None));
    assert_eq!(s.deps_main(&repo("lib")).unwrap(), Some(lib));
}

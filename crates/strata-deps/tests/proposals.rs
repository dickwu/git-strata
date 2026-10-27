//! deps.md §7, Task 7: proposals, the publish chain and main tracking.

mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use strata_deps::{
    ChainState, Proposal, ProposalOrigin, ProposalState, ProposeArgs, attempts, find_set,
    get_proposal, key_of, main_tip, observe_main, open_proposals, propose, publish_chain,
    test_seams,
};
use strata_store::{DepsMain, DepsOrigin, Lifecycle, NewProposal, ProposalDecision, Store};
use strata_types::{
    BaseGen, BaseManifest, Code, CommitOid, DepsSet, DepsSetId, ProposalId, SessionId,
    SessionManifest, TreeOid, WorkspaceState,
};

fn commit_oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}
fn tree_oid(s: &str) -> TreeOid {
    TreeOid::try_from(s.trim().to_string()).unwrap()
}
fn set(c: char) -> DepsSetId {
    DepsSetId::try_from(c.to_string().repeat(64)).unwrap()
}
fn hex(c: char) -> String {
    c.to_string().repeat(64)
}
fn sid(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}

/// A main-branch commit, its tree and an export of it.
#[derive(Clone)]
struct Tip {
    tree: TreeOid,
    head: CommitOid,
    dir: PathBuf,
}

thread_local! {
    static N: Cell<u32> = const { Cell::new(0) };
}

/// The main checkout's HEAD as a `Tip` (exported under a fresh name).
fn tip(fx: &Fx) -> Tip {
    let n = N.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    let head = fx.sb.head(&fx.main);
    Tip {
        tree: tree_oid(&fx.sb.tree_of(&fx.main, "HEAD")),
        head: commit_oid(&head),
        dir: fx.tree(&format!("t{n}")),
    }
}

/// A fixture with a bare origin seeded from the main checkout.
fn setup() -> Fx {
    let fx = Fx::new();
    fx.sb.bare_origin(&fx.main);
    fx
}

/// Force-push the main checkout's HEAD to origin's main and fetch it.
fn publish(fx: &Fx) {
    fx.sb.git(
        &fx.main,
        &["push", "-q", "-f", "origin", "HEAD:refs/heads/main"],
    );
    fx.git.fetch("origin", "main").unwrap();
}

/// Force-push `oid` to origin's main and fetch it.
fn push_oid(fx: &Fx, oid: &str) {
    fx.sb.git(
        &fx.main,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("{oid}:refs/heads/main"),
        ],
    );
    fx.git.fetch("origin", "main").unwrap();
}

/// Commit `files` on the main checkout, publish and return the new tip.
fn advance(fx: &Fx, files: &[(&str, &[u8])], msg: &str) -> Tip {
    fx.sb.commit(&fx.main, files, msg);
    publish(fx);
    tip(fx)
}

/// `git reset --hard <oid>` in the main checkout, force-push and fetch.
fn rewind(fx: &Fx, oid: &CommitOid) {
    fx.sb
        .git(&fx.main, &["reset", "-q", "--hard", oid.as_str()]);
    publish(fx);
}

fn observe(fx: &Fx, t: &Tip) -> strata_deps::Result<Option<Proposal>> {
    observe_main(&fx.ctx(), &t.tree, &t.head, &t.dir)
}

fn key(fx: &Fx, t: &Tip) -> (String, DepsSetId) {
    key_of(&fx.ctx(), &t.dir).unwrap()
}

fn main_row(fx: &Fx) -> Option<(TreeOid, CommitOid, String, DepsSetId)> {
    fx.store()
        .deps_main(&fx.repo)
        .unwrap()
        .map(|m| (m.tree, m.head, m.inputs_hash, m.set))
}

fn row_for(fx: &Fx, t: &Tip) -> Option<(TreeOid, CommitOid, String, DepsSetId)> {
    let (h, id) = key(fx, t);
    Some((t.tree.clone(), t.head.clone(), h, id))
}

fn all(fx: &Fx) -> Vec<Proposal> {
    fx.store()
        .proposals(Some(&fx.repo), false)
        .unwrap()
        .into_iter()
        .map(|r| get_proposal(fx.store(), r.id).unwrap())
        .collect()
}

fn pendings(fx: &Fx) -> Vec<Proposal> {
    all(fx)
        .into_iter()
        .filter(|p| p.state == ProposalState::Pending)
        .collect()
}

fn state(fx: &Fx, id: ProposalId) -> ProposalState {
    get_proposal(fx.store(), id).unwrap().state
}

fn target_of(p: &Proposal) -> (Option<TreeOid>, Option<CommitOid>) {
    (p.tree.clone(), p.head.clone())
}

fn some_target(t: &Tip) -> (Option<TreeOid>, Option<CommitOid>) {
    (Some(t.tree.clone()), Some(t.head.clone()))
}

/// A `git commit-tree` child of `parent` with the same tree.
fn child(fx: &Fx, parent: &str, msg: &str) -> String {
    let tree = fx.sb.tree_of(&fx.main, parent);
    fx.sb
        .git(&fx.main, &["commit-tree", &tree, "-p", parent, "-m", msg])
}

fn set_ref(fx: &Fx, name: &str, oid: &str) {
    fx.sb.git(&fx.main, &["update-ref", name, oid]);
}

fn del_ref(fx: &Fx, name: &str) {
    fx.sb.git(&fx.main, &["update-ref", "-d", name]);
}

type MainRow = Option<(TreeOid, CommitOid, String, DepsSetId)>;

fn refs_and_store(fx: &Fx) -> (String, Vec<Proposal>, MainRow) {
    (
        fx.sb.git(&fx.main, &["for-each-ref"]),
        all(fx),
        main_row(fx),
    )
}

#[test]
fn observe_main_files_proposal_when_main_set_changes() {
    let fx = setup();
    fx.git.fetch("origin", "main").unwrap();
    let t1 = tip(&fx);
    assert_eq!(main_tip(&fx.cfg, &fx.git).unwrap(), Some(t1.head.clone()));
    assert_eq!(observe(&fx, &t1).unwrap(), None);
    assert_eq!(main_row(&fx), row_for(&fx, &t1));
    let (h1, id1) = key(&fx, &t1);

    // Same deps set: only tree and head move.
    let t2 = advance(&fx, &[("README", b"readme\n")], "readme");
    assert_eq!(observe(&fx, &t2).unwrap(), None);
    assert_eq!(
        main_row(&fx),
        Some((t2.tree.clone(), t2.head.clone(), h1.clone(), id1.clone()))
    );
    assert!(all(&fx).is_empty());

    // bun.lock changes: a Pending from main's last record.
    let t3 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
    let (h3, id3) = key(&fx, &t3);
    let p3 = observe(&fx, &t3).unwrap().expect("proposal");
    assert_eq!(p3.state, ProposalState::Pending);
    assert_eq!(p3.origin, ProposalOrigin::Main);
    assert_eq!(
        (p3.from.clone(), p3.from_inputs.clone()),
        (id1.clone(), h1.clone())
    );
    assert_eq!(
        (p3.to.clone(), p3.to_inputs.clone()),
        (id3.clone(), h3.clone())
    );
    assert_eq!(target_of(&p3), some_target(&t3));
    assert_eq!(find_set(fx.store(), &fx.repo, &id3).unwrap(), None);
    assert_eq!(fx.sb.install_count(), 0);
    assert!(
        attempts(fx.store(), &fx.paths, Some(&fx.repo))
            .unwrap()
            .is_empty()
    );
    assert_eq!(main_row(&fx), row_for(&fx, &t3));
    assert_eq!(pendings(&fx), vec![p3.clone()]);

    // Another deps set: a new Pending supersedes the old one.
    let t4 = advance(&fx, &[("bun.lock", b"lock v3\n")], "lock 3");
    let p4 = observe(&fx, &t4).unwrap().expect("proposal");
    assert_eq!(p4.from, id3);
    assert_eq!(state(&fx, p3.id), ProposalState::Superseded { by: p4.id });
    assert_eq!(pendings(&fx), vec![p4.clone()]);

    // The tip moves with the same deps set while a Pending is open.
    let t5 = advance(&fx, &[("README", b"readme 2\n")], "readme 2");
    let p5 = observe(&fx, &t5).unwrap().expect("retargeted proposal");
    assert_ne!(p5.id, p4.id);
    assert_eq!(
        (&p5.origin, &p5.from, &p5.from_inputs, &p5.to, &p5.to_inputs),
        (&p4.origin, &p4.from, &p4.from_inputs, &p4.to, &p4.to_inputs)
    );
    assert_eq!(target_of(&p5), some_target(&t5));
    assert_eq!(state(&fx, p4.id), ProposalState::Superseded { by: p5.id });
    let n = all(&fx).len();
    assert_eq!(observe(&fx, &t5).unwrap(), None);
    assert_eq!(all(&fx).len(), n);
    assert_eq!(pendings(&fx), vec![p5.clone()]);

    // Only the toolchain changes: still a Pending, with equal inputs.
    fx.sb.set_bun_version("1.3.4");
    let p6 = observe(&fx, &t5).unwrap().expect("toolchain proposal");
    assert_eq!(p6.from, p5.to);
    assert_ne!(p6.to, p6.from);
    assert_eq!(p6.from_inputs, p6.to_inputs);
    assert_eq!(state(&fx, p5.id), ProposalState::Superseded { by: p6.id });
    assert_eq!(pendings(&fx), vec![p6]);
    assert_eq!(fx.sb.install_count(), 0);
}

#[test]
fn observe_main_validates_head_and_tree() {
    let fx = setup();
    fx.git.fetch("origin", "main").unwrap();
    let t1 = tip(&fx);
    let t2 = advance(&fx, &[("README", b"x\n")], "readme");
    let e = observe_main(&fx.ctx(), &t1.tree, &t2.head, &t2.dir).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);

    let other = fx.sb.init_project("other");
    let foreign = fx
        .sb
        .commit(&other, &[("only-here", b"foreign\n")], "foreign");
    let ft = tree_oid(&fx.sb.tree_of(&other, "HEAD"));
    let e = observe_main(&fx.ctx(), &ft, &commit_oid(&foreign), &t2.dir).unwrap_err();
    assert_eq!(e.code, Code::NoSuchCommit);

    assert_eq!(main_row(&fx), None);
    assert!(all(&fx).is_empty());
}

#[test]
fn observe_main_crash_before_main_cas_reproposes() {
    let fx = setup();
    fx.git.fetch("origin", "main").unwrap();
    let t1 = tip(&fx);
    assert_eq!(observe(&fx, &t1).unwrap(), None);
    let before = main_row(&fx);
    let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
    let (_, d_b) = key(&fx, &t2);

    test_seams::before_main_cas(Box::new(|| panic!("daemon dies before the main CAS")));
    let enters = fx.lock.enters();
    let r = catch_unwind(AssertUnwindSafe(|| observe(&fx, &t2)));
    assert!(r.is_err());
    test_seams::reset();
    let ps = pendings(&fx);
    assert_eq!(ps.len(), 1);
    let p = ps[0].clone();
    assert_eq!(p.to, d_b);
    assert_eq!(target_of(&p), some_target(&t2));
    assert_eq!(main_row(&fx), before);

    // The lock stand-in was released: the next call takes it again.
    let again = observe(&fx, &t2).unwrap().expect("same proposal");
    assert_eq!(fx.lock.enters(), enters + 2);
    assert_eq!(again.id, p.id);
    assert_eq!(all(&fx).len(), 1);
    assert_eq!(main_row(&fx), row_for(&fx, &t2));
    assert_eq!(observe(&fx, &t2).unwrap(), None);
    assert_eq!(all(&fx).len(), 1);
}

#[test]
fn publish_chain_classifies_against_origin_and_anchor() {
    let fx = setup();
    let check = |want: Option<(ChainState, &str, Option<&str>, Option<&str>)>,
                 tip_want: Option<&str>| {
        let before = refs_and_store(&fx);
        let got = publish_chain(&fx.git, "main").unwrap();
        match want {
            None => assert_eq!(got, None),
            Some((st, o, top, anchor)) => {
                let c = got.expect("chain");
                assert_eq!(c.branch, "main");
                assert_eq!(c.state, st);
                assert_eq!(c.origin, commit_oid(o));
                assert_eq!(c.top, top.map(commit_oid));
                assert_eq!(c.anchor, anchor.map(commit_oid));
            }
        }
        assert_eq!(
            main_tip(&fx.cfg, &fx.git).unwrap(),
            tip_want.map(commit_oid)
        );
        assert_eq!(refs_and_store(&fx), before);
    };
    const C: &str = "refs/strata/publish/main";
    const A: &str = "refs/strata/anchor/main";

    // Never fetched.
    check(None, None);

    let h0 = fx.sb.head(&fx.main);
    let h1 = child(&fx, &h0, "h1");
    push_oid(&fx, &h1);
    check(Some((ChainState::Absent, &h1, None, None)), Some(&h1));
    set_ref(&fx, A, &h1);
    check(Some((ChainState::Absent, &h1, None, Some(&h1))), Some(&h1));

    set_ref(&fx, C, &h1);
    check(
        Some((ChainState::Equal, &h1, Some(&h1), Some(&h1))),
        Some(&h1),
    );

    let h2 = child(&fx, &h1, "h2");
    let h3 = child(&fx, &h2, "h3");
    set_ref(&fx, C, &h3);
    check(
        Some((ChainState::Leads, &h1, Some(&h3), Some(&h1))),
        Some(&h3),
    );

    // A human pushed H2 and then H4 on top.
    let h4 = child(&fx, &h2, "h4");
    push_oid(&fx, &h2);
    push_oid(&fx, &h4);
    set_ref(&fx, C, &h2);
    check(
        Some((ChainState::Behind, &h4, Some(&h2), Some(&h1))),
        Some(&h4),
    );

    // A pushed publication rolled back.
    push_oid(&fx, &h2);
    set_ref(&fx, A, &h2);
    push_oid(&fx, &h1);
    check(
        Some((ChainState::Rewound, &h1, Some(&h2), Some(&h2))),
        Some(&h1),
    );

    // The human commit the chain rests on rolled back.
    set_ref(&fx, A, &h1);
    push_oid(&fx, &h0);
    check(
        Some((ChainState::Rewound, &h0, Some(&h2), Some(&h1))),
        Some(&h0),
    );

    // Diverged.
    let h5 = child(&fx, &h1, "h5");
    push_oid(&fx, &h5);
    check(
        Some((ChainState::Diverged, &h5, Some(&h2), Some(&h1))),
        Some(&h5),
    );

    // A chain without an anchor.
    push_oid(&fx, &h1);
    del_ref(&fx, A);
    check(Some((ChainState::Rewound, &h1, Some(&h2), None)), Some(&h1));

    // Rewound and diverged at once.
    let h6 = child(&fx, &h1, "h6");
    push_oid(&fx, &h6);
    set_ref(&fx, C, &h3);
    set_ref(&fx, A, &h2);
    check(
        Some((ChainState::Rewound, &h6, Some(&h3), Some(&h2))),
        Some(&h6),
    );
}

#[test]
fn main_tip_follows_pin_rule() {
    let mut fx = setup();
    assert_eq!(main_tip(&fx.cfg, &fx.git).unwrap(), None);
    let h0 = fx.sb.head(&fx.main);
    let h1 = child(&fx, &h0, "h1");
    push_oid(&fx, &h1);
    let before = refs_and_store(&fx);
    let tip_is = |fx: &Fx, want: &str| {
        assert_eq!(main_tip(&fx.cfg, &fx.git).unwrap(), Some(commit_oid(want)));
    };
    tip_is(&fx, &h1);

    let h2 = child(&fx, &h1, "h2");
    set_ref(&fx, "refs/strata/publish/main", &h2);
    set_ref(&fx, "refs/strata/anchor/main", &h1);
    let before_chain = refs_and_store(&fx);
    tip_is(&fx, &h2);
    fx.cfg.pin_unpushed = false;
    tip_is(&fx, &h1);
    fx.cfg.pin_unpushed = true;
    fx.cfg.publish.branch = "release".into();
    tip_is(&fx, &h1);
    fx.cfg.publish.branch = "main".into();
    tip_is(&fx, &h2);
    assert_eq!(refs_and_store(&fx), before_chain);

    set_ref(&fx, "refs/strata/publish/main", &h1);
    tip_is(&fx, &h1);
    set_ref(&fx, "refs/strata/publish/main", &h0);
    set_ref(&fx, "refs/strata/anchor/main", &h0);
    tip_is(&fx, &h1);
    assert_eq!(before.1, all(&fx));
    assert_eq!(before.2, main_row(&fx));
}

#[test]
fn observe_main_follows_main_rewind() {
    let fx = setup();
    fx.git.fetch("origin", "main").unwrap();
    let t1 = tip(&fx);
    let (a, d_a) = key(&fx, &t1);
    assert_eq!(observe(&fx, &t1).unwrap(), None);
    assert_eq!(main_row(&fx), row_for(&fx, &t1));
    assert!(all(&fx).is_empty());

    let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
    let (b, d_b) = key(&fx, &t2);
    let p2 = observe(&fx, &t2).unwrap().expect("P2");
    assert_eq!((&p2.from, &p2.to), (&d_a, &d_b));
    assert_eq!(target_of(&p2), some_target(&t2));
    assert_eq!(main_row(&fx), row_for(&fx, &t2));

    // A human resets main to H1 and force-pushes.
    rewind(&fx, &t1.head);
    assert_eq!(main_tip(&fx.cfg, &fx.git).unwrap(), Some(t1.head.clone()));
    let p1 = observe(&fx, &t1).unwrap().expect("P1");
    assert_eq!(
        (&p1.from, &p1.from_inputs, &p1.to, &p1.to_inputs),
        (&d_b, &b, &d_a, &a)
    );
    assert_eq!(target_of(&p1), some_target(&t1));
    assert_eq!(state(&fx, p2.id), ProposalState::Superseded { by: p1.id });
    assert_eq!(main_row(&fx), row_for(&fx, &t1));

    // Another round: forward to H3 (README only), then back to H2.
    rewind(&fx, &t2.head);
    let t3 = advance(&fx, &[("README", b"readme\n")], "readme");
    assert_eq!(observe(&fx, &t3).unwrap().map(|p| p.to), Some(d_b.clone()));
    assert_eq!(main_row(&fx), row_for(&fx, &t3));
    let ps = pendings(&fx);
    assert_eq!(ps.len(), 1);
    let p3 = ps[0].clone();
    assert_eq!((&p3.from, &p3.to), (&d_a, &d_b));
    assert_eq!(target_of(&p3), some_target(&t3));
    assert_eq!(state(&fx, p1.id), ProposalState::Superseded { by: p3.id });

    rewind(&fx, &t2.head);
    let p3b = observe(&fx, &t2).unwrap().expect("P3'");
    assert_eq!(main_row(&fx), row_for(&fx, &t2));
    assert_eq!(
        (&p3b.origin, &p3b.from, &p3b.to),
        (&p3.origin, &p3.from, &p3.to)
    );
    assert_eq!(target_of(&p3b), some_target(&t2));
    assert_eq!(state(&fx, p3.id), ProposalState::Superseded { by: p3b.id });
    assert!(
        pendings(&fx)
            .iter()
            .all(|p| p.head.as_ref() != Some(&t3.head))
    );
    assert_eq!(pendings(&fx).len(), 1);
}

/// Track lock nesting: fail if two entries overlap.
fn no_overlap(fx: &Fx) {
    let depth = Arc::new(AtomicI32::new(0));
    let d = depth.clone();
    fx.lock
        .on_enter(move || assert_eq!(d.fetch_add(1, Ordering::SeqCst), 0, "lock entries overlap"));
    fx.lock.on_exit(move || {
        depth.fetch_sub(1, Ordering::SeqCst);
    });
}

#[test]
fn observe_main_interleaved_with_rewind_converges() {
    let fx = Rc::new(setup());
    fx.git.fetch("origin", "main").unwrap();
    let t1 = tip(&fx);
    observe(&fx, &t1).unwrap();
    let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
    let p2 = observe(&fx, &t2).unwrap().expect("P2");
    no_overlap(&fx);

    // (1) Main rewinds to H1 between computing the key and taking the lock.
    let enters = fx.lock.enters();
    let (f, t1c) = (fx.clone(), t1.clone());
    test_seams::before_main_lock(Box::new(move || {
        rewind(&f, &t1c.head);
        assert!(observe(&f, &t1c).unwrap().is_some());
    }));
    assert_eq!(observe(&fx, &t2).unwrap(), None);
    assert_eq!(fx.lock.enters(), enters + 2);
    assert_eq!(main_row(&fx), row_for(&fx, &t1));
    let ps = pendings(&fx);
    assert_eq!(ps.len(), 1);
    assert_eq!(target_of(&ps[0]), some_target(&t1));
    assert_eq!(
        state(&fx, p2.id),
        ProposalState::Superseded { by: ps[0].id }
    );
    assert!(
        all(&fx)
            .iter()
            .filter(|p| p.head.as_ref() == Some(&t2.head))
            .all(|p| p.id == p2.id)
    );

    // (2) Main moves forward to H3 while observing the rewound H1.
    let enters = fx.lock.enters();
    let f = fx.clone();
    let t3_slot: Rc<Cell<Option<u8>>> = Rc::new(Cell::new(None));
    let slot = t3_slot.clone();
    test_seams::before_main_lock(Box::new(move || {
        let t3 = advance(&f, &[("bun.lock", b"lock v3\n")], "lock 3");
        assert!(observe(&f, &t3).unwrap().is_some());
        slot.set(Some(1));
    }));
    assert_eq!(observe(&fx, &t1).unwrap(), None);
    assert_eq!(t3_slot.get(), Some(1));
    assert_eq!(fx.lock.enters(), enters + 2);
    let t3 = tip(&fx);
    assert_eq!(main_row(&fx), row_for(&fx, &t3));
    let ps = pendings(&fx);
    assert_eq!(ps.len(), 1);
    assert_eq!(target_of(&ps[0]), some_target(&t3));
    test_seams::reset();
}

/// Rewrite main's record without the lock, as an unlocked writer would.
fn rewrite_main(
    store: &Store,
    repo: &strata_types::RepoName,
    n: u64,
    inputs: &str,
    set: &DepsSetId,
) {
    let cur = store.deps_main(repo).unwrap();
    let row = DepsMain {
        repo: repo.clone(),
        tree: tree_oid(&"1".repeat(40)),
        head: commit_oid(&"2".repeat(40)),
        inputs_hash: inputs.to_owned(),
        set: set.clone(),
        observed: UNIX_EPOCH + Duration::from_secs(1_000 + n),
    };
    assert!(store.set_deps_main(&row, cur.as_ref()).unwrap().0);
}

fn rewritten(inputs: &str, set: &DepsSetId) -> (TreeOid, CommitOid, String, DepsSetId) {
    (
        tree_oid(&"1".repeat(40)),
        commit_oid(&"2".repeat(40)),
        inputs.to_owned(),
        set.clone(),
    )
}

fn assert_main_race(e: &strata_types::StrataError) {
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert_eq!(reason(e), Some("main_race"));
}

#[test]
fn observe_main_cas_is_backstop_under_lock() {
    // One unlocked write, then the second CAS lands.
    {
        let fx = setup();
        fx.git.fetch("origin", "main").unwrap();
        let t1 = tip(&fx);
        let (a, d_a) = key(&fx, &t1);
        observe(&fx, &t1).unwrap();
        let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
        let (_, d_b) = key(&fx, &t2);
        let (store, repo) = (fx.store_arc(), fx.repo.clone());
        let calls = Rc::new(Cell::new(0u64));
        let c = calls.clone();
        test_seams::before_main_cas(Box::new(move || {
            c.set(c.get() + 1);
            if c.get() == 1 {
                rewrite_main(&store, &repo, 0, &hex('f'), &set('e'));
            }
        }));
        let p = observe(&fx, &t2).unwrap().expect("proposal");
        test_seams::reset();
        assert_eq!(calls.get(), 2);
        assert_eq!(main_row(&fx), row_for(&fx, &t2));
        assert_eq!(pendings(&fx), vec![p.clone()]);
        assert_eq!(all(&fx).len(), 1);
        assert_eq!((&p.from, &p.from_inputs, &p.to), (&d_a, &a, &d_b));
    }
    // Every round rewritten: Unknown main_not_recorded with the Busy.
    {
        let fx = setup();
        fx.git.fetch("origin", "main").unwrap();
        let t1 = tip(&fx);
        let (_, d_a) = key(&fx, &t1);
        observe(&fx, &t1).unwrap();
        let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
        let (store, repo) = (fx.store_arc(), fx.repo.clone());
        let calls = Rc::new(Cell::new(0u64));
        let c = calls.clone();
        test_seams::before_main_cas(Box::new(move || {
            c.set(c.get() + 1);
            rewrite_main(&store, &repo, c.get(), &hex('f'), &set('e'));
        }));
        let e = observe(&fx, &t2).unwrap_err();
        test_seams::reset();
        assert_eq!(calls.get(), 4);
        assert_eq!(e.code, Code::Unknown, "{e:?}");
        assert_eq!(reason(&e), Some("main_not_recorded"));
        let ps = pendings(&fx);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].from, d_a);
        let d = e.detail.as_ref().unwrap();
        assert_eq!(d["proposal"], serde_json::to_value(&ps[0]).unwrap());
        let inner: strata_types::StrataError = serde_json::from_value(d["error"].clone()).unwrap();
        assert_main_race(&inner);
        assert_eq!(main_row(&fx), Some(rewritten(&hex('f'), &set('e'))));
    }
    // An unlocked write plus main moving on: Unknown, never None.
    {
        let fx = Rc::new(setup());
        fx.git.fetch("origin", "main").unwrap();
        let t1 = tip(&fx);
        observe(&fx, &t1).unwrap();
        let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
        let f = fx.clone();
        let calls = Rc::new(Cell::new(0u64));
        let c = calls.clone();
        test_seams::before_main_cas(Box::new(move || {
            c.set(c.get() + 1);
            if c.get() == 1 {
                rewrite_main(f.store(), &f.repo, 0, &hex('f'), &set('e'));
                advance(&f, &[("README", b"h4\n")], "h4");
            }
        }));
        let e = observe(&fx, &t2).unwrap_err();
        test_seams::reset();
        assert_eq!(calls.get(), 1);
        assert_eq!(e.code, Code::Unknown, "{e:?}");
        assert_eq!(reason(&e), Some("main_not_recorded"));
        let ps = pendings(&fx);
        assert_eq!(ps.len(), 1);
        let d = e.detail.as_ref().unwrap();
        assert_eq!(d["proposal"], serde_json::to_value(&ps[0]).unwrap());
        let inner: strata_types::StrataError = serde_json::from_value(d["error"].clone()).unwrap();
        assert_main_race(&inner);
        assert_eq!(main_row(&fx), Some(rewritten(&hex('f'), &set('e'))));
    }
    // No proposal at all: plain Busy main_race.
    {
        let fx = setup();
        let t2 = advance(&fx, &[("bun.lock", b"lock v2\n")], "lock 2");
        let (b, d_b) = key(&fx, &t2);
        assert_eq!(observe(&fx, &t2).unwrap(), None);
        assert_eq!(main_row(&fx), row_for(&fx, &t2));
        let t3 = advance(&fx, &[("README", b"readme\n")], "readme");
        let (store, repo) = (fx.store_arc(), fx.repo.clone());
        let calls = Rc::new(Cell::new(0u64));
        let c = calls.clone();
        let (b2, d2) = (b.clone(), d_b.clone());
        test_seams::before_main_cas(Box::new(move || {
            c.set(c.get() + 1);
            rewrite_main(&store, &repo, c.get(), &b2, &d2);
        }));
        let e = observe(&fx, &t3).unwrap_err();
        test_seams::reset();
        assert_eq!(calls.get(), 4);
        assert_main_race(&e);
        assert!(all(&fx).is_empty());
        assert_eq!(main_row(&fx), Some(rewritten(&b, &d_b)));
    }
}

fn args(origin: ProposalOrigin, from: char, to: char, t: Option<&Tip>) -> ProposeArgs {
    ProposeArgs {
        origin,
        from: set(from),
        from_inputs: hex(from),
        to: set(to),
        to_inputs: hex(to),
        tree: t.map(|t| t.tree.clone()),
        head: t.map(|t| t.head.clone()),
    }
}

fn session(s: &str) -> ProposalOrigin {
    ProposalOrigin::Session { session: sid(s) }
}

#[test]
fn propose_validates_and_is_idempotent() {
    let fx = setup();
    let t1 = tip(&fx);
    let t2 = advance(&fx, &[("README", b"x\n")], "readme");
    let ctx = fx.ctx();

    let p = propose(&ctx, args(ProposalOrigin::Main, 'a', 'b', Some(&t2))).unwrap();
    assert_eq!(p.id.get(), 1);
    assert_eq!(
        propose(&ctx, args(ProposalOrigin::Main, 'a', 'b', Some(&t2)))
            .unwrap()
            .id,
        p.id
    );
    assert_eq!(all(&fx).len(), 1);

    // Same tree, amended head: a retargeted copy.
    let parent = fx.sb.git(&fx.main, &["rev-parse", "HEAD~1"]);
    let amended = fx.sb.git(
        &fx.main,
        &[
            "commit-tree",
            t2.tree.as_str(),
            "-p",
            &parent,
            "-m",
            "amended",
        ],
    );
    let t2b = Tip {
        tree: t2.tree.clone(),
        head: commit_oid(&amended),
        dir: t2.dir.clone(),
    };
    let q = propose(&ctx, args(ProposalOrigin::Main, 'c', 'b', Some(&t2b))).unwrap();
    assert_eq!(q.id.get(), 2);
    assert_eq!(
        (&q.origin, &q.from, &q.from_inputs),
        (&p.origin, &p.from, &p.from_inputs)
    );
    assert_eq!(target_of(&q), some_target(&t2b));
    assert_eq!(state(&fx, p.id), ProposalState::Superseded { by: q.id });

    let n = all(&fx).len();
    let bad = |a: ProposeArgs, code: Code| {
        assert_eq!(propose(&ctx, a).unwrap_err().code, code);
    };
    bad(
        args(ProposalOrigin::Main, 'a', 'a', Some(&t2)),
        Code::InvalidInput,
    );
    let mut a = args(session("aaaaaaaa"), 'a', 'b', Some(&t2));
    a.head = None;
    bad(a, Code::InvalidInput);
    bad(
        args(ProposalOrigin::Main, 'a', 'b', None),
        Code::InvalidInput,
    );
    let wrong = Tip {
        tree: t1.tree.clone(),
        head: t2.head.clone(),
        dir: t2.dir.clone(),
    };
    bad(
        args(ProposalOrigin::Main, 'a', 'b', Some(&wrong)),
        Code::InvalidInput,
    );
    let missing = Tip {
        tree: t2.tree.clone(),
        head: commit_oid(&"ab".repeat(20)),
        dir: t2.dir.clone(),
    };
    bad(
        args(ProposalOrigin::Main, 'a', 'b', Some(&missing)),
        Code::NoSuchCommit,
    );
    assert_eq!(all(&fx).len(), n);

    // A session's drafts: a different `to` supersedes the earlier one.
    let d1 = propose(&ctx, args(session("aaaaaaaa"), 'a', 'c', None)).unwrap();
    assert_eq!(d1.state, ProposalState::AwaitingPublish);
    assert_eq!(d1.id.get(), 3);
    assert_eq!(
        propose(&ctx, args(session("aaaaaaaa"), 'a', 'c', None))
            .unwrap()
            .id,
        d1.id
    );
    let d2 = propose(&ctx, args(session("aaaaaaaa"), 'a', 'd', None)).unwrap();
    assert_eq!(d2.id.get(), 4);
    assert_eq!(state(&fx, d1.id), ProposalState::Superseded { by: d2.id });
    assert_eq!(state(&fx, q.id), ProposalState::Pending);
}

#[test]
fn publish_promotes_awaiting_proposal() {
    let fx = setup();
    let t1 = tip(&fx);
    let t2 = advance(&fx, &[("README", b"2\n")], "2");
    let t3 = advance(&fx, &[("README", b"3\n")], "3");
    let t5 = advance(&fx, &[("README", b"5\n")], "5");
    let ctx = fx.ctx();
    let s = || session("ssssssss");

    let p = propose(&ctx, args(s(), 'a', 'd', None)).unwrap();
    let p3 = propose(&ctx, args(session("sssssss3"), 'a', 'd', None)).unwrap();
    let p5 = propose(&ctx, args(session("sssssss5"), 'a', 'e', None)).unwrap();
    let c = propose(&ctx, args(ProposalOrigin::Main, 'a', 'b', Some(&t1))).unwrap();

    // S's change lands at (T2, H2).
    let landed = propose(&ctx, args(s(), 'a', 'd', Some(&t2))).unwrap();
    assert_eq!(landed.id, p.id);
    assert_eq!(landed.state, ProposalState::Pending);
    assert_eq!(landed.origin, s());
    assert_eq!(target_of(&landed), some_target(&t2));
    assert_eq!(get_proposal(fx.store(), p.id).unwrap(), landed);
    assert_eq!(state(&fx, c.id), ProposalState::Superseded { by: p.id });
    assert_eq!(state(&fx, p3.id), ProposalState::Superseded { by: p.id });
    assert_eq!(state(&fx, p5.id), ProposalState::AwaitingPublish);
    let n = all(&fx).len();
    assert_eq!(
        propose(&ctx, args(s(), 'a', 'd', Some(&t2))).unwrap().id,
        p.id
    );
    assert_eq!(
        propose(&ctx, args(ProposalOrigin::Main, 'a', 'd', Some(&t2)))
            .unwrap()
            .id,
        p.id
    );
    assert_eq!(all(&fx).len(), n);

    // Another repo: a session's draft is promoted over main's Pending.
    {
        let fx2 = setup();
        let u2 = advance(&fx2, &[("README", b"2\n")], "2");
        let ctx2 = fx2.ctx();
        let c2 = propose(&ctx2, args(ProposalOrigin::Main, 'a', 'd', Some(&u2))).unwrap();
        let p6 = propose(&ctx2, args(session("sssssss6"), 'a', 'd', None)).unwrap();
        let got = propose(&ctx2, args(session("sssssss6"), 'a', 'd', Some(&u2))).unwrap();
        assert_eq!(got.id, p6.id);
        assert_eq!(got.state, ProposalState::Pending);
        assert_eq!(state(&fx2, c2.id), ProposalState::Superseded { by: p6.id });
        assert!(
            all(&fx2)
                .iter()
                .all(|q| q.state != ProposalState::AwaitingPublish)
        );
    }

    // Main retargets the landed Pending; drafts are untouched.
    let p4 = propose(&ctx, args(session("sssssss4"), 'a', 'd', None)).unwrap();
    let p7 = propose(&ctx, args(ProposalOrigin::Main, '7', 'd', Some(&t3))).unwrap();
    assert_ne!(p7.id, p.id);
    assert_eq!((&p7.origin, &p7.from, &p7.to), (&s(), &p.from, &p.to));
    assert_eq!(target_of(&p7), some_target(&t3));
    assert_eq!(state(&fx, p.id), ProposalState::Superseded { by: p7.id });
    assert_eq!(state(&fx, p4.id), ProposalState::AwaitingPublish);
    assert_eq!(state(&fx, p5.id), ProposalState::AwaitingPublish);
    // An older target is not compared by ancestry.
    let p7b = propose(&ctx, args(ProposalOrigin::Main, '7', 'd', Some(&t1))).unwrap();
    assert_eq!((&p7b.origin, &p7b.from), (&s(), &p.from));
    assert_eq!(target_of(&p7b), some_target(&t1));
    assert_eq!(state(&fx, p7.id), ProposalState::Superseded { by: p7b.id });

    // The key changed on landing: re-key, then promote.
    let p8 = propose(&ctx, args(session("sssssssz"), 'a', '8', None)).unwrap();
    let p8b = propose(&ctx, args(session("sssssssz"), 'a', 'f', Some(&t5))).unwrap();
    assert_eq!(state(&fx, p8.id), ProposalState::Superseded { by: p8b.id });
    assert_eq!((&p8b.from, &p8b.to), (&p8.from, &set('f')));
    assert_eq!(p8b.state, ProposalState::Pending);
    assert_eq!(p8b.origin, session("sssssssz"));
    assert_eq!(target_of(&p8b), some_target(&t5));
    assert_eq!(get_proposal(fx.store(), p8b.id).unwrap(), p8b);
    assert_eq!(state(&fx, p7b.id), ProposalState::Superseded { by: p8b.id });
    assert!(
        !all(&fx)
            .iter()
            .any(|q| q.origin == session("sssssssz") && q.state == ProposalState::AwaitingPublish)
    );
    let n = all(&fx).len();
    assert_eq!(
        propose(&ctx, args(session("sssssssz"), 'a', 'f', Some(&t5)))
            .unwrap()
            .id,
        p8b.id
    );
    assert_eq!(all(&fx).len(), n);
}

/// Register an empty deps set and a base for `repo`, and a session on it.
fn make_session(fx: &Fx, id: &SessionId, removed: bool) {
    let at = UNIX_EPOCH + Duration::from_secs(10);
    let base = BaseGen {
        repo: fx.repo.clone(),
        tree: tree_oid(&"1".repeat(40)),
        deps: set('9'),
    };
    let s = fx.store();
    if s.deps_set(&fx.repo, &base.deps).unwrap().is_none() {
        let ds = DepsSet {
            id: base.deps.clone(),
            repo: fx.repo.clone(),
            inputs_hash: hex('0'),
            platform: "test".into(),
            toolchain: String::new(),
            dirs: vec![],
        };
        s.record_deps_set(&ds, DepsOrigin::Empty, None, at).unwrap();
        let m = BaseManifest {
            tree: base.tree.clone(),
            entries: BTreeMap::new(),
        };
        s.record_base_gen(&base, &m, at).unwrap();
    }
    let root = fx.paths.root.join("sessions").join(id.as_str());
    let mut repos = BTreeMap::new();
    repos.insert(
        fx.repo.clone(),
        WorkspaceState {
            base,
            head: commit_oid(&"1".repeat(40)),
            path: root.join("tool"),
            tracked: true,
        },
    );
    s.create_session(
        &SessionManifest {
            id: id.clone(),
            ticket: None,
            agent: None,
            root,
            repos,
        },
        at,
    )
    .unwrap();
    assert!(
        s.set_lifecycle(id, Lifecycle::Creating, Lifecycle::Active, None, at)
            .unwrap()
    );
    if removed {
        assert!(
            s.set_lifecycle(id, Lifecycle::Active, Lifecycle::Removing, None, at)
                .unwrap()
        );
        assert!(
            s.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Removed, None, at)
                .unwrap()
        );
    }
}

#[test]
fn open_proposals_lists_open_and_live_only() {
    let fx = setup();
    let (s1, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    make_session(&fx, &s1, false);
    make_session(&fx, &s2, true);
    let t1 = tip(&fx);
    let ctx = fx.ctx();
    let st = fx.store();
    let now = SystemTime::now();

    let applied = propose(&ctx, args(ProposalOrigin::Main, 'a', 'b', Some(&t1))).unwrap();
    assert!(
        st.decide_proposal(applied.id, ProposalDecision::Applied, now)
            .unwrap()
    );
    let old = propose(&ctx, args(ProposalOrigin::Main, 'a', 'c', Some(&t1))).unwrap();
    let p1 = propose(&ctx, args(ProposalOrigin::Main, 'a', 'd', Some(&t1))).unwrap();
    assert_eq!(state(&fx, old.id), ProposalState::Superseded { by: p1.id });
    let d1 = propose(
        &ctx,
        args(ProposalOrigin::Session { session: s1 }, 'a', 'e', None),
    )
    .unwrap();
    let d2 = propose(
        &ctx,
        args(ProposalOrigin::Session { session: s2 }, 'a', 'f', None),
    )
    .unwrap();

    let front = repo_name("front");
    let new = NewProposal {
        repo: front.clone(),
        origin: None,
        from: set('a'),
        from_inputs: hex('a'),
        to: set('b'),
        to_inputs: hex('b'),
        tree: Some(t1.tree.clone()),
        head: Some(t1.head.clone()),
    };
    let p2 = st.record_proposal(&new, None, now).unwrap().unwrap();

    let ids = |v: Vec<Proposal>| v.into_iter().map(|p| p.id).collect::<Vec<_>>();
    assert_eq!(
        ids(open_proposals(st, Some(&fx.repo)).unwrap()),
        vec![p1.id, d1.id]
    );
    assert_eq!(
        ids(open_proposals(st, None).unwrap()),
        vec![p1.id, d1.id, p2]
    );
    assert_eq!(open_proposals(st, Some(&fx.repo)).unwrap(), vec![p1, d1]);
    assert_eq!(get_proposal(st, d2.id).unwrap(), d2);
    assert_eq!(
        get_proposal(st, ProposalId::try_from(999u64).unwrap())
            .unwrap_err()
            .code,
        Code::NoSuchProposal
    );
}

//! Auto-close §10 row 18: phase-B writer races (Codex r5 blockers 1 and 2).
//! A main-follow writer stopped before its target write, and proposals,
//! drafts and deferrals written while a strict close is in phase B.
mod support;

use std::fs;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::main_sync;
use strata_daemon::startup::{Core, Daemon};
use strata_publish::seams::{PublishFault, pause};
use strata_session::SessionSeam;
use strata_store::{Lifecycle, NewProposal, ProposalDecision};
use strata_types::{CommitOid, DepsSetId, MarkByKind, ProposalId, SessionId};
use support::close::{self, mark_of, ok};
use support::*;

const WAIT: Duration = Duration::from_secs(20);

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn pin(core: &Core, s: &SessionId) -> bool {
    core.gits[&repo_name("tool")].read_ref(&format!("refs/strata/pins/{s}")).unwrap().is_some()
}

fn row_head(core: &Core, s: &SessionId) -> Option<String> {
    core.store.rebase_pending_of(s, &repo_name("tool")).unwrap().and_then(|r| r.head)
}

/// A real commit on origin's main, the shape of strata-publish's `advance`:
/// pull, commit, push, fetch into the daemon's tracking ref.
fn origin_advance(fx: &Dfx, core: &Core, file: &str) -> CommitOid {
    let main = fx.main("tool");
    fx.sb.git(&main, &["pull", "-q", "--no-rebase", "origin", "main"]);
    let h = fx.sb.commit(&main, &[(file, b"next\n")], "next");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo_name("tool")].fetch("origin", "main").unwrap();
    CommitOid::try_from(h).unwrap()
}

fn fixture(quiet: bool) -> Dfx {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.reconcile_every = Duration::from_secs(86400);
    if quiet {
        fx.config.repos.get_mut(&repo_name("tool")).unwrap().rebase.quiet_secs = 86400;
    }
    fx.write_config();
    fx
}

struct Release<'a, 'b>(&'a strata_session::Sessions<'b>);
impl Drop for Release<'_, '_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

// ---- Blocker 1: a stale main-follow writer after `Removing`.

/// U publishes with `push = false`, leaving unpushed chain top C as `main_tip`.
/// U's publish's own main-follow may enroll S onto C; clear that row so the
/// enroll race below can attempt the same target again (§10 row 18 variant).
fn u_push_false_chain_top(
    core: &Core,
    server: &Server<'_, '_>,
    s: &SessionId,
) -> CommitOid {
    let repo = repo_name("tool");
    let u = close::new_only(server, &["tool"]);
    fs::write(ws(core, &u, "tool").join("u.txt"), "u\n").unwrap();
    let out = ok(close::publish(server, core, &u, "tool", &["u.txt"]));
    assert!(out["published"][0]["push"].is_null(), "{out}");
    let c = CommitOid::try_from(out["published"][0]["commit"].as_str().unwrap().to_owned()).unwrap();
    if let Some(row) = core.store.rebase_pending_of(s, &repo).unwrap() {
        assert_eq!(row.head.as_deref(), Some(c.as_str()), "U's publish enrolled S on C");
        assert!(core.store.drop_pending(s, &repo, row.rev).unwrap());
    }
    assert_eq!(row_head(core, s), None);
    c
}

/// `via_push_false`: target is U's `push = false` chain top (Codex's unpushed C).
/// Otherwise the target is a real origin advance (pulled, committed, pushed, fetched).
fn stale_main_follow(retarget: bool, via_push_false: bool) {
    let mut fx = fixture(false);
    if via_push_false {
        fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.push = false;
        fx.write_config();
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    let repo = repo_name("tool");
    let mut h = None;
    if retarget {
        assert!(!via_push_false, "retarget keeps the origin_advance target");
        let first = origin_advance(&fx, &core, "src/first.ts");
        main_sync::reconcile(&core, &repo).unwrap();
        assert_eq!(row_head(&core, &s).as_deref(), Some(first.as_str()));
        h = Some(first);
    }
    let target = if via_push_false {
        u_push_false_chain_top(&core, &server, &s)
    } else {
        origin_advance(&fx, &core, "src/next.ts")
    };
    let writer_pause = pause(&core.gits[&repo], PublishFault::PauseBeforeMainTarget);
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| main_sync::reconcile(&core, &repo));
        writer_pause.wait_until_paused();
        d.sessions.arm(SessionSeam::PauseAt { phase: "remove.unpin".into() });
        let close = scope.spawn(|| {
            let _pin = core.admit();
            core.locks.with_sessions(std::slice::from_ref(&s), || d.sessions.remove_strict(&s))
        });
        let _release = Release(&d.sessions);
        assert!(d.sessions.wait_paused_timeout(WAIT), "remove.unpin was not reached");
        assert_eq!(lifecycle(&core, &s), Lifecycle::Removing);
        writer_pause.release();
        writer.join().unwrap().unwrap();
        match &h {
            None => assert_eq!(row_head(&core, &s), None, "enroll_main wrote after Removing"),
            Some(h) => assert_eq!(row_head(&core, &s).as_deref(), Some(h.as_str()), "retarget wrote after Removing"),
        }
        assert_ne!(row_head(&core, &s).as_deref(), Some(target.as_str()));
        d.sessions.release();
        close.join().unwrap().unwrap();
    });
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(!pin(&core, &s));
}

#[test]
fn stale_main_follow_cannot_enroll_after_removing() {
    // §10 row 18: origin advance, and U's push = false chain top.
    stale_main_follow(false, false);
    stale_main_follow(false, true);
}

#[test]
fn stale_main_follow_cannot_retarget_after_removing() {
    stale_main_follow(true, false);
}

// ---- Blocker 2: clause 6 written during phase B.

/// What the test writes at the pause.
#[derive(Clone, Copy, PartialEq)]
enum Write {
    /// A main proposal (`origin: None`) with a real target, and `enroll_main`.
    ProposalAndEnroll,
    /// The main proposal alone.
    Proposal,
    /// S's own draft, as `deps_propose` writes it.
    Draft,
    /// `defer_pending(…, "conflicts", …)` on the existing row.
    Conflicts,
}

fn record(core: &Core, fx: &Dfx, s: &SessionId, w: Write) -> Option<ProposalId> {
    let repo = repo_name("tool");
    let ws = core.store.session(s).unwrap().unwrap().manifest.repos[&repo].clone();
    if w == Write::Conflicts {
        let p = core.store.rebase_pending_of(s, &repo).unwrap().expect("a deferred row");
        assert!(core.store.defer_pending(s, &repo, p.rev, "conflicts", &json!({"paths": ["src/a.ts"]}), SystemTime::now()).unwrap());
        return None;
    }
    let set = core.store.deps_set(&repo, &ws.base.deps).unwrap().unwrap().set;
    let target = (w != Write::Draft).then(|| origin_advance(fx, core, "src/target.ts"));
    let tree = target.as_ref().map(|t| core.gits[&repo].tree_of(t).unwrap());
    let p = core
        .store
        .record_proposal(
            &NewProposal {
                repo: repo.clone(),
                origin: (w == Write::Draft).then(|| s.clone()),
                from: ws.base.deps.clone(),
                from_inputs: set.inputs_hash,
                from_head: Some(ws.head.clone()),
                to: DepsSetId::try_from("b".repeat(64)).unwrap(),
                to_inputs: "b".repeat(64),
                tree: tree.clone(),
                head: target.clone(),
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap();
    if w == Write::ProposalAndEnroll {
        assert!(core.store.enroll_main(s, &repo, &ws.base, target.as_ref().unwrap(), tree.as_ref().unwrap(), None, SystemTime::now()).unwrap());
    }
    Some(p)
}

/// S edits and checkpoints `tool`, then publishes it on a thread; at `phase`
/// the test writes `w`; the publish answers. `then` sees the reply.
fn phase_b(quiet: bool, phase: &str, w: Write, then: impl FnOnce(&Dfx, &Core, &Server<'_, '_>, &SessionId, Option<ProposalId>, &Value)) {
    let fx = fixture(quiet);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    d.sessions.arm(SessionSeam::PauseAt { phase: phase.into() });
    let (out, p) = std::thread::scope(|scope| {
        let publish = scope.spawn(|| {
            let server = Server::new(&d, &trackers);
            close::publish(&server, &core, &s, "tool", &["new.txt"])
        });
        let _release = Release(&d.sessions);
        assert!(d.sessions.wait_paused_timeout(WAIT), "{phase} was not reached");
        let p = record(&core, &fx, &s, w);
        d.sessions.release();
        (ok(publish.join().unwrap()), p)
    });
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    then(&fx, &core, &server, &s, p, &out);
}

fn held(core: &Core, out: &Value, s: &SessionId, reasons: Value) -> Value {
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(out, s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], reasons, "{out}");
    assert_eq!(e["detail"]["after"], "checkpoint", "{out}");
    assert_eq!(mark_of(core, s).expect("a mark").by.kind, MarkByKind::Daemon);
    assert_eq!(lifecycle(core, s), Lifecycle::Active);
    let root = core.store.session(s).unwrap().unwrap().manifest.root;
    assert!(root.exists(), "the root is in place");
    assert!(pin(core, s), "the pin is in place");
    let attempts = core.store.rm_attempts(Some(s)).unwrap();
    attempts.last().expect("last_rm").detail.clone()
}

fn proposal_state(core: &Core, p: ProposalId) -> String {
    format!("{:?}", core.store.proposal(p).unwrap().unwrap())
}

#[test]
fn deps_proposal_during_phase_b_refuses_the_close() {
    phase_b(false, "remove.before_removing", Write::ProposalAndEnroll, |_, core, _, s, p, out| {
        let p = p.unwrap();
        let last = held(core, out, s, json!(["deps_pending"]));
        let d = &entry(out, s)["detail"];
        assert_eq!(d["proposal"], json!(p.get()), "{out}");
        assert_eq!(d["state"], "pending", "{out}");
        let target = core.store.proposal(p).unwrap().unwrap().head.unwrap();
        assert_eq!(row_head(core, s).as_deref(), Some(target.as_str()));
        assert!(proposal_state(core, p).contains("Pending"), "{}", proposal_state(core, p));
        assert_eq!(last["detail"]["reason"], "changed_during_remove", "{last}");
    });
}

#[test]
fn deps_proposal_and_enroll_after_recheck_refuses_as_changed() {
    phase_b(false, "remove.after_recheck", Write::ProposalAndEnroll, |_, core, _, s, _, out| {
        held(core, out, s, json!(["changed_during_remove"]));
        assert_eq!(entry(out, s)["detail"]["pending_changed"], true, "{out}");
    });
}

#[test]
fn deps_proposal_alone_after_recheck_refuses_the_close() {
    phase_b(false, "remove.after_recheck", Write::Proposal, |_, core, _, s, p, out| {
        held(core, out, s, json!(["deps_pending"]));
        assert_eq!(entry(out, s)["detail"]["proposal"], json!(p.unwrap().get()), "{out}");
    });
}

#[test]
fn own_draft_during_phase_b_refuses_the_close() {
    phase_b(false, "remove.before_removing", Write::Draft, |_, core, _, s, p, out| {
        held(core, out, s, json!(["deps_pending"]));
        let d = &entry(out, s)["detail"];
        assert_eq!(d["proposal"], json!(p.unwrap().get()), "{out}");
        assert_eq!(d["state"], "awaiting_publish", "{out}");
    });
}

#[test]
fn conflicts_deferral_during_phase_b_refuses_the_close() {
    phase_b(true, "remove.after_recheck", Write::Conflicts, |_, core, _, s, _, out| {
        held(core, out, s, json!(["conflicts"]));
        assert_eq!(entry(out, s)["detail"]["auto_reason"], "conflicts", "{out}");
    });
}

#[test]
fn applied_proposal_no_longer_holds_the_session() {
    phase_b(false, "remove.before_removing", Write::ProposalAndEnroll, |_, core, server, s, p, out| {
        held(core, out, s, json!(["deps_pending"]));
        assert!(core.store.decide_proposal(p.unwrap(), ProposalDecision::Applied, SystemTime::now()).unwrap());
        let t = close::new_only(server, &["tool"]);
        fs::write(ws(core, &t, "tool").join("t.txt"), "t\n").unwrap();
        let out = ok(close::publish(server, core, &t, "tool", &["t.txt"]));
        assert_eq!(out["auto_close"]["rejudged"], json!([s]), "{out}");
        assert_eq!(entry(&out, s)["closed"], true, "{out}");
        assert_eq!(lifecycle(core, s), Lifecycle::Removed);
    });
}

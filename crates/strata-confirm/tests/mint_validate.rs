//! confirm.md §7: minting, registering and redeeming tokens (Task 2).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use strata_confirm::seams::{FakeClock, PromptLog, ScriptedAuth, Verdict};
use strata_confirm::{Confirmer, RECORD_RETENTION, prompt_reason};
use strata_types::{
    Action, Code, ConfirmToken, ProposalId, RepoId, RepoName, SessionId, StrataError,
};

const SHORT: Duration = Duration::from_millis(50);

fn seamed(verdicts: Vec<Verdict>) -> (Confirmer, FakeClock, Arc<PromptLog>) {
    let auth = ScriptedAuth::new(true, Ok(()), verdicts);
    let log = auth.log();
    let clock = FakeClock::new();
    (
        Confirmer::with_seams(auth, clock.clone(), SHORT, SHORT),
        clock,
        log,
    )
}

fn passes(n: usize) -> Vec<Verdict> {
    vec![Verdict::pass(); n]
}

fn push(repo: &str, branch: &str) -> Action {
    Action::PushDeploy {
        repo: RepoName::try_from(repo.to_string()).unwrap(),
        branch: branch.to_string(),
        url: "git@github.com:inter/tool.git".to_string(),
    }
}

fn discard(s: &str) -> Action {
    Action::RemoveDiscard {
        session: SessionId::try_from(s.to_string()).unwrap(), kept_trees: vec![]
    }
}

fn approve(p: i64) -> Action {
    Action::DepsApprove {
        proposal: ProposalId::from(p),
    }
}

fn relax(path: &str) -> Action {
    Action::ConfigRelax {
        repo: RepoId::new(PathBuf::from(path)),
    }
}

fn reason(e: &StrataError) -> String {
    e.detail.as_ref().expect("detail")["reason"]
        .as_str()
        .expect("reason")
        .to_string()
}

fn refused(r: strata_confirm::Result<()>, why: &str) -> StrataError {
    let e = r.expect_err("validate should refuse");
    assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
    assert_eq!(reason(&e), why, "{e:?}");
    e
}

#[test]
fn require_pass_mints_a_registered_token() {
    let (c, _clock, log) = seamed(passes(2));
    let action = push("tool", "main");
    let t1 = c.require(action.clone()).expect("pass mints");
    assert_eq!(t1.action, action);
    assert_eq!(log.shown(), vec![prompt_reason(&action)]);
    assert_eq!(c.record_count(), 1);

    // Same action again: another prompt, another nonce; nothing is cached.
    let t2 = c.require(action.clone()).expect("pass mints again");
    assert_eq!(log.shown().len(), 2);
    assert_ne!(t1.nonce, t2.nonce);

    c.validate(&t1, &action).expect("first redeem");
    c.validate(&t2, &action)
        .expect("second token redeems on its own");
    refused(c.validate(&t1, &action), "used");
    refused(c.validate(&t2, &action), "used");
}

#[test]
fn token_single_use() {
    let (c, clock, _log) = seamed(passes(3));
    let action = discard("s-k3q7xm2a");

    let t = c.require(action.clone()).unwrap();
    c.validate(&t, &action).unwrap();
    refused(c.validate(&t, &action), "used");

    // A failed redeem spends the nonce too.
    let t = c.require(action.clone()).unwrap();
    refused(c.validate(&t, &discard("s-aaaaaaaa")), "mismatch");
    refused(c.validate(&t, &action), "used");

    // After RECORD_RETENTION the record is swept: unknown.
    let t = c.require(action.clone()).unwrap();
    assert_eq!(c.record_count(), 3);
    clock.advance(RECORD_RETENTION);
    refused(c.validate(&t, &action), "unknown");
    assert_eq!(c.record_count(), 0);
}

#[test]
fn token_expires() {
    let (c, clock, _log) = seamed(passes(3));
    let action = approve(12);

    let t = c.require(action.clone()).unwrap();
    clock.advance(Duration::from_secs(29));
    c.validate(&t, &action).expect("29 s is still fresh");

    let t = c.require(action.clone()).unwrap();
    clock.advance(Duration::from_secs(30));
    let e = refused(c.validate(&t, &action), "expired");
    let age = e.detail.as_ref().unwrap()["age_ms"]
        .as_u64()
        .expect("age_ms");
    assert!(age >= 30_000, "age_ms = {age}");

    // System sleep: Instant does not move, CLOCK_MONOTONIC does.
    let t = c.require(action.clone()).unwrap();
    clock.sleep(Duration::from_secs(31));
    refused(c.validate(&t, &action), "expired");
    // The expired token was spent by that redeem.
    refused(c.validate(&t, &action), "used");
}

#[test]
fn wrong_action_is_refused() {
    let pairs = vec![
        (discard("s-aaaaaaaa"), discard("s-bbbbbbbb")),
        (push("tool", "main"), push("tool", "release")),
        (push("tool", "main"), push("front", "main")),
        (approve(1), approve(2)),
        (discard("s-aaaaaaaa"), push("tool", "main")),
        (
            relax("/Volumes/GWD/inter/tool"),
            relax("/Volumes/GWD/inter/front"),
        ),
    ];
    let (c, _clock, _log) = seamed(passes(pairs.len()));
    for (confirmed, expected) in pairs {
        let t = c.require(confirmed.clone()).unwrap();
        let e = refused(c.validate(&t, &expected), "mismatch");
        let d = e.detail.as_ref().unwrap();
        let exp: Action = serde_json::from_value(d["expected"].clone()).unwrap();
        let conf: Action = serde_json::from_value(d["confirmed"].clone()).unwrap();
        assert_eq!(exp, expected);
        assert_eq!(conf, confirmed);
        refused(c.validate(&t, &confirmed), "used");
    }
}

#[test]
fn forged_or_tampered_token_is_refused() {
    let (c, _clock, _log) = seamed(passes(3));
    let action = push("tool", "main");

    // Hand-built token.
    let mut nonce = [0u8; 16];
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37).wrapping_add(11);
    }
    let forged = ConfirmToken {
        action: action.clone(),
        issued: Instant::now(),
        nonce: u128::from_le_bytes(nonce),
    };
    refused(c.validate(&forged, &action), "unknown");

    // Another issuer (a restarted daemon) does not accept this one's tokens.
    let (other, _, _) = seamed(passes(1));
    let t = other.require(action.clone()).unwrap();
    refused(c.validate(&t, &action), "unknown");

    // Rewriting the action after minting.
    let mut t = c.require(action.clone()).unwrap();
    let other_action = push("front", "main");
    t.action = other_action.clone();
    refused(c.validate(&t, &other_action), "tampered");
    refused(c.validate(&t, &other_action), "used");

    // Moving `issued` forward to extend its life.
    let mut t = c.require(action.clone()).unwrap();
    t.issued += Duration::from_secs(1);
    refused(c.validate(&t, &action), "tampered");
}

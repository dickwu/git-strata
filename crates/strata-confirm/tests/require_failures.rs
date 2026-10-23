//! confirm.md §7: the refusal paths of `require` (Task 3).

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use strata_confirm::seams::{FakeClock, PromptLog, ScriptedAuth, Verdict};
use strata_confirm::{Confirmer, prompt_reason};
use strata_types::{Action, Code, RepoName, StrataError};

const SHORT: Duration = Duration::from_millis(50);

fn push() -> Action {
    Action::PushDeploy {
        repo: RepoName::try_from("tool".to_string()).unwrap(),
        branch: "main".to_string(),
    }
}

fn build(
    gui: bool,
    can: Result<(), isize>,
    verdicts: Vec<Verdict>,
    timeout: Duration,
    grace: Duration,
) -> (Confirmer, FakeClock, Arc<PromptLog>) {
    let auth = ScriptedAuth::new(gui, can, verdicts);
    let log = auth.log();
    let clock = FakeClock::new();
    (
        Confirmer::with_seams(auth, clock.clone(), timeout, grace),
        clock,
        log,
    )
}

fn scripted(verdicts: Vec<Verdict>) -> (Confirmer, FakeClock, Arc<PromptLog>) {
    build(true, Ok(()), verdicts, SHORT, SHORT)
}

fn reason(e: &StrataError) -> &str {
    e.detail.as_ref().expect("detail")["reason"]
        .as_str()
        .expect("reason")
}

fn la_code(e: &StrataError) -> Option<i64> {
    e.detail
        .as_ref()
        .expect("detail")
        .get("la_code")
        .map(|v| v.as_i64().expect("la_code int"))
}

#[test]
fn user_cancel_mints_nothing() {
    let codes = [
        (-2, "cancelled"),
        (-1, "failed"),
        (-4, "interrupted"),
        (-8, "failed"),
    ];
    let (c, _clock, log) = scripted(codes.iter().map(|(n, _)| Verdict::la(*n)).collect());
    for (i, (code, why)) in codes.iter().enumerate() {
        let e = c.require(push()).expect_err("no token");
        assert_eq!(e.code, Code::NotConfirmed);
        assert_eq!(reason(&e), *why);
        assert_eq!(la_code(&e), Some(*code as i64));
        assert_eq!(
            e.detail.as_ref().unwrap()["action"],
            serde_json::to_value(push()).unwrap()
        );
        assert_eq!(log.shown().len(), i + 1);
        assert_eq!(c.record_count(), 0);
    }
    assert_eq!(log.invalidated(), codes.len() as u32);
    assert_eq!(log.cancelled_while_pending(), 0);
}

#[test]
fn la_error_mapping_covers_every_code() {
    use Code::{NotConfirmed as NC, Unsupported as US};
    let la = |n: isize| Verdict::la(n);
    let foreign = Verdict::Reply {
        success: false,
        error: Some(("NSCocoaErrorDomain".to_string(), -2)),
    };
    let no_error = Verdict::Reply {
        success: false,
        error: None,
    };
    let success_with_error = Verdict::Reply {
        success: true,
        error: Some(("com.apple.LocalAuthentication".to_string(), -1)),
    };
    let table: Vec<(Verdict, Code, &str, Option<i64>)> = vec![
        (la(-1), NC, "failed", Some(-1)),
        (la(-2), NC, "cancelled", Some(-2)),
        (la(-3), NC, "cancelled", Some(-3)),
        (la(-4), NC, "interrupted", Some(-4)),
        (la(-5), US, "passcode_not_set", Some(-5)),
        (la(-6), NC, "unexpected", Some(-6)),
        (la(-7), NC, "unexpected", Some(-7)),
        (la(-8), NC, "failed", Some(-8)),
        (la(-9), NC, "interrupted", Some(-9)),
        (la(-10), NC, "unexpected", Some(-10)),
        (la(-11), NC, "unexpected", Some(-11)),
        (la(-12), NC, "unexpected", Some(-12)),
        (la(-13), NC, "unexpected", Some(-13)),
        (la(-14), NC, "unexpected", Some(-14)),
        (la(-1004), US, "not_interactive", Some(-1004)),
        (la(-999), NC, "unexpected", Some(-999)),
        (foreign, NC, "unexpected", None),
        (no_error, NC, "unexpected", None),
        (success_with_error, NC, "unexpected", Some(-1)),
        (Verdict::Throw, NC, "la_exception", None),
    ];
    let (c, _clock, log) = scripted(table.iter().map(|r| r.0.clone()).collect());
    for (v, code, why, la_c) in &table {
        let e = c
            .require(push())
            .expect_err(&format!("{v:?} must not mint"));
        assert_eq!(
            (e.code, reason(&e), la_code(&e)),
            (*code, *why, *la_c),
            "{v:?}"
        );
    }
    assert_eq!(c.record_count(), 0);
    assert_eq!(log.shown().len(), table.len());
    assert_eq!(log.invalidated(), table.len() as u32);

    // Only a pass mints.
    let (c, _, _) = scripted(vec![Verdict::pass()]);
    c.require(push()).expect("pass mints");
    assert_eq!(c.record_count(), 1);
}

fn timeout_of(e: &StrataError) {
    assert_eq!(e.code, Code::NotConfirmed);
    assert_eq!(reason(e), "timeout");
}

#[test]
fn prompt_timeout_invalidates_and_mints_nothing() {
    let grace = Duration::from_secs(2);

    // Hang: invalidated at the deadline; returns once the late AppCancel arrives, well
    // before the grace runs out.
    let (c, _, log) = build(true, Ok(()), vec![Verdict::Hang], SHORT, grace);
    let t0 = Instant::now();
    timeout_of(&c.require(push()).unwrap_err());
    let took = t0.elapsed();
    assert!(took >= SHORT && took < Duration::from_secs(1), "{took:?}");
    assert_eq!(log.cancelled_while_pending(), 1);
    assert_eq!(log.invalidated(), 1);

    // PassAfterCancel: the late success is discarded.
    let (c, _, log) = build(true, Ok(()), vec![Verdict::PassAfterCancel], SHORT, grace);
    timeout_of(&c.require(push()).unwrap_err());
    assert_eq!(c.record_count(), 0);
    assert_eq!(log.cancelled_while_pending(), 1);

    // Silent: waits the whole grace.
    let short_grace = Duration::from_millis(200);
    let (c, _, _) = build(true, Ok(()), vec![Verdict::Silent], SHORT, short_grace);
    let t0 = Instant::now();
    timeout_of(&c.require(push()).unwrap_err());
    assert!(t0.elapsed() >= SHORT + short_grace, "{:?}", t0.elapsed());
    assert_eq!(c.record_count(), 0);

    // The deadline runs on the clock that keeps going during sleep.
    let (c, clock, log) = build(
        true,
        Ok(()),
        vec![Verdict::Hang],
        Duration::from_secs(60),
        SHORT,
    );
    let sleeper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        clock.sleep(Duration::from_secs(61));
    });
    let t0 = Instant::now();
    timeout_of(&c.require(push()).unwrap_err());
    let took = t0.elapsed();
    sleeper.join().unwrap();
    assert!(took < Duration::from_millis(1500), "{took:?}");
    assert_eq!(log.cancelled_while_pending(), 1);
}

#[test]
fn concurrent_prompt_is_refused_not_queued() {
    let (c, _clock, log) = build(
        true,
        Ok(()),
        vec![Verdict::pass(), Verdict::Hang, Verdict::pass()],
        Duration::from_secs(2),
        SHORT,
    );
    let c = Arc::new(c);
    let early = c.require(push()).expect("first prompt passes");

    let a = {
        let c = c.clone();
        thread::spawn(move || c.require(push()))
    };
    let t0 = Instant::now();
    while log.shown().len() < 2 {
        assert!(t0.elapsed() < Duration::from_secs(1), "A never prompted");
        thread::sleep(Duration::from_millis(5));
    }
    let probes = log.probes();

    let t1 = Instant::now();
    let e = c.require(push()).expect_err("B is refused");
    assert!(t1.elapsed() < Duration::from_millis(200));
    assert_eq!(e.code, Code::NotConfirmed);
    assert_eq!(reason(&e), "prompt_in_progress");
    assert_eq!(log.shown().len(), 2);
    assert_eq!(log.probes(), probes);

    // The registry is not held by the open prompt.
    c.validate(&early, &push())
        .expect("validate runs while A's prompt is open");

    timeout_of(&a.join().unwrap().unwrap_err());
    // B again, now that A is done: its own prompt.
    c.require(push()).expect("B's retry prompts and passes");
    assert_eq!(log.shown(), vec![prompt_reason(&push()); 3]);
}

#[test]
fn no_gui_session_is_unsupported_before_prompt() {
    let cases: Vec<(bool, Result<(), isize>, &str, Option<i64>, u32)> = vec![
        (false, Ok(()), "no_gui_session", None, 1),
        (true, Err(-5), "passcode_not_set", Some(-5), 2),
        (true, Err(-1004), "not_interactive", Some(-1004), 2),
        (true, Err(-6), "cannot_evaluate", Some(-6), 2),
    ];
    for (gui, can, why, la_c, probes) in cases {
        let (c, _, log) = build(gui, can, vec![Verdict::pass()], SHORT, SHORT);
        let e = c.require(push()).expect_err("unsupported");
        assert_eq!(
            (e.code, reason(&e), la_code(&e)),
            (Code::Unsupported, why, la_c)
        );
        // The graphic-session check runs first; the preflight only when it passes.
        assert_eq!(log.probes(), probes, "{why}");
        let e = c.probe().expect_err("probe agrees");
        assert_eq!(
            (e.code, reason(&e), la_code(&e)),
            (Code::Unsupported, why, la_c)
        );
        assert!(log.shown().is_empty(), "{why}: no prompt");
        assert_eq!(c.record_count(), 0);
    }
    let (c, _, log) = build(true, Ok(()), vec![], SHORT, SHORT);
    c.probe().expect("probe ok");
    assert!(log.shown().is_empty());
    assert_eq!(log.probes(), 2);
}

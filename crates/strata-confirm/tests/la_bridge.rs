//! confirm.md §7, Task 4: the real LocalAuthentication bridge, on paths that never show a prompt.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use objc2_security::{SessionAttributeBits, SessionGetInfo, callerSecuritySession};
use strata_confirm::Confirmer;
use strata_confirm::seams::{LaOptions, read_back_la_config};
use strata_types::{Action, Code, RepoName, StrataError};

fn push() -> Action {
    Action::PushDeploy {
        repo: RepoName::try_from("tool".to_string()).unwrap(),
        branch: "main".to_string(),
        url: "git@github.com:acme/tool.git".to_string(),
    }
}

/// Read the graphic-access bit ourselves, independently of the crate.
fn has_gui() -> bool {
    let mut sid = 0u32;
    let mut attrs = SessionAttributeBits(0);
    #[allow(unused_unsafe)]
    let status = unsafe { SessionGetInfo(callerSecuritySession, &mut sid, &mut attrs) };
    assert_eq!(status, 0, "SessionGetInfo failed: {status}");
    attrs.0 & SessionAttributeBits::sessionHasGraphicAccess.0 != 0
}

fn detail_str<'a>(e: &'a StrataError, k: &str) -> Option<&'a str> {
    e.detail.as_ref()?.get(k)?.as_str()
}

fn detail_i64(e: &StrataError, k: &str) -> Option<i64> {
    e.detail.as_ref()?.get(k)?.as_i64()
}

#[test]
fn la_context_is_configured_fresh_without_reuse() {
    let rb = read_back_la_config();
    assert_eq!(rb.reuse_duration, 0.0);
    assert!(!rb.interaction_not_allowed);
    assert!(rb.fallback_title_is_default);
    // Every call builds a fresh context: the same answer twice.
    assert_eq!(read_back_la_config(), rb);
}

fn check(confirmer: &Confirmer, gui: bool, want_code: Code, want_reason: &str, want_la: i64) {
    let t = Instant::now();
    let err = confirmer
        .require(push())
        .expect_err("never confirmed without a human");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "took {:?}",
        t.elapsed()
    );
    if gui {
        assert_eq!(err.code, want_code, "{err:?}");
        assert_eq!(detail_str(&err, "reason"), Some(want_reason), "{err:?}");
        assert_eq!(detail_i64(&err, "la_code"), Some(want_la), "{err:?}");
    } else {
        assert_eq!(err.code, Code::Unsupported, "{err:?}");
        assert_eq!(
            detail_str(&err, "reason"),
            Some("no_gui_session"),
            "{err:?}"
        );
    }
    assert_eq!(confirmer.record_count(), 0);
}

#[test]
fn la_non_interactive_round_trip_maps_codes() {
    let gui = has_gui();
    eprintln!("graphic session: {gui}");

    let quiet = Confirmer::with_la(LaOptions::quiet());
    check(&quiet, gui, Code::Unsupported, "not_interactive", -1004);
    // probe() agrees: without a GUI it says no_gui_session; with one, the production
    // preflight passes or names why it cannot.
    let t = Instant::now();
    let p = quiet.probe();
    assert!(t.elapsed() < Duration::from_secs(5));
    if !gui {
        let e = p.expect_err("no GUI");
        assert_eq!(detail_str(&e, "reason"), Some("no_gui_session"));
    } else if let Err(e) = p {
        assert_eq!(e.code, Code::Unsupported, "{e:?}");
    }

    let stale = Confirmer::with_la(LaOptions {
        invalidate_first: true,
        ..LaOptions::quiet()
    });
    check(&stale, gui, Code::NotConfirmed, "unexpected", -10);

    // The same from a background thread: the reply arrives on the framework's queue.
    let gui2 = gui;
    std::thread::spawn(move || {
        let c = Confirmer::with_la(LaOptions::quiet());
        check(&c, gui2, Code::Unsupported, "not_interactive", -1004);
    })
    .join()
    .unwrap();
}

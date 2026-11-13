//! Deterministic policy timelines: injected monotonic + wall stamps, no live I/O.
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime};
use strata_desktop_lib::{
    daemon::Read,
    error::GuiError,
    tray_model::{Icon, WaitingKind, WarningClass, model_at, warning_class},
    tray_snapshot::{Snapshot, Stamp},
};
fn stamp(t: u64) -> Stamp {
    // All fixture stamps use one monotonic epoch, independent of test execution time.
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    Stamp {
        instant: *BASE.get_or_init(Instant::now) + Duration::from_secs(t),
        wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + t),
    }
}
fn error(reason: &str) -> GuiError {
    GuiError {
        code: "Busy".into(),
        message: "secret error payload".into(),
        detail: json!({"reason":reason}),
    }
}
fn record(s: &mut Snapshot, r: Read, at: u64, completed: u64, result: Result<Value, GuiError>) {
    let generation = s.input(&r).map(|i| i.applied_gen + 1);
    s.record(&r, generation, stamp(at), stamp(completed), &result);
}
fn daemon() -> Value {
    json!({"api":1,"features":["status.upstream"],"fds":{"open":10,"limit":100,"accept_errors":0},"warnings":[]})
}
fn session(id: &str, auto: Value) -> Value {
    json!({"status":{"state":"Active","manifest":{"id":id,"repos":{"repo":{"tracked":true}}},"upstream":{"repos":{}},"workspaces":{"repo":{"health":"Ok","pending":[],"conflicts":[],"upstream":{"auto":auto}}}}})
}
fn good(t: u64) -> Snapshot {
    let mut s = Snapshot::default();
    for (r, v) in [
        (Read::DaemonStatus, daemon()),
        (Read::Sessions, json!({"sessions":[]})),
        (Read::DepsList, json!({"proposals":[]})),
    ] {
        record(&mut s, r, t, t, Ok(v));
    }
    s
}
fn hold(repo: &str, method: &str, since: u64) -> Value {
    json!({"repo":repo,"method":method,"since":stamp(since).wall})
}
fn proposal(s: &mut Snapshot) {
    s.deps.value = Some(
        json!({"proposals":[{"id":1,"repo":"repo","mode":"confirm","state":{"kind":"Pending"}}]}),
    );
}
fn conflict(s: &mut Snapshot) {
    let mut v = session("s-a", json!({}));
    v["status"]["workspaces"]["repo"]["conflicts"] = json!(["secret path"]);
    s.sessions.value.as_mut().unwrap()["sessions"]
        .as_array_mut()
        .unwrap()
        .push(v);
}
fn activate(s: &mut Snapshot, icon: Icon) {
    match icon {
        Icon::Down => record(s, Read::DaemonStatus, 100, 100, Err(error("daemon_down"))),
        Icon::Alert => {
            s.daemon_status.value.as_mut().unwrap()["warnings"] = json!(["StorePoisoned"])
        }
        Icon::Checking => s.deps.at = None,
        Icon::Waiting => proposal(s),
        Icon::Conflict => conflict(s),
        Icon::Syncing => {
            let v = session("s-sync", json!({"state":"pending"}));
            s.sessions.value.as_mut().unwrap()["sessions"]
                .as_array_mut()
                .unwrap()
                .push(v);
        }
        Icon::Normal => {}
    }
}
fn assert_active(s: &Snapshot, icon: Icon) {
    match icon {
        Icon::Down => assert!(s.last_down.is_some()),
        Icon::Alert => assert!(
            s.daemon_status.value.as_ref().unwrap()["warnings"]
                .as_array()
                .unwrap()
                .contains(&json!("StorePoisoned"))
        ),
        Icon::Checking => assert!(s.deps.at.is_none()),
        Icon::Waiting => assert!(
            s.deps.value.as_ref().unwrap()["proposals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["mode"] == "confirm" && p["state"]["kind"] == "Pending")
        ),
        Icon::Conflict | Icon::Syncing => {
            assert!(
                s.sessions.value.as_ref().unwrap()["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| {
                        let ws = &v["status"]["workspaces"]["repo"];
                        if icon == Icon::Conflict {
                            ws["conflicts"].as_array().is_some_and(|c| !c.is_empty())
                        } else {
                            s.upstream_enabled()
                                && v["status"]["upstream"].is_object()
                                && ws["upstream"]["auto"]["state"] == "pending"
                        }
                    })
            );
        }
        Icon::Normal => panic!("Normal is a fallback, not an independent condition"),
    }
}
#[test]
fn every_priority_row_and_pair() {
    for (i, first) in Icon::ALL.into_iter().enumerate() {
        let mut s = good(100);
        activate(&mut s, first);
        assert_eq!(model_at(&s, stamp(100)).icon, first);
        // Normal has no positive trigger; the standalone row above covers its fallback.
        for second in Icon::ALL
            .into_iter()
            .skip(i + 1)
            .filter(|s| *s != Icon::Normal)
        {
            let mut s = good(100);
            activate(&mut s, second);
            activate(&mut s, first);
            assert_active(&s, first);
            assert_active(&s, second);
            assert_eq!(
                model_at(&s, stamp(100)).icon,
                first,
                "{first:?} + {second:?}"
            );
        }
    }
}
#[test]
fn missing_stale_legacy_and_unsupported_inputs() {
    assert_eq!(
        model_at(&Snapshot::default(), stamp(0)).icon,
        Icon::Checking
    );
    let mut s = good(0);
    proposal(&mut s);
    for reason in ["shutting_down", "rpc_failed", "gui_internal"] {
        record(&mut s, Read::Sessions, 40, 40, Err(error(reason)));
    }
    assert_eq!(model_at(&s, stamp(50)).icon, Icon::Waiting);
    record(&mut s, Read::DaemonStatus, 100, 100, Ok(daemon()));
    let deps = s.deps.value.clone().unwrap();
    record(&mut s, Read::DepsList, 100, 100, Ok(deps));
    assert_eq!(model_at(&s, stamp(100)).icon, Icon::Checking);
    s.daemon_status
        .value
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("api");
    assert_eq!(model_at(&s, stamp(100)).icon, Icon::Waiting);
    record(&mut s, Read::DaemonStatus, 301, 301, Ok(json!({"fds":{}})));
    record(
        &mut s,
        Read::DepsList,
        301,
        301,
        Ok(json!({"proposals":[]})),
    );
    assert_eq!(model_at(&s, stamp(301)).icon, Icon::Checking);
    record(
        &mut s,
        Read::Sessions,
        301,
        301,
        Err(error("unknown_method")),
    );
    assert!(s.sessions.unsupported);
    assert!(s.sessions.value.is_none());
    assert_eq!(model_at(&s, stamp(301)).icon, Icon::Normal);
    record(&mut s, Read::Sessions, 302, 302, Ok(json!({"sessions":[]})));
    assert!(!s.sessions.unsupported);
}
#[test]
fn missing_session_enrichment_is_partial_but_does_not_hide_actions() {
    let mut s = good(0);
    let mut v = session("s-missing", json!({"state":"pending"}));
    v["status"].as_object_mut().unwrap().remove("upstream");
    s.sessions.value = Some(json!({"sessions":[v]}));
    assert_eq!(
        model_at(&s, stamp(0)).tooltip,
        "GIT STRATA: Some session details are unavailable"
    );
    assert_eq!(model_at(&s, stamp(0)).unavailable, 1);
    proposal(&mut s);
    assert_eq!(model_at(&s, stamp(0)).icon, Icon::Waiting);
    s.deps.value = Some(json!({"proposals":[]}));
    s.sessions.value.as_mut().unwrap()["sessions"][0]["status"]["workspaces"]["repo"]["conflicts"] =
        json!(["secret"]);
    assert_eq!(model_at(&s, stamp(0)).icon, Icon::Conflict);
    for scene in [false, true] {
        let mut s = good(0);
        let mut v = session("s-a", json!({}));
        v["status"]["workspaces"]["repo"]
            .as_object_mut()
            .unwrap()
            .remove("upstream");
        v["status"]["workspaces"]["repo"]["health"] = json!("Missing");
        if scene {
            v["status"]["scene"] = json!({});
        }
        s.sessions.value = Some(json!({"sessions":[v]}));
        assert_eq!(model_at(&s, stamp(0)).icon, Icon::Normal);
    }
}
#[test]
fn lasting_outage_hung_daemon_and_expired_evidence() {
    let mut s = Snapshot::default();
    for t in (0..=600).step_by(10) {
        record(&mut s, Read::DaemonStatus, t, t, Err(error("daemon_down")));
        assert_eq!(model_at(&s, stamp(t)).icon, Icon::Down);
    }
    assert_eq!(model_at(&s, stamp(661)).icon, Icon::Checking);
    let mut s = good(0);
    for t in (15..=605).step_by(10) {
        s.timeout(&Read::DaemonStatus, stamp(t));
        assert_eq!(s.status_timeouts, (t - 5) as u32 / 10);
        assert_eq!(
            model_at(&s, stamp(t)).icon,
            if t == 15 { Icon::Normal } else { Icon::Down }
        );
    }
}
#[test]
fn late_oks_reset_timeouts_and_17_second_steady_state_uses_completion() {
    let mut s = good(0);
    record(&mut s, Read::DaemonStatus, 0, 7, Ok(daemon()));
    for start in (10..610).step_by(20) {
        s.timeout(&Read::DaemonStatus, stamp(start + 5));
        s.timeout(&Read::DaemonStatus, stamp(start + 15));
        assert_eq!(s.status_timeouts, 2);
        assert!(s.last_status_ok.unwrap().age(stamp(start + 15)) < Duration::from_secs(20));
        assert_ne!(model_at(&s, stamp(start + 15)).icon, Icon::Down);
        record(&mut s, Read::DaemonStatus, start, start + 17, Ok(daemon()));
        assert_eq!(s.daemon_status.at.unwrap().wall, stamp(start).wall);
        assert_eq!(s.status_timeouts, 0);
    }
    let mut s = good(0);
    for start in (10..610).step_by(30) {
        for d in [5, 15, 25] {
            s.timeout(&Read::DaemonStatus, stamp(start + d));
        }
        assert_eq!(model_at(&s, stamp(start + 25)).icon, Icon::Down);
        record(&mut s, Read::DaemonStatus, start, start + 26, Ok(daemon()));
        assert_ne!(model_at(&s, stamp(start + 26)).icon, Icon::Down);
    }
    let mut s = good(0);
    for start in (10..600).step_by(10) {
        s.timeout(&Read::DaemonStatus, stamp(start + 5));
        record(&mut s, Read::DaemonStatus, start, start + 6, Ok(daemon()));
        assert_ne!(model_at(&s, stamp(start + 6)).icon, Icon::Down);
    }
}
#[test]
fn timeout_observations_coalesce_without_blocking_late_reply_generations() {
    let mut s = good(0);
    s.timeout(&Read::DaemonStatus, stamp(25));
    s.timeout(&Read::DaemonStatus, stamp(27));
    assert_eq!(s.status_timeouts, 1);
    record(&mut s, Read::DaemonStatus, 28, 28, Err(error("busy")));
    s.timeout(&Read::DaemonStatus, stamp(35));
    assert_eq!(s.status_timeouts, 2);
    assert_eq!(model_at(&s, stamp(35)).icon, Icon::Down);
    record(&mut s, Read::DaemonStatus, 10, 36, Ok(daemon()));
    assert_eq!(s.status_timeouts, 0);
    assert_ne!(model_at(&s, stamp(36)).icon, Icon::Down);
    for r in [Read::Sessions, Read::DepsList] {
        s.timeout(&r, stamp(50));
    }
    assert_eq!(s.status_timeouts, 0);
}
#[test]
fn shed_wire_and_fd_evidence_survive_stale_inputs_and_interleaved_closes() {
    let raw = "{\"id\":null,\"ok\":false,\"error\":{\"code\":\"Busy\",\"message\":\"secret\",\"detail\":{\"reason\":\"too_many_open_files\"}}}\n";
    let e: GuiError = strata_client::parse_reply("daemon_status", "42", raw)
        .unwrap_err()
        .into();
    let mut s = good(0);
    record(&mut s, Read::DaemonStatus, 0, 0, Err(error("daemon_down")));
    for t in (5..605).step_by(10) {
        record(&mut s, Read::DaemonStatus, t, t, Err(e.clone()));
        record(
            &mut s,
            Read::Sessions,
            t + 5,
            t + 5,
            Err(error("daemon_down")),
        );
        assert_eq!(model_at(&s, stamp(t + 5)).icon, Icon::Alert);
    }
    assert_eq!(model_at(&s, stamp(660)).icon, Icon::Checking);
    record(&mut s, Read::Sessions, 661, 661, Err(error("daemon_down")));
    assert_eq!(model_at(&s, stamp(661)).icon, Icon::Down);
}
#[test]
fn fd_pressure_error_growth_and_warning_priority() {
    let mut s = good(0);
    let mut d = daemon();
    d["fds"]["open"] = json!(90);
    record(&mut s, Read::DaemonStatus, 100, 100, Ok(d));
    assert_eq!(model_at(&s, stamp(100)).icon, Icon::Alert);
    let mut d = daemon();
    d["fds"]["accept_errors"] = json!(1);
    record(&mut s, Read::DaemonStatus, 101, 101, Ok(d));
    assert_eq!(model_at(&s, stamp(101)).icon, Icon::Alert);
    let mut d = daemon();
    d["fds"]["open"] = Value::Null;
    d["warnings"] = json!([{"VerifyFailed":{"path":"secret"}}]);
    record(&mut s, Read::DaemonStatus, 600, 600, Ok(d));
    let m = model_at(&s, stamp(600));
    assert_eq!(m.icon, Icon::Alert);
    assert!(m.tooltip.ends_with("waiting items not current"));
    assert!(!m.tooltip.contains("secret"));
}
#[test]
fn stale_generation_changes_neither_values_nor_freshness_nor_liveness() {
    let mut s = good(0);
    s.record(
        &Read::DaemonStatus,
        Some(20),
        stamp(20),
        stamp(25),
        &Ok(daemon()),
    );
    for result in [
        Ok(json!({"warnings":["StorePoisoned"]})),
        Err(error("daemon_down")),
        Err(error("too_many_open_files")),
    ] {
        s.record(&Read::DaemonStatus, Some(19), stamp(10), stamp(40), &result);
    }
    assert_eq!(s.daemon_status.applied_gen, 20);
    assert_eq!(s.daemon_status.at.unwrap().wall, stamp(20).wall);
    assert_eq!(s.last_status_ok.unwrap().wall, stamp(25).wall);
    assert!(s.fd_at.is_none());
    assert!(s.last_down.is_none());
}
#[test]
fn sleep_and_backwards_wall_clock_do_not_make_old_data_fresh() {
    let s = good(0);
    let mut backward = stamp(600);
    backward.wall = stamp(0).wall - Duration::from_secs(3600);
    assert_eq!(model_at(&s, backward).icon, Icon::Checking);
    let sleep = Stamp {
        instant: stamp(0).instant,
        wall: stamp(600).wall,
    };
    assert_eq!(model_at(&s, sleep).icon, Icon::Checking);
}
#[test]
fn warning_classification_including_unknown_and_malformed() {
    let mut s = good(0);
    let now = stamp(0).wall;
    for name in [
        "StorePoisoned",
        "VerifyFailed",
        "UnknownPublish",
        "UnknownPush",
        "Quarantine",
        "AttachOutside",
    ] {
        assert_eq!(warning_class(&json!(name), &s, now), WarningClass::Error);
    }
    assert_eq!(
        warning_class(&json!("ConfigRelaxPending"), &s, now),
        WarningClass::Waiting
    );
    for name in [
        "LongHold",
        "PublishRunning",
        "AnchorNotAdvanced",
        "FetchFailed",
        "StaleRefLock",
        "FutureWarning",
    ] {
        assert_eq!(warning_class(&json!(name), &s, now), WarningClass::Notice);
    }
    s.daemon_status.value.as_mut().unwrap()["warnings"] = json!(["FutureWarning",{"FetchFailed":{"path":"secret"}},{"ConfigRelaxPending":{"repo":"repo","reason":"secret"}}]);
    let m = model_at(&s, stamp(0));
    assert_eq!(m.icon, Icon::Waiting);
    assert_eq!(m.waiting[0].id(), "waiting:relax:repo");
    assert!(m.status_line.ends_with("2 notices"));
    assert!(!m.waiting[0].label().contains("secret"));
    s.daemon_status.value.as_mut().unwrap()["warnings"] = json!({"StorePoisoned":{}});
    assert_eq!(model_at(&s, stamp(0)).icon, Icon::Normal);
}
#[test]
fn background_long_holds_use_their_own_method_and_thirty_minute_limit() {
    let now = 2_000;
    for method in ["catch_up", "old_tree", "periodic_fetch", "daemon"] {
        for age in [120, 1_799, 1_800, 1_860] {
            let mut s = good(now);
            let d = s.daemon_status.value.as_mut().unwrap();
            d.as_object_mut().unwrap().remove("api");
            // The independently sampled holds need not match this warning.
            d["holds"] =
                json!([{"repo":"other","method":"publish","session":null,"since":stamp(now).wall}]);
            d["warnings"] = json!([{"LongHold":hold("tool", method, now - age)}]);
            let m = model_at(&s, stamp(now));
            assert_eq!(m.icon, Icon::Normal, "{method}, {age} s");
            assert_eq!(
                m.status_line,
                if age < 1_800 {
                    "Daemon running, 0 sessions"
                } else {
                    "Daemon running, 0 sessions · 1 notice"
                },
                "{method}, {age} s"
            );
        }
    }
}
#[test]
fn request_and_abnormal_long_holds_remain_notices() {
    let now = 2_000;
    for method in ["publish", "push", "sync", "new", "future_method"] {
        let mut s = good(now);
        let d = s.daemon_status.value.as_mut().unwrap();
        d["holds"] = json!([hold("tool", "catch_up", now - 40)]);
        d["warnings"] = json!([{"LongHold":hold("tool", method, now - 40)}]);
        assert_eq!(
            model_at(&s, stamp(now)).status_line,
            "Daemon running, 0 sessions · 1 notice",
            "{method}"
        );
    }
    for origin in [json!("background"), json!("request"), Value::Null] {
        let mut s = good(now);
        let mut warning = hold("tool", "catch_up", now - 120);
        warning["origin"] = origin;
        s.daemon_status.value.as_mut().unwrap()["warnings"] = json!([{"LongHold":warning}]);
        assert_eq!(
            model_at(&s, stamp(now)).status_line,
            "Daemon running, 0 sessions · 1 notice"
        );
    }
    let mut s = good(now);
    let d = s.daemon_status.value.as_mut().unwrap();
    d["features"] = json!(["status.upstream", "daemon_status.activity"]);
    d["warnings"] = json!([{"LongHold":hold("tool", "catch_up", now - 120)}]);
    assert_eq!(
        model_at(&s, stamp(now)).status_line,
        "Daemon running, 0 sessions · 1 notice"
    );
}
#[test]
fn long_holds_with_unusable_start_times_stay_notices() {
    let now = 2_000;
    for since in [
        Value::Null,
        json!({}),
        json!({"secs_since_epoch":1_800_000_000}),
        json!({"secs_since_epoch":1_800_000_000,"nanos_since_epoch":1_000_000_000}),
        json!("secret invalid time"),
        json!(stamp(now + 1).wall),
    ] {
        let mut s = good(now);
        s.daemon_status.value.as_mut().unwrap()["warnings"] =
            json!([{"LongHold":{"repo":"tool","method":"catch_up","since":since}}]);
        assert_eq!(
            model_at(&s, stamp(now)).status_line,
            "Daemon running, 0 sessions · 1 notice"
        );
    }
}
#[test]
fn syncing_tooltip_names_catch_up_repo_and_elapsed_without_changing_status_line() {
    let now = 2_000;
    for source in ["holds", "warnings"] {
        let mut s = good(now);
        activate(&mut s, Icon::Syncing);
        s.sessions.value.as_mut().unwrap()["sessions"][0]["status"]["manifest"]["ticket"] =
            json!("secret ticket");
        let mut h = hold("tool", "catch_up", now - 120);
        let d = s.daemon_status.value.as_mut().unwrap();
        d.as_object_mut().unwrap().remove("api");
        d[source] = if source == "holds" {
            h["session"] = Value::Null;
            json!([h])
        } else {
            json!([{"LongHold":h}])
        };
        let m = model_at(&s, stamp(now));
        assert_eq!(m.icon, Icon::Syncing);
        assert_eq!(
            m.tooltip,
            "GIT STRATA: Bringing in newer changes: tool, 2 min"
        );
        assert_eq!(m.status_line, "Daemon running, 1 session");
    }
}
#[test]
fn syncing_tooltip_uses_oldest_catch_up_across_holds_and_long_holds() {
    let now = 2_000;
    let mut s = good(now);
    activate(&mut s, Icon::Syncing);
    let d = s.daemon_status.value.as_mut().unwrap();
    d["holds"] = json!([
        hold("newer", "catch_up", now - 60),
        hold("tool", "catch_up", now - 120),
        hold("not-catch-up", "publish", now - 600)
    ]);
    assert_eq!(
        model_at(&s, stamp(now)).tooltip,
        "GIT STRATA: Bringing in newer changes: tool, 2 min"
    );
    s.daemon_status.value.as_mut().unwrap()["holds"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(
        model_at(&s, stamp(now)).tooltip,
        "GIT STRATA: Bringing in newer changes: tool, 2 min"
    );
    s.daemon_status.value.as_mut().unwrap()["warnings"] =
        json!([{"LongHold":hold("oldest", "catch_up", now - 180)}]);
    assert_eq!(
        model_at(&s, stamp(now)).tooltip,
        "GIT STRATA: Bringing in newer changes: oldest, 3 min"
    );
}
#[test]
fn catch_up_holds_do_not_change_non_syncing_icons_or_copy() {
    for icon in Icon::ALL.into_iter().filter(|i| *i != Icon::Syncing) {
        let mut s = good(120);
        activate(&mut s, icon);
        let before = model_at(&s, stamp(120));
        assert_eq!(before.icon, icon);
        let d = s.daemon_status.value.as_mut().unwrap();
        d["holds"] = json!([hold("tool", "catch_up", 0)]);
        d["warnings"]
            .as_array_mut()
            .unwrap()
            .push(json!({"LongHold":hold("tool", "catch_up", 0)}));
        assert_eq!(model_at(&s, stamp(120)), before, "{icon:?}");
    }
}
#[test]
fn deferral_threshold_conflict_exclusion_dedup_and_capabilities() {
    let now = 100_000;
    let since = strata_types::serde_rfc3339::format(&stamp(now - 86_460).wall).unwrap();
    let mut s = good(now);
    s.sessions.value = Some(
        json!({"sessions":[session("s-a",json!({"state":"deferred","reason":"open_files","since":since}))]}),
    );
    let m = model_at(&s, stamp(now));
    assert_eq!(m.icon, Icon::Waiting);
    assert_eq!(m.waiting[0].kind, WaitingKind::Deferred);
    assert_eq!(m.waiting[0].id(), "waiting:deferred:repo");
    let ws = &mut s.sessions.value.as_mut().unwrap()["sessions"][0]["status"]["workspaces"]["repo"];
    ws["pending"] = json!([1]);
    ws["upstream"]["deps"] = json!({"proposal":1,"mode":"apply","state":"ready"});
    s.deps.value = Some(
        json!({"proposals":[{"id":1,"repo":"repo","mode":"apply","state":{"kind":"Pending"}}]}),
    );
    let m = model_at(&s, stamp(now));
    assert_eq!(m.waiting.len(), 1);
    assert_eq!(m.waiting[0].kind, WaitingKind::DepsProposal);
    s.sessions.value.as_mut().unwrap()["sessions"][0]["status"]["workspaces"]["repo"]["upstream"]
        ["auto"]["reason"] = json!("conflicts");
    assert_eq!(model_at(&s, stamp(now)).icon, Icon::Conflict);
    s.deps.value = Some(json!({"proposals":[]}));
    for since in [
        json!(strata_types::serde_rfc3339::format(&stamp(now - 86_340).wall).unwrap()),
        json!("secret bad timestamp"),
        Value::Null,
    ] {
        s.sessions.value.as_mut().unwrap()["sessions"][0]["status"]["workspaces"]["repo"]["upstream"]
            ["auto"] = json!({"state":"deferred","reason":"open_files","since":since});
        assert_eq!(model_at(&s, stamp(now)).icon, Icon::Syncing);
    }
    s.daemon_status.value.as_mut().unwrap()["features"] = json!([]);
    assert_eq!(model_at(&s, stamp(now)).icon, Icon::Normal);
}
#[test]
fn stale_captions_use_oldest_contributor_counts_ignore_truncation_and_conflicts_dedup() {
    let mut s = good(0);
    proposal(&mut s);
    record(&mut s, Read::Sessions, 500, 500, Ok(json!({"sessions":[]})));
    record(&mut s, Read::DaemonStatus, 600, 600, Ok(daemon()));
    let m = model_at(&s, stamp(600));
    assert!(m.waiting[0].label().ends_with("(as of 10 min ago)"));
    record(
        &mut s,
        Read::DaemonStatus,
        600,
        600,
        Err(error("daemon_down")),
    );
    assert_eq!(model_at(&s, stamp(600)).icon, Icon::Down);
    assert!(
        model_at(&s, stamp(600)).waiting[0]
            .label()
            .ends_with("(as of 10 min ago)")
    );
    let mut s = good(0);
    s.deps.value = Some(
        json!({"proposals":(1..=7).map(|id|json!({"id":id,"repo":"repo","state":{"kind":"Pending"}})).collect::<Vec<_>>()}),
    );
    let m = model_at(&s, stamp(0));
    assert_eq!(m.more, 2);
    assert_eq!(m.tooltip, "GIT STRATA: 7 waiting for you");
    s.deps.value = Some(json!({"proposals":[]}));
    conflict(&mut s);
    let v = s.sessions.value.as_ref().unwrap()["sessions"][0].clone();
    s.sessions.value.as_mut().unwrap()["sessions"]
        .as_array_mut()
        .unwrap()
        .push(v);
    assert_eq!(
        model_at(&s, stamp(0)).tooltip,
        "GIT STRATA: 1 repo needs an agent"
    );
}
#[test]
fn poll_plan_starts_with_status_and_gentles_old_daemons() {
    let mut s = Snapshot::default();
    assert_eq!(
        s.due_in(&Read::DaemonStatus, false, stamp(0)),
        Some(Duration::ZERO)
    );
    assert_eq!(s.due_in(&Read::Sessions, false, stamp(0)), None);
    record(&mut s, Read::DaemonStatus, 0, 0, Ok(json!({})));
    s.daemon_status.last_attempt = Some(stamp(0));
    assert_eq!(
        s.due_in(&Read::DaemonStatus, false, stamp(10)),
        Some(Duration::from_secs(20))
    );
    assert_eq!(s.due_in(&Read::DepsList, false, stamp(30)), None);
    assert!(s.due_in(&Read::DepsList, true, stamp(30)).is_some());
    record(&mut s, Read::DaemonStatus, 31, 31, Ok(daemon()));
    s.sessions.last_attempt = Some(stamp(31));
    assert_eq!(
        s.due_in(&Read::Sessions, false, stamp(35)),
        Some(Duration::from_secs(26))
    );
    record(
        &mut s,
        Read::DaemonStatus,
        35,
        35,
        Err(error("daemon_down")),
    );
    assert_eq!(s.due_in(&Read::Sessions, true, stamp(36)), None);
    record(&mut s, Read::DaemonStatus, 40, 40, Ok(daemon()));
    assert!(s.due_in(&Read::Sessions, false, stamp(40)).is_some());
}

#[test]
fn visible_window_phase_retains_the_approved_twenty_second_gate() {
    let mut s = good(0);
    // Window flight begins at t=5, times out at 10; poller is due at 15,
    // joins and times out at 20. With no Ok for 20 s the approved gate fires.
    s.timeout(&Read::DaemonStatus, stamp(10));
    s.timeout(&Read::DaemonStatus, stamp(20));
    assert_eq!(model_at(&s, stamp(20)).icon, Icon::Down);
    record(&mut s, Read::DaemonStatus, 5, 22, Ok(daemon()));
    assert_ne!(model_at(&s, stamp(22)).icon, Icon::Down);
}
#[test]
fn partial_enrichment_precedes_another_sessions_sync_and_push_is_not_sync() {
    let mut s = good(0);
    let mut missing = session("s-missing", json!({}));
    missing["status"]
        .as_object_mut()
        .unwrap()
        .remove("upstream");
    s.sessions.value =
        Some(json!({"sessions":[missing,session("s-sync",json!({"state":"pending"}))]}));
    assert_eq!(model_at(&s, stamp(0)).icon, Icon::Checking);
    for state in ["needs_push", "publish_unfinished"] {
        let mut s = good(0);
        s.sessions.value = Some(json!({"sessions":[session("s-a",json!({"state":state}))]}));
        s.daemon_status.value.as_mut().unwrap()["holds"] = json!([{"repo":"repo"}]);
        assert_eq!(model_at(&s, stamp(0)).icon, Icon::Normal);
    }
}

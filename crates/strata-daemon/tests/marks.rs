//! Marking is a queued metadata write, visible on every existing read surface.
mod support;

use serde_json::{Value, json};
use strata_daemon::{
    handlers::{Server, Trackers},
    rpc::{Method, Params, Request},
    startup::Daemon,
};
use strata_store::Lifecycle;
use strata_types::SessionId;
use support::*;

fn call(server: &Server<'_, '_>, name: &str, params: Value) -> Value {
    let method = Method::from_name(name).unwrap();
    server
        .handle(Request {
            id: json!(1),
            method,
            params: Params::parse(method, params).unwrap(),
        })
        .0
}

fn ok(reply: Value) -> Value {
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"].clone()
}

#[test]
fn mark_round_trip_on_status_summary_and_sync_check_without_file_changes() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let _catalog = core.admit();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let created = ok(call(
        &server,
        "new",
        json!({"ticket":"T-mark", "repos":["tool"]}),
    ));
    let id: SessionId =
        serde_json::from_value(created["session"]["manifest"]["id"].clone()).unwrap();
    let before = core.store.session(&id).unwrap().unwrap();
    let reply = ok(call(
        &server,
        "mark",
        json!({"session":id,"state":"done","note":"<b>landed</b>\tready\nreview","agent":"codex"}),
    ));
    assert_eq!(reply["session"], id.as_str());
    assert_eq!(reply["mark"]["state"], "done");
    assert_eq!(reply["mark"]["by"], json!({"kind":"agent"}));
    assert!(reply["mark"]["at"].as_str().unwrap().contains('T'));
    for (params, path) in [
        (
            json!({"cwd":null,"session":id,"all":false}),
            vec!["sessions", "0", "status"],
        ),
        (
            json!({"cwd":null,"session":null,"summary":true,"all":true}),
            vec!["sessions", "0"],
        ),
    ] {
        let status = ok(call(&server, "status", params));
        let row = path
            .iter()
            .fold(&status, |v, k| if *k == "0" { &v[0] } else { &v[*k] });
        assert_eq!(row["mark"], reply["mark"]);
    }
    let check = ok(call(&server, "sync", json!({"session":id,"check":true})));
    assert_eq!(check["mark"], reply["mark"]);
    let after = core.store.session(&id).unwrap().unwrap();
    assert_eq!(after.manifest, before.manifest);
    assert_eq!(after.lifecycle, Lifecycle::Active);
    assert!(core.store.checkpoints(&id, None).unwrap().is_empty());
    assert_eq!(
        ok(call(
            &server,
            "mark",
            json!({"session":id,"state":"active"})
        ))["mark"],
        json!({"state":"active"})
    );
    assert!(core.store.mark(&id).unwrap().is_none());
    let check = ok(call(
        &server,
        "sync",
        json!({"session":id,"check":true,"hook":true}),
    ));
    assert!(check.get("mark").is_none());
    let summaries = ok(call(
        &server,
        "status",
        json!({"cwd":null,"session":null,"summary":true,"all":true}),
    ));
    assert!(summaries["sessions"][0].get("mark").is_none());
    trackers.clear();
}

#[test]
fn mark_validates_notes_agent_and_lifecycle_before_metadata_changes() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let _catalog = core.admit();
    let daemon = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&daemon, &trackers);
    let report = daemon.sessions.new(None, &[repo_name("tool")]).unwrap();
    let id = report.manifest.id;
    for note in [
        "".to_string(),
        "é".repeat(1001),
        "bad\rnote".into(),
        "bad\u{001b}note".into(),
    ] {
        let reply = call(
            &server,
            "mark",
            json!({"session":id,"state":"done","note":note}),
        );
        assert_eq!(reply["error"]["code"], "InvalidInput", "{reply}");
        assert_eq!(reply["error"]["detail"]["reason"], "note");
        assert!(core.store.mark(&id).unwrap().is_none());
    }
    let bad_agent = call(
        &server,
        "mark",
        json!({"session":id,"state":"done","agent":"bad agent"}),
    );
    assert_eq!(bad_agent["error"]["detail"]["reason"], "agent");
    let marked = ok(call(&server, "mark", json!({"session":id,"state":"done"})));
    assert_eq!(marked["mark"]["by"]["kind"], "cli");
    core.store
        .set_lifecycle(
            &id,
            Lifecycle::Active,
            Lifecycle::Removing,
            None,
            std::time::SystemTime::now(),
        )
        .unwrap();
    for state in ["done", "active"] {
        let reply = call(&server, "mark", json!({"session":id,"state":state}));
        assert_eq!(reply["error"]["code"], "InvalidInput");
        assert_eq!(reply["error"]["detail"]["reason"], "lifecycle");
    }
    assert!(core.store.mark(&id).unwrap().is_some());
    let missing = call(
        &server,
        "mark",
        json!({"session":"s-missingx","state":"done"}),
    );
    assert_eq!(missing["error"]["code"], "NoSuchSession");
    assert!(strata_daemon::FEATURES.contains(&"session.mark"));
    trackers.clear();
}

#[test]
fn mark_rpc_has_a_closed_parameter_and_state_set() {
    let good = json!({"session":"s-aaaaaaaa","state":"done","note":null,"agent":null});
    assert!(Params::parse(Method::Mark, good.clone()).is_ok());
    for extra in ["force", "discard", "reason", "by_kind"] {
        let mut params = good.clone();
        params[extra] = json!(true);
        assert!(Params::parse(Method::Mark, params).is_err(), "{extra}");
    }
    for state in ["archived", "exported", "DONE", ""] {
        let mut params = good.clone();
        params["state"] = json!(state);
        assert!(Params::parse(Method::Mark, params).is_err(), "{state}");
    }
    assert!(!Method::Mark.is_read_only());
}

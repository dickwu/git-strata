//! cli.md D30: a bad `publish --from` PATH is reported as an input error
//! naming the path and the expected form, before the TTY gate (a guarded
//! repo in the config must not turn it into `NotTty`), and nothing but the
//! test's own setup reaches the daemon.
//!
//! p1-autoclose §6: what the rebuilt `publish` and `push` envelopes keep of the
//! daemon's reply (a fake daemon), and the product default end to end.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use support::live::{Live, write};
use support::{FakeDaemon, Home, RepoSpec};

fn live() -> Live {
    let lv = Live::new(&["app", "lib"], &[("a.txt", b"one\n"), ("src/x.ts", b"x\n")]);
    lv.write_config_guarded(&["app"]);
    lv
}

#[test]
fn publish_from_bad_paths_are_input_errors_not_not_tty() {
    let lv = live();
    lv.serve(|_core| {
        let s = lv.new_session("T-1", "app,lib");
        let ws = lv.ws(&s, "app");
        write(&ws, "a.txt", b"two\n");
        let seen = lv.requests.lock().unwrap().len();

        // A repo-prefixed path: the error names the repo and the fix.
        let prefixed = format!("{s}:app/a.txt");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &prefixed]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_repo_prefix", "{r:?}");
        let d = &r.env()["error"]["detail"];
        assert_eq!(d["repo"], "app");
        assert_eq!(d["path"], "app/a.txt");
        assert_eq!(d["expected"], format!("{s}:a.txt"));

        // A path no workspace has: the error names it and the format.
        let missing = format!("{s}:nope/b.txt");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &missing]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_path", "{r:?}");
        let e = &r.env()["error"];
        assert_eq!(e["detail"]["path"], "nope/b.txt");
        assert!(e["message"].as_str().unwrap_or_default().contains("SESSION:PATH"), "{e}");

        // In both repos: AmbiguousMatch with the candidates, still no gate.
        let both = format!("{s}:src/x.ts");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &both]);
        assert_eq!(r.err_code(), "AmbiguousMatch", "{r:?}");
        assert_eq!(r.reason(), "from_repo", "{r:?}");

        std::thread::sleep(Duration::from_millis(50));
        assert!(lv.requests.lock().unwrap()[seen..].is_empty(), "a bad --from reached the daemon: {:?}", &lv.requests.lock().unwrap()[seen..]);
    });
}

#[test]
fn publish_from_prefix_is_caught_in_a_single_repo_session() {
    let lv = live();
    lv.serve(|_core| {
        let s = lv.new_session("T-2", "lib");
        let seen = lv.requests.lock().unwrap().len();
        let prefixed = format!("{s}:lib/a.txt");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &prefixed]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_repo_prefix", "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["expected"], format!("{s}:a.txt"));
        assert!(lv.requests.lock().unwrap()[seen..].is_empty());
    });
}

/// cli.md D31: `S:PATH` in several repos goes to the one repo whose
/// checkpoints changed it; `S:REPO:PATH` names the repo outright.
#[test]
fn publish_from_prefers_the_changed_repo_and_takes_a_repo_qualified_form() {
    let lv = Live::new(&["app", "lib"], &[("a.txt", b"one\n"), ("src/x.ts", b"x\n"), ("src/y.ts", b"y\n")]);
    lv.serve(|_core| {
        let s = lv.new_session("T-3", "app,lib");
        let app = lv.ws(&s, "app");
        let lib = lv.ws(&s, "lib");

        // No checkpoint changed src/x.ts yet: ambiguous, with the S:REPO:PATH hint.
        let both = format!("{s}:src/x.ts");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &both]);
        assert_eq!(r.err_code(), "AmbiguousMatch", "{r:?}");
        let d = &r.env()["error"]["detail"];
        assert_eq!(d["expected"][0], format!("{s}:app:src/x.ts"), "{d}");
        assert!(r.env()["error"]["message"].as_str().unwrap().contains("SESSION:REPO:PATH"), "{r:?}");

        // Only app's checkpoint changes src/x.ts: S:PATH picks app.
        write(&app, "src/x.ts", b"x2\n");
        lv.run_in(&app, &["checkpoint", "-m", "x"]).ok();
        let r = lv.run_in(&lv.sb, &["publish", "--from", &both]);
        let d = r.ok();
        let commits = d["commits"].as_object().expect("commits");
        assert_eq!(commits.keys().collect::<Vec<_>>(), vec!["app"], "{d}");

        // Explicit repo: S:app:src/y.ts (in both, changed in neither checkpoint yet).
        write(&app, "src/y.ts", b"y2\n");
        let explicit = format!("{s}:app:src/y.ts");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &explicit]);
        let d = r.ok();
        assert_eq!(d["commits"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["app"], "{d}");

        // Both repos changed it: ambiguous again, changed_in lists both.
        write(&app, "a.txt", b"app\n");
        write(&lib, "a.txt", b"lib\n");
        lv.run_in(&app, &["checkpoint", "-m", "a"]).ok();
        lv.run_in(&lib, &["checkpoint", "-m", "a"]).ok();
        let r = lv.run_in(&lv.sb, &["publish", "--from", &format!("{s}:a.txt")]);
        assert_eq!(r.err_code(), "AmbiguousMatch", "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["changed_in"], serde_json::json!(["app", "lib"]));

        // REPO:PATH with a path the repo lacks, and an unknown repo.
        let r = lv.run_in(&lv.sb, &["publish", "--from", &format!("{s}:lib:nope.txt")]);
        assert_eq!(r.reason(), "from_path", "{r:?}");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &format!("{s}:zzz:src/x.ts")]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_path", "{r:?}");
    });
}

// ---- p1-autoclose §6: the envelope over a fake daemon's reply ----

const COMMIT: &str = "1111111111111111111111111111111111111111";
/// A session outside the group, which this call's push re-judged.
const OTHER: &str = "s-bbbbbbbb";

/// A real session, because `build_batch` reads its row, then a fake daemon on
/// the socket. Returns the `--from` value too.
fn fake_publish(reply: fn(&Value) -> Value) -> (Live, FakeDaemon, String) {
    let lv = Live::new(&["tool"], &[("a.txt", b"one\n")]);
    let s = lv.serve(|_| lv.new_session("T-env", "tool"));
    let _ = std::fs::remove_file(lv.home.socket());
    let fake = FakeDaemon::start(&lv.home.socket(), reply);
    let from = format!("{s}:a.txt");
    (lv, fake, from)
}

/// The three requests of one `publish --from`: the before-publish checkpoint,
/// the plan, and `publish`, whose answer is the test's.
fn answer(q: &Value, publish: Value) -> Value {
    let data = match q["method"].as_str() {
        Some("checkpoint") => json!({ "checkpoint": { "id": 1 } }),
        Some("publish_plan") => json!({ "repos": ["tool"] }),
        _ => publish,
    };
    json!({ "id": q["id"], "ok": true, "data": data })
}

/// The reply of a publish whose session closed (§6's example).
fn closed_reply() -> Value {
    json!({
        "published": [{
            "repo": "tool", "id": 1, "commit": COMMIT, "checks_ran": 0,
            "push": { "kind": "Pushed" }, "warnings": [], "rebases": [],
        }],
        "already_published": [],
        "upstream": { "s-aaaaaaaa": { "as_of_rm": "before", "clean": true, "repos": {} } },
        "closed": true,
        "auto_close": {
            "closed": true,
            "reasons": [],
            "rejudged": [],
            "sessions": {
                "s-aaaaaaaa": {
                    "closed": true, "in_group": true, "reasons": [], "detail": {},
                    "rm": {
                        "session": "s-aaaaaaaa", "discarded": false, "checkpoints": { "tool": 17 },
                        "trash": [], "orphans": [], "warnings": [],
                    },
                    "mark": { "written": false },
                },
            },
        },
    })
}

#[test]
fn publish_envelope_carries_closed_and_auto_close() {
    let (lv, _fake, from) = fake_publish(|q| answer(q, closed_reply()));
    let d = lv.run(&["publish", "--from", &from]).ok();
    assert_eq!(d["commits"]["tool"], COMMIT, "{d}");
    assert_eq!(d["pushes"], json!([{ "repo": "tool", "outcome": { "kind": "Pushed" } }]), "{d}");
    assert_eq!(d["closed"], true, "{d}");
    assert_eq!(d["auto_close"], closed_reply()["auto_close"], "{d}");
    assert_eq!(d["upstream"], closed_reply()["upstream"], "{d}");
}

/// An earlier publish landed the repo and its push was refused: the row stays
/// `Done`, so a re-run answers `already_published`, and only `detail.fix` clears it.
fn refused_reply() -> Value {
    let reasons = json!(["not_pushed", "push_refused"]);
    json!({
        "published": [],
        "already_published": [{
            "repo": "tool", "publish": 7, "commit": COMMIT,
            "push": { "kind": "Refused", "message": "denied by policy" },
        }],
        "closed": false,
        "auto_close": {
            "closed": false,
            "reasons": reasons,
            "rejudged": [],
            "sessions": {
                "s-aaaaaaaa": {
                    "closed": false, "in_group": true, "reasons": reasons, "detail": { "repo": "tool" },
                    "rm": null, "mark": { "written": true, "at": "2026-11-05T00:00:00Z" },
                },
            },
        },
    })
}

#[test]
fn refused_already_published_push_is_ok_false() {
    let (lv, _fake, from) = fake_publish(|q| answer(q, refused_reply()));
    let r = lv.run(&["publish", "--from", &from]);
    assert_eq!(r.err_code(), "PushRefused", "{r:?}");
    let env = r.env();
    assert_eq!(env["error"]["message"], "denied by policy", "{env}");
    assert_eq!(env["error"]["detail"]["repo"], "tool", "{env}");
    assert_eq!(env["error"]["detail"]["fix"], "git-strata push tool", "{env}");
    let d = &env["data"];
    assert_eq!(d["closed"], false, "{env}");
    assert_eq!(d["auto_close"]["reasons"], json!(["not_pushed", "push_refused"]), "{env}");
    assert_eq!(d["already_published"], refused_reply()["already_published"], "{env}");
}

/// The daemon refreshed a refused entry whose commit has since reached origin.
#[test]
fn already_published_landed_later_is_ok() {
    let (lv, _fake, from) = fake_publish(|q| {
        let reply = json!({
            "published": [],
            "already_published": [{
                "repo": "tool", "publish": 7, "commit": COMMIT,
                "push": { "kind": "Pushed" }, "landed_later": true,
            }],
            "closed": true,
            "auto_close": { "closed": true, "reasons": [], "rejudged": [], "sessions": {} },
        });
        answer(q, reply)
    });
    let d = lv.run(&["publish", "--from", &from]).ok();
    assert_eq!(d["already_published"][0]["push"], json!({ "kind": "Pushed" }), "{d}");
    assert_eq!(d["already_published"][0]["landed_later"], true, "{d}");
    assert_eq!(d["closed"], true, "{d}");
}

/// `closed` is the group's: a stranger the push re-judged, left open here, is
/// only under `auto_close`, and the rebuild keeps it there.
fn rejudged_reply() -> Value {
    let mut reply = closed_reply();
    reply["auto_close"]["rejudged"] = json!([OTHER]);
    reply["auto_close"]["sessions"][OTHER] = json!({
        "closed": false, "in_group": false, "reasons": ["open_files"], "detail": { "count": 1 },
        "rm": null, "mark": { "written": false, "skipped": "unchanged" },
    });
    reply
}

#[test]
fn envelope_carries_rejudged_sessions() {
    let (lv, _fake, from) = fake_publish(|q| answer(q, rejudged_reply()));
    let d = lv.run(&["publish", "--from", &from]).ok();
    assert_eq!(d["closed"], true, "{d}");
    assert_eq!(d["auto_close"]["rejudged"], json!([OTHER]), "{d}");
    assert_eq!(d["auto_close"]["sessions"][OTHER]["in_group"], false, "{d}");
    assert_eq!(d["auto_close"]["sessions"][OTHER]["closed"], false, "{d}");
    assert_eq!(d["auto_close"], rejudged_reply()["auto_close"], "{d}");
}

/// `push`'s reply, by repo. `tool` judged a session and closed it. `front` has
/// only an opted-out candidate, so nothing was judged and `closed` is omitted.
/// `app`'s push was refused; the hook runs after a refusal too.
fn push_reply(q: &Value) -> Value {
    let data = match q["params"]["repo"].as_str() {
        Some("tool") => json!({
            "push": { "kind": "Pushed" }, "commit": COMMIT, "replayed": null, "rebases": [], "warnings": [],
            "closed": true,
            "auto_close": {
                "closed": true,
                "reasons": [],
                "rejudged": [OTHER],
                "sessions": {
                    "s-bbbbbbbb": {
                        "closed": true, "in_group": false, "reasons": [], "detail": {},
                        "rm": null, "mark": { "written": false },
                    },
                },
            },
        }),
        Some("front") => json!({
            "push": { "kind": "Pushed" }, "commit": COMMIT, "replayed": null, "rebases": [], "warnings": [],
            "auto_close": {
                "closed": false,
                "reasons": [],
                "rejudged": [],
                "sessions": {
                    "s-bbbbbbbb": {
                        "closed": false, "in_group": false, "reasons": ["disabled"], "detail": { "repo": "front" },
                        "rm": null, "mark": { "written": false },
                    },
                },
            },
        }),
        _ => json!({
            "push": { "kind": "Refused", "message": "denied by policy" }, "commit": COMMIT,
            "closed": false,
            "auto_close": {
                "closed": false,
                "reasons": ["not_pushed"],
                "rejudged": [OTHER],
                "sessions": {
                    "s-bbbbbbbb": {
                        "closed": false, "in_group": false, "reasons": ["not_pushed"], "detail": { "repo": "app" },
                        "rm": null, "mark": { "written": false, "skipped": "unchanged" },
                    },
                },
            },
        }),
    };
    json!({ "id": q["id"], "ok": true, "data": data })
}

#[test]
fn push_envelope_carries_closed() {
    let h = Home::new();
    let repos = ["tool", "front", "app"].map(|name| RepoSpec { name, push: true, confirm: false, deploy_on_push: false });
    h.write_config(&repos);
    let _fake = FakeDaemon::start(&h.socket(), push_reply);

    let d = h.run(&["push", "tool"]).ok();
    assert_eq!(d["kind"], "Pushed", "{d}");
    assert_eq!(d["commit"], COMMIT, "{d}");
    assert_eq!(d["closed"], true, "{d}");
    assert_eq!(d["auto_close"]["rejudged"], json!([OTHER]), "{d}");
    assert_eq!(d["auto_close"]["sessions"][OTHER]["closed"], true, "{d}");

    // Nothing judged: an empty conjunction never reads as a close.
    let d = h.run(&["push", "front"]).ok();
    assert!(d.get("closed").is_none(), "{d}");
    assert_eq!(d["auto_close"]["closed"], false, "{d}");
    assert_eq!(d["auto_close"]["sessions"][OTHER]["reasons"], json!(["disabled"]), "{d}");

    let r = h.run(&["push", "app"]);
    assert_eq!(r.err_code(), "PushRefused", "{r:?}");
    let env = r.env();
    assert_eq!(env["data"]["closed"], false, "{env}");
    assert_eq!(env["data"]["auto_close"]["reasons"], json!(["not_pushed"]), "{env}");
}

// ---- p1-autoclose §5: the default, through the real parse, daemon and envelope ----

/// The one CLI test of the default path: every other `Live` test keeps the
/// fixture's `auto_close = false`. Here because cli.rs is past the file limit.
#[test]
fn live_product_default_closes_on_publish() {
    let lv = Live::new(&["tool"], &[("a.txt", b"one\n")]);
    let cfg = lv.home.config_dir().join("config.toml");
    let t = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("[publish]\nauto_close = false\n", "")
        .replace("\npush = false", "\npush = true");
    assert!(!t.contains("auto_close"), "the table is absent: {t}");
    std::fs::write(&cfg, t).unwrap();
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        write(&lv.ws(&s, "tool"), "n.txt", b"n\n");
        let d = lv.run(&["publish", "--from", &format!("{s}:n.txt")]).ok();
        assert_eq!(d["closed"], true, "{d}");
        assert_eq!(d["auto_close"]["sessions"][s.as_str()]["closed"], true, "{d}");
        let r = lv.run(&["status", "--session", s.as_str()]);
        assert_eq!(r.err_code(), "NoSuchSession", "{r:?}");
    });
}

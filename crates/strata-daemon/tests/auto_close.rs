//! Auto-close §10 regression rows (Slice C–E).
//! Temp-dir fixtures only; default Dfx keeps `auto_close = false`.

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::Daemon;
use strata_store::{Lifecycle, NewProposal, PublishPhase};
use strata_types::{DepsSetId, MarkByKind, MarkState, RepoPath, SessionId, Upstream};
use support::close::{self, audit_lines, mark_of, opted_in};
use support::*;

fn pin_gone(core: &strata_daemon::startup::Core, s: &SessionId, repo: &str) -> bool {
    core.gits[&repo_name(repo)]
        .read_ref(&format!("refs/strata/pins/{s}"))
        .unwrap()
        .is_none()
}

fn lifecycle(core: &strata_daemon::startup::Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn session_entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn paths_of(detail: &Value, repo: &str) -> Vec<String> {
    detail["repos"][repo]["paths"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect()
}

/// Mirror of `git_strata::sync::block_reason` (sync.rs:133-188). The daemon
/// crate cannot depend on `git-strata` (cycle), so this §10 row 1 scene calls
/// the same sentences over the real publish/sync upstream JSON here.
fn block_reason(upstream: &Upstream, done: bool) -> Option<String> {
    let mut reasons = Vec::new();
    for (repo, u) in &upstream.repos {
        if let Some(auto) = &u.auto
            && auto.escalation_new
            && auto.state == "deferred"
        {
            let cause = strata_session::rebase_cause(
                auto.reason.as_deref().unwrap_or("busy"),
                auto.detail.as_ref(),
            );
            let action = if u.following {
                format!(
                    "ask the user to release the named holders or wait for a stopping point, then run `{}`{}",
                    u.fix
                        .as_deref()
                        .unwrap_or("cd ~ && git-strata sync --session <session-id>"),
                    if done { "" } else { " and cd back into the workspace" }
                )
            } else {
                "ask the user to release the named holders or pause writes, then review the dependency/rebase plan for this non-following repo".into()
            };
            reasons.push(format!(
                "auto-rebase of {repo} has been deferred {} times since {}: {cause}; {action}.",
                auto.attempts, auto.since
            ));
            continue;
        }
        let behind = if u.unchecked {
            0
        } else {
            u.behind.unwrap_or(0)
        };
        if !u.following
            || (behind == 0 && u.deps.is_none())
            || !(u.unpublished.unwrap_or(true)
                || u.auto.as_ref().and_then(|a| a.reason.as_deref()) == Some("conflicts")
                || !u.conflicts.is_empty())
        {
            continue;
        }
        let summary = u
            .deps
            .as_ref()
            .and_then(|d| d.summary.as_deref())
            .filter(|s| !s.is_empty());
        let prefix = if behind > 0 {
            let count = if u.capped {
                format!("at least {behind}")
            } else {
                behind.to_string()
            };
            let deps = summary.map(|s| format!(" (deps: {s})")).unwrap_or_default();
            format!("main moved {count} commits since your base in {repo}{deps}. Run")
        } else {
            let deps = summary.map(|s| format!(" ({s})")).unwrap_or_default();
            format!("main's dependencies changed in {repo}{deps}; run")
        };
        let then = if done {
            "; if it returns conflicts, resolve them as it says. This session is marked done: stay out of the workspace, because a shell inside it holds the session open, and run `mark active` first if you mean to keep working."
        } else {
            ", cd back to the workspace, resolve any conflicts it returns (merge upstream's change, then `cd ~ && git-strata sync --session <session-id> --resolved PATH --target … --generation …`), cd back, re-run checks, then publish."
        };
        reasons.push(format!("{prefix} `cd ~ && git-strata sync --session <session-id>`{then}"));
    }
    (!reasons.is_empty()).then(|| reasons.join("\n"))
}


#[test]
fn publish_closes_a_fully_landed_session() {
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let root = core.store.session(&s).unwrap().unwrap().manifest.root.clone();

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));

    assert_eq!(out["closed"], true, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["closed"], true, "{entry}");
    assert!(entry["rm"]["checkpoints"]["tool"].is_number(), "{entry}");
    assert!(
        entry["rm"].get("regenerable").is_none()
            || entry["rm"]["regenerable"]
                .as_object()
                .is_some_and(|m| m.is_empty()),
        "{entry}"
    );
    assert_eq!(out["upstream"][s.as_str()]["as_of_rm"], "before", "{out}");
    assert_eq!(
        out["upstream"][s.as_str()]["repos"]["tool"]["unpublished"],
        false,
        "{out}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(pin_gone(&core, &s, "tool"), "pin should be gone");
    assert!(!root.exists(), "session root should leave the live path");
    let trash = entry["rm"]["trash"].as_array().expect("rm.trash");
    assert!(!trash.is_empty(), "root reported in trash: {entry}");
    assert!(
        trash.iter().any(|p| {
            let p = PathBuf::from(p.as_str().unwrap());
            p.exists() || p.to_string_lossy().contains("trash")
        }),
        "{entry}"
    );
    assert!(mark_of(&core, &s).is_none(), "no session_marks row");
    let lines = audit_lines(&fx);
    assert!(
        lines.iter().any(|l| {
            l["event"] == "auto_close" && l["session"] == s.as_str() && l["closed"] == true
        }),
        "{lines:?}"
    );
}

#[test]
fn publish_marks_done_when_a_sibling_repo_has_unpublished_edits() {
    let fx = opted_in(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);

    fs::write(ws(&core, &s, "tool").join("t.txt"), "t").unwrap();
    fs::write(ws(&core, &s, "front").join("f.txt"), "f").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));

    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["closed"], false, "{entry}");
    assert_eq!(entry["reasons"], json!(["unpublished"]), "{entry}");
    assert!(
        entry["detail"]["repos"]
            .as_object()
            .is_some_and(|m| m.contains_key("front")),
        "detail.repos names the other repo: {entry}"
    );
    let mark = mark_of(&core, &s).expect("daemon mark");
    assert_eq!(mark.state, MarkState::Done);
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(
        note.starts_with("auto-close did not close this session: unpublished:"),
        "{note}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(!pin_gone(&core, &s, "tool"));
    assert!(!pin_gone(&core, &s, "front"));
}

#[test]
fn publish_marks_done_when_only_regenerable_edits_remain() {
    let mut fx = opted_in(&["tool"]);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .rm
        .regenerable = vec!["gen/**".into()];
    fx.write_config();
    let core = fx.prepare().unwrap();
    let _catalog = core.admit();
    assert_eq!(
        core.pinned(&repo_name("tool")).unwrap().regenerable,
        vec!["gen/**".to_string()],
        "regenerable must be on the pin"
    );
    drop(_catalog);
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let w = ws(&core, &s, "tool");
    fs::create_dir_all(w.join("gen")).unwrap();
    fs::write(w.join("gen/a.json"), "{}\n").unwrap();
    fs::write(w.join("src/index.ts"), "changed\n").unwrap();

    let out = close::ok(close::publish(
        &server,
        &core,
        &s,
        "tool",
        &["src/index.ts"],
    ));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["unpublished"]), "{entry}");
    assert_eq!(
        paths_of(&entry["detail"], "tool"),
        vec!["gen/a.json".to_string()],
        "{entry}"
    );
    let regen_paths: Vec<String> = entry["detail"]["regenerable"]["tool"]["paths"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    assert_eq!(regen_paths, vec!["gen/a.json".to_string()], "{entry}");
    let mark = mark_of(&core, &s).expect("daemon mark");
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(note.contains("(rm.regenerable)") || note.contains("rm.regenerable"), "{note}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);

    let rm = close::ok(close::call(
        &server,
        "rm",
        json!({ "session": s, "discard": false }),
    ));
    let got: Vec<String> = rm["regenerable"]["tool"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    assert_eq!(got, vec!["gen/a.json".to_string()], "{rm}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}

#[test]
fn publish_marks_done_not_pushed_when_push_is_false() {
    let mut fx = opted_in(&["tool"]);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["not_pushed"]), "{entry}");
    assert_eq!(entry["detail"]["repo"], "tool", "{entry}");
    assert!(
        entry["detail"]["ref"]
            .as_str()
            .is_some_and(|r| r.contains("origin") && r.contains("main")),
        "{entry}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(mark_of(&core, &s).is_some());

    let pushed = close::ok(close::call(&server, "push", json!({ "repo": "tool" })));
    assert_eq!(pushed["closed"], true, "{pushed}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(pin_gone(&core, &s, "tool"));
}

#[test]
fn push_judges_only_done_marked_sessions() {
    let mut fx = opted_in(&["tool"]);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let a = close::new(&server);
    let b = close::new(&server);

    fs::write(ws(&core, &a, "tool").join("a.txt"), "a").unwrap();
    close::ok(close::publish(&server, &core, &a, "tool", &["a.txt"]));
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b").unwrap();
    close::ok(close::publish(&server, &core, &b, "tool", &["b.txt"]));

    // Agent owns A's mark; clear B so push judges only A.
    close::ok(close::call(
        &server,
        "mark",
        json!({ "session": a, "state": "active" }),
    ));
    close::ok(close::call(
        &server,
        "mark",
        json!({ "session": a, "state": "done", "agent": "codex" }),
    ));
    close::ok(close::call(
        &server,
        "mark",
        json!({ "session": b, "state": "active" }),
    ));
    assert_eq!(
        mark_of(&core, &a).unwrap().by.kind,
        MarkByKind::Agent
    );
    assert!(mark_of(&core, &b).is_none());

    let pushed = close::ok(close::call(&server, "push", json!({ "repo": "tool" })));
    assert_eq!(pushed["closed"], true, "{pushed}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
    assert_eq!(lifecycle(&core, &b), Lifecycle::Active);
    assert!(pin_gone(&core, &a, "tool"));
    assert!(!pin_gone(&core, &b, "tool"));
}

#[test]
fn auto_close_disabled_judges_nothing() {
    // Global false, per-repo true on only one of two → still disabled.
    let mut fx = Dfx::with(&["tool", "front"]);
    fx.config.publish.auto_close = false;
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .auto_close = Some(true);
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t").unwrap();
    fs::write(ws(&core, &s, "front").join("f.txt"), "f").unwrap();
    // Publish both so the only refusal left would be none if enabled; here disabled wins.
    let cp_t = checkpoint(&core, &s, "tool");
    let cp_f = checkpoint(&core, &s, "front");
    let out = close::ok(close::call(
        &server,
        "publish",
        json!({
            "group": group(vec![
                batch(&s, "tool", cp_t.id, &["t.txt"]),
                batch(&s, "front", cp_f.id, &["f.txt"]),
            ]),
            "shown": {}
        }),
    ));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["disabled"]), "{entry}");
    assert!(entry["detail"]["repo"].is_string(), "{entry}");
    assert!(mark_of(&core, &s).is_none(), "no mark");
    assert!(
        audit_lines(&fx)
            .iter()
            .all(|l| l["session"] != s.as_str() || l["event"] != "auto_close"),
        "no audit line"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);

    // Per-repo false under global true is the same.
    drop(server);
    drop(trackers);
    drop(d);
    core.close().unwrap();

    let mut fx = opted_in(&["tool", "front"]);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .publish
        .auto_close = Some(false);
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);
    fs::write(ws(&core, &s, "tool").join("t2.txt"), "t").unwrap();
    fs::write(ws(&core, &s, "front").join("f2.txt"), "f").unwrap();
    let cp_t = checkpoint(&core, &s, "tool");
    let cp_f = checkpoint(&core, &s, "front");
    let out = close::ok(close::call(
        &server,
        "publish",
        json!({
            "group": group(vec![
                batch(&s, "tool", cp_t.id, &["t2.txt"]),
                batch(&s, "front", cp_f.id, &["f2.txt"]),
            ]),
            "shown": {}
        }),
    ));
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["disabled"]), "{entry}");
    assert_eq!(entry["detail"]["repo"], "front", "{entry}");
    assert!(mark_of(&core, &s).is_none());
    assert!(
        audit_lines(&fx)
            .iter()
            .all(|l| l["session"] != s.as_str() || l["event"] != "auto_close")
    );
}

#[test]
fn publish_error_path_judges_nothing() {
    let mut fx = opted_in(&["tool"]);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .checks = vec!["exit 1".into()];
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let result = close::publish(&server, &core, &s, "tool", &["new.txt"]);
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["code"], "CheckFailed", "{result}");
    assert!(result.get("data").is_none() || result["data"].get("auto_close").is_none());
    assert!(mark_of(&core, &s).is_none(), "no mark on error path");
    assert!(
        audit_lines(&fx)
            .iter()
            .all(|l| l["session"] != s.as_str() || l["event"] != "auto_close"),
        "no audit line on error path"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}


#[test]
fn multi_session_group_closes_each_session_independently() {
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let a = close::new(&server);
    let b = close::new(&server);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a").unwrap();
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b").unwrap();
    // B stays dirty: an extra unpublished edit outside the publish selectors.
    fs::write(ws(&core, &b, "tool").join("extra.txt"), "still dirty").unwrap();
    let acp = checkpoint(&core, &a, "tool");
    let bcp = checkpoint(&core, &b, "tool");
    let out = close::ok(close::call(
        &server,
        "publish",
        json!({
            "group": group(vec![
                batch(&a, "tool", acp.id, &["a.txt"]),
                batch(&b, "tool", bcp.id, &["b.txt"]),
            ]),
            "shown": {}
        }),
    ));
    assert_eq!(out["closed"], false, "{out}");
    let ea = session_entry(&out, &a);
    let eb = session_entry(&out, &b);
    assert_eq!(ea["closed"], true, "{ea}");
    assert_eq!(eb["closed"], false, "{eb}");
    assert_eq!(eb["reasons"], json!(["unpublished"]), "{eb}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
    assert_eq!(lifecycle(&core, &b), Lifecycle::Active);
    assert!(pin_gone(&core, &a, "tool"));
    assert!(!pin_gone(&core, &b, "tool"));
    assert!(mark_of(&core, &b).is_some());
}

#[test]
fn publish_busy_when_another_publish_is_running() {
    // Plant a Running row on tool; publish front so refuse_open does not
    // block the call, then auto-close sees the session still publishing.
    let mut fx = opted_in(&["tool", "front"]);
    fx.lock_wait = Duration::from_millis(50);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t").unwrap();
    fs::write(ws(&core, &s, "front").join("f.txt"), "f").unwrap();
    let cp_tool = checkpoint(&core, &s, "tool");
    let _running = core
        .store
        .record_publish(
            &group(vec![batch(&s, "tool", cp_tool.id, &["t.txt"])]),
            SystemTime::now(),
        )
        .unwrap();
    assert!(
        core.store
            .open_publishes()
            .unwrap()
            .iter()
            .any(|r| r.phase == PublishPhase::Running),
        "planted Running row"
    );

    let out = close::ok(close::publish(&server, &core, &s, "front", &["f.txt"]));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["busy"]), "{entry}");
    assert_eq!(entry["mark"]["written"], false, "{entry}");
    assert_eq!(entry["mark"]["skipped"], "busy", "{entry}");
    assert!(mark_of(&core, &s).is_none(), "no mark row on busy");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    let lines = audit_lines(&fx);
    assert!(
        lines.iter().any(|l| {
            l["event"] == "auto_close"
                && l["session"] == s.as_str()
                && l["closed"] == false
                && l["reasons"] == json!(["busy"])
        }),
        "{lines:?}"
    );
}

#[test]
fn publish_keeps_an_agent_mark_note() {
    let fx = opted_in(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);
    close::ok(close::call(
        &server,
        "mark",
        json!({ "session": s, "state": "done", "note": "x", "agent": "codex" }),
    ));
    assert_eq!(mark_of(&core, &s).unwrap().note.as_deref(), Some("x"));
    assert_eq!(mark_of(&core, &s).unwrap().by.kind, MarkByKind::Agent);

    fs::write(ws(&core, &s, "tool").join("t.txt"), "t").unwrap();
    fs::write(ws(&core, &s, "front").join("f.txt"), "f").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["unpublished"]), "{entry}");
    assert_eq!(entry["mark"]["written"], false, "{entry}");
    assert_eq!(entry["mark"]["skipped"], "kept_agent", "{entry}");
    let mark = mark_of(&core, &s).expect("agent mark kept");
    assert_eq!(mark.note.as_deref(), Some("x"));
    assert_eq!(mark.by.kind, MarkByKind::Agent);
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn publish_refreshes_the_daemons_own_note() {
    let fx = opted_in(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t").unwrap();
    fs::write(ws(&core, &s, "front").join("f.txt"), "f").unwrap();
    let first = close::ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(session_entry(&first, &s)["reasons"], json!(["unpublished"]), "{first}");
    let first_note = mark_of(&core, &s)
        .expect("daemon mark")
        .note
        .clone()
        .expect("note");
    assert!(
        first_note.starts_with("auto-close did not close this session: unpublished:"),
        "{first_note}"
    );

    // Publish the remaining edit with a cwd holder in the workspace → open_files.
    let root = ws(&core, &s, "front");
    let mut holder = Command::new("/bin/sleep")
        .arg("300")
        .current_dir(&root)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let second = close::ok(close::publish(&server, &core, &s, "front", &["f.txt"]));
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(second["closed"], false, "{second}");
    let entry = session_entry(&second, &s);
    assert_eq!(entry["reasons"], json!(["open_files"]), "{entry}");
    let mark = mark_of(&core, &s).expect("daemon mark refreshed");
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(
        note.starts_with("auto-close did not close this session: open_files:"),
        "{note}"
    );
    assert_ne!(note, first_note, "note must refresh");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn publish_marks_done_open_files() {
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let root = ws(&core, &s, "tool");
    let mut holder = Command::new("/bin/sleep")
        .arg("300")
        .current_dir(&root)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    let pid = holder.id() as i64;
    let _ = holder.kill();
    let _ = holder.wait();

    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["open_files"]), "{entry}");
    let pids = entry["detail"]["pids"].as_array().expect("detail.pids");
    assert!(
        pids.iter().any(|p| p.as_i64() == Some(pid)),
        "pids should name the holder {pid}: {entry}"
    );
    assert!(mark_of(&core, &s).is_some());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(!pin_gone(&core, &s, "tool"));
    let attempts = core.store.rm_attempts(Some(&s)).unwrap();
    assert!(!attempts.is_empty(), "last_rm recorded");
    let detail = &attempts[0].detail;
    assert_eq!(detail["by"], "auto_close", "{detail}");
    // §10 row 22 nit 4: strict OpenFiles text in rm_attempts[0].detail.message.
    let msg = detail["message"].as_str().unwrap_or("");
    assert!(
        msg.starts_with("auto-close refused"),
        "strict OpenFiles message: {msg:?} detail={detail}"
    );
    assert!(
        msg.contains(&format!("git-strata rm --session {s}")),
        "strict OpenFiles message names rm: {msg:?}"
    );
    assert!(
        !msg.contains("run the same command"),
        "strict OpenFiles must not invite a rerun: {msg:?}"
    );
}

#[test]
fn publish_marks_done_conflicts_and_deps_pending() {
    // Fixture shape of deps_and_auto_fields_follow_store_and_cached_summary,
    // plus a workspace conflicts row. quiet_secs high so the post-publish
    // rebase defers and does not clear those rows before the judge.
    let mut fx = opted_in(&["tool"]);
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().rebase.quiet_secs = 3600;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let repo = repo_name("tool");
    let row = core.store.session(&s).unwrap().unwrap();
    let ws_state = row.manifest.repos[&repo].clone();
    let set = core
        .store
        .deps_set(&repo, &ws_state.base.deps)
        .unwrap()
        .unwrap()
        .set;
    let target = close::advance(&fx, &core, "tool", "deps update");
    let tree = core.gits[&repo].tree_of(&target).unwrap();
    let p = core
        .store
        .record_proposal(
            &NewProposal {
                repo: repo.clone(),
                origin: None,
                from: ws_state.base.deps.clone(),
                from_inputs: set.inputs_hash,
                from_head: Some(ws_state.head.clone()),
                to: DepsSetId::try_from("b".repeat(64)).unwrap(),
                to_inputs: "b".repeat(64),
                tree: Some(tree.clone()),
                head: Some(target.clone()),
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap();
    core.store
        .enroll_main(
            &s,
            &repo,
            &ws_state.base,
            &target,
            &tree,
            None,
            SystemTime::now(),
        )
        .unwrap();
    let pending = core.store.rebase_pending_of(&s, &repo).unwrap().unwrap();
    core.store
        .defer_pending(
            &s,
            &repo,
            pending.rev,
            "conflicts",
            &json!({ "paths": ["src/a"] }),
            SystemTime::now(),
        )
        .unwrap();

    // Workspace conflicts on a path we are not publishing (stage only
    // refuses when a selector intersects the conflict set).
    let staging = ws_state.path.with_file_name(".scene-move-ac");
    fs::create_dir_all(&staging).unwrap();
    let now = core.store.session(&s).unwrap().unwrap();
    let cur = now.manifest.repos[&repo].clone();
    let n = core
        .store
        .begin_move(
            &s,
            &repo,
            &cur.base,
            &cur.head,
            &staging,
            SystemTime::now(),
        )
        .unwrap();
    let conflict_path = RepoPath::try_from("src/c.ts".to_string()).unwrap();
    assert!(core
        .store
        .set_move_state(
            &s,
            &repo,
            n,
            strata_store::MoveState::Preparing,
            strata_store::MoveState::Swapping,
            Some(&[conflict_path.clone()]),
            SystemTime::now(),
        )
        .unwrap());
    core.store
        .finish_move(&s, &repo, n, &[conflict_path], SystemTime::now())
        .unwrap();
    assert!(
        !core
            .store
            .session(&s)
            .unwrap()
            .unwrap()
            .workspaces[&repo]
            .conflicts
            .is_empty(),
        "workspace row has conflicts"
    );

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    let reasons = entry["reasons"].as_array().expect("reasons");
    let reason_strs: Vec<&str> = reasons.iter().filter_map(|r| r.as_str()).collect();
    assert!(
        reason_strs.contains(&"conflicts") && reason_strs.contains(&"deps_pending"),
        "want conflicts+deps_pending, got {entry}"
    );
    assert_eq!(entry["detail"]["proposal"], json!(p), "{entry}");
    assert!(mark_of(&core, &s).is_some());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn already_published_refused_push_marks_push_refused() {
    let fx = opted_in(&["tool"]);
    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();

    let first = close::publish(&server, &core, &s, "tool", &["new.txt"]);
    assert_eq!(first["ok"], false, "{first}");
    assert!(mark_of(&core, &s).is_none(), "error path writes no mark");
    let row = core
        .store
        .publishes()
        .unwrap()
        .into_iter()
        .rev()
        .find(|r| r.phase == PublishPhase::Done)
        .expect("Done row");
    assert_eq!(
        row.outcome.as_ref().unwrap()["stopped"],
        "PushRefused",
        "{:?}",
        row.outcome
    );

    let second = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(
        second["already_published"][0]["push"]["kind"],
        "Refused",
        "{second}"
    );
    assert_eq!(second["closed"], false, "{second}");
    let entry = session_entry(&second, &s);
    assert_eq!(
        entry["reasons"],
        json!(["not_pushed", "push_refused"]),
        "{entry}"
    );
    assert_eq!(entry["detail"]["stopped"], "PushRefused", "{entry}");
    assert!(mark_of(&core, &s).is_some());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}


#[test]
fn publish_marks_done_not_pushed_for_an_untracked_sibling_on_an_unpushed_chain() {
    // §10 row 1 (r3): front push=false leaves an unpushed chain; B picks it up
    // untracked and must refuse close with not_pushed, not delete the pin.
    let mut fx = opted_in(&["tool", "front"]);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let a = close::new_only(&server, &["front"]);
    fs::write(ws(&core, &a, "front").join("a.txt"), "a").unwrap();
    let a_out = close::ok(close::publish(&server, &core, &a, "front", &["a.txt"]));
    assert_eq!(a_out["closed"], false, "{a_out}");
    assert_eq!(
        session_entry(&a_out, &a)["reasons"],
        json!(["not_pushed"]),
        "{a_out}"
    );
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);
    assert!(mark_of(&core, &a).is_some());

    // Chain top = A's front workspace head after the deferred-or-moved publish.
    let chain_top = core
        .store
        .session(&a)
        .unwrap()
        .unwrap()
        .manifest
        .repos[&repo_name("front")]
        .head
        .clone();

    // only absent → front joins B untracked at that chain top.
    let b = close::new_in(&server, &["tool"]);
    let b_front = core
        .store
        .session(&b)
        .unwrap()
        .unwrap()
        .manifest
        .repos[&repo_name("front")]
        .clone();
    assert!(!b_front.tracked, "front joins B untracked");
    assert_eq!(b_front.head, chain_top, "B's front head is the chain top");

    fs::write(ws(&core, &b, "tool").join("t.txt"), "t").unwrap();
    let b_out = close::ok(close::publish(&server, &core, &b, "tool", &["t.txt"]));
    assert_eq!(b_out["closed"], false, "{b_out}");
    let entry = session_entry(&b_out, &b);
    assert_eq!(entry["reasons"], json!(["not_pushed"]), "{entry}");
    assert_eq!(entry["detail"]["repo"], "front", "{entry}");
    assert_eq!(entry["detail"]["tracked"], false, "{entry}");
    assert_eq!(
        entry["detail"]["head"].as_str(),
        Some(chain_top.as_str()),
        "{entry}"
    );
    assert_eq!(lifecycle(&core, &b), Lifecycle::Active);
    assert!(mark_of(&core, &b).is_some());
    assert!(
        !pin_gone(&core, &b, "front"),
        "pin must stay in front (r2 wrongly deleted it)"
    );
    assert!(!pin_gone(&core, &b, "tool"));

    let pushed = close::ok(close::call(&server, "push", json!({ "repo": "front" })));
    assert_eq!(pushed["closed"], true, "{pushed}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
    assert_eq!(lifecycle(&core, &b), Lifecycle::Removed);
    assert!(pin_gone(&core, &a, "front"));
    assert!(pin_gone(&core, &b, "front"));
    assert!(pin_gone(&core, &b, "tool"));
}


#[test]
fn publish_rejudges_done_marked_sessions_when_it_pushes_their_repo() {
    // §10 row 1 (r3): refused publish → mark not_pushed+push_refused; a later
    // session's push of the same repo rejudges and closes the earlier one.
    let fx = opted_in(&["tool"]);
    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let a = close::new(&server);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a").unwrap();
    let first = close::publish(&server, &core, &a, "tool", &["a.txt"]);
    assert_eq!(first["ok"], false, "{first}");
    assert!(mark_of(&core, &a).is_none(), "error path writes no mark");
    assert!(
        audit_lines(&fx)
            .iter()
            .all(|l| l["session"] != a.as_str() || l["event"] != "auto_close"),
        "no audit line on error path"
    );
    let row = core
        .store
        .publishes()
        .unwrap()
        .into_iter()
        .rev()
        .find(|r| r.phase == PublishPhase::Done)
        .expect("Done row");
    assert_eq!(
        row.outcome.as_ref().unwrap()["stopped"],
        "PushRefused",
        "{:?}",
        row.outcome
    );

    let second = close::ok(close::publish(&server, &core, &a, "tool", &["a.txt"]));
    assert_eq!(
        second["already_published"][0]["push"]["kind"],
        "Refused",
        "{second}"
    );
    let entry = session_entry(&second, &a);
    assert_eq!(
        entry["reasons"],
        json!(["not_pushed", "push_refused"]),
        "{entry}"
    );
    assert!(mark_of(&core, &a).is_some());
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);

    fs::remove_file(&hook).unwrap();

    // B created now so its `new` builds the chain top's base (A's projection).
    let b = close::new(&server);
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b").unwrap();
    let b_out = close::ok(close::publish(&server, &core, &b, "tool", &["b.txt"]));
    assert_eq!(
        b_out["published"][0]["push"]["kind"],
        "Pushed",
        "{b_out}"
    );
    let ea = session_entry(&b_out, &a);
    let eb = session_entry(&b_out, &b);
    assert_eq!(ea["closed"], true, "{ea}");
    assert_eq!(ea["in_group"], false, "{ea}");
    assert_eq!(eb["closed"], true, "{eb}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
    assert_eq!(lifecycle(&core, &b), Lifecycle::Removed);
    assert!(pin_gone(&core, &a, "tool"));
    assert!(pin_gone(&core, &b, "tool"));
}

#[test]
fn publish_rejudges_done_marked_sessions_b_created_before_gets_no_built_base() {
    // §10 row 1 variant: B created before A's first publish → set (b) rejudge
    // answers unpublished/no_built_base (B's `new` never built A's tip); the
    // note names the agent's rm, and that rm passes by D57.
    let fx = opted_in(&["tool"]);
    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    // B first: its `new` builds only the pre-A tip, not A's later commit.
    let b = close::new(&server);

    let a = close::new(&server);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a").unwrap();
    let first = close::publish(&server, &core, &a, "tool", &["a.txt"]);
    assert_eq!(first["ok"], false, "{first}");
    assert!(mark_of(&core, &a).is_none(), "error path writes no mark");

    let second = close::ok(close::publish(&server, &core, &a, "tool", &["a.txt"]));
    assert_eq!(
        second["already_published"][0]["push"]["kind"],
        "Refused",
        "{second}"
    );
    let entry = session_entry(&second, &a);
    assert_eq!(
        entry["reasons"],
        json!(["not_pushed", "push_refused"]),
        "{entry}"
    );
    assert!(mark_of(&core, &a).is_some());
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);

    fs::remove_file(&hook).unwrap();

    fs::write(ws(&core, &b, "tool").join("b.txt"), "b").unwrap();
    let b_out = close::ok(close::publish(&server, &core, &b, "tool", &["b.txt"]));
    assert_eq!(
        b_out["published"][0]["push"]["kind"],
        "Pushed",
        "{b_out}"
    );
    let ea = session_entry(&b_out, &a);
    assert_eq!(ea["closed"], false, "{ea}");
    assert_eq!(ea["reasons"], json!(["unpublished"]), "{ea}");
    assert_eq!(
        ea["detail"]["landing"]["tool"]["why_none"],
        "no_built_base",
        "{ea}"
    );
    let mark = mark_of(&core, &a).expect("daemon mark");
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(
        note.contains(&format!("git-strata rm --session {a}")),
        "note names rm: {note}"
    );
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);

    // Agent's non-discard rm still passes by D57.
    let rm = close::ok(close::call(
        &server,
        "rm",
        json!({ "session": a, "discard": false }),
    ));
    assert!(
        rm["checkpoints"]["tool"].is_number(),
        "agent rm passes by D57: {rm}"
    );
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
}


#[test]
fn publish_marks_done_when_a_revert_matches_an_older_publish() {
    // §10 row 1 (r3): content matching only an older publish is unpublished
    // under the strict judge. Setup under auto_close=false so early publishes
    // do not close the session; reload turns the flag on before the last one.
    let mut fx = Dfx::with(&["tool"]);
    assert!(!fx.config.publish.auto_close, "fixture default is off");
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let w = ws(&core, &s, "tool");
    fs::create_dir_all(w.join("src")).unwrap();

    fs::write(w.join("src/p.ts"), "v1\n").unwrap();
    let p1 = close::ok(close::publish(&server, &core, &s, "tool", &["src/p.ts"]));
    assert_eq!(p1["closed"], false, "auto_close off: {p1}");
    close::ok(close::call(
        &server,
        "sync",
        json!({ "session": s, "fetch": false }),
    ));

    fs::write(w.join("src/p.ts"), "v2\n").unwrap();
    let p2 = close::ok(close::publish(&server, &core, &s, "tool", &["src/p.ts"]));
    assert_eq!(p2["closed"], false, "auto_close off: {p2}");
    close::ok(close::call(
        &server,
        "sync",
        json!({ "session": s, "fetch": false }),
    ));

    // Revert to v1: matches the older publish only.
    fs::write(w.join("src/p.ts"), "v1\n").unwrap();
    fx.config.publish.auto_close = true;
    fx.write_config();
    // No serve loop here: request_reload would hang pending. In-process
    // tests apply via reload_for_test (reload.rs pattern).
    assert_eq!(core.reload_for_test().outcome, "applied");

    fs::write(w.join("src/q.ts"), "q\n").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["src/q.ts"]));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["unpublished"]), "{entry}");
    assert_eq!(
        paths_of(&entry["detail"], "tool"),
        vec!["src/p.ts".to_string()],
        "{entry}"
    );
    let historical: Vec<String> = entry["detail"]["landing"]["tool"]["historical"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    assert_eq!(historical, vec!["src/p.ts".to_string()], "{entry}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(mark_of(&core, &s).is_some());

    let rm = close::ok(close::call(
        &server,
        "rm",
        json!({ "session": s, "discard": false }),
    ));
    assert!(
        rm["checkpoints"]["tool"].is_number(),
        "BeforeRemove holds src/p.ts: {rm}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}


fn split_tool_to_release(fx: &mut Dfx) {
    let main = fx.main("tool");
    fx.sb.git(&main, &["checkout", "-q", "-b", "release"]);
    fx.sb.commit(&main, &[("REL", b"r\n")], "release tip");
    fx.sb.git(&main, &["push", "-q", "origin", "release"]);
    fx.sb.git(&main, &["checkout", "-q", "main"]);
    let cfg = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    cfg.publish.branch = "release".into();
    cfg.deps_dirs.clear();
    cfg.seed_caches.clear();
    cfg.install_inputs.clear();
    cfg.install = None;
}

#[test]
fn publish_marks_done_not_merged_for_a_split_branch_repo() {
    // §10 row 1 (r4): publish.branch != default_branch lands on release only;
    // clause 4 refuses not_merged against origin/main (deferred rebase case).
    let mut fx = opted_in(&["tool"]);
    split_tool_to_release(&mut fx);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .rebase
        .quiet_secs = 3600;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let main_before = core.gits[&repo_name("tool")]
        .read_ref("refs/strata/remotes/origin/main")
        .unwrap();

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(
        out["published"][0]["push"]["kind"],
        "Pushed",
        "{out}"
    );
    let rep = &out["published"][0]["rebases"][0]["report"];
    assert_eq!(rep["kind"], "Deferred", "{out}");
    assert_eq!(
        out["upstream"][s.as_str()]["repos"]["tool"]["ahead"],
        0,
        "r3 hole precondition: {out}"
    );
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["not_merged"]), "{entry}");
    assert_eq!(entry["detail"]["what"], "publish", "{entry}");
    let commit = out["published"][0]["commit"].as_str().unwrap();
    assert_eq!(entry["detail"]["commit"], commit, "{entry}");
    assert_eq!(entry["detail"]["publish_branch"], "release", "{entry}");
    assert_eq!(entry["detail"]["default_branch"], "main", "{entry}");
    assert_eq!(
        entry["detail"]["ref"],
        "refs/strata/remotes/origin/main",
        "{entry}"
    );
    let mark = mark_of(&core, &s).expect("daemon mark");
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(note.contains("origin/release"), "{note}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(!pin_gone(&core, &s, "tool"));
    let release_oid = core.gits[&repo_name("tool")]
        .read_ref("refs/strata/remotes/origin/release")
        .unwrap();
    assert_eq!(
        release_oid.as_ref().map(|c| c.as_str()),
        Some(commit),
        "origin/release at publish commit"
    );
    assert_eq!(
        core.gits[&repo_name("tool")]
            .read_ref("refs/strata/remotes/origin/main")
            .unwrap(),
        main_before,
        "origin/main unchanged"
    );
}

#[test]
fn publish_marks_done_not_merged_moved_base_variant() {
    // Variant (ii): quiet_secs 0 → Moved onto release → not_merged what=base.
    let mut fx = opted_in(&["tool"]);
    split_tool_to_release(&mut fx);
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["not_merged"]), "{entry}");
    assert_eq!(entry["detail"]["what"], "base", "{entry}");
    let head = core
        .store
        .session(&s)
        .unwrap()
        .unwrap()
        .manifest
        .repos[&repo_name("tool")]
        .head
        .clone();
    assert_eq!(
        entry["detail"]["head"].as_str(),
        Some(head.as_str()),
        "{entry}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn publish_closes_with_an_untouched_split_branch_sibling() {
    // Variant (iii): untouched split-branch sibling does not hold the close.
    let mut fx = opted_in(&["tool", "front"]);
    let main = fx.main("front");
    fx.sb.git(&main, &["checkout", "-q", "-b", "release"]);
    fx.sb.commit(&main, &[("REL", b"r\n")], "release tip");
    fx.sb.git(&main, &["push", "-q", "origin", "release"]);
    fx.sb.git(&main, &["checkout", "-q", "main"]);
    let cfg = fx.config.repos.get_mut(&repo_name("front")).unwrap();
    cfg.publish.branch = "release".into();
    cfg.deps_dirs.clear();
    cfg.seed_caches.clear();
    cfg.install_inputs.clear();
    cfg.install = None;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool", "front"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(pin_gone(&core, &s, "tool"));
    assert!(pin_gone(&core, &s, "front"));
}

#[test]
fn publish_marks_done_not_merged_agent_rm_after_merge() {
    // §10 row 1 variant (iv): after (i)'s not_merged mark, merge release into
    // main on the remote, fetch main, then the agent's rm {discard: false}
    // passes (D57 against origin/release) — the close of a split-branch
    // session is the agent's.
    let mut fx = opted_in(&["tool"]);
    split_tool_to_release(&mut fx);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .rebase
        .quiet_secs = 3600;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["not_merged"]), "{entry}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(mark_of(&core, &s).is_some());

    let main = fx.main("tool");
    fx.sb.git(&main, &["fetch", "origin", "release"]);
    fx.sb.git(&main, &["merge", "--no-edit", "origin/release"]);
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo_name("tool")]
        .fetch("origin", "main")
        .unwrap();

    let rm = close::ok(close::call(
        &server,
        "rm",
        json!({ "session": s, "discard": false }),
    ));
    assert!(
        rm["checkpoints"]["tool"].is_number(),
        "agent rm passes by D57: {rm}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(pin_gone(&core, &s, "tool"));
}


#[test]
fn publish_rejudges_sessions_whose_repo_an_earlier_run_pushed() {
    // §10 row 1 (r4): front sorts before tool; B's first run pushes front
    // (landing A's refused commit) then CheckFailed on tool; the success
    // re-run rejudges A via already_published[front].push = Pushed.
    let mut fx = opted_in(&["front", "tool"]);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .checks = vec!["test -f ready.txt".into()];
    // Keep B's front baseline still after the first push so the re-run's
    // already_published path sees the same checkpoint selectors.
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .rebase
        .quiet_secs = 3600;
    let hook = fx.sb.path("front-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let a = close::new_only(&server, &["front"]);
    fs::write(ws(&core, &a, "front").join("a.txt"), "a").unwrap();
    let first = close::publish(&server, &core, &a, "front", &["a.txt"]);
    assert_eq!(first["ok"], false, "{first}");
    assert!(mark_of(&core, &a).is_none());

    let second = close::ok(close::publish(&server, &core, &a, "front", &["a.txt"]));
    assert_eq!(
        session_entry(&second, &a)["reasons"],
        json!(["not_pushed", "push_refused"]),
        "{second}"
    );
    let mark_at = mark_of(&core, &a).expect("daemon mark").at;
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);

    fs::remove_file(&hook).unwrap();

    let b = close::new_in(&server, &["front", "tool"]);
    fs::write(ws(&core, &b, "front").join("bf.txt"), "bf").unwrap();
    fs::write(ws(&core, &b, "tool").join("bt.txt"), "bt").unwrap();
    let b_front_cp = checkpoint(&core, &b, "front");
    let b_tool_cp = checkpoint(&core, &b, "tool");
    let fail = close::call(
        &server,
        "publish",
        json!({
            "group": group(vec![
                batch(&b, "front", b_front_cp.id, &["bf.txt"]),
                batch(&b, "tool", b_tool_cp.id, &["bt.txt"]),
            ]),
            "shown": {}
        }),
    );
    assert_eq!(fail["ok"], false, "{fail}");
    assert_eq!(fail["error"]["code"], "CheckFailed", "{fail}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);
    assert_eq!(
        mark_of(&core, &a).unwrap().at,
        mark_at,
        "error path must not touch A's mark"
    );

    fs::write(ws(&core, &b, "tool").join("ready.txt"), "ok").unwrap();
    let b_tool_cp2 = checkpoint(&core, &b, "tool");
    // Same front checkpoint → already_published; tool now includes ready.txt.
    let ok_out = close::ok(close::call(
        &server,
        "publish",
        json!({
            "group": group(vec![
                batch(&b, "front", b_front_cp.id, &["bf.txt"]),
                batch(&b, "tool", b_tool_cp2.id, &["bt.txt", "ready.txt"]),
            ]),
            "shown": {}
        }),
    ));
    assert_eq!(
        ok_out["already_published"][0]["repo"],
        "front",
        "{ok_out}"
    );
    assert_eq!(
        ok_out["already_published"][0]["push"]["kind"],
        "Pushed",
        "{ok_out}"
    );
    let rejudged = ok_out["auto_close"]["rejudged"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        rejudged.iter().any(|v| v.as_str() == Some(a.as_str())),
        "rejudged names A: {ok_out}"
    );
    let ea = session_entry(&ok_out, &a);
    assert_eq!(ea["closed"], true, "{ea}");
    assert_eq!(ea["in_group"], false, "{ea}");
    assert_eq!(session_entry(&ok_out, &b)["closed"], true, "{ok_out}");
    // `closed` reports B's group only.
    assert_eq!(ok_out["closed"], true, "{ok_out}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
    assert_eq!(lifecycle(&core, &b), Lifecycle::Removed);
}


#[test]
fn closed_reports_the_group_only() {
    // §10 row 1 (r4 Codex nit 2): rejudged A stays open on open_files;
    // top-level `closed` is still true for B's group.
    let fx = opted_in(&["tool"]);
    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let a = close::new(&server);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a").unwrap();
    assert_eq!(close::publish(&server, &core, &a, "tool", &["a.txt"])["ok"], false);
    let _ = close::ok(close::publish(&server, &core, &a, "tool", &["a.txt"]));
    assert!(mark_of(&core, &a).is_some());
    fs::remove_file(&hook).unwrap();

    // Plant a cwd holder in A so the rejudge refuses open_files.
    let mut holder = Command::new("/bin/sleep")
        .arg("300")
        .current_dir(ws(&core, &a, "tool"))
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let b = close::new(&server);
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b").unwrap();
    let out = close::ok(close::publish(&server, &core, &b, "tool", &["b.txt"]));
    let _ = holder.kill();
    let _ = holder.wait();

    assert_eq!(out["closed"], true, "group B closed: {out}");
    assert_eq!(out["auto_close"]["reasons"], json!([]), "{out}");
    let ea = session_entry(&out, &a);
    assert_eq!(ea["closed"], false, "{ea}");
    assert_eq!(ea["in_group"], false, "{ea}");
    assert_eq!(ea["reasons"], json!(["open_files"]), "{ea}");
    let rejudged = out["auto_close"]["rejudged"].as_array().cloned().unwrap_or_default();
    assert!(
        rejudged.iter().any(|v| v.as_str() == Some(a.as_str())),
        "{out}"
    );
    assert_eq!(session_entry(&out, &b)["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);
    assert_eq!(lifecycle(&core, &b), Lifecycle::Removed);
}

#[test]
fn push_of_a_split_branch_repo_runs_no_hook() {
    // §10 row 2 (r4): push of a split-branch repo lands nothing on
    // origin/<default_branch>, so the hook does not run.
    let mut fx = opted_in(&["tool"]);
    split_tool_to_release(&mut fx);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let pub_out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(
        session_entry(&pub_out, &s)["reasons"],
        json!(["not_merged"]),
        "{pub_out}"
    );
    let mark_before = mark_of(&core, &s).expect("daemon mark");
    let note_before = mark_before.note.clone();
    let at_before = mark_before.at;
    let audit_before = audit_lines(&fx).len();

    let pushed = close::ok(close::call(&server, "push", json!({ "repo": "tool" })));
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert!(
        pushed.get("closed").is_none() || pushed["closed"].is_null(),
        "no top-level closed: {pushed}"
    );
    assert!(
        pushed.get("auto_close").is_none() || pushed["auto_close"].is_null(),
        "no auto_close: {pushed}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    let mark = mark_of(&core, &s).expect("mark unchanged");
    assert_eq!(mark.note, note_before);
    assert_eq!(mark.at, at_before);
    assert_eq!(
        audit_lines(&fx).len(),
        audit_before,
        "no new audit line on split-branch push"
    );
}


#[test]
fn publish_marks_done_open_files_for_a_deferred_session() {
    // §10 row 1 (r4 Codex nit 3): holder during publish → Deferred open_files
    // and the hook marks open_files; after sync the session stays Active+done.
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine").unwrap();
    let root = ws(&core, &s, "tool");
    let mut holder = Command::new("/bin/sleep")
        .arg("300")
        .current_dir(&root)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    let _ = holder.kill();
    let _ = holder.wait();

    let rep = &out["published"][0]["rebases"][0]["report"];
    assert_eq!(rep["kind"], "Deferred", "{out}");
    assert_eq!(rep["reason"], "open_files", "{out}");
    assert_eq!(out["closed"], false, "{out}");
    let entry = session_entry(&out, &s);
    assert_eq!(entry["reasons"], json!(["open_files"]), "{entry}");
    let up = &out["upstream"][s.as_str()]["repos"]["tool"];
    assert!(
        up["behind"].as_u64().unwrap_or(0) >= 1,
        "behind after land: {out}"
    );
    assert_eq!(up["unpublished"], true, "{out}");
    assert!(mark_of(&core, &s).is_some());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);

    // §10 row 1 / Claude r5 blocker 2: Stop-hook sentences over the real
    // publish upstream (done mark) name "main moved" and never "cd back"/"publish".
    let upstream: Upstream = serde_json::from_value(out["upstream"][s.as_str()].clone())
        .unwrap_or_else(|e| panic!("publish upstream: {e}: {out}"));
    let reason = block_reason(&upstream, true).expect("hook speaks while deferred behind");
    assert!(
        reason.contains("main moved"),
        "expected main-moved sentence: {reason}"
    );
    assert!(
        !reason.contains("cd back"),
        "done mark must omit cd-back advice: {reason}"
    );
    assert!(
        !reason.contains("publish"),
        "done mark must omit publish advice: {reason}"
    );

    let synced = close::ok(close::call(
        &server,
        "sync",
        json!({ "session": s, "fetch": false }),
    ));
    assert_eq!(
        synced["repos"]["tool"]["result"],
        "moved",
        "{synced}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(mark_of(&core, &s).is_some(), "still marked done");

    // After the move, behind is 0 and the hook is silent.
    let after = synced
        .get("upstream")
        .cloned()
        .unwrap_or_else(|| {
            close::ok(close::call(
                &server,
                "sync",
                json!({ "session": s, "check": true, "fetch": false }),
            ))["upstream"]
                .clone()
        });
    assert_eq!(
        after["repos"]["tool"]["behind"].as_u64().unwrap_or(u64::MAX),
        0,
        "behind after sync: {after}"
    );
    let upstream_after: Upstream = serde_json::from_value(after.clone())
        .unwrap_or_else(|e| panic!("post-sync upstream: {e}: {after}"));
    assert_eq!(
        block_reason(&upstream_after, true),
        None,
        "hook silent after move: {after}"
    );
}

//! Auto-close §10 row 21 (r8) handler/message nits still missing after W.
//! Temp-dir fixtures only. Does not edit `auto_close_row21.rs` (ASSIGN races).
mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_publish::seams::{PublishFault, pause};
use strata_session::SessionSeam;
use strata_session::cwd::{MoveCwd, with_move_cwd};
use strata_store::{Lifecycle, RmAttempt, RmOutcome};
use strata_types::{CommitOid, MarkByKind, SessionId};
use support::close::{self, audit_lines, call, mark_of, ok};
use support::*;

const WAIT: Duration = Duration::from_secs(30);

struct Holder(std::process::Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fixture(names: &[&str]) -> Dfx {
    let mut fx = close::opted_in(names);
    fx.lock_wait = Duration::from_secs(60);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.write_config();
    fx
}

/// `front` opted out of auto-close; `tool` opted in.
fn fixture_front_opted_out(tool_push: bool) -> Dfx {
    let mut fx = fixture(&["front", "tool"]);
    let front = fx.config.repos.get_mut(&repo_name("front")).unwrap();
    front.publish.auto_close = Some(false);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = tool_push;
    fx.write_config();
    fx
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn audits(fx: &Dfx, s: &SessionId) -> Vec<Value> {
    audit_lines(fx)
        .into_iter()
        .filter(|l| l["event"] == "auto_close" && l["session"] == s.as_str())
        .collect()
}

fn attempt_of(core: &Core, s: &SessionId) -> RmAttempt {
    core.store
        .rm_attempts(Some(s))
        .unwrap()
        .into_iter()
        .next()
        .expect("rm_attempts row")
}

fn agent_done(server: &Server<'_, '_>, s: &SessionId) {
    ok(call(
        server,
        "mark",
        json!({"session": s, "state": "active"}),
    ));
    ok(call(
        server,
        "mark",
        json!({"session": s, "state": "done", "agent": "codex"}),
    ));
}

/// Plant a kept pre-move tree on `tool` for session `s`, then drop the holder.
fn plant_kept_old_tree(d: &Daemon<'_>, fx: &Dfx, s: &SessionId) -> (Holder, std::path::PathBuf) {
    let _catalog = d.core.admit();
    let repo = repo_name("tool");
    let ws_path = ws(d.core, s, "tool");
    let holder = Holder(
        Command::new("/bin/sleep")
            .arg("300")
            .current_dir(&ws_path)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(200));
    let main = fx.main("tool");
    // A prior publish in the same fixture advances origin; sync local main first.
    fx.sb.git(&main, &["fetch", "-q", "origin"]);
    fx.sb.git(&main, &["reset", "--hard", "origin/main"]);
    let head = CommitOid::try_from(fx.sb.commit(
        &main,
        &[(
            "kept-up.txt",
            b"upstream
",
        )],
        "new head",
    ))
    .unwrap();
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    d.core.gits[&repo].fetch("origin", "main").unwrap();
    let base = d.sessions.ensure_base(&repo, &head).unwrap();
    let report = d
        .core
        .locks
        .with_repo(&repo, || {
            with_move_cwd(
                MoveCwd {
                    allow_cwd_holders: true,
                    ..Default::default()
                },
                || {
                    d.sessions
                        .move_workspace(s, &d.core.gits[&repo].repo_id(), base, head)
                },
            )
        })
        .unwrap();
    // Keep the holder: the daemon's old-tree loop would otherwise retire the
    // tree before remove_strict runs. Its cwd is the old path, not the new ws.
    (holder, report.old_tree.expect("held tree"))
}

#[test]
fn add_clears_the_daemons_mark_and_keeps_an_agents() {
    // Daemon mark cleared by add of a new repo; agent mark kept; add of an
    // already-held repo leaves the daemon mark alone.
    let mut fx = fixture(&["front", "tool"]);
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

    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let first = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(
        entry(&first, &s)["reasons"],
        json!(["not_pushed"]),
        "{first}"
    );
    assert_eq!(mark_of(&core, &s).unwrap().by.kind, MarkByKind::Daemon);

    ok(call(
        &server,
        "add",
        json!({"session": s, "repos": ["front"]}),
    ));
    assert!(
        mark_of(&core, &s).is_none(),
        "add of a new repo clears the daemon mark"
    );

    let pushed = ok(call(&server, "push", json!({"repo": "tool"})));
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert!(
        pushed.get("auto_close").is_none()
            || pushed["auto_close"].is_null()
            || pushed["auto_close"]["sessions"].get(s.as_str()).is_none(),
        "no mark → S is not judged: {pushed}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);

    // Variant: an agent's mark survives add.
    let a = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a\n").unwrap();
    let a_out = ok(close::publish(&server, &core, &a, "tool", &["a.txt"]));
    assert_eq!(
        entry(&a_out, &a)["reasons"],
        json!(["not_pushed"]),
        "{a_out}"
    );
    agent_done(&server, &a);
    assert_eq!(mark_of(&core, &a).unwrap().by.kind, MarkByKind::Agent);
    ok(call(
        &server,
        "add",
        json!({"session": a, "repos": ["front"]}),
    ));
    let kept = mark_of(&core, &a).expect("agent mark kept");
    assert_eq!(kept.by.kind, MarkByKind::Agent);

    // Variant: add naming only a repo S already has leaves the daemon mark.
    let b = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b\n").unwrap();
    let b_out = ok(close::publish(&server, &core, &b, "tool", &["b.txt"]));
    assert_eq!(
        entry(&b_out, &b)["reasons"],
        json!(["not_pushed"]),
        "{b_out}"
    );
    assert_eq!(mark_of(&core, &b).unwrap().by.kind, MarkByKind::Daemon);
    let added = call(&server, "add", json!({"session": b, "repos": ["tool"]}));
    let data = ok(added);
    assert!(
        data["report"]["added"]
            .as_object()
            .map(|m| m.is_empty())
            .unwrap_or(true),
        "already held: {data}"
    );
    assert_eq!(
        mark_of(&core, &b).unwrap().by.kind,
        MarkByKind::Daemon,
        "daemon mark stays when add names only a held repo"
    );
}

#[test]
fn add_during_the_publish_is_extended_too() {
    // Same add as the hook-time extended case, but at PauseBeforeRebase so a
    // step-1 baseline would already see `front`.
    let fx = fixture(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["t.txt"])]);
    let git = &core.gits[&repo_name("tool")];
    let gap = pause(git, PublishFault::PauseBeforeRebase);
    let out = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        gap.wait_until_paused();
        let added = call(&server, "add", json!({"session": s, "repos": ["front"]}));
        ok(added);
        gap.release();
        ok(h.join().unwrap())
    });
    let e = entry(&out, &s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], json!(["extended"]), "{out}");
    assert_eq!(e["detail"]["repos"], json!(["front"]), "{out}");
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn attempt_summary_names_kept_trees_and_nested_repositories() {
    // Public `rm_notes` path over planted `RmAttempt` rows (attempt_summary
    // itself is pub(crate) in rm_note.rs).
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let rec = core.store.session(&s).unwrap().unwrap();
    let root = &fx.config.paths.root;
    let at = SystemTime::now();

    let kept = RmAttempt {
        session: s.clone(),
        at,
        outcome: RmOutcome::Refused,
        detail: json!({
            "code": "Dirty",
            "message": "auto-close refused",
            "detail": {
                "old_trees": [
                    {"repo": "tool", "n": 1, "path": "/s/old", "changed": false},
                    {"repo": "tool", "n": 2, "path": "/s/old2", "changed": false}
                ]
            }
        }),
    };
    assert!(core.store.record_rm_attempt(&kept).unwrap());
    let notes = strata_session::rm_notes(&core.store, root, &rec).unwrap();
    assert_eq!(
        notes.last_rm.as_ref().unwrap().summary,
        "Dirty (kept old trees)"
    );

    let mixed = RmAttempt {
        session: s.clone(),
        at,
        outcome: RmOutcome::Refused,
        detail: json!({
            "code": "Dirty",
            "message": "auto-close refused",
            "detail": {
                "old_trees": [
                    {"repo": "tool", "n": 1, "path": "/s/old", "changed": false},
                    {"repo": "tool", "n": 2, "path": "/s/old2", "changed": true}
                ]
            }
        }),
    };
    assert!(core.store.record_rm_attempt(&mixed).unwrap());
    let notes = strata_session::rm_notes(&core.store, root, &rec).unwrap();
    assert_eq!(
        notes.last_rm.as_ref().unwrap().summary,
        "Dirty (changed old trees)"
    );

    let no_key = RmAttempt {
        session: s.clone(),
        at,
        outcome: RmOutcome::Refused,
        detail: json!({
            "code": "Dirty",
            "message": "auto-close refused",
            "detail": {
                "old_trees": [{"repo": "tool", "n": 1, "path": "/s/old"}]
            }
        }),
    };
    assert!(core.store.record_rm_attempt(&no_key).unwrap());
    let notes = strata_session::rm_notes(&core.store, root, &rec).unwrap();
    assert_eq!(
        notes.last_rm.as_ref().unwrap().summary,
        "Dirty (changed old trees)"
    );

    let nested = RmAttempt {
        session: s.clone(),
        at,
        outcome: RmOutcome::Refused,
        detail: json!({
            "code": "Dirty",
            "message": "auto-close refused",
            "detail": {
                "embedded_repos": {
                    "tool": {"paths": ["vendor/x"], "count": 1, "truncated": false}
                }
            }
        }),
    };
    assert!(core.store.record_rm_attempt(&nested).unwrap());
    let notes = strata_session::rm_notes(&core.store, root, &rec).unwrap();
    assert_eq!(
        notes.last_rm.as_ref().unwrap().summary,
        "Dirty (nested git repositories)"
    );
}

#[test]
fn audit_write_failure_keeps_the_close() {
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let audit = strata_daemon::pins_path(&fx.config_dir).with_file_name("sessions-audit.jsonl");
    if audit.exists() {
        fs::remove_file(&audit).unwrap();
    }
    fs::create_dir(&audit).unwrap();

    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);

    let status = ok(call(&server, "daemon_status", json!({})));
    let err = status["sessions_audit_error"]
        .as_str()
        .expect("sessions_audit_error named");
    assert!(
        err.contains("not a regular file") || err.contains("Is a directory") || !err.is_empty(),
        "daemon_status names the failure: {status}"
    );
}

#[test]
fn close_refusal_stores_no_discard_advice() {
    // Nested-repository scene (row 20) and kept-tree scene (row 7).
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let nested = close::new(&server);
    let nested_path = ws(&core, &nested, "tool").join("vendor/x");
    fs::create_dir_all(&nested_path).unwrap();
    fx.sb.git(&nested_path, &["init", "-q"]);
    fs::write(nested_path.join("f"), "f\n").unwrap();
    fx.sb.git(&nested_path, &["add", "f"]);
    fx.sb.git(
        &nested_path,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "x",
        ],
    );
    fs::write(ws(&core, &nested, "tool").join("t.txt"), "t\n").unwrap();
    let out = ok(close::publish(&server, &core, &nested, "tool", &["t.txt"]));
    assert_eq!(
        entry(&out, &nested)["reasons"],
        json!(["embedded_repo"]),
        "{out}"
    );
    let a = attempt_of(&core, &nested);
    let msg = a.detail["message"].as_str().unwrap_or("");
    assert!(
        msg.starts_with("auto-close refused"),
        "message={msg:?} detail={}",
        a.detail
    );
    assert!(!msg.contains("--discard"), "message={msg:?}");

    // Kept-tree scene: plant a Done old tree, then rejudge via another
    // session's push so this workspace is not moved again (a second move
    // would retire the tree once the holder is gone).
    let kept = close::new(&server);
    agent_done(&server, &kept);
    let (holder, old) = plant_kept_old_tree(&d, &fx, &kept);
    assert!(old.is_dir(), "{old:?}");
    drop(holder);
    assert!(
        old.is_dir(),
        "tree must remain without the old-tree loop: {old:?}"
    );

    let t = close::new(&server);
    fs::write(ws(&core, &t, "tool").join("trig.txt"), "trig\n").unwrap();
    let out = ok(close::publish(&server, &core, &t, "tool", &["trig.txt"]));
    assert_eq!(out["closed"], true, "{out}");
    let e = entry(&out, &kept);
    assert_eq!(e["closed"], false, "{out}");
    assert!(
        e["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str() == Some("kept_trees")),
        "expected kept_trees among {}: {out}",
        e["reasons"]
    );
    let a = attempt_of(&core, &kept);
    let msg = a.detail["message"].as_str().unwrap_or("");
    assert!(
        msg.starts_with("auto-close refused"),
        "message={msg:?} detail={}",
        a.detail
    );
    assert!(!msg.contains("--discard"), "message={msg:?}");
}

#[test]
fn failed_scan_records_refused_not_failed() {
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let dir = fx
        .config
        .paths
        .root
        .join("sessions/.repos")
        .join(s.as_str());
    strata_deps::test_seams::fail_open_files_under(
        &dir,
        strata_types::StrataError::new(strata_types::Code::Unknown, "scan failed"),
    );
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(entry(&out, &s)["reasons"], json!(["error"]), "{out}");
    let a = attempt_of(&core, &s);
    assert_eq!(a.outcome, RmOutcome::Refused, "{a:?}");
    let st = ok(call(
        &server,
        "status",
        json!({"cwd": null, "session": s, "all": false}),
    ));
    let row = &st["sessions"][0]["status"];
    // RmNotes is #[serde(flatten)] onto SessionStatus.
    let notes = row["notes"].as_array().cloned().unwrap_or_default();
    let joined = notes
        .iter()
        .filter_map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let last = row.get("last_rm").cloned().unwrap_or(Value::Null);
    assert!(
        (joined.contains("refused")
            && joined.contains("Unknown")
            && joined.contains("nothing removed"))
            || (last["outcome"] == "refused"
                && last["summary"].as_str().unwrap_or("").contains("Unknown")),
        "status note={joined:?} last_rm={last} st={st}"
    );

    // Same under the agent's rm.
    let a2 = close::new(&server);
    fs::write(ws(&core, &a2, "tool").join("a.txt"), "a\n").unwrap();
    let dir2 = fx
        .config
        .paths
        .root
        .join("sessions/.repos")
        .join(a2.as_str());
    strata_deps::test_seams::fail_open_files_under(
        &dir2,
        strata_types::StrataError::new(strata_types::Code::Unknown, "scan failed"),
    );
    let rm = call(&server, "rm", json!({"session": a2, "discard": false}));
    assert_eq!(rm["ok"], false, "{rm}");
    let a = attempt_of(&core, &a2);
    assert_eq!(a.outcome, RmOutcome::Refused, "{a:?}");
    assert_eq!(lifecycle(&core, &a2), Lifecycle::Active);

    // Row 20's failure_after_removing scene → Failed.
    let u = close::new(&server);
    fs::write(ws(&core, &u, "tool").join("u.txt"), "u\n").unwrap();
    d.sessions.arm(SessionSeam::FailAt {
        phase: "remove.unpin".into(),
        errno: libc::EIO,
    });
    let out = ok(close::publish(&server, &core, &u, "tool", &["u.txt"]));
    assert_eq!(entry(&out, &u)["reasons"], json!(["unknown"]), "{out}");
    let a = attempt_of(&core, &u);
    assert_eq!(a.outcome, RmOutcome::Failed, "{a:?}");
    assert_eq!(lifecycle(&core, &u), Lifecycle::Unknown);
}

#[test]
fn removed_group_session_with_an_opted_out_repo_reads_gone() {
    let fx = fixture_front_opted_out(true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let x = close::new_only(&server, &["tool"]);
    let y = close::new_only(&server, &["tool"]);
    let (a, s) = if x.as_str() < y.as_str() {
        (x, y)
    } else {
        (y, x)
    };

    ok(call(
        &server,
        "add",
        json!({"session": s, "repos": ["front"]}),
    ));

    fs::write(ws(&core, &a, "tool").join("a.txt"), "a\n").unwrap();
    fs::write(ws(&core, &s, "tool").join("s.txt"), "s\n").unwrap();
    let acp = checkpoint(&core, &a, "tool");
    let scp = checkpoint(&core, &s, "tool");
    let g = group(vec![
        batch(&a, "tool", acp.id, &["a.txt"]),
        batch(&s, "tool", scp.id, &["s.txt"]),
    ]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let out = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        assert!(
            d.sessions.wait_paused_timeout(WAIT),
            "before_judge not reached"
        );
        let removed = call(&server, "rm", json!({"session": s, "discard": false}));
        assert_eq!(removed["ok"], true, "{removed}");
        d.sessions.release();
        ok(h.join().unwrap())
    });

    let es = entry(&out, &s);
    assert_eq!(
        es,
        &json!({
            "closed": true, "in_group": true, "by": "other", "reasons": [],
            "detail": {}, "rm": null, "mark": {"written": false}
        }),
        "must read gone/by:other, not disabled: {out}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}

#[test]
fn set_b_judges_a_session_holding_two_pushed_repos_once() {
    let fx = fixture(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let s = close::new_only(&server, &["front", "tool"]);
    agent_done(&server, &s);
    assert_eq!(mark_of(&core, &s).unwrap().by.kind, MarkByKind::Agent);
    let audits_before = audits(&fx, &s).len();

    let t = close::new_only(&server, &["front", "tool"]);
    fs::write(ws(&core, &t, "front").join("f.txt"), "f\n").unwrap();
    fs::write(ws(&core, &t, "tool").join("t.txt"), "t\n").unwrap();
    let fcp = checkpoint(&core, &t, "front");
    let tcp = checkpoint(&core, &t, "tool");
    let g = group(vec![
        batch(&t, "front", fcp.id, &["f.txt"]),
        batch(&t, "tool", tcp.id, &["t.txt"]),
    ]);
    let out = ok(call(&server, "publish", json!({"group": g, "shown": {}})));
    assert_eq!(out["closed"], true, "{out}");
    let rejudged = out["auto_close"]["rejudged"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rejudged,
        vec![json!(s.as_str())],
        "S judged once across both pushes: {out}"
    );
    let s_lines = audits(&fx, &s);
    assert_eq!(
        s_lines.len(),
        audits_before + 1,
        "one audit line for S: {s_lines:?}"
    );
}

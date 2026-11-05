mod support;

use serde_json::{Value, json};
use support::live::Live;
use support::{FakeDaemon, Home, Run};

fn old_daemon(req: &Value) -> Value {
    json!({"id":req["id"],"ok":true,"data":{"features":[]}})
}

#[test]
fn mark_requires_feature_and_prints_envelope() {
    let home = Home::new();
    let daemon = FakeDaemon::start(&home.socket(), old_daemon);
    let reply = home.run(&["mark", "--session", "s-aaaaaaaa", "done"]);
    assert_eq!(reply.err_code(), "Unsupported");
    assert_eq!(reply.reason(), "daemon_too_old");
    assert_eq!(
        daemon
            .requests()
            .iter()
            .map(|r| r["method"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        ["daemon_status"]
    );

    let live = Live::new(&["tool"], &[("a.txt", b"unchanged\n")]);
    let id = live.serve(|_| {
        let id = live.new_session("T-mark", "tool");
        let before = std::fs::read(live.ws(&id, "tool").join("a.txt")).unwrap();
        let run = live.run(&[
            "mark",
            "--session",
            "T-mark",
            "done",
            "--note",
            "landed\tall\nreview",
        ]);
        assert_eq!(
            run.stdout.lines().count(),
            1,
            "one compact envelope: {run:?}"
        );
        let marked = run.ok();
        assert_eq!(marked["session"], id.as_str());
        assert_eq!(marked["mark"]["state"], "done");
        assert_eq!(marked["mark"]["by"]["kind"], "cli");
        assert_eq!(marked["mark"]["note"], "landed\tall\nreview");
        assert_eq!(
            std::fs::read(live.ws(&id, "tool").join("a.txt")).unwrap(),
            before
        );
        assert_eq!(
            live.run(&["status", "--session", id.as_str()]).ok()["mark"],
            marked["mark"]
        );
        assert_eq!(
            live.run(&["status", "--brief", "--session", id.as_str()])
                .ok()["mark"],
            marked["mark"]
        );
        assert_eq!(
            live.run(&["ls"]).ok()["sessions"][0]["mark"],
            marked["mark"]
        );
        assert_eq!(
            live.run(&["sync", "--check", "--session", id.as_str()])
                .ok()["mark"],
            marked["mark"]
        );
        id
    });
    // New CLI with daemon down still reads the persisted mark in both summaries.
    assert_eq!(
        live.run(&["ls"]).ok()["sessions"][0]["mark"]["state"],
        "done"
    );
    assert_eq!(
        live.run(&["status", "--brief", "--session", id.as_str()])
            .ok()["mark"]["state"],
        "done"
    );
}

#[test]
fn mark_detects_agent_like_new_and_active_clears_metadata() {
    let live = Live::new(&["tool"], &[("a.txt", b"one\n")]);
    live.serve(|_| {
        let id = live.new_session("T-agent", "tool");
        for (vars, flag) in [
            (vec![("STRATA_AGENT", "codex")], None),
            (vec![("CLAUDE_CODE_ENTRYPOINT", "cli")], None),
            (vec![("CODEX_THREAD_ID", "thread")], None),
            (vec![("CURSOR_AGENT", "1")], None),
            (vec![("STRATA_AGENT", "bad agent")], Some("mine.v2")),
        ] {
            let mut args = vec!["mark", "--session", id.as_str(), "done", "--note", "ready"];
            if let Some(flag) = flag {
                args.extend(["--agent", flag]);
            }
            let mut cmd = live.home.cmd(&args);
            for (key, value) in vars {
                cmd.env(key, value);
            }
            let reply = Run::from(cmd.output().unwrap());
            assert_eq!(reply.ok()["mark"]["by"]["kind"], "agent", "{reply:?}");
        }
        let reply = live.run(&["mark", "--session", id.as_str(), "active"]).ok();
        assert_eq!(reply["mark"], json!({"state":"active"}));
        assert!(
            live.run(&["status", "--brief", "--session", id.as_str()])
                .ok()
                .get("mark")
                .is_none()
        );
        // Repeating the undo is harmless.
        live.run(&["mark", "--session", id.as_str(), "active"]).ok();
    });
}

#[test]
fn mark_invalid_note_or_agent_is_rejected_before_connection() {
    let home = Home::new();
    // With no daemon, a transport attempt would return Busy/daemon_down.
    for note in [
        "".to_string(),
        "é".repeat(1001),
        "bad\rnote".into(),
        "bad\u{001b}note".into(),
    ] {
        let reply = home.run(&["mark", "--session", "s-aaaaaaaa", "done", "--note", &note]);
        assert_eq!(reply.reason(), "note", "{reply:?}");
    }
    let reply = home.run(&[
        "mark",
        "--session",
        "s-aaaaaaaa",
        "done",
        "--agent",
        "bad name",
    ]);
    assert_eq!(reply.reason(), "agent");
    let reply = home.run(&[
        "mark",
        "--session",
        "s-aaaaaaaa",
        "active",
        "--note",
        "unused",
    ]);
    assert_eq!(reply.reason(), "note");
}

#[test]
fn mark_ambiguous_ticket_never_writes() {
    let live = Live::new(&["tool"], &[("a.txt", b"one\n")]);
    live.serve(|_| {
        live.new_session("same", "tool");
        live.new_session("same", "tool");
        let before = live
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "mark")
            .count();
        assert_eq!(
            live.run(&["mark", "--session", "same", "done"]).err_code(),
            "AmbiguousMatch"
        );
        assert_eq!(
            live.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|m| *m == "mark")
                .count(),
            before
        );
    });
}

#[test]
fn mark_parser_requires_selector_and_accepts_only_declared_states() {
    use clap::Parser;
    use git_strata::args::{Cli, Cmd};

    let cli = Cli::try_parse_from([
        "git-strata",
        "mark",
        "--session",
        "T-one",
        "done",
        "--note",
        "ready",
    ])
    .unwrap();
    assert!(
        matches!(cli.cmd, Cmd::Mark { session, state, note, agent: None }
        if session == "T-one" && state == "done" && note.as_deref() == Some("ready"))
    );
    for args in [
        vec!["git-strata", "mark", "done"],
        vec!["git-strata", "mark", "--session", "T-one", "archived"],
        vec![
            "git-strata",
            "mark",
            "--session",
            "T-one",
            "done",
            "--force",
        ],
        vec!["git-strata", "mark", "--session", "T-one", "done", "--yes"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn ls_table_has_mark_column() {
    let text = git_strata::cmds::table_of(&json!({"sessions":[
        {"id":"s-aaaaaaaa","ticket":"T-one","state":"Active","repos":["tool"],"created":"today","mark":{"state":"done"}},
        {"id":"s-bbbbbbbb","ticket":"T-two","state":"Unknown","repos":["tool"],"created":"today"}
    ]}));
    let mut lines = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>());
    assert_eq!(
        lines.next().unwrap(),
        ["ID", "TICKET", "STATE", "MARK", "REPOS", "CREATED"]
    );
    assert_eq!(lines.next().unwrap()[3], "done");
    assert_eq!(lines.next().unwrap()[3], "active");
}

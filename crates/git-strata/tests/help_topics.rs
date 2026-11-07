mod support;
use clap::{Command, CommandFactory, Parser};
use git_strata::args::Cli;
use support::{Home, Run};

fn commands(command: &Command, path: Vec<String>, out: &mut Vec<Vec<String>>) {
    out.push(path.clone());
    for child in command.get_subcommands() {
        let mut next = path.clone();
        next.push(child.get_name().to_string());
        commands(child, next, out);
    }
}
#[test]
fn all_help_is_bounded_and_gate_has_one_full_output() {
    let home = Home::new(); // No config or live store.
    let read_help = |args: &[&str]| {
        let run = Run::from(home.cmd(args).env_remove("HOME").output().unwrap());
        assert_eq!(run.code, 0, "{args:?}: {run:?}");
        assert!(run.stderr.is_empty(), "{args:?}: {run:?}");
        assert!(
            run.stdout.ends_with('\n'),
            "{args:?}: missing trailing newline"
        );
        // Help may show indented JSON examples, but never emit an envelope
        // before or after its text, even when HOME is missing.
        for line in run.stdout.lines().filter(|line| line.starts_with('{')) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                assert!(
                    !(value.get("ok").is_some()
                        && value.get("data").is_some()
                        && value.get("error").is_some()),
                    "{args:?}: emitted envelope {line}"
                );
            }
        }
        run
    };
    let index = read_help(&["help"]);
    let topics: Vec<_> = index
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("Topics: "))
        .expect("topic index")
        .split(", ")
        .collect();
    let mut paths = Vec::new();
    commands(&Cli::command(), Vec::new(), &mut paths);
    let mut gate_count = 0;
    for path in paths {
        let mut words: Vec<&str> = path.iter().map(String::as_str).collect();
        words.push("--help");
        let run = read_help(&words);
        assert_eq!(run.code, 0, "{words:?}: {run:?}");
        // RM_AFTER carries the complete per-reason auto-close recovery map (§8).
        // Keep every other command's original bound.
        let limit = if path == ["rm"] { 8192 } else { 4096 };
        assert!(run.stdout.len() <= limit, "{words:?}: {} bytes", run.stdout.len());
        gate_count += run.stdout.matches("CONFIRMATION GATE (Touch ID / TTY)").count();
        if !path.is_empty() {
            let mut help = vec!["help"];
            help.extend(path.iter().map(String::as_str));
            assert_eq!(read_help(&help).stdout, run.stdout, "{path:?}");
        }
    }
    for topic in &topics {
        let run = read_help(&["help", topic]);
        assert_eq!(run.code, 0);
        assert!(
            run.stdout.len() <= 4096,
            "{topic}: {} bytes",
            run.stdout.len()
        );
        gate_count += run
            .stdout
            .matches("CONFIRMATION GATE (Touch ID / TTY)")
            .count();
        assert_eq!(
            run.stdout
                .matches("CONFIRMATION GATE (Touch ID / TTY)")
                .count(),
            usize::from(*topic == "gate")
        );
        assert!(serde_json::from_str::<serde_json::Value>(&run.stdout).is_err());
        assert!(run.stderr.is_empty());
    }
    assert_eq!(gate_count, 1);
    for words in [
        vec![],
        vec!["publish"],
        vec!["push"],
        vec!["rm"],
        vec!["deps", "approve"],
        vec!["config", "relax"],
        vec!["resolve"],
        vec!["chain"],
    ] {
        let mut args = words.clone();
        args.push("--help");
        let run = read_help(&args);
        assert!(run.stdout.contains("git-strata help gate"), "{words:?}");
        assert!(run.stdout.contains("faking a TTY is a bypass"), "{words:?}");
        assert!(
            run.stdout
                .contains("Without a TTY: NotTty; stop and give the human the exact command."),
            "{words:?}"
        );
    }
    let run = read_help(&["help"]);
    assert_eq!(run.code, 0);
    assert!(run.stdout.len() <= 1536);
    let bad = Run::from(
        home.cmd(&["help", "nope"])
            .env_remove("HOME")
            .output()
            .unwrap(),
    );
    assert_eq!(bad.err_code(), "InvalidInput");
    assert_eq!(bad.reason(), "help_topic");
    let envelope = bad.env();
    let detail = &envelope["error"]["detail"];
    assert_eq!(detail["topics"], serde_json::json!(topics));
    assert_eq!(
        detail["commands"],
        serde_json::json!(
            Cli::command()
                .get_subcommands()
                .map(|c| c.get_name().to_string())
                .collect::<Vec<_>>()
        )
    );
}

/// A help text as one line: a sentence the source wraps still matches, and a
/// retired sentence cannot hide behind a line break.
fn one_line(home: &Home, args: &[&str]) -> String {
    let run = home.run(args);
    assert_eq!(run.code, 0, "{args:?}: {run:?}");
    run.stdout.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn publish_help_data_names_closed_and_auto_close() {
    let text = one_line(&Home::new(), &["publish", "--help"]);
    assert!(
        text.contains("closed, auto_close:{closed, reasons[], sessions}"),
        "{text}"
    );
}

/// Every command p1-autoclose's reasons and notes tell an agent or a human to run.
#[test]
fn note_commands_parse() {
    for args in [
        vec!["chain", "--repo", "REPO", "--accept-origin", "replay"],
        vec!["push", "REPO"],
        vec!["rm", "--session", "S"],
        vec!["sync", "--session", "S"],
        vec!["sync", "--session", "S", "--repo", "REPO"],
        vec!["mark", "--session", "S", "active"],
        vec!["assign", "--file", "F", "--session", "S"],
        vec!["deps", "list", "--repo", "REPO"],
    ] {
        let argv = std::iter::once("git-strata").chain(args.iter().copied());
        if let Err(e) = Cli::try_parse_from(argv) {
            panic!("{args:?}: {e}");
        }
    }
    // The control: r6's sentence named the repo as `chain`'s positional.
    assert!(
        Cli::try_parse_from(["git-strata", "chain", "REPO", "--accept-origin", "replay"]).is_err()
    );
}

/// A done mark enrols a session in auto-close (p1-autoclose §8's table), so the
/// text may not promise the workspace stays until a human removes it.
#[test]
fn mark_help_says_a_done_session_can_close() {
    let home = Home::new();

    let mark = one_line(&home, &["mark", "--help"]);
    for said in [
        "A full publish closes its session by itself.",
        "is closed by the daemon on a later publish or push of its repos",
        "publish.auto_close = false",
        "its workspace goes to the trash (recoverable until trash gc)",
        "To keep a session, mark it active",
    ] {
        assert!(mark.contains(said), "{said}: {mark}");
    }
    assert!(!mark.contains("leave removal to the human"), "{mark}");

    let rm = one_line(&home, &["rm", "--help"]);
    for said in [
        "# the daemon's fallback after a publish",
        "Agents finish with publish: a fully landed session closes itself.",
        "data.auto_close.sessions[<your session id>]",
        "`disabled`: the daemon neither closes nor marks",
        "`unpublished`: publish the listed paths",
        "`detail.regenerable`",
        "`detail.landing.<repo>.historical`",
        "`no_built_base`, the paths are published but the judge cannot prove it",
        "`no_landed_publish` with `published: true`",
        "`open_files`: close the holders",
        "`changed_during_remove`: wait until your tools are idle",
        "`not_pushed`, `push_refused`",
        "`detail.outstanding`",
        "`detail.on_chain`",
        "`not_merged`: the repo publishes to another branch than its default",
        "merge is theirs.",
        "`kept_trees` with every `changed` false",
        "`embedded_repo`",
        "`detail.embedded_repos`",
        "`assigned`",
        "`detail.assigned`",
        "`deps_pending` with `detail.own` true and `detail.state` `awaiting_publish`",
        "your own deps draft never landed: publish its install inputs",
        "no command cancels a draft",
        "`conflicts` with `detail.report` true: your `sync` left a conflict",
        "`cd ~ && git-strata sync --session S --repo <detail.repo>` reports it again",
        "`busy`: another operation had the session",
        "`extended`: a repo was added",
        "If `mark.written` is `false` and `mark.skipped` is not `unchanged`",
        "Never `rm --discard`.",
        "A Dirty refusal means mark done and report what remains",
    ] {
        assert!(rm.contains(said), "{said}: {rm}");
    }
    for retired in [
        "# agents finish here",
        "Agents finish with mark done",
        "leave removal to the human",
    ] {
        assert!(!rm.contains(retired), "{retired}: {rm}");
    }

    let top = one_line(&home, &["--help"]);
    assert!(
        top.contains("A full publish closes the session; otherwise read data.auto_close, which says what is left."),
        "{top}"
    );
    assert!(!top.contains("leave removal to the human"), "{top}");
    // The command list's `rm` line, whatever else the page says of `rm`.
    let page = home.run(&["--help"]);
    let rm_line = page
        .stdout
        .lines()
        .find(|l| l.contains("Remove a session"))
        .expect("the rm line");
    assert!(
        rm_line.contains("a full publish closes its session by itself"),
        "{rm_line}"
    );
    assert!(!rm_line.contains("agents finish"), "{rm_line}");

    let gate = one_line(&home, &["help", "gate"]);
    assert!(
        gate.contains("a done session can be auto-closed later (git-strata mark --help)"),
        "{gate}"
    );
    assert!(!gate.contains("agents finish with mark done"), "{gate}");
}

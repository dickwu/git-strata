mod support;
use clap::{Command, CommandFactory};
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
        assert!(run.stdout.len() <= 4096, "{words:?}: {} bytes", run.stdout.len());
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

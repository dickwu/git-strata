mod support;
use clap::{Command, CommandFactory};
use git_strata::args::Cli;
use support::Home;

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
    let mut paths = Vec::new();
    commands(&Cli::command(), Vec::new(), &mut paths);
    let mut gate_count = 0;
    for path in paths {
        let mut words: Vec<&str> = path.iter().map(String::as_str).collect();
        words.push("--help");
        let run = home.run(&words);
        assert_eq!(run.code, 0, "{words:?}: {run:?}");
        assert!(run.stdout.len() <= 4096, "{words:?}: {} bytes", run.stdout.len());
        gate_count += run.stdout.matches("CONFIRMATION GATE (Touch ID / TTY)").count();
        if !path.is_empty() {
            let mut help = vec!["help"];
            help.extend(path.iter().map(String::as_str));
            assert_eq!(home.run(&help).stdout, run.stdout, "{path:?}");
        }
    }
    for topic in ["gate", "push-guards", "errors", "errors-publish", "output", "headless", "sync-ack"] {
        let run = home.run(&["help", topic]);
        assert_eq!(run.code, 0);
        assert!(run.stdout.len() <= 4096, "{topic}: {} bytes", run.stdout.len());
        gate_count += run.stdout.matches("CONFIRMATION GATE (Touch ID / TTY)").count();
    }
    assert_eq!(gate_count, 1);
    for words in [
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
        let run = home.run(&args);
        assert!(run.stdout.contains("git-strata help gate"), "{words:?}");
        assert!(run.stdout.contains("faking a TTY is a bypass"), "{words:?}");
    }
    let run = home.run(&["help"]);
    assert_eq!(run.code, 0);
    assert!(run.stdout.len() <= 1536);
    let bad = home.run(&["help", "nope"]);
    assert_eq!(bad.err_code(), "InvalidInput");
    assert_eq!(bad.reason(), "help_topic");
}

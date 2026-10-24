//! daemon.md §7 `cli_readonly_commands_work_without_daemon` (the cli half
//! of daemon.md §8 Task 6): with no daemon listening, `status`, `grep` and
//! `files` still answer from the store's read-only open; `publish` and
//! `push` fail with `Busy` `daemon_down` (cli.md D2, which replaces the
//! `ReadOnly` wording of that row).

mod support;

use serde_json::json;
use support::live::{Live, write};

#[test]
fn cli_readonly_commands_work_without_daemon() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let (s, ws) = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "b.txt", b"beta alpha\n");
        lv.run_in(&ws, &["checkpoint", "-m", "before stop"]).ok();
        (s, ws)
    });
    let _ = std::fs::remove_file(lv.home.socket());
    let st = lv.run_in(&ws, &["status"]).ok();
    assert_eq!(st["manifest"]["id"], json!(s.as_str()));
    assert_eq!(st["workspaces"]["tool"]["checkpoint"], json!(1));
    let g = lv.run_in(&ws, &["grep", "alpha"]).ok();
    let hits: Vec<&str> = g["hits"].as_array().unwrap().iter().map(|h| h["path"].as_str().unwrap()).collect();
    assert_eq!(hits, vec!["a.txt", "b.txt"]);
    let f = lv.run(&["files", "--session", s.as_str()]).ok();
    let files: Vec<&str> = f["paths"].as_array().unwrap().iter().map(|p| p["path"].as_str().unwrap()).collect();
    assert_eq!(files, vec![".gitignore", "a.txt", "b.txt"]);
    for args in [vec!["publish".to_string(), "--from".into(), format!("{s}:b.txt")], vec!["push".into(), "tool".into()]] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = lv.run_in(&ws, &args);
        assert_eq!(r.code, 1, "{args:?}: {r:?}");
        assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("Busy", "daemon_down"), "{args:?}");
    }
}

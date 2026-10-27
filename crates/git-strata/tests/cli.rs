//! cli.md §8 Task 3 / §7: subcommands, the read-only path and the hook,
//! end to end against an in-process git-stratad (tests/support/live.rs).
//! Every repository is a local throwaway with a local bare origin.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use strata_types::{CheckpointReason, RepoName, SessionId};
use support::live::{Live, write};
use support::{BIN, FakeDaemon, Home, code_names};

fn base_files() -> Vec<(&'static str, &'static [u8])> {
    vec![("a.txt", b"one\ntwo\n"), ("keep.txt", b"same\n"), ("del.txt", b"gone\n"), ("src/lib.txt", b"fn x\n")]
}

/// The base files plus a committed binary `bin.dat` (both sides then
/// modify it: an add/add would be a `Conflict`, not `Binary`).
fn bin_files() -> Vec<(&'static str, &'static [u8])> {
    let mut v = base_files();
    v.push(("bin.dat", b"base\0bin\n"));
    v
}

fn rn(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}

fn store_rows(lv: &Live, s: &SessionId, repo: &str, tool_call: &str) -> Vec<u64> {
    let store = strata_store::Store::open_read_only(&lv.root).unwrap();
    store
        .checkpoints(s, Some(&rn(repo)))
        .unwrap()
        .into_iter()
        .filter(|c| matches!(&c.reason, CheckpointReason::ToolCall { id } if id.as_str() == tool_call))
        .map(|c| u64::from(c.id))
        .collect()
}

fn ids(d: &Value) -> Vec<(String, u64)> {
    d["checkpoints"].as_array().unwrap().iter().map(|c| (c["repo"].as_str().unwrap().to_string(), c["id"].as_u64().unwrap())).collect()
}

fn paths(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap().to_string()).collect()
}

#[test]
fn hook_checkpoint_is_idempotent_per_tool_call() {
    let lv = Live::new(&["hr-core", "tool"], &base_files());
    let (s, tws, input) = lv.serve(|_| {
        let s = lv.new_session("T-1", "hr-core,tool");
        let tws = lv.ws(&s, "tool");
        let hws = lv.ws(&s, "hr-core");
        write(&tws, "a.txt", b"changed\n");
        write(&hws, "b.txt", b"new in hr-core\n");
        let input = json!({
            "session_id": "claude",
            "cwd": tws,
            "tool_name": "MultiEdit",
            "tool_input": {
                "file_paths": [
                    tws.join("a.txt"),
                    format!("{}/../hr-core/b.txt", tws.display()),
                    "/private/tmp/outside-any-session.txt",
                ],
            },
        })
        .to_string();
        let r1 = lv.run_piped(&tws, &["checkpoint", "--tool-call", "tu-1"], input.as_bytes());
        let d1 = r1.ok();
        let got = ids(&d1);
        assert_eq!(got.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(), vec!["hr-core", "tool"], "{d1}");
        assert!(r1.stderr.contains("outside-any-session"), "the outside path is reported: {}", r1.stderr);
        for c in d1["checkpoints"].as_array().unwrap() {
            assert_eq!(c["reason"], json!({ "kind": "ToolCall", "id": "tu-1" }));
            assert_eq!(c["session"], json!(s.as_str()));
        }
        // hr-core's row records b.txt (not cwd's repo).
        let h = &d1["checkpoints"][0];
        assert!(paths(&h["entries"]).contains(&"b.txt".to_string()), "{h}");
        // The same call again: the same rows.
        let r2 = lv.run_piped(&tws, &["checkpoint", "--tool-call", "tu-1"], input.as_bytes());
        assert_eq!(ids(&r2.ok()), got);
        (s, tws, input)
    });
    for repo in ["hr-core", "tool"] {
        assert_eq!(store_rows(&lv, &s, repo, "tu-1").len(), 1, "one row per (session, repo, tool call)");
    }
    // A third run with no daemon at all still answers: no write RPC is sent.
    let r3 = lv.run_piped(&tws, &["checkpoint", "--tool-call", "tu-1"], input.as_bytes());
    let d3 = r3.ok();
    assert_eq!(ids(&d3).len(), 2, "{d3}");
}

#[test]
fn hook_replay_of_a_no_op_tool_call_returns_its_own_row() {
    // Review M8: C1 (change) -> T1 (no change) -> T2 (change) -> replay T1
    // with the daemon down must answer T1's row, not C2's.
    let lv = Live::new(&["tool"], &base_files());
    let (s, tws, t1) = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let tws = lv.ws(&s, "tool");
        let input = json!({
            "session_id": "claude",
            "cwd": tws,
            "tool_name": "Edit",
            "tool_input": { "file_path": tws.join("a.txt") },
        })
        .to_string();
        let run = |id: &str| ids(&lv.run_piped(&tws, &["checkpoint", "--tool-call", id], input.as_bytes()).ok());
        write(&tws, "a.txt", b"c1\n");
        let c1 = run("tu-c1");
        let t1 = run("tu-t1");
        assert_eq!(t1.len(), 1);
        assert_ne!(t1, c1, "the no-op tool call records its own row");
        write(&tws, "a.txt", b"t2\n");
        let t2 = run("tu-t2");
        assert_ne!(t2, t1);
        (s, tws, t1)
    });
    assert_eq!(store_rows(&lv, &s, "tool", "tu-t1").len(), 1);
    let input = json!({
        "session_id": "claude",
        "cwd": tws,
        "tool_name": "Edit",
        "tool_input": { "file_path": tws.join("a.txt") },
    })
    .to_string();
    let d = lv.run_piped(&tws, &["checkpoint", "--tool-call", "tu-t1"], input.as_bytes()).ok();
    assert_eq!(ids(&d), t1, "{d}");
}

#[test]
fn hook_tool_call_id_comes_from_stdin_tool_use_id() {
    // cli.md §3.5 / D22: Claude Code sets no TOOL_USE_ID env var; the id is
    // the hook JSON's `tool_use_id` when `--tool-call` has no value.
    let lv = Live::new(&["tool"], &base_files());
    let s = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "a.txt", b"hooked\n");
        let hook = |id: Option<&str>| {
            let mut v = json!({
                "session_id": "claude",
                "cwd": ws,
                "tool_name": "Edit",
                "tool_input": { "file_path": ws.join("a.txt") },
            });
            if let Some(id) = id {
                v["tool_use_id"] = json!(id);
            }
            v.to_string()
        };
        // No value: the id is tool_use_id.
        let r1 = lv.run_piped(&ws, &["checkpoint", "--tool-call"], hook(Some("toolu_A")).as_bytes());
        let d1 = r1.ok();
        assert_eq!(d1["checkpoints"][0]["reason"], json!({ "kind": "ToolCall", "id": "toolu_A" }), "{d1}");
        assert!(paths(&d1["checkpoints"][0]["entries"]).contains(&"a.txt".to_string()), "{d1}");
        // Empty value: same fallback, and idempotent on the same id.
        let r2 = lv.run_piped(&ws, &["checkpoint", "--tool-call="], hook(Some("toolu_A")).as_bytes());
        assert_eq!(ids(&r2.ok()), ids(&d1));
        // A non-empty value wins over tool_use_id.
        write(&ws, "a.txt", b"hooked again\n");
        let r3 = lv.run_piped(&ws, &["checkpoint", "--tool-call", "cli-id"], hook(Some("toolu_B")).as_bytes());
        assert_eq!(r3.ok()["checkpoints"][0]["reason"], json!({ "kind": "ToolCall", "id": "cli-id" }));
        // Missing, empty, or not JSON: InvalidInput checkpoint_reason, nothing recorded.
        for input in [hook(None), hook(Some("")), "not json".to_string()] {
            for args in [&["checkpoint", "--tool-call"][..], &["checkpoint", "--tool-call="][..]] {
                let r = lv.run_piped(&ws, args, input.as_bytes());
                assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
                assert_eq!(r.reason(), "checkpoint_reason", "{r:?}");
            }
        }
        // A TTY stdin has no hook JSON to read.
        let t = lv.run_tty_env(&ws, &["checkpoint", "--tool-call"], &[]);
        assert_eq!(t.err_code(), "InvalidInput", "{t:?}");
        assert_eq!(t.reason(), "checkpoint_reason", "{t:?}");
        // --tool-call with no value and -m together is still both reasons.
        let both = lv.run_piped(&ws, &["checkpoint", "--tool-call", "-m", "x"], hook(Some("toolu_C")).as_bytes());
        assert_eq!(both.reason(), "checkpoint_reason", "{both:?}");
        s
    });
    assert_eq!(store_rows(&lv, &s, "tool", "toolu_A").len(), 1);
    assert_eq!(store_rows(&lv, &s, "tool", "cli-id").len(), 1);
    assert!(store_rows(&lv, &s, "tool", "toolu_B").is_empty());
    assert!(store_rows(&lv, &s, "tool", "").is_empty());
}

#[test]
fn daemon_up_read_does_not_open_sessions() {
    // The local path opens Git and `Sessions` for every configured repo.
    // With the main checkout moved away that open fails, so a success while
    // it is away shows the read went to the daemon and never opened them.
    let lv = Live::new(&["tool"], &base_files());
    let main = lv.main("tool");
    let away = main.with_file_name("tool.away");
    let s = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "a.txt", b"edit\n");
        let first = lv.run_in(&ws, &["checkpoint", "--tool-call", "tu-9"]).ok();
        fs::rename(&main, &away).unwrap();
        let r = std::panic::catch_unwind(|| {
            let d = lv.run_in(&ws, &["deps", "list"]).ok();
            assert_eq!(d, json!({ "proposals": [] }));
            let again = lv.run_in(&ws, &["checkpoint", "--tool-call", "tu-9"]).ok();
            assert_eq!(ids(&again), ids(&first));
        });
        fs::rename(&away, &main).unwrap();
        r.unwrap();
        s
    });
    // Control: the same moved checkout breaks the local (daemon-down) read
    // that does open sessions.
    fs::rename(&main, &away).unwrap();
    let down = lv.run(&["status", "--session", s.as_str()]);
    fs::rename(&away, &main).unwrap();
    assert_eq!(down.code, 1, "the local path opens the repo: {down:?}");
}

#[test]
fn readonly_works_without_daemon() {
    let lv = Live::new(&["tool"], &base_files());
    let (s, ws) = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "b.txt", b"two\n");
        lv.run_in(&ws, &["checkpoint", "-m", "x"]).ok();
        (s, ws)
    });
    // A stale socket file (nobody listening) and no socket file at all.
    readonly_checks(&lv, &s, &ws);
    let _ = fs::remove_file(lv.home.socket());
    assert!(!lv.home.socket().exists());
    readonly_checks(&lv, &s, &ws);
}

fn readonly_checks(lv: &Live, s: &SessionId, ws: &Path) {
    let st = lv.run_in(ws, &["status"]).ok();
    assert_eq!(st["manifest"]["id"], json!(s.as_str()));
    let g = lv.run_in(ws, &["grep", "two"]).ok();
    assert_eq!(paths(&g["hits"]), vec!["a.txt", "b.txt"], "{g}");
    let f = lv.run(&["files", "--all", "*.txt"]).ok();
    assert_eq!(paths(&f["paths"]), vec!["a.txt", "b.txt", "del.txt", "keep.txt"]);
    let d = lv.run_in(ws, &["diff"]).ok();
    assert_eq!(d["only_a"], json!([{ "path": "b.txt", "kind": "Added" }]));
    let d = lv.run(&["diff", "--between", s.as_str(), s.as_str(), "--repo", "tool"]).ok();
    // A session against itself: every change is in `both`, clean.
    assert_eq!(d["both"], json!([{ "path": "b.txt", "kind": "Added", "preview": { "kind": "Clean", "hunks": [] } }]));
    assert_eq!(lv.run(&["deps", "list"]).ok(), json!({ "proposals": [] }));
    let ds = lv.run(&["daemon", "status"]).ok();
    assert_eq!(ds["running"], json!(false));
    for args in [vec!["publish", "--from", &format!("{s}:b.txt")], vec!["push", "tool"], vec!["gc"]] {
        let args: Vec<&str> = args.iter().map(|a| a.as_ref()).collect();
        let r = lv.run_in(ws, &args);
        assert_eq!(r.code, 1, "{args:?}: {r:?}");
        assert_eq!(r.err_code(), "Busy", "{args:?}: {r:?}");
        assert_eq!(r.reason(), "daemon_down", "{args:?}: {r:?}");
        assert!(r.env()["error"]["message"].as_str().unwrap().contains("git-strata daemon start"), "{r:?}");
    }
}

#[test]
fn history_is_read_from_the_store_alone() {
    let lv = Live::new(&["tool"], &base_files());
    let s = lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "b.txt", b"1\n");
        lv.run_in(&ws, &["checkpoint", "-m", "one"]).ok();
        write(&ws, "b.txt", b"2\n");
        lv.run_in(&ws, &["checkpoint", "-m", "two"]).ok();
        s
    });
    let _ = fs::remove_file(lv.home.socket());
    assert!(!lv.home.socket().exists(), "no socket");
    let st = lv.run(&["status", "--session", s.as_str()]).ok();
    assert_eq!(st["workspaces"]["tool"]["checkpoint"], json!(2), "{st}");
    let all = lv.run(&["status", "--all"]).ok();
    assert_eq!(all["sessions"][0]["workspaces"]["tool"]["checkpoint"], json!(2));
}

fn fake_launchctl(dir: &Path) -> (PathBuf, PathBuf) {
    let bin = dir.join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    let log = dir.join("launchctl.log");
    let state = dir.join("launchctl.loaded");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1\" in\n print) [ -f '{state}' ] && exit 0; exit 113;;\n bootstrap) touch '{state}';;\n bootout) rm -f '{state}';;\n *) exit 64;;\nesac\nexit 0\n",
        log = log.display(),
        state = state.display()
    );
    let p = bin.join("launchctl");
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

#[test]
fn daemon_stop_waits_until_launchd_drops_the_agent() {
    // launchd can keep listing the service for a moment after bootout.
    let h = Home::new();
    let bin = h.path().join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    let log = h.path().join("launchctl.log");
    let lag = h.path().join("launchctl.lag");
    fs::write(&lag, "").unwrap();
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1\" in\n print) n=$(wc -c < '{lag}'); [ \"$n\" -lt 4 ] && {{ printf x >> '{lag}'; exit 0; }}; exit 113;;\n bootout) exit 0;;\n *) exit 64;;\nesac\n",
        log = log.display(),
        lag = lag.display(),
    );
    let p = bin.join("launchctl");
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let r = support::Run::from(h.cmd(&["daemon", "stop"]).env("PATH", &path).output().unwrap());
    assert_eq!(r.ok(), json!({}));
    let got: Vec<String> = fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
    let uid = unsafe { libc::getuid() };
    let t = format!("gui/{uid}/com.gwd.git-stratad");
    // loaded before bootout, still listed three times after, then gone.
    let mut want = vec![format!("print {t}"), format!("bootout {t}")];
    want.extend(std::iter::repeat_n(format!("print {t}"), 4));
    assert_eq!(got, want);
}

#[test]
fn daemon_start_stop_do_not_use_the_socket() {
    let h = Home::new();
    let (bin, log) = fake_launchctl(h.path());
    let plist = strata_daemon::plist::agent_plist_path(h.path());
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(&plist, "<plist/>\n").unwrap();
    let fake = FakeDaemon::start(&h.socket(), |_| json!({ "id": 0, "result": {} }));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let run = |args: &[&str]| support::Run::from(h.cmd(args).env("PATH", &path).output().unwrap());
    assert_eq!(run(&["daemon", "start"]).ok(), json!({}));
    assert_eq!(run(&["daemon", "start"]).ok(), json!({ "already": true }));
    assert_eq!(run(&["daemon", "stop"]).ok(), json!({}));
    assert_eq!(run(&["daemon", "stop"]).ok(), json!({ "already": true }));
    assert_eq!(fake.connections(), 0, "no byte on the socket");
    let uid = unsafe { libc::getuid() };
    let t = format!("gui/{uid}/com.gwd.git-stratad");
    let want = vec![
        format!("print {t}"),
        format!("bootstrap gui/{uid} {}", plist.display()),
        format!("print {t}"),
        format!("print {t}"),
        format!("bootout {t}"),
        format!("print {t}"),
        format!("print {t}"),
    ];
    let got: Vec<String> = fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
    assert_eq!(got, want);
    // Without the plist, start refuses before launchctl.
    fs::remove_file(&plist).unwrap();
    let r = run(&["daemon", "start"]);
    assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("InvalidInput", "no_plist"));
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), want.len());
}

#[test]
fn git_strata_is_the_same_binary() {
    let lv = Live::new(&["tool"], &base_files());
    let bin = lv.home.path().join("gitbin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(BIN, bin.join("git-strata")).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    for args in [vec!["status", "--all"], vec!["files", "--all"], vec!["no-such"], vec!["deps"]] {
        let direct = lv.home.cmd(&args).env("PATH", &path).output().unwrap();
        let mut via = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                via.env_remove(&k);
            }
        }
        let via = via
            .arg("strata")
            .args(&args)
            .current_dir(lv.home.path())
            .env("HOME", lv.home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PATH", &path)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&via.stdout), String::from_utf8_lossy(&direct.stdout), "{args:?}");
        assert_eq!(via.status.code(), direct.status.code(), "{args:?}");
    }
    // One command tree: one bin target and no src/bin.
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    assert_eq!(manifest.matches("[[bin]]").count(), 1);
    assert!(!Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin").exists());
}

/// Two sessions of `tool` (built with `bin_files`) that both change
/// `a.txt` and `bin.dat`.
fn two_sessions(lv: &Live, a_txt: (&[u8], &[u8])) -> (SessionId, SessionId) {
    let a = lv.new_session("T-a", "tool");
    let b = lv.new_session("T-b", "tool");
    let (wa, wb) = (lv.ws(&a, "tool"), lv.ws(&b, "tool"));
    write(&wa, "a.txt", a_txt.0);
    write(&wb, "a.txt", a_txt.1);
    write(&wa, "bin.dat", b"xx\0match a\n");
    write(&wb, "bin.dat", b"xx\0match b\n");
    // Deliberately not checkpointed: `diff --between` previews the current
    // workspace content, recorded or not.
    (a, b)
}

#[test]
fn binary_file_is_marked_not_rejected() {
    let lv = Live::new(&["tool"], &bin_files());
    lv.serve(|_| {
        let (a, b) = two_sessions(&lv, (b"one\nmatch A\n", b"one\nmatch B\n"));
        let d = lv.run(&["diff", "--between", a.as_str(), b.as_str(), "--repo", "tool"]).ok();
        let both = d["both"].as_array().unwrap();
        let bin = both.iter().find(|e| e["path"] == "bin.dat").expect("bin.dat in both");
        assert_eq!(bin["code"], "Binary", "{bin}");
        assert!(bin.get("preview").is_none(), "{bin}");
        let txt = both.iter().find(|e| e["path"] == "a.txt").expect("a.txt in both");
        assert!(txt["preview"]["hunks"].as_array().is_some_and(|h| !h.is_empty()), "{txt}");
        let g = lv.run(&["grep", "match", "--all"]).ok();
        assert!(g["hits"].as_array().unwrap().iter().all(|h| h["path"] != "bin.dat"), "{g}");
        assert_eq!(paths(&g["hits"]), vec!["a.txt", "a.txt"]);
        let sk = g["skipped"].as_array().unwrap();
        assert_eq!(sk.len(), 2, "{g}");
        for e in sk {
            assert_eq!((e["path"].as_str(), e["code"].as_str()), (Some("bin.dat"), Some("Binary")));
        }
    });
}

#[test]
fn lossy_utf8_one_fffd_per_bad_sequence() {
    let lv = Live::new(&["tool"], &bin_files());
    lv.serve(|_| {
        let (a, b) = two_sessions(&lv, (b"one\nours \xff mark\n", b"one\ntheirs \xe2\x82 mark\n"));
        let g = lv.run(&["grep", "mark", "--all"]).ok();
        let mut texts: Vec<String> = g["hits"].as_array().unwrap().iter().map(|h| h["text"].as_str().unwrap().to_string()).collect();
        texts.sort();
        assert_eq!(texts, vec!["ours \u{FFFD} mark", "theirs \u{FFFD} mark"], "{g}");
        let d = lv.run(&["diff", "--between", a.as_str(), b.as_str(), "--repo", "tool"]).ok();
        let txt = d["both"].as_array().unwrap().iter().find(|e| e["path"] == "a.txt").unwrap().clone();
        assert_eq!(txt["preview"]["kind"], "Conflict", "{txt}");
        let h = &txt["preview"]["hunks"][0];
        assert!(h["ours"].as_str().unwrap().contains("ours \u{FFFD} mark"), "{h}");
        assert!(h["theirs"].as_str().unwrap().contains("theirs \u{FFFD} mark"), "{h}");
        assert_eq!(h["theirs"].as_str().unwrap().matches('\u{FFFD}').count(), 1);
    });
}

#[test]
fn diff_lists_content_changes_only() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "a.txt", b"one\nTWO\n");
        write(&ws, "new.txt", b"new\n");
        fs::remove_file(ws.join("del.txt")).unwrap();
        // Same bytes, new inode and mtime: not a change.
        fs::remove_file(ws.join("keep.txt")).unwrap();
        write(&ws, "keep.txt", b"same\n");
        let d = lv.run_in(&ws, &["diff"]).ok();
        assert_eq!(
            d,
            json!({
                "repo": "tool",
                "only_a": [
                    { "path": "a.txt", "kind": "Modified" },
                    { "path": "del.txt", "kind": "Deleted" },
                    { "path": "new.txt", "kind": "Added" },
                ],
                "only_b": [],
                "both": [],
            })
        );
        // --session gives the same document from anywhere.
        assert_eq!(lv.run(&["diff", "--session", s.as_str()]).ok(), d);
    });
}

#[test]
fn diff_session_defaults_to_the_one_changed_repo() {
    let lv = Live::new(&["app", "tool"], &base_files());
    lv.serve(|_| {
        // Every configured repo gets a workspace; only `tool` is tracked.
        let s = lv.new_session("T-1", "tool");
        let none = lv.run(&["diff", "--session", s.as_str()]).ok();
        assert_eq!(none, json!({ "repo": null, "only_a": [], "only_b": [], "both": [] }));

        let tool = lv.ws(&s, "tool");
        write(&tool, "new.txt", b"new\n");
        let d = lv.run(&["diff", "--session", s.as_str()]).ok();
        assert_eq!(d["repo"], json!("tool"), "{d}");
        assert_eq!(d["only_a"], json!([{ "path": "new.txt", "kind": "Added" }]));

        // A checkpoint whose change was undone still makes `tool` a candidate.
        lv.run_in(&tool, &["checkpoint", "-m", "x"]).ok();
        fs::remove_file(tool.join("new.txt")).unwrap();
        let d = lv.run(&["diff", "--session", s.as_str()]).ok();
        assert_eq!(d, json!({ "repo": "tool", "only_a": [], "only_b": [], "both": [] }));

        let app = lv.ws(&s, "app");
        write(&app, "b.txt", b"b\n");
        let r = lv.run(&["diff", "--session", s.as_str()]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "repo", "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["candidates"], json!(["app", "tool"]), "{r:?}");
        // --repo and the cwd still pick one.
        let d = lv.run(&["diff", "--session", s.as_str(), "--repo", "app"]).ok();
        assert_eq!(d["repo"], json!("app"));
        let d = lv.run_in(&app, &["diff"]).ok();
        assert_eq!(d["only_a"], json!([{ "path": "b.txt", "kind": "Added" }]));
    });
}

#[test]
fn daemon_status_up_reports_the_cwd_session() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        let t = std::time::Instant::now();
        let ds = lv.run_in(&ws, &["daemon", "status"]).ok();
        assert_eq!(ds["running"], json!(true));
        assert_eq!(ds["session"], json!(s.as_str()), "{ds}");
        assert!(ds.get("session_lookup").is_none(), "{ds}");
        assert!(t.elapsed() < std::time::Duration::from_secs(10));
    });
}

#[test]
fn diff_omits_hunks_for_binary_and_ignores_inode() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "h1.txt", b"linked\n");
        fs::hard_link(ws.join("h1.txt"), ws.join("h2.txt")).unwrap();
        // A type change: the regular file becomes a symlink.
        fs::remove_file(ws.join("keep.txt")).unwrap();
        std::os::unix::fs::symlink("a.txt", ws.join("keep.txt")).unwrap();
        let r = lv.run_in(&ws, &["diff"]);
        let d = r.ok();
        assert_eq!(
            d["only_a"],
            json!([
                { "path": "h1.txt", "kind": "Added" },
                { "path": "h2.txt", "kind": "Added" },
                { "path": "keep.txt", "kind": "Modified" },
            ])
        );
        for key in ["inode", "ino", "nlink", "dev", "hardlink", "link"] {
            assert!(!r.stdout.contains(&format!("\"{key}\"")), "{key}: {}", r.stdout);
        }
        let lv = Live::new(&["tool"], &bin_files());
        let (a, b) = lv.serve(|_| two_sessions(&lv, (b"x\n", b"y\n")));
        let d = lv.run(&["diff", "--between", a.as_str(), b.as_str(), "--repo", "tool"]).ok();
        let bin = d["both"].as_array().unwrap().iter().find(|e| e["path"] == "bin.dat").unwrap().clone();
        assert_eq!(bin, json!({ "path": "bin.dat", "kind": "Modified", "code": "Binary" }));
    });
}

#[test]
fn grep_skips_nul_and_keeps_valid_hits() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        let mut nul = vec![b'a'; 20_000];
        nul.extend_from_slice(b"\0 needle\n");
        write(&ws, "late-nul.txt", &nul);
        write(&ws, "nul.txt", b"needle\0\n");
        write(&ws, "bad.txt", b"needle \xff\nok\n\xc0needle\n");
        write(&ws, "multi.txt", "日本 needle 語\n".as_bytes());
        let mut long = "x".repeat(200_000);
        long.push_str("needle\nneedle 2\n");
        write(&ws, "long.txt", long.as_bytes());
        let g = lv.run_in(&ws, &["grep", "needle"]).ok();
        let hits: Vec<(String, u64, String)> = g["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| (h["path"].as_str().unwrap().into(), h["line"].as_u64().unwrap(), h["text"].as_str().unwrap().chars().rev().take(12).collect::<String>().chars().rev().collect()))
            .collect();
        assert_eq!(
            hits,
            vec![
                ("bad.txt".into(), 1, "needle \u{FFFD}".into()),
                ("bad.txt".into(), 3, "\u{FFFD}needle".into()),
                ("long.txt".into(), 1, "xxxxxxneedle".into()),
                ("long.txt".into(), 2, "needle 2".into()),
                ("multi.txt".into(), 1, "日本 needle 語".into()),
            ],
            "{g}"
        );
        let sk: Vec<(String, String)> = g["skipped"].as_array().unwrap().iter().map(|e| (e["path"].as_str().unwrap().into(), e["code"].as_str().unwrap().into())).collect();
        assert_eq!(sk, vec![("late-nul.txt".into(), "Binary".into()), ("nul.txt".into(), "Binary".into())]);
    });
}

#[test]
fn no_multi_fault_order() {
    // clap refuses first; there is no contract on which of two faults wins
    // beyond that, and every code is a types.md name.
    let lv = Live::new(&["tool"], &base_files());
    let names = code_names();
    for args in [vec!["grep", "--session", "no-such"], vec!["restore", "--session", "no-such"], vec!["diff", "--between", "x"]] {
        let r = lv.run(&args);
        assert_eq!(r.code, 1);
        assert_eq!(r.err_code(), "InvalidInput", "{args:?}: {r:?}");
        assert!(names.contains(&r.err_code()));
    }
    let r = lv.run(&["status", "--session", "no-such"]);
    assert!(names.contains(&r.err_code()), "{r:?}");
}

#[test]
fn path_order_is_repopath_ord() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        for i in 0..70 {
            write(&ws, &format!("d/f{i}.txt"), b"hit\n");
        }
        for p in ["B.txt", "a b.txt", "Z/x.txt", "é.txt", "d-x.txt", "d.txt", "d0.txt"] {
            write(&ws, p, b"hit\n");
        }
        let f = lv.run_in(&ws, &["files"]).ok();
        let got = paths(&f["paths"]);
        let mut want = got.clone();
        want.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(got, want);
        assert_eq!(got.iter().filter(|p| p.starts_with("d/")).count(), 70);
        let g = lv.run_in(&ws, &["grep", "^hit$"]).ok();
        let gp = paths(&g["hits"]);
        let mut gw = gp.clone();
        gw.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(gp, gw);
        assert_eq!(gp.len(), 77);
        let d = lv.run_in(&ws, &["diff"]).ok();
        let dp = paths(&d["only_a"]);
        let mut dw = dp.clone();
        dw.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(dp, dw);
    });
}

fn assert_one_document(stdout: &str) {
    assert_eq!(stdout.matches('\n').count(), 1, "one line");
    for k in ["cursor", "next", "continue", "page", "truncated_at", "more"] {
        assert!(!stdout.contains(&format!("\"{k}\"")), "{k}");
    }
}

#[test]
fn no_pagination() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        for i in 0..300 {
            write(&ws, &format!("many/{i:03}.txt"), format!("needle {i}\n").as_bytes());
        }
        let r = lv.run_in(&ws, &["grep", "needle"]);
        assert_one_document(&r.stdout);
        assert_eq!(r.ok()["hits"].as_array().unwrap().len(), 300);
        let r = lv.run_in(&ws, &["files", "many/**"]);
        assert_one_document(&r.stdout);
        assert_eq!(r.ok()["paths"].as_array().unwrap().len(), 300);
        let r = lv.run_in(&ws, &["diff"]);
        assert_one_document(&r.stdout);
        assert_eq!(r.ok()["only_a"].as_array().unwrap().len(), 300);
    });
}

#[test]
fn conflict_json_is_not_windowed() {
    // The daemon's Conflict detail (publish's whole list) passes through.
    let lv = Live::new(&["tool"], &base_files());
    let fake = FakeDaemon::start(&lv.home.socket(), |req| {
        let conflicts: Vec<Value> = (0..2000).map(|i| json!({ "path": format!("f{i:04}.txt"), "sessions": ["s-a", "s-b"] })).collect();
        json!({ "id": req["id"], "error": { "code": "Conflict", "message": "conflicts", "detail": { "conflicts": conflicts } } })
    });
    let r = lv.run(&["gc"]);
    assert_eq!(r.err_code(), "Conflict", "{r:?}");
    let detail = &r.env()["error"]["detail"];
    assert_eq!(detail["conflicts"].as_array().unwrap().len(), 2000);
    assert_eq!(detail.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["conflicts"]);
    assert!(!r.stdout.contains("runcated"), "no window flag");
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn shape_is_this_doc_not_a_golden() {
    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(|k| k.as_str()).collect()
    }
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        write(&ws, "b.txt", b"two\n");
        let st = lv.run_in(&ws, &["status"]).ok();
        for k in ["manifest", "state", "workspaces"] {
            assert!(st.get(k).is_some(), "status.{k}: {st}");
        }
        assert_eq!(keys(&lv.run(&["status", "--all"]).ok()), vec!["sessions"]);
        let cp = lv.run_in(&ws, &["checkpoint", "-m", "m"]).ok();
        assert_eq!(keys(&cp), vec!["checkpoints"]);
        let id = cp["checkpoints"][0]["id"].as_u64().unwrap().to_string();
        let gv = lv.run_in(&ws, &["grep", "two"]).ok();
        let mut g = keys(&gv);
        g.sort();
        assert_eq!(g, vec!["hits", "skipped"]);
        let empty = lv.run_in(&ws, &["grep", "no such text anywhere"]).ok();
        assert_eq!(empty["hits"], json!([]));
        assert_eq!(keys(&lv.run_in(&ws, &["files"]).ok()), vec!["paths"]);
        let f = lv.run_in(&ws, &["files", "nothing/**"]).ok();
        assert_eq!(f, json!({ "paths": [] }));
        let dv = lv.run_in(&ws, &["diff"]).ok();
        let mut d = keys(&dv);
        d.sort();
        assert_eq!(d, vec!["both", "only_a", "only_b", "repo"]);
        assert_eq!(lv.run(&["deps", "list"]).ok(), json!({ "proposals": [] }));
        assert_eq!(lv.run(&["gc"]).ok(), json!({}));
        let r = lv.run_in(&ws, &["restore", &id]).ok();
        assert_eq!(r, json!({ "checkpoint": id.parse::<u64>().unwrap(), "session": s.as_str() }));
    });
}

#[test]
fn one_method_table() {
    // Every method name the cli sends is one of daemon.md §4's, and its
    // read/write kind agrees with the daemon's table.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let re = regex::Regex::new(r#"call\(\s*"([a-z_]+)"\s*,[^;]*?Kind::(Read|Write)"#).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for e in fs::read_dir(&src).unwrap() {
        let p = e.unwrap().path();
        let text = fs::read_to_string(&p).unwrap();
        for c in re.captures_iter(&text) {
            let m = strata_daemon::rpc::Method::from_name(&c[1]).unwrap_or_else(|| panic!("{}: {} is not a daemon method", p.display(), &c[1]));
            assert_eq!(m.is_read_only(), &c[2] == "Read", "{}", &c[1]);
            seen.insert(c[1].to_string());
        }
    }
    for m in ["new", "status", "checkpoint", "restore", "grep", "files", "diff", "deps_propose", "deps_list", "gc", "daemon_status", "publish", "push"] {
        assert!(seen.contains(m), "{m} not found in {seen:?}");
    }
}

#[test]
fn results_live_in_data_not_a_log() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let r = lv.run(&["new", "--ticket", "T-1", "--repos", "tool"]);
        let s = r.ok()["manifest"]["id"].as_str().unwrap().to_string();
        assert!(!r.stderr.contains(&s), "{}", r.stderr);
        let ws = lv.ws(&SessionId::try_from(s.clone()).unwrap(), "tool");
        write(&ws, "b.txt", b"x\n");
        let r = lv.run_in(&ws, &["checkpoint", "-m", "m"]);
        let d = r.ok();
        assert_eq!(d["checkpoints"][0]["session"], json!(s));
        assert!(r.stderr.is_empty(), "{}", r.stderr);
        for args in [vec!["status"], vec!["files"], vec!["grep", "x"], vec!["diff"]] {
            let r = lv.run_in(&ws, &args);
            r.ok();
            assert!(!r.stderr.contains(&s), "{args:?}: {}", r.stderr);
        }
    });
}

#[test]
fn listing_is_one_document() {
    let lv = Live::new(&["tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let ws = lv.ws(&s, "tool");
        for i in 0..2500 {
            write(&ws, &format!("l/{}/{i:05}-{}.txt", i % 7, "p".repeat(40)), b"x\n");
        }
        let r = lv.run_in(&ws, &["files", "l/**"]);
        assert_one_document(&r.stdout);
        assert_eq!(r.ok()["paths"].as_array().unwrap().len(), 2500);
    });
}


/// Review H4, the agent loop through the cli (SKILL.md "Publish across
/// repos"): front lands, tool conflicts with upstream. The envelope names
/// the landed commit (`data.commits.front`) and the stopped repo with its
/// paths (`error.detail.repo`, `error.detail.conflicts`). After the fix the
/// very same command publishes tool and reports front in
/// `data.already_published`, with no second front commit.
#[test]
fn publish_conflict_fix_rerun_loop() {
    let lv = Live::new(&["front", "tool"], &base_files());
    lv.serve(|_| {
        let s = lv.new_session("T-1", "front,tool");
        write(&lv.ws(&s, "front"), "f.txt", b"front\n");
        write(&lv.ws(&s, "tool"), "t.txt", b"mine\n");
        write(&lv.ws(&s, "tool"), "new.txt", b"new\n");
        let main = lv.main("tool");
        write(&main, "t.txt", b"upstream\n");
        support::git(&main, lv.home.path(), &["add", "t.txt"]);
        support::git(&main, lv.home.path(), &["commit", "-q", "-m", "upstream"]);
        support::git(&main, lv.home.path(), &["push", "-q", "origin", "main"]);
        let from = [format!("{s}:f.txt"), format!("{s}:t.txt"), format!("{s}:new.txt")];
        let args = ["publish", "--from", &from[0], "--from", &from[1], "--from", &from[2]];
        let chain = |r: &str| support::git(&lv.main(r), lv.home.path(), &["rev-parse", "-q", "--verify", "refs/strata/publish/main"]);

        let r = lv.run(&args);
        assert_eq!(r.err_code(), "Conflict", "{r:?}");
        let env = r.env();
        assert_eq!(env["error"]["detail"]["repo"], "tool", "{env}");
        assert_eq!(env["error"]["detail"]["conflicts"][0]["path"], "t.txt", "{env}");
        let landed = chain("front");
        assert_eq!(env["data"]["commits"]["front"], json!(landed), "{env}");

        write(&lv.ws(&s, "tool"), "t.txt", b"upstream\n");
        let d = lv.run(&args).ok();
        assert_eq!(chain("front"), landed, "no second front commit");
        assert_eq!(d["already_published"][0]["repo"], "front", "{d}");
        assert_eq!(d["already_published"][0]["commit"], json!(landed), "{d}");
        let t = chain("tool");
        assert_eq!(d["commits"], json!({ "tool": t }), "{d}");
        assert_eq!(support::git(&main, lv.home.path(), &["show", &format!("{t}:new.txt")]), "new");
    });
}

/// Review M3: `deps propose --switch` moves the proposer onto its new set
/// without a prompt; another session stays pinned; a proposer holding a
/// file open in its deps dir is not moved (skipped, `OpenFiles`).
#[test]
fn deps_propose_switch_moves_only_the_proposer() {
    const NEW_DEP: &str = "{\n  \"name\": \"tool\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"x\": \"^1.0.0\", \"y\": \"^2.0.0\" }\n}\n";
    let lv = Live::with_deps(&["tool"], &base_files(), &["tool"]);
    lv.serve(|core| {
        let a = lv.new_session("T-1", "tool");
        let b = lv.new_session("T-2", "tool");
        let c = lv.new_session("T-3", "tool");
        let base = |s: &SessionId| core.store.session(s).unwrap().unwrap().manifest.repos[&rn("tool")].base.clone();
        let pinned = base(&a);
        assert_eq!(base(&b), pinned);
        let (wa, wb) = (lv.ws(&a, "tool"), lv.ws(&b, "tool"));
        assert!(wa.join("node_modules/x/index.js").is_file(), "deps installed");

        // b: a file open in its node_modules keeps it where it is.
        write(&wb, "package.json", NEW_DEP.as_bytes());
        let held = std::fs::File::open(wb.join("node_modules/x/index.js")).unwrap();
        let d = lv.run_in(&wb, &["deps", "propose", "--switch"]).ok();
        assert_eq!(d["switched"]["kind"], json!("skipped"), "{d:#}");
        assert_eq!(d["switched"]["code"], json!("OpenFiles"), "{d:#}");
        assert_eq!(base(&b), pinned, "b did not move");
        drop(held);

        // a: moves onto the proposed set, no prompt; c stays pinned.
        write(&wa, "package.json", NEW_DEP.as_bytes());
        let d = lv.run_in(&wa, &["deps", "propose", "--switch"]).ok();
        assert_eq!(d["switched"]["kind"], json!("moved"), "{d:#}");
        assert_eq!(d["switched"]["session"], json!(a.as_str()), "{d:#}");
        let moved = base(&a);
        assert_eq!(moved.tree, pinned.tree);
        assert_eq!(moved.deps.as_str(), d["proposal"]["to"].as_str().unwrap());
        assert_ne!(moved.deps, pinned.deps);
        assert_eq!(base(&c), pinned, "another session stays pinned");
        assert_eq!(std::fs::read_to_string(wa.join("node_modules/x/package.json")).unwrap(), NEW_DEP);

        // Without --switch the proposer stays where it is (data is the Proposal).
        let wc = lv.ws(&c, "tool");
        write(&wc, "package.json", NEW_DEP.as_bytes());
        let d = lv.run_in(&wc, &["deps", "propose"]).ok();
        assert!(d["id"].is_number() && d.get("switched").is_none(), "{d:#}");
        assert_eq!(base(&c), pinned);
    });
}

/// Review M4: a push that times out after `commit_tree` (local bare
/// origin whose hook outlasts the timeout) is `ok: false`, `Unknown`, and
/// `data` keeps the commit and the push outcome of that repo.
#[test]
fn publish_push_timeout_keeps_commit_and_push_in_data() {
    let lv = Live::new(&["tool"], &base_files());
    let cfg = lv.home.config_dir().join("config.toml");
    let t = std::fs::read_to_string(&cfg).unwrap().replace("\npush = false", "\npush = true");
    std::fs::write(&cfg, t).unwrap();
    let origin = lv.sb.join("tool-origin.git");
    let hook = origin.join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\nsleep 2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let git = strata_gitio::Git::open(&lv.main("tool")).unwrap();
    strata_publish::seams::set_push_timeout(&git, Some(std::time::Duration::from_millis(300)));
    let t0 = std::time::Instant::now();
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        write(&lv.ws(&s, "tool"), "n.txt", b"n\n");
        let r = lv.run(&["publish", "--from", &format!("{s}:n.txt")]);
        assert_eq!(r.err_code(), "Unknown", "{r:?}");
        let env = r.env();
        let chain = support::git(&lv.main("tool"), lv.home.path(), &["rev-parse", "-q", "--verify", "refs/strata/publish/main"]);
        assert_eq!(env["data"]["commits"]["tool"], json!(chain), "{env}");
        assert_eq!(env["data"]["pushes"], json!([{ "repo": "tool", "outcome": { "kind": "Unknown" } }]), "{env}");
    });
    strata_publish::seams::set_push_timeout(&git, None);
    std::thread::sleep(std::time::Duration::from_secs(3).saturating_sub(t0.elapsed()));
}

/// Review M4: a remote refusal after the commit is `PushRefused` with the
/// commit and the `Refused` outcome in `data`; the chain stays.
#[test]
fn publish_push_refused_keeps_commit_and_push_in_data() {
    let lv = Live::new(&["tool"], &base_files());
    let cfg = lv.home.config_dir().join("config.toml");
    let t = std::fs::read_to_string(&cfg).unwrap().replace("\npush = false", "\npush = true");
    std::fs::write(&cfg, t).unwrap();
    let hook = lv.sb.join("tool-origin.git/hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        write(&lv.ws(&s, "tool"), "n.txt", b"n\n");
        let r = lv.run(&["publish", "--from", &format!("{s}:n.txt")]);
        assert_eq!(r.err_code(), "PushRefused", "{r:?}");
        let env = r.env();
        let chain = support::git(&lv.main("tool"), lv.home.path(), &["rev-parse", "-q", "--verify", "refs/strata/publish/main"]);
        assert_eq!(env["data"]["commits"]["tool"], json!(chain), "{env}");
        assert_eq!(env["data"]["pushes"][0]["repo"], "tool", "{env}");
        assert_eq!(env["data"]["pushes"][0]["outcome"]["kind"], "Refused", "{env}");
        assert!(env["data"]["pushes"][0]["outcome"]["message"].as_str().unwrap().contains("denied by policy"), "{env}");
    });
}

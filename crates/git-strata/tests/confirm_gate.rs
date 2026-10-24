//! cli.md §8 Task 2: TTY before confirmation; nothing reaches the socket
//! when a confirm-class command runs without a terminal.
//!
//! confirm.md §8 Task 6 (end to end, the real `git-strata` binary against
//! an in-process daemon whose `Confirmer` is a `ScriptedAuth`):
//! `non_tty_refused_before_prompt`, `no_env_or_flag_bypass`,
//! `release_daemon_has_no_test_seams`.

mod support;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::CommandFactory;
use strata_daemon::seams::{PromptLog, Verdict, bypass_env_names};
use strata_daemon::startup::Core;
use strata_types::{CommitOid, DepsSetId, RepoName, SessionId, TreeOid};
use support::live::{Live, write};
use support::{FakeDaemon, Home, RepoSpec, Run, git};

fn guarded_home() -> Home {
    let h = Home::new();
    h.write_config(&[RepoSpec { name: "app", push: true, confirm: true, deploy_on_push: true }]);
    h
}

fn refuse_all(req: &Value) -> Value {
    json!({ "id": req["id"], "ok": false, "error": { "rpc": "Failed", "message": "the test daemon answers nothing" } })
}

#[test]
fn confirm_commands_refuse_non_tty() {
    let h = guarded_home();
    let d = FakeDaemon::start(&h.socket(), refuse_all);
    let cases: &[&[&str]] = &[
        &["deps", "approve", "1"],
        &["push", "app"],
        &["publish", "--from", "s-aaaaaaaa:a.txt"],
        &["chain", "--repo", "app", "--restore"],
        &["chain", "--repo", "app", "--accept-origin", "replay"],
        &["resolve", "--session", "s-aaaaaaaa", "--repo", "app", "--accept"],
        &["config", "relax", "app"],
        &["rm", "--session", "s-aaaaaaaa", "--discard"],
    ];
    for args in cases {
        let r = h.run(args);
        assert_eq!(r.err_code(), "NotTty", "{args:?}: {r:?}");
        assert_eq!(r.reason(), "not_tty", "{args:?}");
        assert!(r.stderr.is_empty() || !r.stderr.contains("confirm"), "{args:?} showed a prompt: {r:?}");
    }
    // Publish without --from on a non-TTY is no_batch, not a prompt.
    let r = h.run(&["publish"]);
    assert_eq!(r.err_code(), "InvalidInput");
    assert_eq!(r.reason(), "no_batch");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(d.connections(), 0, "a refused command connected: {:?}", d.requests());
}

fn superseded(req: &Value) -> Value {
    assert_eq!(req["method"], "deps_approve");
    json!({
        "id": req["id"],
        "ok": false,
        "error": {
            "code": "NoSuchProposal",
            "message": "proposal 1 is superseded",
            "detail": {
                "proposal": 1,
                "state": { "kind": "Superseded", "by": 2 },
                "replacement": { "id": 2, "repo": "app", "state": { "kind": "Pending" } }
            }
        }
    })
}

#[test]
fn superseded_proposal_is_not_auto_approved() {
    let h = guarded_home();
    let d = FakeDaemon::start(&h.socket(), superseded);
    let r = h.run_tty(&["deps", "approve", "1"]);
    assert_eq!(r.err_code(), "NoSuchProposal", "{r:?}");
    assert!(r.stderr.contains("superseded by proposal 2"), "{r:?}");
    assert!(r.stderr.contains("replacement"), "{r:?}");
    let reqs = d.requests();
    assert_eq!(reqs.len(), 1, "approved again: {reqs:?}");
    assert_eq!(reqs[0]["method"], "deps_approve");
    assert_eq!(reqs[0]["params"]["proposal"], 1);
    assert_eq!(reqs[0]["params"]["sessions"], Value::Null);
}

#[test]
fn resolve_and_chain_require_an_explicit_choice() {
    let h = guarded_home();
    let d = FakeDaemon::start(&h.socket(), refuse_all);
    for (args, reason) in [
        (vec!["resolve", "--session", "s-aaaaaaaa", "--repo", "app"], "no_choice"),
        (vec!["resolve", "--session", "s-aaaaaaaa", "--repo", "app", "--abandon", "--accept"], "both_choices"),
        (vec!["chain", "--repo", "app"], "no_choice"),
        (vec!["chain", "--repo", "app", "--accept-origin", "drop", "--restore"], "both_choices"),
    ] {
        // Even on a terminal: the choice is checked first.
        let r = h.run_tty(&args);
        assert_eq!(r.err_code(), "InvalidInput", "{args:?}: {r:?}");
        assert_eq!(r.reason(), reason, "{args:?}");
        let r = h.run(&args);
        assert_eq!(r.reason(), reason, "{args:?}");
    }
    // A diverged chain cannot be restored: not_ancestor before any push.
    let repo = h.repo_dir("app");
    git(&repo, h.path(), &["init", "-q", "-b", "main"]);
    let tree = git(&repo, h.path(), &["write-tree"]);
    let a = git(&repo, h.path(), &["commit-tree", &tree, "-m", "a"]);
    let b = git(&repo, h.path(), &["commit-tree", &tree, "-p", &a, "-m", "origin b"]);
    let c = git(&repo, h.path(), &["commit-tree", &tree, "-p", &a, "-m", "local c"]);
    git(&repo, h.path(), &["update-ref", "refs/strata/remotes/origin/main", &b]);
    git(&repo, h.path(), &["update-ref", "refs/strata/publish/main", &c]);
    git(&repo, h.path(), &["update-ref", "refs/strata/anchor/main", &a]);
    let r = h.run_tty(&["chain", "--repo", "app", "--restore"]);
    assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
    assert_eq!(r.reason(), "not_ancestor", "{r:?}");
    let refs = git(&repo, h.path(), &["for-each-ref", "--format=%(refname) %(objectname)"]);
    assert!(refs.contains(&format!("refs/strata/publish/main {c}")), "{refs}");
    assert!(refs.contains(&format!("refs/strata/remotes/origin/main {b}")), "{refs}");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(d.connections(), 0, "{:?}", d.requests());
}

// ---- confirm.md §8 Task 6 ----

/// The guarded fixture: repo `app` pushes, confirms and deploys on push.
struct Fx {
    s: SessionId,
    ws: PathBuf,
    pid: u64,
    cid: u64,
    origin_main: String,
}

fn guarded_live() -> Live {
    let lv = Live::new(&["app"], &[("a.txt", b"one\n")]);
    lv.write_config_guarded(&["app"]);
    lv
}

fn origin_main(lv: &Live) -> String {
    git(&lv.sb.join("app-origin.git"), lv.home.path(), &["rev-parse", "refs/heads/main"])
}

/// A session with an unpublished, checkpointed change; a Pending proposal
/// from main; the origin branch as it was.
fn fixture(lv: &Live, core: &Core) -> Fx {
    let s = lv.new_session("T-1", "app");
    let ws = lv.ws(&s, "app");
    write(&ws, "a.txt", b"two\n");
    let cp = lv.run_in(&ws, &["checkpoint", "-m", "unpublished"]).ok();
    let cid = cp["checkpoints"][0]["id"].as_u64().unwrap_or_else(|| panic!("{cp}"));
    let repo = RepoName::try_from("app".to_string()).unwrap();
    let main = lv.main("app");
    let head = git(&main, lv.home.path(), &["rev-parse", "HEAD"]);
    let tree = git(&main, lv.home.path(), &["rev-parse", "HEAD^{tree}"]);
    let base = core.store.session(&s).unwrap().unwrap().manifest.repos[&repo].base.clone();
    let rc = core.config.repos[&repo].clone();
    let ctx = strata_deps::Ctx {
        store: &core.store,
        paths: &core.config.paths,
        repo: &repo,
        cfg: &rc,
        git: &core.gits[&repo],
        env: &core.env,
        locks: &core.locks,
    };
    let to = if base.deps.as_str() == "e".repeat(64) { "d".repeat(64) } else { "e".repeat(64) };
    let p = strata_deps::propose(
        &ctx,
        strata_deps::ProposeArgs {
            origin: strata_deps::ProposalOrigin::Main,
            from: base.deps.clone(),
            from_inputs: "1".repeat(64),
            to: DepsSetId::try_from(to).unwrap(),
            to_inputs: "2".repeat(64),
            tree: Some(TreeOid::try_from(tree).unwrap()),
            head: Some(CommitOid::try_from(head).unwrap()),
        },
    )
    .expect("pending proposal");
    assert_eq!(p.state, strata_deps::ProposalState::Pending);
    Fx { s, ws, pid: p.id.get(), cid, origin_main: origin_main(lv) }
}

/// The three checks: the session is there, the proposal is still Pending,
/// origin's branch did not move.
fn untouched(lv: &Live, core: &Core, fx: &Fx, what: &str) {
    let rec = core.store.session(&fx.s).unwrap().expect("session row");
    assert_ne!(rec.lifecycle, strata_store::Lifecycle::Removed, "{what}: session removed");
    let id = strata_types::ProposalId::try_from(fx.pid).unwrap();
    let p = strata_deps::get_proposal(&core.store, id).unwrap();
    assert_eq!(p.state, strata_deps::ProposalState::Pending, "{what}: proposal moved");
    assert_eq!(origin_main(lv), fx.origin_main, "{what}: origin moved");
}

/// Every confirm-class command against the fixture.
fn confirm_cmds(fx: &Fx) -> Vec<Vec<String>> {
    let s = fx.s.as_str();
    [
        vec!["rm".to_string(), "--session".into(), s.into(), "--discard".into()],
        vec!["deps".into(), "approve".into(), fx.pid.to_string()],
        vec!["push".into(), "app".into()],
        vec!["publish".into(), "--from".into(), format!("{s}:a.txt")],
        vec!["config".into(), "relax".into(), "app".into()],
    ]
    .to_vec()
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn passes(n: usize) -> Vec<Verdict> {
    (0..n).map(|_| Verdict::pass()).collect()
}

#[test]
fn non_tty_refused_before_prompt() {
    let lv = guarded_live();
    lv.serve_scripted(passes(32), |core, log| {
        let fx = fixture(&lv, core);
        let seen = lv.requests.lock().unwrap().len();
        for cmd in confirm_cmds(&fx) {
            let args = strs(&cmd);
            for r in [lv.run_in(&fx.ws, &args), lv.run_piped(&fx.ws, &args, b"y\n")] {
                assert_eq!(r.err_code(), "NotTty", "{args:?}: {r:?}");
                assert!(log.shown().is_empty(), "{args:?} prompted: {:?}", log.shown());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        // Review-2 N2: a refused publish sends only its before-publish
        // checkpoint and the read-only plan (to learn which repos remain);
        // no `publish`, no prompt. Every other command sends nothing.
        assert_eq!(
            lv.requests.lock().unwrap()[seen..],
            ["checkpoint", "publish_plan", "checkpoint", "publish_plan"],
            "a refused command reached the daemon"
        );
        untouched(&lv, core, &fx, "non-tty");

        // Control: on a pseudo-terminal each prompts exactly once.
        // (From the sandbox root: a cwd inside the workspace would itself
        // be an open file for `rm`.)
        let once = |args: &[&str]| -> Run {
            let n = log.shown().len();
            let r = lv.run_tty_env(&lv.sb, args, &[]);
            assert_eq!(log.shown().len(), n + 1, "{args:?} did not prompt exactly once: {r:?}");
            r
        };
        // deps approve: one prompt; the rollout after it needs a real
        // install, so only the prompt is asserted (impl-progress/confirm.md).
        let pid = fx.pid.to_string();
        let r = once(&["deps", "approve", &pid]);
        eprintln!("deps approve after the prompt: {}", r.stdout.trim_end());
        let publish_from = format!("{}:a.txt", fx.s);
        once(&["publish", "--from", &publish_from]).ok();
        assert_ne!(origin_main(&lv), fx.origin_main, "publish did not push");
        // push needs a chain ahead of origin: one more commit on the
        // local publish chain (as a later publish without push leaves it).
        let main = lv.main("app");
        let top = git(&main, lv.home.path(), &["rev-parse", "refs/strata/publish/main"]);
        let tree = git(&main, lv.home.path(), &["rev-parse", &format!("{top}^{{tree}}")]);
        let next = git(&main, lv.home.path(), &["commit-tree", &tree, "-p", &top, "-m", "next"]);
        git(&main, lv.home.path(), &["update-ref", "refs/strata/publish/main", &next, &top]);
        once(&["push", "app"]).ok();
        assert_eq!(origin_main(&lv), next, "push did not move origin");
        once(&["rm", "--session", fx.s.as_str(), "--discard"]).ok();
        let rec = core.store.session(&fx.s).unwrap();
        assert!(rec.is_none_or(|r| r.lifecycle == strata_store::Lifecycle::Removed), "rm --discard did not remove");
    });
}

/// Review-2 N2: the TTY gate covers only the repos the publish will still
/// commit. A non-TTY re-run whose guarded repo already landed (same
/// content) succeeds with `already_published` and no prompt; once that repo
/// has new content the gate refuses again, before any `publish` request.
#[test]
fn non_tty_rerun_of_a_landed_guarded_repo_is_not_gated() {
    let lv = guarded_live();
    lv.serve_scripted(passes(4), |_core, log| {
        let s = lv.new_session("T-2", "app");
        let ws = lv.ws(&s, "app");
        write(&ws, "a.txt", b"two\n");
        let from = format!("{s}:a.txt");
        let before = origin_main(&lv);
        lv.run_tty_env(&lv.sb, &["publish", "--from", &from], &[]).ok();
        assert_eq!(log.shown().len(), 1, "the first publish prompts once");
        let landed = origin_main(&lv);
        assert_ne!(landed, before, "publish did not push");

        let seen = lv.requests.lock().unwrap().len();
        let r = lv.run_in(&ws, &["publish", "--from", &from]).ok();
        assert_eq!(r["already_published"][0]["repo"], "app", "{r:#}");
        assert_eq!(r["commits"], json!({}), "{r:#}");
        assert_eq!(log.shown().len(), 1, "the re-run prompted");
        assert_eq!(origin_main(&lv), landed);
        assert_eq!(lv.requests.lock().unwrap()[seen..], ["checkpoint", "publish_plan", "publish"]);

        write(&ws, "a.txt", b"three\n");
        let seen = lv.requests.lock().unwrap().len();
        let r = lv.run_in(&ws, &["publish", "--from", &from]);
        assert_eq!(r.err_code(), "NotTty", "{r:?}");
        assert_eq!(log.shown().len(), 1, "a refused publish prompted");
        assert_eq!(lv.requests.lock().unwrap()[seen..], ["checkpoint", "publish_plan"]);
        assert_eq!(origin_main(&lv), landed, "a refused publish pushed");
    });
}

/// Every run's verdict: never a success; a prompt only before `NotConfirmed`.
fn judge(r: &Run, shown_before: usize, log: &PromptLog, what: &str, codes: &mut std::collections::BTreeSet<String>) {
    let after = log.shown().len();
    assert!(r.code != 0, "{what} succeeded: {r:?}");
    let v = r.env();
    assert_eq!(v["ok"], false, "{what}: {r:?}");
    let code = v["error"]["code"].as_str().unwrap_or_default().to_string();
    match after - shown_before {
        0 => {
            assert_ne!(code, "", "{what}: {r:?}");
            codes.insert(code);
        }
        1 => assert_eq!(code, "NotConfirmed", "{what}: {r:?}"),
        n => panic!("{what}: {n} prompts"),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

const ENV_CHILD: &str = "GS_GATE_ENV_VALUE";

/// A legal value for a flag (by its long name).
fn values_for(long: &str, n: usize, fx: &Fx, possible: Option<String>) -> Vec<String> {
    let s = fx.s.to_string();
    let v: Vec<String> = match long {
        "session" => vec![s.clone()],
        "repo" => vec!["app".into()],
        "path" | "file" => vec!["a.txt".into()],
        "from" => vec![format!("{s}:a.txt")],
        "assign" => vec!["a.txt".into(), s.clone()],
        "between" => vec![s.clone(), s.clone()],
        _ => vec![possible.unwrap_or_else(|| "x".into())],
    };
    (0..n.max(1)).map(|i| v[i.min(v.len() - 1)].clone()).collect()
}

/// (subcommand path, tokens) for every named argument of every subcommand.
fn every_flag(fx: &Fx) -> (Vec<Vec<String>>, std::collections::BTreeMap<String, Vec<String>>) {
    fn walk(c: &clap::Command, path: String, fx: &Fx, out: &mut Vec<Vec<String>>, bools: &mut std::collections::BTreeMap<String, Vec<String>>) {
        for a in c.get_arguments() {
            let id = a.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            let Some(long) = a.get_long() else { continue };
            let mut t = vec![format!("--{long}")];
            if a.get_action().takes_values() {
                let n = a.get_num_args().map(|r| r.min_values()).unwrap_or(1);
                let possible = a.get_possible_values().first().map(|p| p.get_name().to_string());
                t.extend(values_for(long, n, fx, possible));
            } else {
                bools.entry(path.clone()).or_default().push(format!("--{long}"));
            }
            if !out.contains(&t) {
                out.push(t);
            }
        }
        for sc in c.get_subcommands() {
            let p = if path.is_empty() { sc.get_name().to_string() } else { format!("{path} {}", sc.get_name()) };
            walk(sc, p, fx, out, bools);
        }
    }
    let mut out = Vec::new();
    let mut bools = std::collections::BTreeMap::new();
    walk(&git_strata::args::Cli::command(), String::new(), fx, &mut out, &mut bools);
    (out, bools)
}

/// The confirm command's subcommand path (`deps approve`, `rm`, …).
fn sub_path(cmd: &[String]) -> String {
    match cmd[0].as_str() {
        "deps" | "config" => format!("{} {}", cmd[0], cmd[1]),
        _ => cmd[0].clone(),
    }
}

fn rpc(sock: &Path, method: &str, params: serde_json::Value) -> serde_json::Value {
    let mut s = UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let line = json!({ "id": "1", "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

#[test]
fn no_env_or_flag_bypass() {
    let names = bypass_env_names(&workspace_root());
    assert!(names.len() >= 12, "{names:?}");
    if let Ok(value) = std::env::var(ENV_CHILD) {
        // (a) in a child whose environment (and so the in-process
        // daemon's) carries every name; the cli gets them too.
        let env: Vec<(String, String)> = names.iter().map(|n| (n.clone(), value.clone())).collect();
        let lv = guarded_live();
        lv.serve_scripted(vec![], |core, log| {
            let fx = fixture(&lv, core);
            let mut codes = Default::default();
            for cmd in confirm_cmds(&fx) {
                let n = log.shown().len();
                let r = lv.run_tty_env(&fx.ws, &strs(&cmd), &env);
                judge(&r, n, log, &format!("env={value} {cmd:?}"), &mut codes);
            }
            untouched(&lv, core, &fx, "env");
            eprintln!("env {value}: codes without a prompt {codes:?}");
        });
        return;
    }
    for value in ["1", "true", "yes", "on", "0"] {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "no_env_or_flag_bypass", "--nocapture", "--test-threads=1"])
            .envs(names.iter().map(|n| (n.as_str(), value)))
            .env(ENV_CHILD, value)
            .output()
            .unwrap();
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "env child {value}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let lv = guarded_live();
    lv.serve_scripted(vec![], |core, log| {
        let fx = fixture(&lv, core);
        let mut codes = std::collections::BTreeSet::new();
        let (flags, bools) = every_flag(&fx);
        assert!(flags.len() >= 15, "{flags:?}");
        let mut runs = 0;
        // (b) flags: each named argument of every subcommand on each
        // confirm command, then all of the command's own booleans at once.
        for cmd in confirm_cmds(&fx) {
            for f in &flags {
                let mut args = cmd.clone();
                args.extend(f.iter().cloned());
                let n = log.shown().len();
                let r = lv.run_tty_env(&fx.ws, &strs(&args), &[]);
                judge(&r, n, log, &format!("{args:?}"), &mut codes);
                runs += 1;
            }
            let mut args = cmd.clone();
            for b in bools.get(&sub_path(&cmd)).cloned().unwrap_or_default() {
                if !args.contains(&b) {
                    args.push(b);
                }
            }
            let n = log.shown().len();
            let r = lv.run_tty_env(&fx.ws, &strs(&args), &[]);
            judge(&r, n, log, &format!("all booleans {args:?}"), &mut codes);
            runs += 1;
        }
        untouched(&lv, core, &fx, "flags");

        // (c) RPC: a client on the socket, plain and with one extra field.
        let sock = lv.home.socket();
        let s = fx.s.as_str();
        let methods = [
            ("rm", json!({ "session": s, "discard": true })),
            ("deps_approve", json!({ "proposal": fx.pid, "sessions": null })),
            ("push", json!({ "repo": "app" })),
            ("publish", json!({ "group": { "id": 0, "batches": [{ "session": s, "repo": "app", "checkpoint": fx.cid, "paths": ["a.txt"] }] } })),
            ("config_relax", json!({ "repo": "app" })),
        ];
        let extras = ["confirmed", "confirm", "token", "nonce", "yes", "assume_yes", "force", "skip_confirm", "tty"];
        for (m, p) in &methods {
            let mut variants = vec![(String::from("plain"), p.clone())];
            for x in extras {
                for v in [json!(true), json!("1")] {
                    let mut q = p.clone();
                    q[x] = v.clone();
                    variants.push((format!("{x}={v}"), q));
                }
            }
            for (what, q) in variants {
                let n = log.shown().len();
                let r = rpc(&sock, m, q);
                assert_eq!(r["ok"], false, "rpc {m} {what}: {r}");
                match log.shown().len() - n {
                    0 => {}
                    1 => assert_eq!(r["error"]["code"], "NotConfirmed", "rpc {m} {what}: {r}"),
                    k => panic!("rpc {m} {what}: {k} prompts"),
                }
                if what != "plain" && log.shown().len() > n {
                    panic!("rpc {m} {what}: an unknown field reached the prompt: {r}");
                }
                runs += 1;
            }
        }
        untouched(&lv, core, &fx, "rpc");
        eprintln!("{runs} runs; codes without a prompt: {codes:?}");
    });
}

#[test]
fn release_daemon_has_no_test_seams() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    for pkg in ["strata-daemon", "git-strata"] {
        let out = Command::new(&cargo)
            .current_dir(workspace_root())
            .args(["tree", "-p", pkg, "-e", "features", "--edges", "normal", "--target", "aarch64-apple-darwin", "--offline"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{pkg}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("strata-confirm feature \"default\""), "{pkg}: {text}");
        assert!(!text.contains("feature \"test-seams\""), "{pkg} release build has test seams:\n{text}");
    }
}

//! deps_propose / deps_approve over the socket (deps.md §5.12, wired for
//! the cli): refusals come before any prompt; a proposal is recorded only
//! on drift; `switch` moves the proposer itself without a confirmation.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::{Value, json};
use strata_daemon::handlers;
use strata_daemon::startup::{self, Daemon};
use strata_types::SessionId;
use support::*;

fn serving<R>(d: &Daemon<'_>, f: impl FnOnce() -> R) -> R {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(d, &listener, &ext));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn call(socket: &Path, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let line = json!({ "id": "1", "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

fn ok(v: Value) -> Value {
    assert_eq!(v["ok"], json!(true), "{v:#}");
    v["data"].clone()
}

fn new_session(socket: &Path) -> SessionId {
    let d = ok(call(socket, "new", json!({ "ticket": null, "repos": ["tool"] })));
    serde_json::from_value(d["session"]["manifest"]["id"].clone()).unwrap()
}

#[test]
fn approve_unknown_proposal_is_refused_without_prompt() {
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let r = call(&sock, "deps_approve", json!({ "proposal": 4242, "sessions": null }));
        assert_eq!(r["ok"], json!(false), "{r:#}");
        assert_eq!(r["error"]["code"], json!("NoSuchProposal"), "{r:#}");
    });
    assert!(log.shown().is_empty());
}

#[test]
fn propose_without_drift_records_nothing() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let p = ok(call(&sock, "deps_propose", json!({ "session": id, "repo": "tool", "switch": false })));
        assert_eq!(p["proposal"], Value::Null, "{p:#}");
        let l = ok(call(&sock, "deps_list", json!({ "repo": null })));
        assert_eq!(l["proposals"], json!([]), "{l:#}");
    });
}

#[test]
fn propose_on_drift_awaits_publish_and_approve_refuses_it() {
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        let from_head = core.store.session(&id).unwrap().unwrap().manifest.repos[&repo_name("tool")].head.clone();
        std::fs::write(root.join("package.json"), NEW_DEP).unwrap();
        let p = ok(call(&sock, "deps_propose", json!({ "session": id, "repo": "tool", "switch": false })));
        assert_eq!(p["proposal"]["from_head"], json!(from_head));
        assert_eq!(p["proposal"]["state"], json!({ "kind": "AwaitingPublish" }), "{p:#}");
        assert!(p["drift"].is_object(), "{p:#}");
        assert_eq!(p["switched"], Value::Null);
        let pid = p["proposal"]["id"].clone();
        // Not Pending: refused before the confirmation, state in detail.
        let r = call(&sock, "deps_approve", json!({ "proposal": pid, "sessions": null }));
        assert_eq!(r["error"]["code"], json!("NoSuchProposal"), "{r:#}");
        assert_eq!(r["error"]["detail"]["state"], json!({ "kind": "AwaitingPublish" }), "{r:#}");
    });
    assert!(log.shown().is_empty());
}

#[test]
fn propose_switch_moves_the_proposer_without_prompt() {
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        let before = core.store.session(&id).unwrap().unwrap().manifest.repos[&repo_name("tool")].base.clone();
        std::fs::write(root.join("package.json"), NEW_DEP).unwrap();
        let p = ok(call(&sock, "deps_propose", json!({ "session": id, "repo": "tool", "switch": true })));
        let sw = p["switched"].as_array().unwrap_or_else(|| panic!("{p:#}"));
        assert_eq!(sw.len(), 1, "{p:#}");
        assert_eq!(sw[0]["kind"], json!("moved"), "{p:#}");
        let after = core.store.session(&id).unwrap().unwrap().manifest.repos[&repo_name("tool")].base.clone();
        assert_eq!(after.tree, before.tree);
        assert_ne!(after.deps, before.deps);
        assert_eq!(after.deps.as_str(), p["proposal"]["to"].as_str().unwrap());
    });
    assert!(log.shown().is_empty());
}

#[test]
fn deps_list_changes_legacy_base_and_best_effort_error() {
    use std::time::SystemTime;
    use strata_store::NewProposal;
    use strata_types::{CommitOid, DepsSetId, TreeOid};
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let repo = repo_name("tool");
        let session = core.store.session(&id).unwrap().unwrap();
        let from = session.manifest.repos[&repo].base.deps.clone();
        let main = fx.sb.path("tool");
        let head = CommitOid::try_from(fx.sb.commit(&main, &[("package.json", NEW_DEP.as_bytes())], "add y")).unwrap();
        let tree = core.gits[&repo].tree_of(&head).unwrap();
        let proposal = NewProposal {
            repo: repo.clone(),
            origin: None,
            from,
            from_inputs: "1".repeat(64),
            to: DepsSetId::try_from("2".repeat(64)).unwrap(),
            to_inputs: "3".repeat(64),
            head: Some(head),
            tree: Some(tree),
            from_head: None,
        };
        let pid = core.store.record_proposal(&proposal, None, SystemTime::now()).unwrap().unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let l = loop {
            let reply = ok(call(&sock, "deps_list", json!({"repo":"tool"})));
            if reply["proposals"][0].get("changes").is_some() || std::time::Instant::now() >= until {
                break reply;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(l["proposals"][0]["id"], pid.get());
        assert_eq!(l["proposals"][0]["changes"]["since"], "session_base", "{l:#}");
        assert!(l["proposals"][0]["changes"]["summary"].as_str().unwrap().contains("+y ^2.0.0"), "{l:#}");
        let bad = NewProposal {
            head: Some(CommitOid::try_from("9".repeat(40)).unwrap()),
            tree: Some(TreeOid::try_from("8".repeat(40)).unwrap()),
            ..proposal
        };
        let next = core.store.record_proposal(&bad, Some(pid), SystemTime::now()).unwrap().unwrap();
        let l = ok(call(&sock, "deps_list", json!({"repo":"tool"})));
        assert_eq!(l["proposals"][0]["id"], next.get());
        assert!(l["proposals"][0].get("changes").is_none(), "{l:#}");
    });
    assert!(log.shown().is_empty(), "read-only enrichment must not confirm");
}

/// A shell-owned child represents another user's shell to an in-process
/// daemon. Closing stdin asks only the children this fixture started to exit.
struct CwdHolder(std::process::Child, i32);
impl CwdHolder {
    fn new(path: &Path) -> Self {
        use std::process::{Command, Stdio};
        let mut shell = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 300 & child=$!; echo $child; read done; kill $child; wait $child 2>/dev/null"])
            .current_dir(path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut ready = String::new();
        BufReader::new(shell.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let pid = ready.trim().parse::<i32>().unwrap();
        assert!(pid > 0);
        std::thread::sleep(Duration::from_millis(50));
        Self(shell, pid)
    }
}
impl Drop for CwdHolder {
    fn drop(&mut self) { drop(self.0.stdin.take()); let _=self.0.wait(); }
}

#[test]
fn propose_switch_refuses_other_cwd_and_override_reports_the_old_tree() {
    for caller_exemption in [false, true] {
        let fx = Dfx::new();
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            let id = new_session(&sock);
            let root = ws(&core, &id, "tool");
            let holder = CwdHolder::new(&root.join("node_modules"));
            let before = core.store.session(&id).unwrap().unwrap().manifest.repos
                [&repo_name("tool")]
                .clone();
            std::fs::write(root.join("package.json"), NEW_DEP).unwrap();
            let reply = ok(call(
                &sock,
                "deps_propose",
                json!({"session":id,"repo":"tool","switch":true}),
            ));
            assert_eq!(reply["switched"][0]["code"], "OpenFiles", "{reply:#}");
            assert_eq!(reply["switched"][0]["detail"]["reason"], "cwd_holders");
            assert_eq!(
                core.store.session(&id).unwrap().unwrap().manifest.repos[&repo_name("tool")].base,
                before.base
            );
            let reply = ok(call(
                &sock,
                "deps_propose",
                json!({"session":id,"repo":"tool","switch":true,
            "allow_cwd_holders": !caller_exemption, "allow_cwd": caller_exemption, "caller_pids": [holder.1]}),
            ));
            assert_eq!(reply["switched"][0]["kind"], "moved", "{reply:#}");
            assert_eq!(
                reply["switched"][0]["report"]["warnings"][0]["reason"],
                "cwd_stale"
            );
        });
    }
}

#[test]
fn approval_skips_cwd_holders_unless_explicitly_overridden() {
    for caller_exemption in [false, true] {
        use strata_confirm::seams::Verdict;
        let fx = Dfx::new();
        let (auth, _, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass()]);
        let core = fx.prepare_with(auth).unwrap();
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            let id = new_session(&sock);
            let root = ws(&core, &id, "tool");
            let holder = CwdHolder::new(&root.join("node_modules"));
            let main = fx.main("tool");
            let repo = repo_name("tool");
            let head = strata_types::CommitOid::try_from(fx.sb.commit(
                &main,
                &[("package.json", NEW_DEP.as_bytes())],
                "new deps",
            ))
            .unwrap();
            fx.sb.git(&main, &["push", "-q", "origin", "main"]);
            core.gits
                .get(&repo)
                .unwrap()
                .fetch("origin", "main")
                .unwrap();
            let tree = core.gits.get(&repo).unwrap().tree_of(&head).unwrap();
            let ctx = strata_deps::Ctx {
                store: &core.store,
                paths: &core.config.paths,
                repo: &repo,
                cfg: core.config.repos.get(&repo).unwrap(),
                git: core.gits.get(&repo).unwrap(),
                env: &core.env,
                locks: &core.locks,
            };
            let proposal = strata_deps::observe_main(&ctx, &tree, &head, &main)
                .unwrap()
                .unwrap();
            let reply = ok(call(
                &sock,
                "deps_approve",
                json!({"proposal":proposal.id,"sessions":[id]}),
            ));
            assert_eq!(reply["results"][0]["code"], "OpenFiles", "{reply:#}");
            assert_eq!(reply["results"][0]["detail"]["reason"], "cwd_holders");
            let reply = ok(call(
                &sock,
                "deps_approve",
                json!({"proposal":proposal.id,"sessions":[id],
            "allow_cwd_holders": !caller_exemption, "allow_cwd": caller_exemption, "caller_pids": [holder.1]}),
            ));
            assert_eq!(reply["results"][0]["kind"], "moved", "{reply:#}");
            assert_eq!(
                reply["results"][0]["report"]["warnings"][0]["reason"],
                "cwd_stale"
            );
        });
    }
}

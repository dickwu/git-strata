mod support;

use serde_json::{Value, json};
use std::process::{Child, Command, Stdio};
use support::{FakeDaemon, live::Live};

struct Holder(Child);
impl Holder {
    fn new(path: &std::path::Path) -> Self {
        Self(
            Command::new("/bin/sleep")
                .arg("300")
                .current_dir(path)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn legacy(q: &Value) -> Value {
    if q["params"].get("caller_pids").is_some() {
        json!({"id":q["id"],"ok":false,"error":{"rpc":"Failed","message":"unknown field `caller_pids`"}})
    } else {
        json!({"id":q["id"],"ok":true,"data":{"results":[]}})
    }
}
const CHANGED: &str = r#"{"name":"tool","dependencies":{"y":"^2.0.0"}}"#;

#[test]
fn explicit_status_selectors_work_from_stale_cwd_with_daemon_and_local_fallback() {
    let live = Live::new(&["tool"], &[("a.txt", b"base\n")]);
    let (id, stale) = live.serve(|_| {
        let id = live.new_session("stale-status", "tool");
        let stale = live
            .ws(&id, "tool")
            .with_file_name(".strata-tmp-move-tool-1234");
        std::fs::create_dir_all(&stale).unwrap();
        for brief in [false, true] {
            let mut args = vec!["status"];
            if brief {
                args.push("--brief");
            }
            assert_eq!(live.run_in(&stale, &args).reason(), "stale_cwd");
            args.extend(["--session", id.as_str()]);
            live.run_in(&stale, &args).ok();
        }
        (id, stale)
    });
    for brief in [false, true] {
        let mut args = vec!["status"];
        if brief {
            args.push("--brief");
        }
        assert_eq!(live.run_in(&stale, &args).reason(), "stale_cwd");
        args.extend(["--session", id.as_str()]);
        live.run_in(&stale, &args).ok();
    }
}

#[test]
fn legacy_propose_guards_only_moves_and_exempts_known_daemon_children() {
    let live = Live::with_deps(&["tool"], &[], &["tool"]);
    let id = live.serve(|_| live.new_session("legacy-deps", "tool"));
    let ws = live.ws(&id, "tool");
    std::fs::remove_file(live.home.socket()).unwrap();
    let daemon = FakeDaemon::start(&live.home.socket(), legacy);
    let _holder = Holder::new(&ws);
    let args = [
        "deps",
        "propose",
        "--session",
        id.as_str(),
        "--repo",
        "tool",
        "--switch",
    ];
    live.run(&args).ok(); // No drift: the old daemon would not move this workspace.
    assert_eq!(daemon.requests().len(), 2);
    std::fs::write(ws.join("package.json"), CHANGED).unwrap();
    assert_eq!(live.run(&args).reason(), "cwd_holders");
    assert_eq!(daemon.requests().len(), 3); // Refused before the legacy resend.
    let _lock = strata_daemon::startup::InstanceLock::take(&strata_daemon::lock_path(
        &live.home.config_dir(),
    ))
    .unwrap();
    live.run(&args).ok(); // The fake daemon's direct child has the same exemption.
    assert_eq!(daemon.requests().len(), 5);
}

#[test]
fn legacy_approval_ignores_cwd_holders_in_sessions_the_plan_skips() {
    let live = Live::with_deps(&["tool"], &[], &["tool"]);
    let (skipped, proposal) = live.serve(|core| {
        live.new_session("eligible", "tool");
        let skipped = live.new_session("drifted", "tool");
        std::fs::write(
            live.ws(&skipped, "tool").join("package.json"),
            r#"{"dependencies":{"z":"3"}}"#,
        )
        .unwrap();
        let repo = strata_types::RepoName::try_from("tool".to_string()).unwrap();
        let main = live.main("tool");
        std::fs::write(main.join("package.json"), CHANGED).unwrap();
        support::git(&main, live.home.path(), &["add", "package.json"]);
        support::git(&main, live.home.path(), &["commit", "-qm", "new deps"]);
        support::git(&main, live.home.path(), &["push", "-q", "origin", "main"]);
        let git = &core.gits[&repo];
        git.fetch("origin", "main").unwrap();
        let head = strata_types::CommitOid::try_from(support::git(
            &main,
            live.home.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap();
        let tree = git.tree_of(&head).unwrap();
        let ctx = strata_deps::Ctx {
            store: &core.store,
            paths: &core.config.paths,
            repo: &repo,
            cfg: &core.config.repos[&repo],
            git,
            env: &core.env,
            locks: &core.locks,
        };
        let proposal = strata_deps::observe_main(&ctx, &tree, &head, &main)
            .unwrap()
            .unwrap();
        (skipped, proposal.id.get().to_string())
    });
    std::fs::remove_file(live.home.socket()).unwrap();
    let daemon = FakeDaemon::start(&live.home.socket(), legacy);
    let _holder = Holder::new(&live.ws(&skipped, "tool"));
    live.home.run_tty(&["deps", "approve", &proposal]).ok();
    assert_eq!(
        daemon
            .requests()
            .iter()
            .filter(|q| q["method"] == "deps_approve")
            .count(),
        2
    );
}

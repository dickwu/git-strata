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

fn legacy_sync_resolution_repos(saved_report: bool) {
    use std::time::SystemTime;
    use strata_store::{MoveState, SyncReportRow};
    use strata_types::{RepoName, RepoPath};

    let live = Live::new(&["tool", "web"], &[("a.txt", b"base\n")]);
    let id = live.serve(|core| {
        let id = live.new_session("legacy-resolution", "tool,web");
        let rec = core.store.session(&id).unwrap().unwrap();
        for name in ["tool", "web"] {
            let repo = RepoName::try_from(name.to_string()).unwrap();
            let ws = &rec.manifest.repos[&repo];
            assert!(ws.tracked);
            let conflicts = [format!("{name}.txt"), "shared.txt".into()]
                .into_iter()
                .map(|p| RepoPath::try_from(p).unwrap())
                .collect::<Vec<_>>();
            if saved_report {
                let rows = conflicts
                    .into_iter()
                    .map(|path| SyncReportRow {
                        target: ws.head.clone(),
                        generation: rec.workspaces[&repo].generation,
                        path,
                        kind: "text".into(),
                        ours_blob: None,
                        hunks: json!([]),
                    })
                    .collect::<Vec<_>>();
                core.store
                    .put_sync_report(
                        &id,
                        &repo,
                        &ws.head,
                        rec.workspaces[&repo].generation,
                        &rows,
                    )
                    .unwrap();
                assert!(rec.workspaces[&repo].conflicts.is_empty());
            } else {
                // Persist the same kept-side conflicts as a completed legacy
                // move, without swapping directories in this RPC fixture.
                let at = SystemTime::now();
                let staging = ws
                    .path
                    .with_file_name(format!(".strata-tmp-move-{name}-fixture"));
                let n = core
                    .store
                    .begin_move(&id, &repo, &ws.base, &ws.head, &staging, at)
                    .unwrap();
                core.store
                    .set_move_state(
                        &id,
                        &repo,
                        n,
                        MoveState::Preparing,
                        MoveState::Swapping,
                        Some(&conflicts),
                        at,
                    )
                    .unwrap();
                core.store
                    .finish_move(&id, &repo, n, &conflicts, at)
                    .unwrap();
                assert!(core.store.sync_report(&id, &repo).unwrap().is_empty());
            }
        }
        id
    });
    std::fs::remove_file(live.home.socket()).unwrap();
    let daemon = FakeDaemon::start(&live.home.socket(), legacy);
    let cwd = live.ws(&id, "tool");
    for flag in ["--keep", "--resolved"] {
        let mut args = vec!["sync", "--session", id.as_str(), flag, "web.txt"];
        if saved_report {
            args.extend([
                "--target",
                "0123456789abcdef0123456789abcdef01234567",
                "--generation",
                "1",
            ]);
        }
        let before = daemon.requests().len();
        live.run_in(&cwd, &args).ok();
        let requests = daemon.requests();
        assert_eq!(requests.len(), before + 2, "other repo must resend");
        let mut expected = requests[before]["params"].clone();
        assert_eq!(expected["repos"], json!([]));
        for field in ["caller_pids", "allow_cwd", "allow_cwd_holders"] {
            assert!(expected.as_object_mut().unwrap().remove(field).is_some());
        }
        assert_eq!(requests[before + 1]["params"], expected);

        args[4] = "tool.txt";
        let run = live.run_in(&cwd, &args);
        assert_eq!(run.err_code(), "OpenFiles", "{run:?}");
        assert_eq!(run.reason(), "caller_cwd");
        assert_eq!(daemon.requests().len(), before + 3, "no refused resend");

        for path in ["shared.txt", "missing.txt"] {
            args[4] = path;
            let before = daemon.requests().len();
            let run = live.run_in(&cwd, &args);
            assert_eq!(run.err_code(), "InvalidInput", "{run:?}");
            assert_eq!(run.reason(), "ambiguous_resolution");
            assert_eq!(daemon.requests().len(), before + 1);
        }

        args[4] = "shared.txt";
        args.extend(["--repo", "web"]);
        let before = daemon.requests().len();
        live.run_in(&cwd, &args).ok();
        assert_eq!(daemon.requests().len(), before + 2);
    }
}

#[test]
fn legacy_sync_narrows_cwd_guard_to_workspace_conflicts() {
    legacy_sync_resolution_repos(false);
}

#[test]
fn legacy_sync_narrows_cwd_guard_to_saved_reports() {
    legacy_sync_resolution_repos(true);
}

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

#[path = "../support/mod.rs"]
mod support;
use super::*;
use support::*;

fn prepare(fx: &Dfx) -> Core {
    fx.seed_pins();
    let opts = crate::startup::Options {
        config_dir: fx.config_dir.clone(),
        config_file: fx.config_dir.join("config.toml"),
        socket: fx.socket(),
        lock_wait: fx.lock_wait,
        install_path: fx.sb.install_path(),
        alert: Arc::new(|_| {}),
        reconcile_every: fx.reconcile_every,
        catch_up_every: fx.catch_up_every,
        config_poll_every: std::time::Duration::from_millis(50),
        background_main_sync: false,
    };
    let env: Vec<(String, Option<std::ffi::OsString>)> = fx
        .sb
        .git_env()
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_os_string())))
        .collect();
    crate::startup::prepare(opts, fx.confirmer(vec![]).0, &move |p| {
        Git::open_with_env(
            p,
            &env.iter()
                .map(|(k, v)| (k.as_str(), v.as_deref()))
                .collect::<Vec<_>>(),
        )
    })
    .unwrap()
}

fn checkpoint(core: &Core, id: &SessionId, name: &str) {
    let repo = repo_name(name);
    let row = core.store.session(id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    let base = base_of(core, &ws.base).unwrap();
    let wref = WorkspaceRef {
        session: id,
        repo: &repo,
        root: &ws.path,
        config: &core.config.repos[&repo],
        git: &core.gits[&repo],
        store: &core.store,
    };
    strata_overlay::checkpoint(
        &wref,
        &base,
        None,
        strata_types::CheckpointReason::Manual {
            message: "test".into(),
        },
    )
    .unwrap();
}

fn setup(d: &Daemon<'_>, trackers: &Trackers<'_>) -> SessionId {
    let report = d.sessions.new(None, &[repo_name("tool")]).unwrap();
    let id = report.manifest.id.clone();
    checkpoint(d.core, &id, "tool");
    trackers.watch_tracked();
    // FSEvents is unavailable in some execution sandboxes. Model an active
    // watcher explicitly; events are delivered by Inbox/event_on_flush.
    guard(&trackers.map)
        .get_mut(&(id.clone(), repo_name("tool")))
        .unwrap()
        .watching = true;
    trackers.reconcile_once();
    id
}

#[test]
fn unpublished_true_when_tracker_reset_unscanned() {
    let fx = Dfx::new();
    let core = prepare(&fx);
    let _catalog = core.admit();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let id = setup(&d, &trackers);
    let repo = repo_name("tool");
    let row = core.store.session(&id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    let generation = row.workspaces[&repo].generation;
    assert!(!trackers.unpublished(&id, &repo, ws, generation, true));
    trackers.restart(&id, &repo);
    assert!(trackers.unpublished(&id, &repo, ws, generation, true));
    trackers.clear();
}

#[test]
fn unpublished_compares_generation_separately_and_checks_pending_inbox() {
    let fx = Dfx::new();
    let core = prepare(&fx);
    let _catalog = core.admit();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let id = setup(&d, &trackers);
    let repo = repo_name("tool");
    let row = core.store.session(&id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    let generation = row.workspaces[&repo].generation;
    assert!(trackers.unpublished(&id, &repo, ws, generation + 1, false));
    guard(&trackers.map)[&(id.clone(), repo.clone())]
        .tracker
        .inbox()
        .require_full();
    assert!(trackers.unpublished(&id, &repo, ws, generation, false));
    trackers.clear();
}

#[test]
fn hook_flushes_tracker_before_reading_inbox() {
    let fx = Dfx::new();
    let core = prepare(&fx);
    let _catalog = core.admit();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let id = setup(&d, &trackers);
    let repo = repo_name("tool");
    let row = core.store.session(&id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    assert!(!trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, false));
    guard(&trackers.map)[&(id.clone(), repo.clone())]
        .tracker
        .event_on_flush(strata_apfs::WatchEvent::Changed(vec![
            ws.path.join("changed.txt"),
        ]));
    assert!(!trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, false));
    assert!(trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, true));
    trackers.clear();
}

#[test]
fn unpublished_uses_reconciled_overlay_even_with_empty_checkpoint() {
    let fx = Dfx::new();
    let core = prepare(&fx);
    let _catalog = core.admit();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let id = setup(&d, &trackers);
    let repo = repo_name("tool");
    let row = core.store.session(&id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    assert!(!trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, false));
    fs::write(ws.path.join("changed.txt"), "new change").unwrap();
    trackers.reconcile_once();
    assert!(
        core.store
            .latest_checkpoint(&id, &repo)
            .unwrap()
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, false));
    trackers.clear();
}

#[test]
fn unpublished_false_after_checkpointed_edits_are_reverted() {
    let fx = Dfx::new();
    let core = prepare(&fx);
    let _catalog = core.admit();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let id = setup(&d, &trackers);
    let repo = repo_name("tool");
    let row = core.store.session(&id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    fs::write(ws.path.join("reverted.txt"), "checkpointed change").unwrap();
    checkpoint(&core, &id, "tool");
    assert!(
        !core
            .store
            .latest_checkpoint(&id, &repo)
            .unwrap()
            .unwrap()
            .entries
            .is_empty()
    );
    fs::remove_file(ws.path.join("reverted.txt")).unwrap();
    trackers.reconcile_once();
    assert!(!trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, false));
    // Historical content is still restorable, although it is no longer live work.
    assert!(
        !core
            .store
            .latest_checkpoint(&id, &repo)
            .unwrap()
            .unwrap()
            .entries
            .is_empty()
    );
    trackers.clear();
}

#[test]
fn unpublished_false_with_empty_overlay_and_checkpoint_on_old_base() {
    let fx = Dfx::new();
    let core = prepare(&fx);
    let _catalog = core.admit();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let id = setup(&d, &trackers);
    let repo = repo_name("tool");
    let old = core.store.session(&id).unwrap().unwrap();
    fs::write(
        old.manifest.repos[&repo].path.join("landed.txt"),
        "published content",
    )
    .unwrap();
    checkpoint(&core, &id, "tool");
    let cp = core.store.latest_checkpoint(&id, &repo).unwrap().unwrap();
    let server = Server::new(&d, &trackers);
    let method = crate::rpc::Method::Publish;
    let params = crate::rpc::Params::parse(
        method,
        json!({
            "group": group(vec![batch(&id, "tool", cp.id, &["landed.txt"])]), "shown": {}
        }),
    )
    .unwrap();
    let reply = server
        .handle(crate::rpc::Request {
            id: json!(1),
            method,
            params,
        })
        .0;
    assert_eq!(reply["ok"], true, "{reply}");
    let row = core.store.session(&id).unwrap().unwrap();
    let ws = &row.manifest.repos[&repo];
    let latest = core.store.latest_checkpoint(&id, &repo).unwrap().unwrap();
    assert_ne!(
        latest.base, ws.base,
        "the move leaves its checkpoint on the old base"
    );
    trackers.watch_tracked();
    guard(&trackers.map)
        .get_mut(&(id.clone(), repo.clone()))
        .unwrap()
        .watching = true;
    trackers.reconcile_once();
    assert!(!trackers.unpublished(&id, &repo, ws, row.workspaces[&repo].generation, false));
    let method = crate::rpc::Method::Sync;
    let params = crate::rpc::Params::parse(method, json!({"session": id, "check": true})).unwrap();
    let up = server
        .handle(crate::rpc::Request {
            id: json!(2),
            method,
            params,
        })
        .0;
    assert_eq!(up["ok"], true, "{up}");
    assert_eq!(up["data"]["upstream"]["repos"]["tool"]["ahead"], 0);
    assert_eq!(up["data"]["upstream"]["repos"]["tool"]["behind"], 0);
    assert_eq!(
        up["data"]["upstream"]["repos"]["tool"]["unpublished"],
        false
    );
    trackers.clear();
}

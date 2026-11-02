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
    };
    crate::startup::prepare(opts, fx.confirmer(vec![]).0, &|p| {
        Git::open_with_env(p, &fx.sb.git_env())
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

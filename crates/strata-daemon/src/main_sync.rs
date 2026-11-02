//! Background main-follow work. Installs and fetches have independent,
//! per-repo admission; neither blocks the T2 thread with network/build I/O.
use crate::{Result, startup::Core};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use strata_deps::{self as deps, ProposalState, Target};
use strata_publish::main_follow::{self, TempTree};
use strata_store::DepsAutoState;
use strata_types::{Code, ProposalId, RepoName, StrataError, SyncDeps};

#[derive(Default)]
pub struct State {
    pub(crate) fetching: Mutex<BTreeSet<RepoName>>,
    installing: Mutex<BTreeSet<RepoName>>,
}
pub(crate) struct Job<'a> {
    set: &'a Mutex<BTreeSet<RepoName>>,
    repo: RepoName,
}
impl<'a> Job<'a> {
    pub(crate) fn take(set: &'a Mutex<BTreeSet<RepoName>>, repo: &RepoName) -> Option<Self> {
        set.lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(repo.clone())
            .then(|| Self {
                set,
                repo: repo.clone(),
            })
    }
}
impl Drop for Job<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.repo);
    }
}

pub(crate) fn with_deps<T>(
    core: &Core,
    repo: &RepoName,
    f: impl FnOnce(&deps::Ctx) -> Result<T>,
) -> Result<T> {
    let mut cfg = core.repo_cfg(repo)?.clone();
    cfg.sync.deps = core.effective_sync_deps(repo);
    let ctx = deps::Ctx {
        store: &core.store,
        paths: &core.config.paths,
        repo,
        cfg: &cfg,
        git: core.git(repo)?,
        env: &core.env,
        locks: &core.locks,
    };
    f(&ctx)
}

/// Shared post-fetch hook; prepare before taking the non-reentrant lock.
pub fn reconcile(core: &Core, repo: &RepoName) -> Result<()> {
    with_deps(core, repo, main_follow::reconcile_deps)
}

pub fn deps_auto_pass(core: &Core, repo: &RepoName) -> Result<()> {
    deps_auto_pass_inner(core, repo, &mut |ctx, id, dir| {
        deps::ensure_target(ctx, id, dir)
    })
}

#[cfg(feature = "test-seams")]
pub fn deps_auto_pass_with(
    core: &Core,
    repo: &RepoName,
    ensure: &mut dyn FnMut(&deps::Ctx, ProposalId, &Path) -> Result<Target>,
) -> Result<()> {
    deps_auto_pass_inner(core, repo, ensure)
}

fn terminal(e: &StrataError) -> bool {
    e.code == Code::Unknown
        && matches!(
            e.detail.as_ref().and_then(|d| d["reason"].as_str()),
            Some(
                "install_failed"
                    | "timeout"
                    | "inputs_modified"
                    | "staging_failed"
                    | "previous_attempt"
            )
        )
}

// Caller holds the repo lock. T2 may have settled this job while the
// installer was returning; an Applied row is durable audit, not stale work.
fn delete_unapplied(core: &Core, id: ProposalId) -> Result<()> {
    if core
        .store
        .deps_auto(id)?
        .is_none_or(|a| a.state != DepsAutoState::Applied)
    {
        core.store.delete_deps_auto(id)?;
    }
    Ok(())
}

fn deps_auto_pass_inner(
    core: &Core,
    repo: &RepoName,
    ensure: &mut dyn FnMut(&deps::Ctx, ProposalId, &Path) -> Result<Target>,
) -> Result<()> {
    let Some(_job) = Job::take(&core.main_sync.installing, repo) else {
        return Ok(());
    };
    with_deps(core, repo, |ctx| {
        if ctx.cfg.sync.deps != SyncDeps::Apply {
            return Ok(());
        }
        // Discard obsolete jobs, including proposals superseded while an
        // install was in flight. Applied rows remain as the automatic audit.
        core.locks.with_repo(repo, || {
            for a in core.store.deps_auto_all(Some(repo))? {
                if a.state != DepsAutoState::Applied
                    && deps::get_proposal(&core.store, a.proposal)
                        .is_ok_and(|p| p.state != ProposalState::Pending)
                {
                    delete_unapplied(core, a.proposal)?;
                }
            }
            Ok(())
        })?;
        let pending: Vec<_> = deps::open_proposals(&core.store, Some(repo))?
            .into_iter()
            .filter(|p| p.state == ProposalState::Pending)
            .collect();
        // T4 runs on T2 even without a proposal. An idle installer has no
        // reason to duplicate that export and reconciliation work.
        if pending.is_empty() {
            return Ok(());
        }
        for p in pending {
            if deps::find_set(&core.store, repo, &p.to)?.is_some() {
                core.locks.with_repo(repo, || {
                    if deps::get_proposal(&core.store, p.id)?.state == ProposalState::Pending {
                        core.store.set_deps_auto(
                            p.id,
                            DepsAutoState::Ready,
                            None,
                            SystemTime::now(),
                        )?;
                    }
                    Ok(())
                })?;
                continue;
            }
            if core.store.deps_auto(p.id)?.is_some_and(|a| {
                matches!(
                    a.state,
                    DepsAutoState::InstallFailed | DepsAutoState::Applied
                )
            }) {
                continue;
            }
            let admitted = core.locks.with_repo(repo, || {
                if deps::get_proposal(&core.store, p.id)?.state != ProposalState::Pending {
                    delete_unapplied(core, p.id)?;
                    return Ok(false);
                }
                if deps::find_set(&core.store, repo, &p.to)?.is_some() {
                    core.store.set_deps_auto(
                        p.id,
                        DepsAutoState::Ready,
                        None,
                        SystemTime::now(),
                    )?;
                    return Ok(false);
                }
                if core.store.deps_auto(p.id)?.is_some_and(|a| {
                    matches!(
                        a.state,
                        DepsAutoState::InstallFailed | DepsAutoState::Applied
                    )
                }) {
                    return Ok(false);
                }
                core.store.set_deps_auto(
                    p.id,
                    DepsAutoState::Installing,
                    None,
                    SystemTime::now(),
                )?;
                Ok(true)
            })?;
            if !admitted {
                continue;
            }
            let outcome = (|| {
                let tree = p.tree.as_ref().ok_or_else(|| {
                    StrataError::new(Code::InvalidInput, "pending proposal has no tree")
                })?;
                let tmp = TempTree::new(&core.config.paths.root)?;
                ctx.git.export_tree(tree, &tmp.path())?;
                ensure(ctx, p.id, &tmp.path())
            })();
            core.locks.with_repo(repo, || {
                // ensure_target may seal after a newer main observation.
                if deps::get_proposal(&core.store,p.id)?.state != ProposalState::Pending {
                    delete_unapplied(core, p.id)?;
                    return Ok(());
                }
                match &outcome {
                    Ok(Target::Ready{..}) => core.store.set_deps_auto(p.id,DepsAutoState::Ready,None,SystemTime::now())?,
                    Ok(Target::Replaced{..}) => { delete_unapplied(core, p.id)?; }
                    Err(e) if e.code==Code::NoSuchProposal => { delete_unapplied(core, p.id)?; }
                    Err(e) if terminal(e) => core.store.set_deps_auto(p.id,DepsAutoState::InstallFailed,Some(&serde_json::to_value(e).unwrap()),SystemTime::now())?,
                    Err(e) => {
                        delete_unapplied(core, p.id)?;
                        tracing::debug!(repo=%repo,error=%e.message,"automatic deps preparation will retry");
                    }
                }
                Ok(())
            })?;
        }
        // Sealing can recover a prior terminal row; T4 enrolls residue now.
        main_follow::reconcile_deps(ctx)?;
        core.locks.with_repo(repo, || main_follow::settle_auto(ctx))
    })
}

pub(crate) fn run_loop(core: &Core, repo: &RepoName, install: bool, stop: &dyn Fn() -> bool) {
    #[cfg(feature = "test-seams")]
    if !core.opts.background_main_sync {
        return;
    }
    let every = if install {
        core.opts.catch_up_every
    } else {
        Duration::from_secs(1)
    };
    while !stop() {
        let started = Instant::now();
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if install {
                deps_auto_pass(core, repo)
            } else {
                crate::periodic_fetch::fetch_due(core, repo, SystemTime::now()).map(|_| ())
            }
        }));
        match run {
            Ok(Err(e)) if e.code != Code::ReadOnly => {
                tracing::warn!(repo=%repo,error=%e.message,"main-sync background pass failed")
            }
            Err(_) => tracing::warn!(repo=%repo,"main-sync background pass panicked"),
            _ => {}
        }
        while !stop() && started.elapsed() < every {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

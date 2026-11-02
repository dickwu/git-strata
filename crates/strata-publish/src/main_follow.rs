//! Main-sync T4: prepare immutable input keys without locks, then reconcile
//! against a re-read tip under the caller's repo lock. Locked helpers never
//! acquire that lock (including observation and settlement).

use crate::{Ctx, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use strata_deps::{self as deps, ProposalState};
use strata_store::{DepsAutoState, Health, Lifecycle, MoveState, PendingSource};
use strata_types::{Code, CommitOid, DepsSetId, StrataError, SyncDeps, TreeOid};

static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);

/// A private temporary export under daemon state, removed on every exit.
/// The parent is exclusively created before any export; cleanup owns only it.
pub struct TempTree {
    root: PathBuf,
}
impl TempTree {
    pub fn new(root: &Path) -> Result<Self> {
        let parent = root.join("daemon/main-follow");
        fs::create_dir_all(&parent).map_err(|e| io(e, &parent))?;
        loop {
            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let root = parent.join(format!(
                "{}-{stamp}-{}",
                std::process::id(),
                NEXT_EXPORT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self { root }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io(e, &root)),
            }
        }
    }
    pub fn path(&self) -> PathBuf {
        self.root.join("tree")
    }
    pub fn export(ctx: &deps::Ctx, tree: &TreeOid) -> Result<Self> {
        let tmp = Self::new(&ctx.paths.root)?;
        // Use checkout-direction conversions exactly as ensure_base/ensure_target.
        // gitio currently exposes only full-tree canonical export; raw blobs
        // would produce a different key for CRLF, filters, or encodings.
        ctx.git.export_tree(tree, &tmp.path())?;
        Ok(tmp)
    }
}
impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn io(e: std::io::Error, p: &Path) -> StrataError {
    strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Write, p)
}

#[derive(Clone, Debug)]
pub struct PreparedMain {
    pub head: CommitOid,
    pub tree: TreeOid,
    pub inputs: String,
    pub deps: DepsSetId,
}

pub fn prepare_at(ctx: &deps::Ctx, head: &CommitOid) -> Result<PreparedMain> {
    let tree = ctx.git.tree_of(head)?;
    let export = TempTree::export(ctx, &tree)?;
    let (inputs, deps) = deps::key_of(ctx, &export.path())?;
    Ok(PreparedMain {
        head: head.clone(),
        tree,
        inputs,
        deps,
    })
}

pub fn prepare(ctx: &deps::Ctx) -> Result<Option<PreparedMain>> {
    if !ctx.cfg.follow_main() || ctx.cfg.publish.branch != ctx.cfg.default_branch {
        return Ok(None);
    }
    deps::main_tip(ctx.cfg, ctx.git)?
        .map(|h| prepare_at(ctx, &h))
        .transpose()
}

pub fn reconcile(ctx: &Ctx) -> Result<()> {
    reconcile_deps(ctx.deps)
}
pub fn reconcile_deps(ctx: &deps::Ctx) -> Result<()> {
    let Some(p) = prepare(ctx)? else {
        return Ok(());
    };
    ctx.locks
        .with_write(ctx.repo, &mut || reconcile_locked(ctx, &p))??;
    Ok(())
}

/// A sealed set recovers a failed automatic attempt, regardless of who sealed it.
pub fn refresh_ready(ctx: &deps::Ctx) -> Result<()> {
    if ctx.cfg.sync.deps != SyncDeps::Apply {
        return Ok(());
    }
    for p in deps::open_proposals(ctx.store, Some(ctx.repo))?
        .into_iter()
        .filter(|p| p.state == ProposalState::Pending)
    {
        if deps::find_set(ctx.store, ctx.repo, &p.to)?.is_some()
            && ctx
                .store
                .deps_auto(p.id)?
                .is_none_or(|a| a.state != DepsAutoState::Ready)
        {
            ctx.store
                .set_deps_auto(p.id, DepsAutoState::Ready, None, SystemTime::now())?;
        }
    }
    Ok(())
}

pub fn install_failed(ctx: &deps::Ctx, target: &DepsSetId) -> Result<bool> {
    if deps::find_set(ctx.store, ctx.repo, target)?.is_some() {
        return Ok(false);
    }
    for p in deps::open_proposals(ctx.store, Some(ctx.repo))? {
        if p.state == ProposalState::Pending
            && p.to == *target
            && ctx
                .store
                .deps_auto(p.id)?
                .is_some_and(|a| a.state == DepsAutoState::InstallFailed)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `resolve_move` retains the scene JSON as history. A terminal latest
/// intent proves it was resolved; a later refusal did not create a new scene.
/// An unresolved scene still blocks Auto.
pub(crate) fn workspace_has_scene(
    store: &strata_store::Store,
    session: &strata_types::SessionId,
    repo: &strata_types::RepoName,
    scene: bool,
) -> Result<bool> {
    if !scene {
        return Ok(false);
    }
    Ok(!store
        .move_intents(session)?
        .iter()
        .filter(|m| m.repo == *repo)
        .max_by_key(|m| m.n)
        .is_some_and(|m| {
            matches!(
                m.state,
                MoveState::Done | MoveState::Abandoned | MoveState::Refused
            )
        }))
}

/// Caller holds the repo lock. No helper on this path takes it again.
pub fn reconcile_locked(ctx: &deps::Ctx, p: &PreparedMain) -> Result<()> {
    if !ctx.cfg.follow_main()
        || ctx.cfg.publish.branch != ctx.cfg.default_branch
        || deps::main_tip(ctx.cfg, ctx.git)?.as_ref() != Some(&p.head)
    {
        return Ok(());
    }
    // Once per pass, so the key includes fresh config/toolchain identity and is
    // shared by every workspace on this tip. Refresh also repairs ObserveFailed.
    deps::observe_main_locked(ctx, &p.tree, &p.head, &p.inputs, &p.deps)?;
    refresh_ready(ctx)?;
    let apply = ctx.cfg.sync.deps == SyncDeps::Apply;
    let failed = install_failed(ctx, &p.deps)?;
    for rec in ctx.store.sessions()? {
        let id = &rec.manifest.id;
        let (Some(ws), Some(state)) = (
            rec.manifest.repos.get(ctx.repo),
            rec.workspaces.get(ctx.repo),
        ) else {
            continue;
        };
        if rec.lifecycle != Lifecycle::Active
            || !ws.tracked
            || state.health != Health::Ok
            || rec.scene.is_some()
            || workspace_has_scene(ctx.store, id, ctx.repo, state.scene.is_some())?
            || ctx.store.move_intents(id)?.iter().any(|m| {
                m.repo == *ctx.repo && matches!(m.state, MoveState::Preparing | MoveState::Swapping)
            })
        {
            continue;
        }
        if let Some(row) = ctx.store.rebase_pending_of(id, ctx.repo)? {
            if row.to_deps.is_some()
                && row.head.as_deref() == Some(p.head.as_str())
                && ws.head == p.head
            {
                if !apply
                    || ws.base.deps == p.deps
                    || failed
                    || deps::classify_rollout(ctx, id, ws, &p.inputs)?.is_some()
                {
                    ctx.store.drop_pending(id, ctx.repo, row.rev)?;
                } else {
                    ensure_owner(ctx, &ws.base, p)?;
                    if row.to_deps.as_ref() != Some(&p.deps) {
                        ctx.store.set_pending_to_deps(
                            id,
                            ctx.repo,
                            row.rev,
                            Some(p.deps.clone()),
                            SystemTime::now(),
                        )?;
                    }
                }
            } else if row.source != PendingSource::Replay
                && !row.unpushed
                && row.reason != "publish_unfinished"
                && let Some(head) = row
                    .head
                    .as_ref()
                    .and_then(|h| CommitOid::try_from(h.clone()).ok())
                && head != p.head
                && ctx.git.is_ancestor(&head, &p.head)?
            {
                let keep = matches!(
                    row.reason.as_str(),
                    "open_files" | "recent_writes" | "conflicts"
                );
                ctx.store.retarget_pending(
                    id,
                    ctx.repo,
                    row.rev,
                    &p.head,
                    &p.tree,
                    &ws.base,
                    keep,
                    SystemTime::now(),
                )?;
            }
            continue;
        }
        let to_deps = if ws.head == p.head {
            if !apply
                || ws.base.deps == p.deps
                || failed
                || deps::classify_rollout(ctx, id, ws, &p.inputs)?.is_some()
            {
                continue;
            }
            ensure_owner(ctx, &ws.base, p)?;
            Some(p.deps.clone())
        } else if ctx.git.is_ancestor(&ws.head, &p.head)? {
            None
        } else {
            continue;
        };
        ctx.store.enroll_main(
            id,
            ctx.repo,
            &ws.base,
            &p.head,
            &p.tree,
            to_deps,
            SystemTime::now(),
        )?;
    }
    settle_auto(ctx)
}

// An observation with an already-current deps_main (or no previous record)
// need not create a Pending. Residue still needs an owner before its row exists.
fn ensure_owner(ctx: &deps::Ctx, from: &strata_types::BaseGen, p: &PreparedMain) -> Result<()> {
    if deps::find_set(ctx.store, ctx.repo, &p.deps)?.is_some()
        || deps::open_proposals(ctx.store, Some(ctx.repo))?
            .iter()
            .any(|q| q.state == ProposalState::Pending && q.to == p.deps)
    {
        return Ok(());
    }
    let pinned = deps::find_set(ctx.store, ctx.repo, &from.deps)?.ok_or_else(|| {
        StrataError::new(Code::Corrupt, "residue workspace has no sealed pinned set")
    })?;
    deps::propose(
        ctx,
        deps::ProposeArgs {
            origin: deps::ProposalOrigin::Main,
            from: from.deps.clone(),
            from_inputs: pinned.inputs_hash,
            from_head: None,
            to: p.deps.clone(),
            to_inputs: p.inputs.clone(),
            tree: Some(p.tree.clone()),
            head: Some(p.head.clone()),
        },
    )?;
    Ok(())
}

/// Caller holds the repo lock. Unknown/Creating/scene workspaces count;
/// only Removed sessions and untracked workspaces are excluded.
pub fn settle_auto(ctx: &deps::Ctx) -> Result<()> {
    if ctx.cfg.sync.deps != SyncDeps::Apply {
        return Ok(());
    }
    refresh_ready(ctx)?;
    let sessions = ctx.store.sessions()?;
    for p in deps::open_proposals(ctx.store, Some(ctx.repo))?
        .into_iter()
        .filter(|p| p.state == ProposalState::Pending)
    {
        if deps::find_set(ctx.store, ctx.repo, &p.to)?.is_some()
            && !sessions.iter().any(|s| {
                s.lifecycle != Lifecycle::Removed
                    && s.manifest
                        .repos
                        .get(ctx.repo)
                        .is_some_and(|w| w.tracked && w.base.deps != p.to)
            })
        {
            ctx.store.auto_applied(p.id, SystemTime::now())?;
        }
    }
    Ok(())
}

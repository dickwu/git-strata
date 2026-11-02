//! Agent-requested main sync. Builds/ensures precede queue -> repo lock;
//! the only workspace swap runs under `with_queue_not_publishing`.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_deps::{Ctx as DepsCtx, SkipReason};
use strata_overlay::{Base, Overlay, PreviewBlobs, RebaseOptions, Resolution, WorkspaceRef};
use strata_store::{DepsAutoState, Health, Lifecycle, MoveState, SessionRecord, SyncReportRow};
use strata_types::{
    BaseGen, Code, CommitOid, RepoName, RepoPath, SessionId, StrataError, SyncDeps, SyncRequest,
    WorkspaceState,
};

use crate::Result;
use crate::startup::{Core, Daemon};

fn error(code: Code, reason: &str, message: &str) -> StrataError {
    StrataError::new(code, message).with_detail(json!({"reason":reason}))
}
fn busy(reason: &str, detail: Value) -> StrataError {
    StrataError::new(Code::Busy, "workspace changed or is busy; retry sync")
        .with_detail(json!({"reason":"auto_deferred", "defer":{"reason":reason,"detail":detail}}))
}
fn record(core: &Core, s: &SessionId) -> Result<SessionRecord> {
    core.store
        .session(s)?
        .filter(|r| r.lifecycle != Lifecycle::Removed)
        .ok_or_else(|| error(Code::NoSuchSession, "no_such_session", "no such session"))
}
fn workspace(rec: &SessionRecord, repo: &RepoName) -> Result<WorkspaceState> {
    let ws = rec
        .manifest
        .repos
        .get(repo)
        .ok_or_else(|| StrataError::not_in_session(rec.manifest.id.as_str(), repo.as_str()))?;
    if !ws.tracked {
        return Err(error(
            Code::InvalidInput,
            "not_tracked",
            "sync requires a tracked workspace",
        ));
    }
    Ok(ws.clone())
}
fn g0(core: &Core, s: &SessionId, repo: &RepoName, rec: &SessionRecord) -> Result<()> {
    let row = rec
        .workspaces
        .get(repo)
        .ok_or_else(|| error(Code::NoSuchRepo, "not_tracked", "workspace is missing"))?;
    let open =
        core.store.move_intents(s)?.iter().any(|i| {
            &i.repo == repo && matches!(i.state, MoveState::Preparing | MoveState::Swapping)
        });
    if rec.lifecycle != Lifecycle::Active
        || rec.scene.is_some()
        || core.store.workspace_scene_live(s, repo, row.scene.is_some())?
        || row.health != Health::Ok
        || open
    {
        return Err(busy(
            "state",
            json!({"open_move":open,"lifecycle":format!("{:?}",rec.lifecycle),"health":format!("{:?}",row.health)}),
        ));
    }
    Ok(())
}
fn deps_ctx<'a>(core: &'a Core, repo: &'a RepoName) -> Result<DepsCtx<'a>> {
    Ok(DepsCtx {
        store: &core.store,
        paths: &core.config.paths,
        repo,
        cfg: core.repo_cfg(repo)?,
        git: core.git(repo)?,
        env: &core.env,
        locks: &core.locks,
    })
}
fn base(core: &Core, g: &BaseGen) -> Result<Base> {
    let manifest = core.store.manifest(&g.repo, &g.tree)?.ok_or_else(|| {
        error(
            Code::Corrupt,
            "manifest_missing",
            "base manifest is missing",
        )
    })?;
    Ok(Base {
        r#gen: g.clone(),
        manifest: Arc::new(manifest),
        dir: core.store.base_dir(g),
    })
}
fn scan(
    core: &Core,
    s: &SessionId,
    repo: &RepoName,
    ws: &WorkspaceState,
    old: &Base,
) -> Result<Overlay> {
    let wr = WorkspaceRef {
        session: s,
        repo,
        root: &ws.path,
        config: core.repo_cfg(repo)?,
        git: core.git(repo)?,
        store: &core.store,
    };
    strata_overlay::reconcile_full(&wr, old)
}

/// Non-check handler; the upstream lane dispatches `check` before this call.
pub fn run(d: &Daemon<'_>, req: SyncRequest) -> Result<Value> {
    if req.check {
        return Err(error(
            Code::Unsupported,
            "sync_check_unavailable",
            "the upstream check handler is not integrated",
        ));
    }
    let core = d.core;
    let rec = record(core, &req.session)?;
    let mut repos: Vec<_> = req.repos.clone().unwrap_or_else(|| {
        rec.manifest
            .repos
            .iter()
            .filter(|(_, w)| w.tracked)
            .map(|(r, _)| r.clone())
            .collect()
    });
    repos.sort();
    repos.dedup();
    for r in &repos {
        core.repo_cfg(r)?;
        workspace(&rec, r)?;
    }
    let named: BTreeSet<_> = req.resolved.iter().chain(&req.keep).cloned().collect();
    if req.resolved.iter().any(|p| req.keep.contains(p)) {
        return Err(error(
            Code::InvalidInput,
            "resolved_and_keep",
            "a path cannot be both resolved and kept",
        ));
    }
    // Without --repo, a saved report (or legacy conflicts) can identify a
    // unique repository. Never acknowledge an unrelated repo implicitly.
    if !named.is_empty() && repos.len() > 1 {
        let mut candidates = Vec::new();
        for r in &repos {
            let report = core.store.sync_report(&req.session, r)?;
            if report.iter().any(|row| named.contains(&row.path))
                || rec.workspaces[r]
                    .conflicts
                    .iter()
                    .any(|p| named.contains(p))
            {
                candidates.push(r.clone());
            }
        }
        if candidates.len() != 1 {
            return Err(error(
                Code::InvalidInput,
                "ambiguous_resolution",
                "use --repo to identify the conflict report",
            ));
        }
        repos = candidates;
    }
    let mut fetched = BTreeMap::new();
    crate::handlers::writing();
    for repo in &repos {
        let cfg = core.repo_cfg(repo)?;
        if !cfg.follow_main() {
            continue;
        }
        if req.fetch {
            let result = core.locks.with_repo(repo, || {
                let r = core.git(repo)?.fetch("origin", &cfg.default_branch);
                let at = SystemTime::now();
                core.store.record_fetch(repo,r.is_ok(),r.as_ref().err().map(|e|e.message.as_str()),at)?;
                if r.is_ok() { crate::fetch_after::advance_anchor_locked(core.git(repo)?,&cfg.default_branch)?; }
                Ok(json!({"ok":r.is_ok(),"at":strata_types::serde_rfc3339::format(&at).unwrap_or_default(),"error":r.err()}))
            })?;
            fetched.insert(repo.clone(), result);
        }
        // T4 (observe + reconcile every eligible workspace) even when the
        // fetch did not move the tip; it takes the repo lock itself.
        crate::main_sync::reconcile(core, repo)?;
    }
    let mut done = BTreeMap::new();
    for repo in &repos {
        let r = if core.repo_cfg(repo)?.follow_main() {
            sync_one(d, &req, repo, &named)
        } else if !named.is_empty() {
            acknowledge_unfollowed(d, &req, repo, &named)
        } else {
            Ok(json!({"result":"not_following"}))
        };
        match r {
            Ok(v) => {
                done.insert(repo.clone(), v);
            }
            Err(mut e) => {
                if let Some(Value::Object(detail)) = &mut e.detail {
                    detail.insert("done".into(), json!(done));
                }
                return Err(e);
            }
        }
    }
    // The dispatcher adds `upstream` (best effort) after the tracker restart.
    Ok(
        json!({"session":req.session,"fetched":fetched,"repos":done,"next":"re-run your checks, checkpoint, then git-strata publish"}),
    )
}

struct Export(PathBuf);
impl Drop for Export {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn export(core: &Core, repo: &RepoName, tree: &strata_types::TreeOid) -> Result<Export> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let parent = core.config.paths.root.join("daemon/sync");
    std::fs::create_dir_all(&parent)
        .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Write, &parent))?;
    let dir = parent.join(format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let out = Export(dir);
    core.git(repo)?.export_tree(tree, &out.0)?;
    Ok(out)
}

fn divergence(
    core: &Core,
    repo: &RepoName,
    ws: &WorkspaceState,
    tip: &CommitOid,
    accept: bool,
) -> Result<Vec<Value>> {
    let git = core.git(repo)?;
    if git.is_ancestor(&ws.head, tip)? || git.is_ancestor(tip, &ws.head)? {
        return Ok(Vec::new());
    }
    let (commits, more) = git.commits_between(tip, &ws.head, 20)?;
    let commits: Vec<_> = commits
        .into_iter()
        .map(|(head, subject)| json!({"head":head,"subject":subject}))
        .collect();
    if !accept {
        return Err(StrataError::new(Code::Conflict,"the session base diverged from main; recover the chain first")
            .with_detail(json!({"reason":"base_diverged","repo":repo,"base":ws.head,"target":tip,"commits":commits,"more":more})));
    }
    // Propagate read errors: uncertainty about reachability cannot grant
    // permission to discard base-only commits.
    for reference in [
        strata_publish::chain_ref(&core.repo_cfg(repo)?.publish.branch),
        strata_gitio::remote_ref("origin", &core.repo_cfg(repo)?.publish.branch),
    ] {
        if let Some(head) = git.read_ref(&reference)?
            && git.is_ancestor(&ws.head, &head)?
        {
            return Err(error(
                Code::InvalidInput,
                "diverged_still_on_chain",
                "drop the diverged chain before accepting the old base",
            ));
        }
    }
    Ok(commits)
}

fn source_reason(
    ctx: &DepsCtx<'_>,
    s: &SessionId,
    ws: &WorkspaceState,
    inputs: &str,
    mode: SyncDeps,
) -> Result<Option<Value>> {
    let skip = strata_deps::classify_rollout(ctx, s, ws, inputs)?;
    Ok(match skip {
        Some(SkipReason::OwnChange { proposal }) => {
            Some(json!({"state":"own_change","proposal":proposal}))
        }
        Some(SkipReason::Drift { drift }) => Some(json!({"state":"drift","drift":drift})),
        Some(_) => unreachable!("input classifier returns only own-change or drift"),
        None if mode == SyncDeps::Confirm => Some(json!({"state":"waiting_human"})),
        None => None,
    })
}

/// D-b for a repo that does not follow main: O7 still refuses publishing its
/// recorded conflicts, so `--resolved`/`--keep` acknowledge them here without
/// any tip, report or move (there is no sync report for such a repo).
fn acknowledge_unfollowed(
    d: &Daemon<'_>,
    req: &SyncRequest,
    repo: &RepoName,
    named: &BTreeSet<RepoPath>,
) -> Result<Value> {
    let core = d.core;
    crate::resolve::with_queue_not_publishing(core, &req.session, || {
        core.locks.with_repo(repo, || {
            let now = record(core, &req.session)?;
            workspace(&now, repo)?;
            g0(core, &req.session, repo, &now)?;
            core.store
                .clear_conflicts(&req.session, repo, &named.iter().cloned().collect::<Vec<_>>())?;
            Ok(json!({"result":"conflicts_cleared","following":false,"resolved":req.resolved,"kept":req.keep}))
        })
    })
}

fn sync_one(
    d: &Daemon<'_>,
    req: &SyncRequest,
    repo: &RepoName,
    named: &BTreeSet<RepoPath>,
) -> Result<Value> {
    let core = d.core;
    let cfg = core.repo_cfg(repo)?;
    let git = core.git(repo)?;
    // Ref/base movement during lock-free preparation restarts preparation;
    // report binding is checked only against the final, locked snapshot.
    for _ in 0..3 {
        let rec = record(core, &req.session)?;
        let ws = workspace(&rec, repo)?;
        g0(core, &req.session, repo, &rec)?;
        let tip = strata_deps::main_tip(cfg, git)?
            .ok_or_else(|| error(Code::NoSuchCommit, "main_missing", "main has no tip"))?;
        let dropped = divergence(core, repo, &ws, &tip, req.accept_diverged)?;
        let ahead = git.is_ancestor(&tip, &ws.head)?;
        let head = if ahead { ws.head.clone() } else { tip.clone() };
        let tree = git.tree_of(&head)?;
        let ctx = deps_ctx(core, repo)?;
        let dir = export(core, repo, &tree)?;
        let (inputs, n) = strata_deps::key_of(&ctx, &dir.0)?;
        let mode = core.effective_sync_deps(repo);
        let reason = if n != ws.base.deps {
            source_reason(&ctx, &req.session, &ws, &inputs, mode)?
        } else {
            None
        };
        let proposal = strata_deps::open_proposals(&core.store, Some(repo))?
            .into_iter()
            .find(|p| p.state == strata_deps::ProposalState::Pending && p.to == n);
        if n != ws.base.deps
            && reason.is_none()
            && strata_deps::find_set(&core.store, repo, &n)?.is_none()
        {
            if let Some(p) = &proposal
                && core
                    .store
                    .deps_auto(p.id)?
                    .is_some_and(|a| a.state == DepsAutoState::Installing)
            {
                return Ok(
                    json!({"result":"waiting","deps":{"state":"installing","proposal":p.id}}),
                );
            }
            // I4: this may wait for the build lock and run a cold install.
            // No session queue or repo lock is held here.
            strata_deps::ensure(&ctx, &dir.0)?;
        }
        let deps = if reason.is_some() { &ws.base.deps } else { &n };
        let target = d.sessions.ensure_base_for(repo, &tree, deps)?;
        let result = crate::resolve::with_queue_not_publishing(core, &req.session, || {
            core.locks.with_repo(repo,|| {
            let now = record(core,&req.session)?;
            let current = workspace(&now,repo)?;
            if current.base != ws.base || current.head != ws.head || now.workspaces[repo].generation != rec.workspaces[repo].generation
                || strata_deps::main_tip(cfg,git)?.as_ref() != Some(&tip) {
                return Err(error(Code::Busy,"sync_retry","main or workspace moved during preparation"));
            }
            g0(core,&req.session,repo,&now)?;
            divergence(core,repo,&current,&tip,req.accept_diverged)?;
            let activity = strata_session::walk_activity(&current.path)?;
            if activity.late.is_some() { return Err(busy("recent_writes",json!({"path":activity.late}))); }
            // Recheck input ownership after the lock-free install gap.
            let current_reason = if n != current.base.deps { source_reason(&ctx,&req.session,&current,&inputs,core.effective_sync_deps(repo))? } else { None };
            if current_reason != reason { return Err(error(Code::Busy,"sync_retry","dependency ownership changed during preparation")); }
            let old = base(core,&current.base)?;
            let new = base(core,&target)?;
            let overlay = scan(core,&req.session,repo,&current,&old)?;
            let baseline = strata_session::baseline_of(&activity);
            let report = core.store.sync_report(&req.session,repo)?;
            let generation = now.workspaces[repo].generation;
            if target == current.base && head == current.head && report.is_empty() && !named.is_empty()
                && !now.workspaces[repo].conflicts.is_empty() {
                core.store.clear_conflicts(&req.session,repo,&named.iter().cloned().collect::<Vec<_>>())?;
                return Ok(json!({"result":"conflicts_cleared","resolved":req.resolved,"kept":req.keep}));
            }
            if !named.is_empty() && (req.target.is_none() || req.generation.is_none()) {
                return Err(error(Code::InvalidInput,"resolution_binding_required","--resolved and --keep require --target and --generation"));
            }
            let mut opts = RebaseOptions::for_move(&new,git.read_index(&current.path)?.ignorecase)?;
            if !named.is_empty() {
                if report.is_empty() || req.target.as_ref() != Some(&head) || req.generation != Some(generation)
                    || report.iter().any(|r|r.target != head || r.generation != generation) {
                    return Err(conflict_report(core,req,repo,&current,&old,&new,&overlay,&opts,&head,generation,Code::HeadMoved)?);
                }
                for p in named {
                    let row = report.iter().find(|r|&r.path == p).ok_or_else(|| error(Code::InvalidInput,"resolved_unrelated","path was not in the saved conflict report"))?;
                    let ours = strata_overlay::overlay_side(&overlay,&old,p).map(|s|s.0);
                    if !req.keep.contains(p) && ours == row.ours_blob {
                        return Err(error(Code::InvalidInput,"resolved_unchanged","edit the conflict or explicitly use --keep"));
                    }
                    let resolution = if row.kind == "text" {
                        Resolution::Text(serde_json::from_value(row.hunks.clone()).map_err(|_|error(Code::Corrupt,"sync_report","invalid saved conflict regions"))?)
                    } else { Resolution::WholeFile };
                    opts.resolutions.insert(p.clone(),resolution);
                }
            }
            let blobs = PreviewBlobs::new(&core.store,&current.path,&overlay);
            let preview = strata_overlay::rebase_preview(&overlay,&old,&new,&blobs,&opts).map_err(scan_error)?;
            let patterns = d.sessions.regenerable(repo);
            let regen = strata_session::Regenerable::new(&patterns,cfg);
            let (kept,blocking): (Vec<_>,Vec<_>) = preview.conflicts.iter().cloned().partition(|p|overlay.get(p).is_some_and(|e|regen.matches(e)));
            if !blocking.is_empty() {
                return Err(conflict_report(core,req,repo,&current,&old,&new,&overlay,&opts,&head,generation,Code::Conflict)?);
            }
            let changed = strata_session::changed_since(&current.path,&baseline)?;
            if !changed.is_empty() { return Err(busy("changed_since_scan",json!({"paths":changed}))); }
            if target == current.base && head == current.head {
                let mut v = json!({"result":"up_to_date"});
                if let Some(r) = &reason { v["result"] = json!("waiting"); v["deps"] = r.clone(); }
                return Ok(v);
            }
            let consume = core.store.rebase_pending_of(&req.session,repo)?.map(|r|r.rev);
            let moved = d.sessions.move_workspace_agent_resolving(&req.session,&git.repo_id(),target.clone(),head.clone(),consume,&baseline,&opts,&req.keep)?;
            let whole_file: Vec<_> = opts.resolutions.iter().filter(|(_,r)|matches!(r,Resolution::WholeFile)).map(|(p,_)|p).collect();
            let mut out = json!({"result":if reason.is_some(){"source_only"}else{"moved"},
                "from":{"head":moved.from_head,"deps":moved.from.deps},"to":{"head":head,"deps":target.deps},
                "resolved":req.resolved,"kept":req.keep,"whole_file":whole_file,"lww":moved.lww,"regenerable_kept":kept,
                "old_tree":moved.old_tree});
            if !dropped.is_empty() { out["accepted_diverged"] = json!(dropped); }
            if let Some(mut state) = reason.clone() {
                if let Some(p) = &proposal { state["proposal"] = json!(p.id); if state["state"] == "waiting_human" { state["fix"] = json!(format!("ask the human: git-strata deps approve {}",p.id)); } }
                out["deps"] = state;
            }
            Ok(out)
        })
        });
        match result {
            Err(e)
                if e.detail
                    .as_ref()
                    .is_some_and(|d| d["reason"] == "sync_retry") =>
            {
                continue;
            }
            Err(e)
                if e.code == Code::Busy
                    && e.detail
                        .as_ref()
                        .is_some_and(|d| d["reason"] == "publishing") =>
            {
                return Ok(json!({"result":"waiting","reason":"publishing"}));
            }
            r => return r,
        }
    }
    Err(busy(
        "recent_writes",
        json!({"reason":"main_or_workspace_moving"}),
    ))
}

fn scan_error(e: StrataError) -> StrataError {
    if e.detail
        .as_ref()
        .is_some_and(|d| d["reason"] == strata_overlay::CHANGED_SINCE_SCAN)
    {
        busy("changed_since_scan", e.detail.unwrap_or(Value::Null))
    } else {
        e
    }
}

#[allow(clippy::too_many_arguments)]
fn conflict_report(
    core: &Core,
    req: &SyncRequest,
    repo: &RepoName,
    ws: &WorkspaceState,
    old: &Base,
    new: &Base,
    overlay: &Overlay,
    opts: &RebaseOptions,
    head: &CommitOid,
    generation: u64,
    code: Code,
) -> Result<StrataError> {
    // HeadMoved regenerates an ordinary report against the current target;
    // previous resolution choices do not authorize the new target.
    let mut plain = opts.clone();
    plain.resolutions.clear();
    let blobs = PreviewBlobs::new(&core.store, &ws.path, overlay);
    let preview =
        strata_overlay::rebase_preview(overlay, old, new, &blobs, &plain).map_err(scan_error)?;
    let pinned = core.pinned(repo)?;
    let regen = strata_session::Regenerable::new(&pinned.regenerable, core.repo_cfg(repo)?);
    let paths: Vec<_> = preview
        .conflicts
        .into_iter()
        .filter(|p| !overlay.get(p).is_some_and(|e| regen.matches(e)))
        .collect();
    let conflicts =
        strata_overlay::rebase_conflicts(overlay, old, new, &blobs, &paths).map_err(scan_error)?;
    let mut rows = Vec::new();
    for c in &conflicts {
        if let Some(want) = &c.ours {
            let got = core
                .store
                .put_blob_rooted(&ws.path, &c.path)
                .map_err(scan_error)?;
            if &got != want {
                return Err(busy("changed_since_scan", json!({"path":c.path})));
            }
        }
        rows.push(SyncReportRow {
            target: head.clone(),
            generation,
            path: c.path.clone(),
            kind: c.kind.clone(),
            ours_blob: c.ours.clone(),
            hunks: json!(c.hunks),
        });
    }
    core.store
        .put_sync_report(&req.session, repo, head, generation, &rows)?;
    Ok(StrataError::new(code,"sync needs a resolution against this target and generation")
        .with_detail(json!({"reason":"sync_conflict","repo":repo,"target":{"head":head,"tree":new.r#gen.tree},"generation":generation,"conflicts":conflicts})))
}

//! After publish: the post-publish rebase (publish.md §5.8), `assign`
//! (§5.7) and `diff_between` (§5.9).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use strata_deps::{self as deps, Proposal, ProposalOrigin, ProposalState, ProposeArgs};
use strata_gitio::{Merge3, is_binary, merge3};
use strata_overlay::{Base, Overlay, RebaseOptions, WorkspaceRef};
use strata_store::{Lifecycle, PublishPhase, Store};
use strata_types::{
    BaseGen, DepsChanged, Code, CommitOid, DepsSetId, EntryKind, OverlayEntry, RepoName, RepoPath, SessionId, StrataError,
    WorkspaceState,
};

use crate::stage::{Src, Ver, cap_text, git_mode, read_rooted};
use crate::{Ctx, Result};

/// Who asked for a move (rm-followups §1.3): an `Explicit` caller (rollout,
/// `git-strata rebase`) moves now or gets `OpenFiles`; an `Auto` move (after
/// a publish) is made only when the workspace is idle and the projection has
/// no conflict outside `rm.regenerable`, else it is `Deferred`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovePolicy {
    Explicit,
    Auto,
}

/// What the post-publish rebase did to one session (§3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum RebaseReport {
    /// The workspace is on the new base; `dropped` left the overlay.
    /// `regenerable_kept`: conflicting `rm.regenerable` paths whose session
    /// version was kept (`Auto` only).
    Moved {
        dropped: Vec<RepoPath>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lww: Vec<RepoPath>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regenerable_kept: Vec<RepoPath>,
        /// C2 (rm-followups §1.3): the source moved onto the new tree with
        /// the OLD deps set; this proposal (Pending) brings the new set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deps_pending: Option<Proposal>,
    },
    /// `Auto` only: the session stays on its old base for now. `reason` is
    /// `open_files`, `recent_writes`, `conflicts`, `busy` or `state`.
    Deferred {
        reason: String,
        detail: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        proposal: Option<Proposal>,
    },
    /// The new base needs another deps set; the session stays on its old
    /// base. `proposal` is the one recorded or promoted.
    Waiting { proposal: Proposal },
    /// `move_workspace` found open files in the deps directories; the
    /// session did not move. The publish still succeeded.
    OpenFiles { detail: Value },
}

/// `diff_between`'s answer (§3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BetweenDiff {
    pub only_a: Vec<RepoPath>,
    pub only_b: Vec<RepoPath>,
    /// Paths both sides changed; a preview that cannot be given is an
    /// `Err` on the entry, not on the call.
    pub both: Vec<(RepoPath, Result<Merge3Preview>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge3Preview {
    pub merged: bool,
    /// On conflict, per hunk, capped at `HUNK_TEXT_CAP` (§4).
    pub hunks: Vec<HunkPreview>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkPreview {
    pub start: u32,
    pub ours: String,
    pub theirs: String,
    pub truncated: bool,
}

// ---- shared helpers -------------------------------------------------------

fn invalid(reason: &str, msg: String, mut detail: Value) -> StrataError {
    detail["reason"] = json!(reason);
    StrataError::new(Code::InvalidInput, msg).with_detail(detail)
}

fn no_such_session(s: &SessionId) -> StrataError {
    StrataError::new(Code::NoSuchSession, format!("no session {s}")).with_detail(json!({ "session": s.as_str() }))
}

/// A live session's workspace of `ctx.repo`.
pub(crate) fn workspace(ctx: &Ctx, s: &SessionId) -> Result<WorkspaceState> {
    let rec = match ctx.store.session(s)? {
        Some(r) if r.lifecycle != Lifecycle::Removed => r,
        _ => return Err(no_such_session(s)),
    };
    rec.manifest.repos.get(ctx.repo).cloned().ok_or_else(|| StrataError::not_in_session(s.as_str(), ctx.repo.as_str()))
}

fn base_struct(store: &Store, g: &BaseGen) -> Result<Base> {
    let manifest = store.manifest(&g.repo, &g.tree)?.ok_or_else(|| {
        StrataError::new(Code::Corrupt, "the base manifest is missing")
            .with_detail(json!({ "tree": g.tree.as_str(), "repo": g.repo.as_str() }))
    })?;
    Ok(Base { r#gen: g.clone(), manifest: Arc::new(manifest), dir: store.base_dir(g) })
}

/// The session's current overlay against its pinned base (a cold scan;
/// never writes the store or the workspace).
fn current_overlay(ctx: &Ctx, s: &SessionId, ws: &WorkspaceState, base: &Base) -> Result<Overlay> {
    let w = WorkspaceRef { session: s, repo: ctx.repo, root: &ws.path, config: ctx.cfg, git: ctx.git, store: ctx.store };
    strata_overlay::reconcile_full(&w, base)
}

fn sleep_busy() {
    std::thread::sleep(Duration::from_millis(20));
}

/// The session queue (for `sessions`) then the repo write lock, both
/// waited for without a bound: this runs after the first visible write.
/// With locks that do not wait (T2, rm-followups §1.6) a `Busy` take is
/// returned instead.
fn under_locks<T>(ctx: &Ctx, sessions: &[SessionId], f: &mut dyn FnMut() -> Result<T>) -> Result<T> {
    loop {
        let mut slot: Option<Result<T>> = None;
        let r = ctx.locks.with_rebase_sessions(sessions, &mut || {
            loop {
                match ctx.locks.with_repo(ctx.repo, &mut || {
                    slot = Some(f());
                    Ok(())
                }) {
                    Err(e) if e.code == Code::Busy && ctx.locks.waits() => sleep_busy(),
                    Err(e) => return Err(e),
                    Ok(r) => return r,
                }
            }
        });
        match r {
            Err(e) if e.code == Code::Busy && ctx.locks.waits() => sleep_busy(),
            Err(e) => return Err(e),
            Ok(r) => {
                r?;
                return slot.unwrap_or_else(|| {
                    Err(StrataError::new(Code::Unknown, "the repo lock never ran").with_detail(json!({ "reason": "lock" })))
                });
            }
        }
    }
}

// ---- rebase (§5.8) --------------------------------------------------------

/// `ensure_base_for`, whose base step takes the repo write lock with the
/// bounded wait: after the first visible write that wait is not bounded
/// (§5.1), so a plain lock `Busy` (no `reason`) is asked again. With locks
/// that do not wait (T2) it is asked once and a `Busy` is returned.
fn base_for(ctx: &Ctx, tree: &strata_types::TreeOid, deps: &DepsSetId) -> Result<BaseGen> {
    loop {
        match ctx.sessions.ensure_base_for(ctx.repo, tree, deps) {
            Err(e)
                if ctx.locks.waits()
                    && e.code == Code::Busy
                    && e.detail.as_ref().and_then(|d| d.get("reason")).is_none() =>
            {
                sleep_busy()
            }
            r => return r,
        }
    }
}

/// Whether every install-input path of `overlay` already has, on the new
/// base, exactly the session's current version: published this time and
/// not changed since. Empty → false (nothing of the draft landed).
fn inputs_landed(ctx: &Ctx, overlay: &Overlay, new: &Base) -> bool {
    let inputs: Vec<&OverlayEntry> = overlay
        .entries
        .iter()
        .filter(|e| {
            deps::is_input(ctx.cfg, &e.path)
                || matches!(&e.kind, EntryKind::Renamed { from, .. } if deps::is_input(ctx.cfg, from))
        })
        .collect();
    if inputs.is_empty() {
        return false;
    }
    let on_new = |p: &RepoPath| new.manifest.entries.get(p);
    inputs.iter().all(|e| {
        let here = match &e.kind {
            EntryKind::Deleted => on_new(&e.path).is_none(),
            EntryKind::Added { blob }
            | EntryKind::Modified { blob }
            | EntryKind::Untracked { blob }
            | EntryKind::Renamed { blob, .. } => {
                on_new(&e.path).is_some_and(|m| m.blob == *blob && git_mode(m.mode) == git_mode(e.mode))
            }
        };
        let from_gone = match &e.kind {
            EntryKind::Renamed { from, .. } => on_new(from).is_none(),
            _ => true,
        };
        here && from_gone
    })
}

pub(crate) fn deferred(reason: &str, detail: Value) -> RebaseReport {
    RebaseReport::Deferred { reason: reason.to_owned(), detail, proposal: None }
}

/// rm-followups §1.4 G0: an `Auto` move needs an Active session, no scene on
/// it or the workspace, no open move intent and the workspace still on `old`.
fn g0_state(ctx: &Ctx, s: &SessionId, old: &BaseGen) -> Result<Option<RebaseReport>> {
    let rec = match ctx.store.session(s)? {
        Some(r) if r.lifecycle != Lifecycle::Removed => r,
        _ => return Err(no_such_session(s)),
    };
    let ws_scene = rec.workspaces.get(ctx.repo).and_then(|w| w.scene.clone());
    let open_intent = ctx
        .store
        .move_intents(s)?
        .iter()
        .any(|i| i.repo == *ctx.repo && matches!(i.state, strata_store::MoveState::Preparing | strata_store::MoveState::Swapping));
    let base = rec.manifest.repos.get(ctx.repo).map(|w| w.base.clone());
    if rec.lifecycle != Lifecycle::Active || rec.scene.is_some() || ws_scene.is_some() || open_intent || base.as_ref() != Some(old) {
        return Ok(Some(deferred(
            "state",
            json!({
                "lifecycle": format!("{:?}", rec.lifecycle),
                "scene": rec.scene.is_some() || ws_scene.is_some(),
                "open_move": open_intent,
                "base_changed": base.as_ref() != Some(old),
            }),
        )));
    }
    Ok(None)
}

/// rm-followups §1.4 G1–G3 outside every lock: idle workspace, then the
/// read-only projection. `Ok(None)` → move; `Ok(Some(paths))` is never
/// returned empty: it is the regenerable conflicts kept on a move.
fn auto_gates(ctx: &Ctx, s: &SessionId, old: &BaseGen, new: &Base) -> Result<std::result::Result<Vec<RepoPath>, RebaseReport>> {
    if let Some(d) = g0_state(ctx, s, old)? {
        return Ok(Err(d));
    }
    let ws = workspace(ctx, s)?;
    let quiet = Duration::from_secs(ctx.cfg.rebase.quiet_secs);
    if let Some(idle) = strata_session::busy_reason(&ws.path, quiet)? {
        return Ok(Err(deferred(idle.reason, idle.detail)));
    }
    let old_base = base_struct(ctx.store, old)?;
    let overlay = current_overlay(ctx, s, &ws, &old_base)?;
    let ignorecase = ctx.git.read_index(&ws.path)?.ignorecase;
    let opts = RebaseOptions::for_move(new, ignorecase)?;
    let blobs = strata_overlay::PreviewBlobs::new(ctx.store, &ws.path, &overlay);
    let preview = match strata_overlay::rebase_preview(&overlay, &old_base, new, &blobs, &opts) {
        Ok(p) => p,
        Err(e) if e.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some(strata_overlay::CHANGED_SINCE_SCAN) => {
            return Ok(Err(deferred("recent_writes", e.detail.unwrap_or(Value::Null))));
        }
        Err(e) => return Err(e),
    };
    let patterns = ctx.sessions.regenerable(ctx.repo);
    let regen = strata_session::Regenerable::new(&patterns, ctx.cfg);
    let (kept, blocking): (Vec<RepoPath>, Vec<RepoPath>) =
        preview.conflicts.into_iter().partition(|p| overlay.get(p).is_some_and(|e| ws.tracked && regen.matches(e)));
    if !blocking.is_empty() {
        let shown: Vec<&str> = blocking.iter().take(20).map(|p| p.as_str()).collect();
        return Ok(Err(deferred("conflicts", json!({ "paths": shown, "count": blocking.len() }))));
    }
    Ok(Ok(kept))
}

/// A re-check run under the session queue and the repo write lock, given
/// the workspace's base the attempt started from: `Some` ends the attempt
/// with that report before anything is written (T2, rm-followups §1.6).
pub(crate) type Recheck<'r> = &'r dyn Fn(&BaseGen) -> Result<Option<RebaseReport>>;

/// How one attempt runs (rm-followups §1.6): `consume` is the
/// `rebase_pending` rev a move deletes in `finish_move`'s transaction.
#[derive(Clone, Copy, Default)]
pub(crate) struct Attempt<'r> {
    pub consume: Option<u64>,
    pub recheck: Option<Recheck<'r>>,
}

/// Steps 1–4 of §5.8 for one session. Step 5 (`observe_main`) is the
/// caller's: `publish` runs it once for the whole group.
pub(crate) fn rebase_one_with(
    ctx: &Ctx,
    s: &SessionId,
    new_base: &BaseGen,
    head: &CommitOid,
    policy: MovePolicy,
    at: Attempt<'_>,
) -> Result<RebaseReport> {
    if new_base.repo != *ctx.repo {
        return Err(invalid(
            "repo_mismatch",
            format!("the new base is of {}, not {}", new_base.repo, ctx.repo),
            json!({ "repo": ctx.repo.as_str(), "base_repo": new_base.repo.as_str() }),
        ));
    }
    if ctx.git.tree_of(head)? != new_base.tree {
        return Err(invalid("tree_mismatch", "the head's tree is not the new base's tree".into(), json!({})));
    }
    let old = workspace(ctx, s)?.base;
    // 1. Outside every lock: the new base directory, then its key.
    let mut target = new_base.clone();
    if !ctx.store.base_dir(&target).exists() {
        target = base_for(ctx, &target.tree, &target.deps)?;
    }
    let (n_inputs, n) = deps::key_of(ctx.deps, &ctx.store.base_dir(&target))?;
    // rm-followups §1.3 C2: an `Auto` move whose deps set changed moves the
    // source only, onto (new tree, OLD sealed set); nothing is installed.
    let c2 = policy == MovePolicy::Auto && n != old.deps && ctx.cfg.rebase.deps_changed == DepsChanged::MoveSource;
    if c2 {
        return move_source(ctx, s, &old, new_base, head, &n, &n_inputs, at);
    }
    if n == old.deps {
        if target.deps != n {
            target = base_for(ctx, &new_base.tree, &n)?;
        }
    } else if let Err(e) = deps::ensure(ctx.deps, &ctx.store.base_dir(&target)) {
        // §5.2 step 8: build the new set here, outside the lock, so the
        // later rollout is fast. Not needed for the proposal itself.
        tracing::warn!(repo = %ctx.repo, error = %e.message, "the new base's deps set could not be ensured");
    }
    let new = base_struct(ctx.store, &target)?;
    // rm-followups §1.4: an `Auto` move of the same deps set first passes
    // the gates outside every lock (another deps set keeps today's path).
    let mut kept: Vec<RepoPath> = Vec::new();
    if policy == MovePolicy::Auto && n == old.deps {
        match auto_gates(ctx, s, &old, &new)? {
            Ok(k) => kept = k,
            Err(d) => return Ok(d),
        }
    }
    let repo_id = ctx.git.repo_id();
    let queue = [s.clone()];
    under_locks(ctx, &queue, &mut || {
        let ws = workspace(ctx, s)?;
        if ws.base != old && policy == MovePolicy::Auto {
            return Ok(deferred("state", json!({ "base_changed": true })));
        }
        if ws.base != old {
            return Err(invalid(
                "base_changed",
                format!("session {s} moved to another base while its rebase waited"),
                json!({ "session": s.as_str(), "base": ws.base.tree.as_str(), "deps": ws.base.deps.as_str() }),
            ));
        }
        if let Some(check) = at.recheck
            && let Some(d) = check(&old)?
        {
            return Ok(d);
        }
        // 2. A draft whose input changes all landed is promoted.
        let promoted = if n != old.deps { promote_draft(ctx, s, &ws, &new, &old, &n, &n_inputs, new_base, head)? } else { None };
        // 3. Same deps set: move (G0 again under the locks for `Auto`).
        if n == old.deps {
            if policy == MovePolicy::Auto
                && let Some(d) = g0_state(ctx, s, &old)?
            {
                return Ok(d);
            }
            return match ctx.sessions.move_workspace_consuming(s, &repo_id, target.clone(), head.clone(), at.consume) {
                Ok(r) => Ok(RebaseReport::Moved { dropped: r.dropped, lww: r.lww, regenerable_kept: kept.clone(), deps_pending: None }),
                // §1.7: `OpenFiles` is for `Explicit` callers only.
                Err(e) if e.code == Code::OpenFiles && policy == MovePolicy::Auto => {
                    Ok(deferred("open_files", e.detail.unwrap_or(Value::Null)))
                }
                Err(e) if e.code == Code::OpenFiles => {
                    Ok(RebaseReport::OpenFiles { detail: e.detail.unwrap_or(Value::Null) })
                }
                Err(e) => Err(e),
            };
        }
        // 4. Another deps set: the session waits on its old base.
        let p = match promoted {
            Some(p) => p,
            None => propose_main(ctx, &old, &n, &n_inputs, new_base, head)?,
        };
        Ok(RebaseReport::Waiting { proposal: p })
    })
}

/// Step 2: the session's own draft, promoted when all its input changes
/// landed on the new base. Under the session queue and the repo lock.
#[allow(clippy::too_many_arguments)]
fn promote_draft(
    ctx: &Ctx,
    s: &SessionId,
    ws: &WorkspaceState,
    new: &Base,
    old: &BaseGen,
    n: &DepsSetId,
    n_inputs: &str,
    new_base: &BaseGen,
    head: &CommitOid,
) -> Result<Option<Proposal>> {
    let draft = deps::open_proposals(ctx.store, Some(ctx.repo))?.into_iter().rev().find(|p| {
        p.state == ProposalState::AwaitingPublish && p.origin == (ProposalOrigin::Session { session: s.clone() })
    });
    let Some(d) = draft else { return Ok(None) };
    if d.from == *n {
        return Ok(None);
    }
    let overlay = current_overlay(ctx, s, ws, &base_struct(ctx.store, old)?)?;
    if !inputs_landed(ctx, &overlay, new) {
        return Ok(None);
    }
    Ok(Some(deps::propose(
        ctx.deps,
        ProposeArgs {
            origin: ProposalOrigin::Session { session: s.clone() },
            from: d.from.clone(),
            from_inputs: d.from_inputs.clone(),
            to: n.clone(),
            to_inputs: n_inputs.to_string(),
            tree: Some(new_base.tree.clone()),
            head: Some(head.clone()),
        },
    )?))
}

/// Step 4's Main-origin proposal `old.deps → n`. Under the repo lock.
fn propose_main(
    ctx: &Ctx,
    old: &BaseGen,
    n: &DepsSetId,
    n_inputs: &str,
    new_base: &BaseGen,
    head: &CommitOid,
) -> Result<Proposal> {
    let from_inputs = deps::find_set(ctx.store, ctx.repo, &old.deps)?
        .map(|set| set.inputs_hash)
        .ok_or_else(|| deps_unsealed(ctx.repo, &old.deps))?;
    deps::propose(
        ctx.deps,
        ProposeArgs {
            origin: ProposalOrigin::Main,
            from: old.deps.clone(),
            from_inputs,
            to: n.clone(),
            to_inputs: n_inputs.to_string(),
            tree: Some(new_base.tree.clone()),
            head: Some(head.clone()),
        },
    )
}

/// rm-followups §1.3 C2 under `Auto`: record the deps proposal first (under
/// the locks, whether or not the move then happens), then move the source
/// onto (new tree, old deps set) through the same gates as C1. Every
/// deferral carries the proposal; a move reports it as `deps_pending`.
#[allow(clippy::too_many_arguments)]
fn move_source(
    ctx: &Ctx,
    s: &SessionId,
    old: &BaseGen,
    new_base: &BaseGen,
    head: &CommitOid,
    n: &DepsSetId,
    n_inputs: &str,
    at: Attempt<'_>,
) -> Result<RebaseReport> {
    let mut src = BaseGen { repo: ctx.repo.clone(), tree: new_base.tree.clone(), deps: old.deps.clone() };
    if !ctx.store.base_dir(&src).exists() || !ctx.store.has_base_gen(&src)? {
        src = base_for(ctx, &src.tree, &src.deps)?;
    }
    let repo_id = ctx.git.repo_id();
    let queue = [s.clone()];
    let with_p = |r: RebaseReport, p: &Proposal| match r {
        RebaseReport::Deferred { reason, detail, .. } => RebaseReport::Deferred { reason, detail, proposal: Some(p.clone()) },
        r => r,
    };
    let new = base_struct(ctx.store, &src)?;
    // 1. The proposal, before any gate.
    let first = under_locks(ctx, &queue, &mut || {
        let ws = workspace(ctx, s)?;
        if ws.base != *old {
            return Ok(Err(deferred("state", json!({ "base_changed": true }))));
        }
        if let Some(check) = at.recheck
            && let Some(d) = check(old)?
        {
            return Ok(Err(d));
        }
        let p = match promote_draft(ctx, s, &ws, &new, old, n, n_inputs, new_base, head)? {
            Some(p) => p,
            None => propose_main(ctx, old, n, n_inputs, new_base, head)?,
        };
        Ok(Ok(p))
    })?;
    let p = match first {
        Ok(p) => p,
        Err(d) => return Ok(d),
    };
    // 2. G0–G3 outside the locks, against the source-only base.
    let kept = match auto_gates(ctx, s, old, &new)? {
        Ok(k) => k,
        Err(d) => return Ok(with_p(d, &p)),
    };
    // 3. Under the locks: G0 again, then today's move.
    let r = under_locks(ctx, &queue, &mut || {
        let ws = workspace(ctx, s)?;
        if ws.base != *old {
            return Ok(deferred("state", json!({ "base_changed": true })));
        }
        if let Some(check) = at.recheck
            && let Some(d) = check(old)?
        {
            return Ok(d);
        }
        if let Some(d) = g0_state(ctx, s, old)? {
            return Ok(d);
        }
        match ctx.sessions.move_workspace_consuming(s, &repo_id, src.clone(), head.clone(), at.consume) {
            Ok(r) => Ok(RebaseReport::Moved { dropped: r.dropped, lww: r.lww, regenerable_kept: kept.clone(), deps_pending: Some(p.clone()) }),
            Err(e) if e.code == Code::OpenFiles => Ok(deferred("open_files", e.detail.unwrap_or(Value::Null))),
            Err(e) => Err(e),
        }
    })?;
    Ok(with_p(r, &p))
}

/// The base of `s`'s workspace of `ctx.repo`.
pub(crate) fn workspace_base(ctx: &Ctx, s: &SessionId) -> Result<BaseGen> {
    Ok(workspace(ctx, s)?.base)
}

/// `publish`'s rebase of one session onto the landed tree: its old deps
/// set is the starting guess for the new base generation. A
/// `rebase_pending` row whose committed target is this commit is the one
/// attempted (rm-followups §1.6 T1): a move consumes it, anything else is
/// recorded on it.
pub(crate) fn after_publish(ctx: &Ctx, s: &SessionId, tree: &strata_types::TreeOid, head: &CommitOid) -> Result<RebaseReport> {
    let old = workspace(ctx, s)?.base;
    let g = BaseGen { repo: ctx.repo.clone(), tree: tree.clone(), deps: old.deps };
    let row = ctx.store.rebase_pending_of(s, ctx.repo).ok().flatten().filter(|r| r.head.as_deref() == Some(head.as_str()));
    let r = rebase_one_with(ctx, s, &g, head, MovePolicy::Auto, Attempt { consume: row.as_ref().map(|r| r.rev), recheck: None })?;
    if let Some(row) = &row {
        crate::pending::note_outcome(ctx, row, &r);
    }
    Ok(r)
}

fn deps_unsealed(repo: &RepoName, id: &DepsSetId) -> StrataError {
    StrataError::new(Code::Corrupt, format!("the session's deps set {id} of {repo} has no record"))
        .with_detail(json!({ "reason": "deps_unsealed", "repo": repo.as_str(), "deps": id.as_str() }))
}

/// §3 `rebase_session`: put `session` on `new_base` after a publish, or
/// record the deps change it needs and leave it on its old base.
pub fn rebase_session(
    ctx: &Ctx,
    session: SessionId,
    repo: &RepoName,
    new_base: BaseGen,
    head: CommitOid,
) -> Result<RebaseReport> {
    rebase_session_with(ctx, session, repo, new_base, head, MovePolicy::Explicit)
}

/// [`rebase_session`] under `policy` (rm-followups §1.3).
pub fn rebase_session_with(
    ctx: &Ctx,
    session: SessionId,
    repo: &RepoName,
    new_base: BaseGen,
    head: CommitOid,
    policy: MovePolicy,
) -> Result<RebaseReport> {
    if repo != ctx.repo {
        return Err(invalid(
            "repo_mismatch",
            format!("the context is for {}, not {repo}", ctx.repo),
            json!({ "repo": repo.as_str(), "ctx_repo": ctx.repo.as_str() }),
        ));
    }
    let report = rebase_one_with(ctx, &session, &new_base, &head, policy, Attempt::default())?;
    // 5. After the lock: main's record, best-effort.
    if deps::main_tip(ctx.cfg, ctx.git).ok().flatten().as_ref() == Some(&head) {
        let dir = ctx.store.base_dir(&BaseGen { deps: workspace(ctx, &session)?.base.deps, ..new_base.clone() });
        let dir = if dir.exists() { dir } else { ctx.store.base_dir(&new_base) };
        if let Err(e) = deps::observe_main(ctx.deps, &new_base.tree, &head, &dir) {
            tracing::warn!(repo = %ctx.repo, error = %e.message, "observe_main failed after rebase");
        }
    }
    Ok(report)
}

/// Step 5 of [`rebase_session_with`] for a rebase made elsewhere
/// (rm-followups §1.6 attempts): best-effort.
pub(crate) fn observe_after(ctx: &Ctx, session: &SessionId, new_base: &BaseGen, head: &CommitOid) {
    if deps::main_tip(ctx.cfg, ctx.git).ok().flatten().as_ref() != Some(head) {
        return;
    }
    let Ok(ws) = workspace(ctx, session) else { return };
    let dir = ctx.store.base_dir(&BaseGen { deps: ws.base.deps, ..new_base.clone() });
    let dir = if dir.exists() { dir } else { ctx.store.base_dir(new_base) };
    if let Err(e) = deps::observe_main(ctx.deps, &new_base.tree, head, &dir) {
        tracing::warn!(repo = %ctx.repo, error = %e.message, "observe_main failed after rebase");
    }
}

// ---- assign (§5.7) --------------------------------------------------------

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn conflict_paths(outcome: &Value) -> Vec<String> {
    outcome
        .get("conflicts")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|c| c.get("path").and_then(|p| p.as_str()).map(String::from)).collect())
        .unwrap_or_default()
}

/// §3 `assign`: name the session that resolves `path` of publish `id`.
/// Writes only `outcome.assigned` of that row: no file, no lock, no
/// commit, no push.
pub fn assign(store: &Store, id: u64, path: &RepoPath, session: SessionId) -> Result<()> {
    let no_path = || {
        StrataError::new(Code::NoSuchPath, format!("{} is not a conflict of publish {id}", path.as_str()))
            .with_detail(json!({ "publish": id, "path": path.as_str() }))
    };
    match store.session(&session)? {
        Some(r) if r.lifecycle != Lifecycle::Removed => {}
        _ => return Err(no_such_session(&session)),
    }
    for _ in 0..16 {
        let Some(r) = store.publish_record(id)? else { return Err(no_path()) };
        let outcome = r.outcome.clone().unwrap_or_else(|| json!({}));
        let paths = conflict_paths(&outcome);
        let p = path.as_str();
        if !paths.iter().any(|c| c == p) {
            if paths.iter().any(|c| c.len() > p.len() && c.starts_with(p) && c.as_bytes()[p.len()] == b'/') {
                return Err(StrataError::new(Code::IsDirectory, format!("{p} is a directory; assign one path"))
                    .with_detail(json!({ "publish": id, "path": p })));
            }
            return Err(no_path());
        }
        let mut o = outcome;
        let mut list: Vec<Value> = o
            .get("assigned")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.get("path").and_then(|v| v.as_str()) != Some(p))
            .collect();
        list.push(json!({ "path": p, "session": session.as_str(), "at": millis(SystemTime::now()) }));
        o["assigned"] = json!(list);
        if store.set_publish_outcome(id, r.phase, &o)? {
            return Ok(());
        }
    }
    Err(StrataError::new(Code::Busy, format!("publish {id} kept changing while assign wrote it"))
        .with_detail(json!({ "reason": "assign_race", "publish": id })))
}

/// The paths of `repo` named by `assign` and not yet landed since: path →
/// session. An assignment ends when a later publish lands the path.
pub(crate) fn assignments(store: &Store, repo: &RepoName) -> Result<BTreeMap<String, SessionId>> {
    let rows = store.publishes()?;
    let of_repo = |o: &Value| o.get("repo").and_then(|r| r.as_str()) == Some(repo.as_str());
    let mut named: BTreeMap<String, (SessionId, u64)> = BTreeMap::new();
    for r in &rows {
        let Some(o) = &r.outcome else { continue };
        if !of_repo(o) {
            continue;
        }
        for a in o.get("assigned").and_then(|a| a.as_array()).into_iter().flatten() {
            let (Some(p), Some(s)) = (a.get("path").and_then(|v| v.as_str()), a.get("session").and_then(|v| v.as_str()))
            else {
                continue;
            };
            let Ok(s) = SessionId::try_from(s.to_string()) else { continue };
            let at = a.get("at").and_then(|v| v.as_u64()).unwrap_or(0);
            named.insert(p.to_string(), (s, at));
        }
    }
    for r in &rows {
        let (Some(o), Some(ended)) = (&r.outcome, r.ended) else { continue };
        if r.phase != PublishPhase::Done || !of_repo(o) || o.get("commit").is_none_or(|c| c.is_null()) {
            continue;
        }
        let landed: BTreeSet<&str> =
            o.get("paths").and_then(|p| p.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).collect();
        named.retain(|p, (_, at)| !(landed.contains(p.as_str()) && millis(ended) >= *at));
    }
    Ok(named.into_iter().map(|(p, (s, _))| (p, s)).collect())
}

// ---- diff_between (§5.9) --------------------------------------------------

fn entry_ver(e: &OverlayEntry) -> Option<Ver> {
    match &e.kind {
        EntryKind::Deleted => None,
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Untracked { blob }
        | EntryKind::Renamed { blob, .. } => Some(Ver {
            mode: git_mode(e.mode),
            blake: Some(blob.as_str().to_string()),
            oid: None,
            size: None,
            file: None,
            data: None,
        }),
    }
}

fn workspace_changed(s: &SessionId, p: &RepoPath) -> StrataError {
    StrataError::new(Code::Busy, format!("{} in session {s} changed while it was read; retry", p.as_str()))
        .with_detail(json!({ "reason": "workspace_changed", "session": s.as_str(), "path": p.as_str() }))
}

/// One side's version of `e` read from the workspace itself, not the
/// store: an edit made after the last checkpoint has a BLAKE3 from the
/// scan but no blob yet (§5.9 previews the current content, checkpointed
/// or not). The bytes must still hash to the scanned id; otherwise the
/// file changed after the scan → `Busy` (`workspace_changed`), retry.
/// Above `MERGE_MAX_BYTES` nothing is read (the entry is `TooLarge`).
fn workspace_ver(root: &Path, s: &SessionId, e: &OverlayEntry) -> Result<Option<Ver>> {
    let Some(mut v) = entry_ver(e) else { return Ok(None) };
    let full = root.join(e.path.as_str());
    let md = match std::fs::symlink_metadata(&full) {
        Ok(md) => md,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(workspace_changed(s, &e.path)),
        Err(err) => return Err(strata_apfs::classify_io_error(&err, strata_apfs::IoSide::Read, &full)),
    };
    if !md.file_type().is_symlink() && md.len() > strata_overlay::MERGE_MAX_BYTES {
        v.size = Some(md.len());
        return Ok(Some(v));
    }
    let bytes = match read_rooted(root, &e.path) {
        Ok(b) => b,
        Err(err) if matches!(err.code, Code::NoSuchPath | Code::IsDirectory | Code::NotFile) => {
            return Err(workspace_changed(s, &e.path));
        }
        Err(err) => return Err(err),
    };
    if v.blake.as_deref() != Some(blake3::hash(&bytes).to_hex().as_str()) {
        return Err(workspace_changed(s, &e.path));
    }
    v.size = Some(bytes.len() as u64);
    v.data = Some(Arc::new(bytes));
    Ok(Some(v))
}

/// Before projecting a side onto a newer base, its blobs must be in the
/// store (overlay.md §5.8 precondition). Edits after the last checkpoint
/// have none yet: store them from the workspace. An unreferenced blob is a
/// cache fill, not a visible write (overlay.md, Addendum 6 refinement 4);
/// gc collects it after the window. A put that does not match the scan
/// means the file changed → `Busy` (`workspace_changed`).
fn fill_blobs(ctx: &Ctx, s: &SessionId, root: &Path, o: &Overlay) -> Result<()> {
    for e in &o.entries {
        let Some(Ver { blake: Some(want), .. }) = entry_ver(e) else { continue };
        let got = match ctx.store.put_blob_rooted(root, &e.path) {
            Ok(id) => id,
            Err(err) if matches!(err.code, Code::NoSuchPath | Code::IsDirectory | Code::NotFile) => {
                return Err(workspace_changed(s, &e.path));
            }
            Err(err) => return Err(err),
        };
        if got.as_str() != want {
            return Err(workspace_changed(s, &e.path));
        }
    }
    Ok(())
}

fn conflict(kind: &str, p: &RepoPath) -> StrataError {
    StrataError::new(Code::Conflict, format!("{} cannot be merged ({kind})", p.as_str()))
        .with_detail(json!({ "reason": kind, "path": p.as_str() }))
}

/// The §5.3 three-way rules as a preview of one path.
fn preview(src: &Src, p: &RepoPath, base: Option<&Ver>, a: Option<&Ver>, b: Option<&Ver>) -> Result<Merge3Preview> {
    let clean = Merge3Preview { merged: true, hunks: vec![] };
    if src.same(a, b)? {
        return Ok(clean);
    }
    let (o, t) = match (a, b) {
        (Some(o), Some(t)) => (o, t),
        _ => return Err(conflict("modify_delete", p)),
    };
    let Some(bv) = base else { return Err(conflict("add_add", p)) };
    if o.is_link() || t.is_link() || bv.is_link() {
        return Err(conflict("link", p));
    }
    if o.mode != bv.mode && t.mode != bv.mode && o.mode != t.mode {
        return Err(conflict("mode", p));
    }
    let too_large = || {
        StrataError::new(Code::TooLarge, format!("{} is larger than the merge limit", p.as_str()))
            .with_detail(json!({ "path": p.as_str(), "limit": strata_overlay::MERGE_MAX_BYTES }))
    };
    let (Some(bb), Some(ob), Some(tb)) = (src.load(bv)?, src.load(o)?, src.load(t)?) else {
        return Err(too_large());
    };
    if is_binary(&bb) || is_binary(&ob) || is_binary(&tb) {
        return Err(StrataError::new(Code::Binary, format!("{} is binary; no hunk preview", p.as_str()))
            .with_detail(json!({ "path": p.as_str() })));
    }
    Ok(match merge3(&bb, &ob, &tb) {
        Merge3::Clean(_) => clean,
        Merge3::Conflict(hunks) => Merge3Preview {
            merged: false,
            hunks: hunks
                .iter()
                .map(|h| {
                    let (ours, to) = cap_text(&h.ours);
                    let (theirs, tt) = cap_text(&h.theirs);
                    HunkPreview { start: h.ours_start as u32, ours, theirs, truncated: to || tt }
                })
                .collect(),
        },
    })
}

/// §3 `diff_between`: read-only; no repo lock, no ref. The only store
/// writes are cache fills (unreferenced blobs) when a side is projected.
pub fn diff_between(ctx: &Ctx, a: SessionId, b: SessionId, repo: &RepoName) -> Result<BetweenDiff> {
    if repo != ctx.repo {
        return Err(invalid(
            "repo_mismatch",
            format!("the context is for {}, not {repo}", ctx.repo),
            json!({ "repo": repo.as_str(), "ctx_repo": ctx.repo.as_str() }),
        ));
    }
    let wa = workspace(ctx, &a)?;
    let wb = workspace(ctx, &b)?;
    let ba = base_struct(ctx.store, &wa.base)?;
    let bb = base_struct(ctx.store, &wb.base)?;
    let mut oa = current_overlay(ctx, &a, &wa, &ba)?;
    let mut ob = current_overlay(ctx, &b, &wb, &bb)?;
    // Different pins: project the older side onto the newer base.
    // Which side, if any, was projected (its entries then come from the
    // store; the other side is read from its workspace).
    let mut projected_a = false;
    let mut projected_b = false;
    let common = if wa.base == wb.base {
        ba
    } else {
        let a_older = if wa.head == wb.head || ctx.git.is_ancestor(&wa.head, &wb.head)? {
            true
        } else if ctx.git.is_ancestor(&wb.head, &wa.head)? {
            false
        } else {
            return Err(invalid(
                "bases_unordered",
                format!("the bases of {a} and {b} are not one an ancestor of the other"),
                json!({ "a": wa.head.as_str(), "b": wb.head.as_str() }),
            ));
        };
        let (old_o, old_b, new_b, old_ws, old_s) =
            if a_older { (&oa, &ba, &bb, &wa, &a) } else { (&ob, &bb, &ba, &wb, &b) };
        fill_blobs(ctx, old_s, &old_ws.path, old_o)?;
        let ignorecase = ctx.git.read_index(&old_ws.path)?.ignorecase;
        let opts = RebaseOptions::for_move(new_b, ignorecase)?;
        let projected = strata_overlay::rebase(old_o, old_b, new_b, ctx.store, &opts)?.overlay;
        if a_older {
            oa = projected;
            projected_a = true;
            bb
        } else {
            ob = projected;
            projected_b = true;
            ba
        }
    };
    let map = |o: &Overlay| -> BTreeMap<RepoPath, OverlayEntry> {
        o.entries.iter().map(|e| (e.path.clone(), e.clone())).collect()
    };
    let (ma, mb) = (map(&oa), map(&ob));
    let src = Src { store: ctx.store, git: ctx.git };
    let mut out = BetweenDiff { only_a: vec![], only_b: vec![], both: vec![] };
    for (p, ea) in &ma {
        match mb.get(p) {
            None => out.only_a.push(p.clone()),
            Some(eb) => {
                let base = common.manifest.entries.get(p).map(|m| Ver {
                    mode: git_mode(m.mode),
                    blake: Some(m.blob.as_str().to_string()),
                    oid: None,
                    size: Some(m.size),
                    file: Some((common.dir.clone(), p.clone())),
                    data: None,
                });
                let va = if projected_a { entry_ver(ea) } else { workspace_ver(&wa.path, &a, ea)? };
                let vb = if projected_b { entry_ver(eb) } else { workspace_ver(&wb.path, &b, eb)? };
                let r = preview(&src, p, base.as_ref(), va.as_ref(), vb.as_ref());
                out.both.push((p.clone(), r));
            }
        }
    }
    out.only_b = mb.keys().filter(|p| !ma.contains_key(*p)).cloned().collect();
    Ok(out)
}

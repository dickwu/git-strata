//! rm-followups §1.6: the `rebase_pending` protocol's git side. Every
//! function here that writes runs under the repo write lock (the caller
//! holds it), except [`attempt`], which takes the locks itself. All of it is
//! best-effort after the first visible write: a store or git error is
//! logged and the row is left for the next reconstruction.

use std::time::SystemTime;

use serde_json::{Value, json};
use strata_gitio::{Git, remote_ref};
use strata_store::{PublishPhase, RebasePending, RefOp, RefOutcome, ReplayPlan, Store};
use strata_types::{BaseGen, CommitOid, RepoName, SessionId, TreeOid};

use crate::rebase::{MovePolicy, RebaseReport};
use crate::{Ctx, Result, anchor_ref, chain_ref};

const REMOTE: &str = "origin";

/// What the protocol needs of one repo: its store, git and publish branch.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub store: &'a Store,
    pub git: &'a Git,
    pub repo: &'a RepoName,
    pub branch: &'a str,
}

impl<'a> Scope<'a> {
    pub fn of(ctx: &'a Ctx<'a>) -> Self {
        Scope { store: ctx.store, git: ctx.git, repo: ctx.repo, branch: &ctx.cfg.publish.branch }
    }
}

fn oid(s: &str) -> Option<CommitOid> {
    CommitOid::try_from(s.to_string()).ok()
}

fn warn(what: &str, s: &SessionId, e: &strata_types::StrataError) {
    tracing::warn!(session = %s, error = %e.message, "rebase_pending: {what} failed");
}

fn tip_reaches(git: &Git, r: &str, c: &CommitOid) -> bool {
    match git.read_ref(r) {
        Ok(Some(tip)) => git.is_ancestor(c, &tip).unwrap_or(false),
        _ => false,
    }
}

/// §1.6: `c` is reachable from the publish chain ref or the tracking ref.
pub fn reachable(sc: Scope, c: &CommitOid) -> bool {
    tip_reaches(sc.git, &chain_ref(sc.branch), c) || tip_reaches(sc.git, &remote_ref(REMOTE, sc.branch), c)
}

/// `c` is reachable from the tracking ref (what was pushed).
pub fn pushed(sc: Scope, c: &CommitOid) -> bool {
    tip_reaches(sc.git, &remote_ref(REMOTE, sc.branch), c)
}

fn reread(sc: Scope, s: &SessionId) -> Option<RebasePending> {
    match sc.store.rebase_pending_of(s, sc.repo) {
        Ok(r) => r,
        Err(e) => {
            warn("read", s, &e);
            None
        }
    }
}

/// Rule 2's validation of the row's other targets: a planned replay target
/// is adopted when the chain reaches it, else cleared; a committed target
/// that neither ref reaches is discarded (`stale_target`).
fn validate(sc: Scope, row: RebasePending) -> Option<RebasePending> {
    let s = row.session.clone();
    let mut changed = false;
    if let Some(ph) = row.planned_head.as_deref() {
        let adopt = oid(ph).is_some_and(|c| tip_reaches(sc.git, &chain_ref(sc.branch), &c));
        if let Err(e) = sc.store.settle_planned(&s, sc.repo, ph, adopt, SystemTime::now()) {
            warn("settle_planned", &s, &e);
        }
        changed = true;
    }
    let row = if changed { reread(sc, &s)? } else { row };
    if let Some(q) = row.head.as_deref()
        && !oid(q).is_some_and(|c| reachable(sc, &c))
    {
        tracing::warn!(session = %s, head = q, "rebase_pending: the committed target is unreachable; discarded");
        if let Err(e) = sc.store.discard_pending_target(&s, sc.repo, row.rev, "stale_target", &json!({ "head": q }), SystemTime::now()) {
            warn("discard_pending_target", &s, &e);
        }
        return reread(sc, &s);
    }
    Some(row)
}

/// Rules 2/3: publish `id` landed `commit` (tree `tree`) for `s`. The plan
/// is cleared; the commit becomes the committed target unless a committed
/// target it does not descend from is kept (`RebaseTargetConflict`).
fn promote_one(sc: Scope, s: &SessionId, id: u64, commit: &CommitOid, tree: &TreeOid, reason: &str) {
    let Some(row) = reread(sc, s) else { return };
    let Some(row) = validate(sc, row) else { return };
    if row.planned_publish != Some(id) {
        return;
    }
    let fits = match row.head.as_deref().and_then(oid) {
        None => true,
        Some(q) => sc.git.is_ancestor(&q, commit).unwrap_or(false),
    };
    if !fits {
        tracing::warn!(session = %s, publish = id, kept = ?row.head, "RebaseTargetConflict: the committed target is kept");
    }
    let target = fits.then_some((commit.as_str(), tree.as_str()));
    if let Err(e) = sc.store.promote_pending(s, sc.repo, row.rev, id, target, reason, SystemTime::now()) {
        warn("promote_pending", s, &e);
    }
}

/// Rule 2, under the repo lock right after `commit_into` returned Ok.
pub(crate) fn promote_published(sc: Scope, sessions: &[SessionId], id: u64, commit: &CommitOid, tree: &TreeOid) {
    for s in sessions {
        promote_one(sc, s, id, commit, tree, "pending");
    }
}

/// Rules 3/5 for a publish that failed under the repo lock: a promoted
/// commit is marked `publish_unfinished`; a plan is resolved by the
/// reachability of `written`, else cleared.
pub(crate) fn resolve_failed(sc: Scope, sessions: &[SessionId], id: u64, written: Option<&CommitOid>) {
    for s in sessions {
        let Some(row) = reread(sc, s) else { continue };
        if let Some(c) = written
            && row.head.as_deref() == Some(c.as_str())
            && row.planned_publish.is_none()
        {
            if let Err(e) = sc.store.set_pending_reason(s, sc.repo, row.rev, "publish_unfinished") {
                warn("set_pending_reason", s, &e);
            }
            continue;
        }
        if row.planned_publish != Some(id) {
            continue;
        }
        match written {
            Some(c) if reachable(sc, c) => match sc.git.tree_of(c) {
                Ok(t) => promote_one(sc, s, id, c, &t, "publish_unfinished"),
                Err(e) => warn("tree_of", s, &e),
            },
            _ => clear_plan(sc, s, id),
        }
    }
}

/// Rule 5 after the lock: the publish wrote `commit` but did not finish
/// (the post-publish rebase or the row's close failed), so each session
/// still targeting it is `publish_unfinished`.
pub(crate) fn mark_unfinished(sc: Scope, sessions: &[SessionId], commit: &CommitOid) {
    for s in sessions {
        let Some(row) = reread(sc, s) else { continue };
        if row.head.as_deref() != Some(commit.as_str()) {
            continue;
        }
        if let Err(e) = sc.store.set_pending_reason(s, sc.repo, row.rev, "publish_unfinished") {
            warn("set_pending_reason", s, &e);
        }
    }
}

/// Rule 3 with no lock held: only the plan of `id` goes (nothing was
/// written, so there is nothing to resolve).
pub(crate) fn clear_plan(sc: Scope, s: &SessionId, id: u64) {
    if let Err(e) = sc.store.clear_planned_publish(s, sc.repo, id) {
        warn("clear_planned_publish", s, &e);
    }
}

/// The commit publish `id` wrote, if any record of it is left: the row's
/// outcome, the applied chain-ref change, or the chain commit whose
/// subject names it.
fn commit_of(sc: Scope, id: u64, outcome: Option<&Value>) -> Option<CommitOid> {
    if let Some(c) = outcome.and_then(|o| o.get("commit")).and_then(Value::as_str).and_then(oid) {
        return Some(c);
    }
    if let Ok(changes) = sc.store.ref_changes(id)
        && let Some(c) = changes
            .iter()
            .find(|c| c.op == RefOp::UpdateRef && c.outcome == RefOutcome::Applied && c.ref_name == chain_ref(sc.branch))
    {
        return Some(c.new.clone());
    }
    let tip = sc.git.read_ref(&chain_ref(sc.branch)).ok().flatten()?;
    let anchor = sc.git.read_ref(&anchor_ref(sc.branch)).ok().flatten()?;
    let subject = format!("strata publish {id} ({})", sc.repo);
    let (list, _) = sc.git.commits_between(&anchor, &tip, 500).ok()?;
    list.into_iter().find(|(_, s)| *s == subject).map(|(c, _)| c)
}

/// Rule 4: reconstruct every row of the repo. The caller holds the repo
/// write lock (daemon start step 10, replay start, T2's try-lock step).
pub fn reconstruct(sc: Scope) -> Result<()> {
    let rows: Vec<RebasePending> = sc.store.rebase_pending(None)?.into_iter().filter(|r| r.repo == *sc.repo).collect();
    for row in rows {
        let s = row.session.clone();
        let Some(row) = validate(sc, row) else { continue };
        let Some(id) = row.planned_publish else { continue };
        let rec = match sc.store.publish_record(id) {
            Ok(r) => r,
            Err(e) => {
                warn("publish_record", &s, &e);
                continue;
            }
        };
        if rec.as_ref().is_some_and(|r| r.phase == PublishPhase::Running) {
            continue;
        }
        let outcome = rec.as_ref().and_then(|r| r.outcome.as_ref());
        match commit_of(sc, id, outcome).filter(|c| reachable(sc, c)) {
            Some(c) => {
                let clean = rec.as_ref().is_some_and(|r| r.phase == PublishPhase::Done)
                    && outcome.is_some_and(|o| o.get("error").is_none_or(Value::is_null) && o.get("stopped").is_none_or(Value::is_null));
                match sc.git.tree_of(&c) {
                    Ok(t) => promote_one(sc, &s, id, &c, &t, if clean { "pending" } else { "publish_unfinished" }),
                    Err(e) => warn("tree_of", &s, &e),
                }
            }
            None => clear_plan(sc, &s, id),
        }
    }
    Ok(())
}

/// Replay (§1.6): before the ref transaction, under the repo lock and
/// after [`reconstruct`]. (a) rows whose committed target was replayed are
/// retargeted to the copy; (b) sessions whose head was replayed are
/// enrolled: insert-only against committed targets (a copy that does not
/// strictly descend from the existing target loses, with a
/// `RebaseTargetConflict` warning). Returns what was planned, for
/// [`adopt_replay`].
pub fn plan_replay(sc: Scope, map: &[(CommitOid, CommitOid)], heads: &[(SessionId, CommitOid)]) -> Result<Vec<ReplayPlan>> {
    let copy_of = |c: &str| map.iter().find(|(o, _)| o.as_str() == c).map(|(_, n)| n.clone());
    let rows: Vec<RebasePending> = sc.store.rebase_pending(None)?.into_iter().filter(|r| r.repo == *sc.repo).collect();
    let mut planned: Vec<(SessionId, CommitOid)> = Vec::new();
    for r in &rows {
        if let Some(n) = r.head.as_deref().and_then(copy_of) {
            planned.push((r.session.clone(), n));
        }
    }
    for (s, head) in heads {
        let Some(e) = copy_of(head.as_str()) else { continue };
        let existing = planned
            .iter()
            .find(|(p, _)| p == s)
            .map(|(_, n)| n.clone())
            .or_else(|| rows.iter().find(|r| r.session == *s).and_then(|r| r.head.as_deref()).and_then(oid));
        match existing {
            None => planned.push((s.clone(), e)),
            Some(x) if x == e => {}
            Some(x) => {
                if sc.git.is_ancestor(&x, &e).unwrap_or(false) {
                    planned.retain(|(p, _)| p != s);
                    planned.push((s.clone(), e));
                } else if !sc.git.is_ancestor(&e, &x).unwrap_or(false) {
                    tracing::warn!(session = %s, kept = %x, replayed = %e, "RebaseTargetConflict: the existing target is kept");
                }
            }
        }
    }
    let mut plans = Vec::new();
    for (s, c) in planned {
        let tree = sc.git.tree_of(&c)?;
        plans.push(ReplayPlan { session: s, planned_head: c.as_str().to_owned(), planned_tree: tree.as_str().to_owned() });
    }
    sc.store.plan_replay(sc.repo, &plans, SystemTime::now())?;
    Ok(plans)
}

/// Replay, after the ref transaction: each plan becomes the committed
/// target when the chain reaches it, else it is cleared.
pub fn adopt_replay(sc: Scope, plans: &[ReplayPlan]) {
    for p in plans {
        let adopt = oid(&p.planned_head).is_some_and(|c| tip_reaches(sc.git, &chain_ref(sc.branch), &c));
        if let Err(e) = sc.store.settle_planned(&p.session, sc.repo, &p.planned_head, adopt, SystemTime::now()) {
            warn("settle_planned", &p.session, &e);
        }
    }
}

/// One attempt of a row's committed target under `policy` (T1 after a
/// publish or replay, T2 later). A move consumes the row in its finishing
/// transaction; a deferral is recorded on the attempted rev only.
pub fn attempt(ctx: &Ctx, row: &RebasePending, policy: MovePolicy) -> Result<Option<RebaseReport>> {
    let (Some(head), Some(tree)) = (row.head.as_deref().and_then(oid), row.tree.as_deref()) else { return Ok(None) };
    let Ok(tree) = TreeOid::try_from(tree.to_string()) else { return Ok(None) };
    let s = &row.session;
    let deps = crate::rebase::workspace_base(ctx, s)?.deps;
    let g = BaseGen { repo: ctx.repo.clone(), tree, deps };
    let r = crate::rebase::rebase_one_with(ctx, s, &g, &head, policy, Some(row.rev))?;
    note_outcome(ctx, row, &r);
    crate::rebase::observe_after(ctx, s, &g, &head);
    Ok(Some(r))
}

/// Records a non-move outcome of an attempt of `row` (rule 6).
pub(crate) fn note_outcome(ctx: &Ctx, row: &RebasePending, r: &RebaseReport) {
    let publish = row.detail.as_ref().and_then(|d| d.get("publish")).cloned().unwrap_or(Value::Null);
    let (reason, mut detail) = match r {
        RebaseReport::Moved { .. } => return,
        RebaseReport::Deferred { reason, detail, .. } => (reason.clone(), detail.clone()),
        RebaseReport::OpenFiles { detail } => ("open_files".to_owned(), detail.clone()),
        RebaseReport::Waiting { proposal } => ("deps_changed".to_owned(), json!({ "proposal": proposal.id.to_string() })),
    };
    if !detail.is_object() {
        detail = json!({ "detail": detail });
    }
    detail["publish"] = publish;
    if let Err(e) = ctx.store.defer_pending(&row.session, ctx.repo, row.rev, &reason, &detail, SystemTime::now()) {
        warn("defer_pending", &row.session, &e);
    }
}

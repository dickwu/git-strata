//! Commit, push and the publish flow (publish.md §5.2 steps 1–9, §5.6, §6).
//! Before `commit_tree` a refusal is named and the row closes `Done`
//! (D16); after it, every failure is `Unknown`, the scene stays, and
//! nothing is retried or replayed.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;
use serde_json::{Value, json};
use strata_deps::{ChainState, publish_chain};
use strata_gitio::{Git, RefUpdate, remote_ref};
use strata_store::{PublishPhase, RefChange, RefOp, RefOutcome};
use strata_types::{
    Action, Code, CommitOid, ConfirmToken, PushOutcome, ReleaseGroup, RepoConfig, RepoName, SessionId, StrataError,
};

use crate::chain::{advance_anchor, anchor_ref, chain_ref, refuse_state};
use crate::check::{check, remove_staged_layout};
use crate::rebase::RebaseReport;
use crate::seams::{self, FailPoint, Point};
use crate::stage::{self, Staged};
use crate::{Ctx, Result};

/// The remote publish pushes to (§2): always `origin`.
const REMOTE: &str = "origin";

/// The result of one publish (§3). The commit on the chain is the success;
/// what follows it never turns it into a failure (§6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub id: u64,
    pub commit: CommitOid,
    pub checks_ran: u32,
    /// `None` when this call did not push.
    pub push: Option<PushOutcome>,
    pub warnings: Vec<PublishWarning>,
    /// The post-publish rebase of each session of the group, ascending.
    pub rebases: Vec<(SessionId, RebaseReport)>,
}

/// Not a `Code`: the publish already succeeded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublishWarning {
    pub kind: WarningKind,
    pub detail: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum WarningKind {
    ObserveFailed,
}

fn err_json(e: &StrataError) -> Value {
    json!({ "code": e.code.as_str(), "message": e.message, "detail": e.detail })
}

/// An `Unknown` that keeps `cause` (the original code and detail) in
/// `detail.cause` (§6), plus `extra` fields.
pub(crate) fn unknown_with(reason: &str, message: impl Into<String>, cause: Option<&StrataError>, extra: Value) -> StrataError {
    let mut d = json!({ "reason": reason });
    if let Some(c) = cause {
        d["cause"] = err_json(c);
    }
    if let Value::Object(m) = extra {
        for (k, v) in m {
            d[k] = v;
        }
    }
    StrataError::new(Code::Unknown, message).with_detail(d)
}

/// After the object exists, every error is `Unknown` (P8); an `Unknown`
/// stays as it is with the commit added.
fn after_write(commit: &CommitOid, e: StrataError) -> StrataError {
    if e.code == Code::Unknown {
        let mut e = e;
        let d = e.detail.get_or_insert_with(|| json!({}));
        if d.get("commit").is_none() {
            d["commit"] = json!(commit.as_str());
        }
        return e;
    }
    unknown_with(
        "after_commit",
        format!("{} after the commit was written; the outcome is unknown", e.message),
        Some(&e),
        json!({ "commit": commit.as_str() }),
    )
}

/// Review-2 N3: an error after the commit carries what is known of it, the
/// push outcome (`push`: a `PushOutcome` as JSON, null when nothing was
/// sent) and `checks_ran`, so the daemon's envelope never loses a landed
/// result. Keys already set are kept.
fn with_landed(e: StrataError, push: Value, checks_ran: u32) -> StrataError {
    let mut e = e;
    let d = e.detail.get_or_insert_with(|| json!({}));
    if d.is_object() {
        if d.get("push").is_none() {
            d["push"] = push;
        }
        if d.get("checks_ran").is_none() {
            d["checks_ran"] = json!(checks_ran);
        }
    }
    e
}

/// Review-2 N3: the landed keys of `inner`'s detail copied into `extra`, for
/// an error that replaces `inner` (the row could not be closed). Only the
/// result fields travel (review-3 T2): `kind`/`message` describe `inner`
/// and stay in `detail.cause`, so the recording failure is not mistaken for
/// the refusal it wraps.
fn carry(inner: &StrataError, mut extra: Value) -> Value {
    if let (Some(d), Value::Object(m)) = (inner.detail.as_ref(), &mut extra) {
        for k in ["commit", "push", "checks_ran"] {
            if let Some(v) = d.get(k)
                && !m.contains_key(k)
            {
                m.insert(k.to_string(), v.clone());
            }
        }
    }
    extra
}

// ---- confirmation (§5.6) -------------------------------------------------

/// The confirmation gate: when the push needs a human, the token's action
/// must be exactly `PushDeploy { repo, branch: cfg.publish.branch }`. Only
/// the action is checked (nonce and age are confirm's job). Returns the
/// confirmed action. Reads nothing else: no flag, env var or config bypass.
fn confirm_gate(cfg: &RepoConfig, repo: &RepoName, confirm: Option<ConfirmToken>) -> Result<Option<Action>> {
    if !cfg.needs_human_confirm() {
        return Ok(None);
    }
    let want = Action::PushDeploy { repo: repo.clone(), branch: cfg.publish.branch.clone() };
    match confirm {
        Some(t) if t.action == want => Ok(Some(t.action)),
        Some(t) => Err(StrataError::new(Code::NotConfirmed, "the confirmation is for a different action")
            .with_detail(json!({ "reason": "action_mismatch", "want": want, "got": t.action }))),
        None => Err(StrataError::new(Code::NotConfirmed, format!("pushing {repo} to {} needs a confirmation", cfg.publish.branch))
            .with_detail(json!({ "reason": "missing", "want": want }))),
    }
}

// ---- commit (§5.2 step 6) ------------------------------------------------

/// The chain/anchor transaction after `commit_tree`: the chain moves from
/// `expected` to `commit`; when `parent` is origin's tip the anchor moves to
/// `parent` in the same transaction (§5.4).
fn move_chain(git: &Git, branch: &str, commit: &CommitOid, parent: &CommitOid, expected: Option<&CommitOid>) -> Result<()> {
    seams::fail(git, FailPoint::BeforeRefs)?;
    let mut ups = vec![RefUpdate::set(chain_ref(branch), commit.clone(), expected.cloned())];
    if git.read_ref(&remote_ref(REMOTE, branch))?.as_ref() == Some(parent) {
        let a = git.read_ref(&anchor_ref(branch))?;
        if a.as_ref() != Some(parent) {
            ups.push(RefUpdate::set(anchor_ref(branch), parent.clone(), a));
        }
    }
    git.ref_transaction(&ups).map_err(|e| {
        if e.code == Code::HeadMoved {
            unknown_with(
                "head_moved",
                "the publish chain moved after it was read; the commit is not on the chain and is not retried",
                Some(&e),
                json!({ "commit": commit.as_str(), "head_moved": e.detail }),
            )
        } else {
            after_write(commit, e)
        }
    })
}

/// `commit_tree`, then the transaction. `written` is set as soon as the
/// object exists, so the caller can report it on `Unknown`.
fn commit_into(
    git: &Git,
    branch: &str,
    staged: &Staged,
    parent: &CommitOid,
    expected: Option<&CommitOid>,
    message: &str,
    written: &mut Option<CommitOid>,
) -> Result<CommitOid> {
    seams::hit(git, Point::BeforeCommit);
    let c = git.commit_tree(&staged.tree, parent, message)?;
    *written = Some(c.clone());
    seams::hit(git, Point::AfterCommit);
    move_chain(git, branch, &c, parent, expected)?;
    Ok(c)
}

/// §3 `commit`: `commit_tree` (identity missing → `InvalidInput`
/// `git-identity`, no object), then one ref transaction moving the chain of
/// `branch` from `expected_chain` (and the anchor, §5.4). After the object
/// exists only `Unknown` comes out; `HeadMoved` is in `detail.head_moved`.
/// `branch` is not in the §3 list (the chain ref needs it); recorded in
/// docs/impl-progress/publish.md. The caller holds the repo write lock.
pub fn commit(
    git: &Git,
    branch: &str,
    staged: &Staged,
    parent: &CommitOid,
    expected_chain: Option<&CommitOid>,
    message: &str,
) -> Result<CommitOid> {
    let mut written = None;
    commit_into(git, branch, staged, parent, expected_chain, message, &mut written)
}

// ---- push (§5.6) ---------------------------------------------------------

/// The recording push: chain must lead, then gitio's `push_published`
/// (tracking ref and anchor in one transaction). Raw outcome.
fn send(git: &Git, branch: &str, staging: &Path, url: Option<&str>) -> Result<PushOutcome> {
    match publish_chain(git, branch)? {
        Some(c) if c.state == ChainState::Leads => {}
        c => {
            return Err(StrataError::new(Code::NoCommit, "the publish chain does not lead origin; nothing to push")
                .with_detail(json!({ "branch": branch, "state": c.map(|c| format!("{:?}", c.state)) })));
        }
    }
    match url {
        Some(u) => git.push_published_to(staging, REMOTE, u, &chain_ref(branch), branch, &anchor_ref(branch), seams::push_timeout(git)),
        None => git.push_published(staging, REMOTE, &chain_ref(branch), branch, &anchor_ref(branch), seams::push_timeout(git)),
    }
}

/// §3 `push`: the confirmation first (`NotConfirmed`, nothing started), then
/// the chain must lead (`NoCommit`), then the recording push. `Refused` →
/// `PushRefused` (remote text in `detail.message`); a timeout or an
/// unrecorded push → `Unknown`. Only `Ok(Pushed)` is returned. `repo` is not
/// in the §3 list (the action names it); recorded in
/// docs/impl-progress/publish.md. The caller holds the repo write lock.
pub fn push(git: &Git, cfg: &RepoConfig, repo: &RepoName, staging: &Path, confirm: Option<ConfirmToken>) -> Result<PushOutcome> {
    push_to(git, cfg, repo, staging, confirm, None)
}

/// `push` to exactly `url` when given (gitio `push_published_to`): the
/// daemon passes the push URL it recorded with the staging scene, so the
/// record names the destination the push went to. [实现时补的决定]
/// (review-4 F1)
pub fn push_to(git: &Git, cfg: &RepoConfig, repo: &RepoName, staging: &Path, confirm: Option<ConfirmToken>, url: Option<&str>) -> Result<PushOutcome> {
    confirm_gate(cfg, repo, confirm)?;
    match send(git, &cfg.publish.branch, staging, url)? {
        PushOutcome::Pushed => Ok(PushOutcome::Pushed),
        PushOutcome::Refused(m) => Err(StrataError::new(Code::PushRefused, "the remote refused the push")
            .with_detail(json!({ "kind": "PushRefused", "message": m }))),
        PushOutcome::Unknown => Err(unknown_with("push_unknown", "the push did not finish; its outcome is unknown", None, json!({}))),
    }
}

// ---- publish (§5.2) ------------------------------------------------------

/// What the locked section hands to steps 8–9.
struct Landed {
    commit: CommitOid,
    checks_ran: u32,
    push: Option<PushOutcome>,
    staged: Staged,
}

fn now() -> SystemTime {
    SystemTime::now()
}

fn busy(reason: &str, message: String, id: u64) -> StrataError {
    StrataError::new(Code::Busy, message).with_detail(json!({ "reason": reason, "publish": id }))
}

/// §6: a `Running` row in the same scope (a session of this group, this
/// repo) or any `Unknown` row of this repo refuses the call. Rows are
/// never overwritten and never replayed.
fn refuse_open(ctx: &Ctx, sessions: &BTreeSet<SessionId>, own: Option<u64>, unknown_only: bool) -> Result<()> {
    for r in ctx.store.open_publishes()? {
        if Some(r.group.id) == own {
            continue;
        }
        let mine: Vec<_> = r.group.batches.iter().filter(|b| b.repo == *ctx.repo).collect();
        if mine.is_empty() {
            continue;
        }
        if r.phase == PublishPhase::Unknown {
            return Err(busy(
                "publish_unknown",
                format!("publish {} of {} has an unknown outcome; see status", r.group.id, ctx.repo),
                r.group.id,
            ));
        }
        if !unknown_only && mine.iter().any(|b| sessions.contains(&b.session)) {
            return Err(busy("publish_open", format!("publish {} of {} is still open", r.group.id, ctx.repo), r.group.id));
        }
    }
    Ok(())
}

/// One `outcome.rebase` element.
fn report_json(s: &SessionId, r: &RebaseReport) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
    v["session"] = json!(s.as_str());
    if let RebaseReport::Waiting { proposal } = r {
        v["proposal"] = json!(proposal.id);
    }
    v
}

fn outcome_base(ctx: &Ctx) -> Value {
    json!({ "repo": ctx.repo.as_str(), "commit": null, "checks_ran": null, "push": null, "warnings": [], "rebase": [] })
}

/// Close the row after a failure: named and nothing written → `Done` with
/// `stopped` (D16); `Unknown` or anything after the commit → `Unknown`.
/// Returns the error the caller reports.
fn close_failed(ctx: &Ctx, id: u64, written: Option<&CommitOid>, e: StrataError) -> StrataError {
    let mut e = match written {
        Some(c) => after_write(c, e),
        None => e,
    };
    // §5.7: the row id is what `assign` names.
    if e.code == Code::Conflict
        && let Some(d) = e.detail.as_mut()
        && d.get("publish").is_none()
    {
        d["publish"] = json!(id);
    }
    let mut o = outcome_base(ctx);
    o["commit"] = json!(written.map(|c| c.as_str()));
    let to = if is_bare_refusal(&e) {
        // Review P0-2 (publish.md D-refused): the remote said no. The
        // commit stays on the local chain and nothing is in flight, so
        // the outcome is definite: the row closes `Done` with the refusal
        // and does not block the repo's next publish.
        o["stopped"] = json!(Code::PushRefused.as_str());
        if let Some(d) = e.detail.as_ref() {
            o["push"] = d.get("push").cloned().unwrap_or(Value::Null);
            o["checks_ran"] = d.get("checks_ran").cloned().unwrap_or(Value::Null);
            o["message"] = d.get("message").cloned().unwrap_or(Value::Null);
        }
        PublishPhase::Done
    } else if e.code == Code::Unknown {
        o["error"] = err_json(&e);
        PublishPhase::Unknown
    } else {
        o["stopped"] = json!(e.code.as_str());
        // §5.7: `assign` names a resolver for one of these paths.
        if e.code == Code::Conflict
            && let Some(c) = e.detail.as_ref().and_then(|d| d.get("conflicts"))
        {
            o["conflicts"] = c.clone();
        }
        PublishPhase::Done
    };
    match ctx.store.set_publish_phase(id, PublishPhase::Running, to, Some(&o), now()) {
        Ok(true) => e,
        Ok(false) => unknown_with("row_moved", "the publish row was not Running when it was closed", Some(&e), carry(&e, json!({ "publish": id }))),
        Err(se) => {
            tracing::warn!(publish = id, error = %se.message, "publish row could not be closed");
            unknown_with("row_not_closed", "the publish row could not be closed", Some(&e), carry(&e, json!({ "publish": id, "store": err_json(&se) })))
        }
    }
}

/// The definite refusal `locked` returns after the commit: `Unknown` with
/// `detail.kind = "PushRefused"` and no `reason` (anything wrapped after
/// it carries a `reason` and stays unknown).
fn is_bare_refusal(e: &StrataError) -> bool {
    e.code == Code::Unknown
        && e.detail
            .as_ref()
            .is_some_and(|d| d.get("kind").and_then(Value::as_str) == Some("PushRefused") && d.get("reason").is_none())
}

/// The commit an `Unknown` row wrote: `outcome.commit`, else the applied
/// chain-ref update recorded for the row (a row closed by startup has no
/// outcome).
fn row_commit(ctx: &Ctx, r: &strata_store::PublishRecord) -> Option<CommitOid> {
    if let Some(c) = r.outcome.as_ref().and_then(|o| o.get("commit")).and_then(Value::as_str) {
        return CommitOid::try_from(c.to_owned()).ok();
    }
    let changes = ctx.store.ref_changes(r.group.id).ok()?;
    changes.into_iter().rev().find(|c| c.op == RefOp::UpdateRef && c.outcome == RefOutcome::Applied).map(|c| c.new)
}

/// Review P0-2: the way out of an `Unknown` row. Before the §6 check, the
/// repo's `Unknown` rows are reconciled against the remote under the repo
/// lock: a row whose commit is now contained in `origin/<branch>` (fetched
/// here) did land, so it closes `Done` with `push = Pushed` and
/// `reconciled` naming the remote tip. A row whose commit is not on the
/// remote stays `Unknown` (status keeps reporting it); the human pushes
/// the chain with `git-strata push REPO` (which pushes that commit when
/// it is still on the chain) and the next publish reconciles. Nothing is
/// replayed. Fetch or git failures leave the rows as they are.
fn reconcile_unknown(ctx: &Ctx) {
    let Ok(open) = ctx.store.open_publishes() else { return };
    let rows: Vec<_> = open
        .into_iter()
        .filter(|r| r.phase == PublishPhase::Unknown && r.group.batches.iter().any(|b| b.repo == *ctx.repo))
        .collect();
    if rows.is_empty() {
        return;
    }
    let branch = &ctx.cfg.publish.branch;
    let r = ctx.locks.with_repo(ctx.repo, &mut || {
        let tip = ctx.git.fetch(REMOTE, branch)?;
        for row in &rows {
            let Some(c) = row_commit(ctx, row) else { continue };
            if !ctx.git.is_ancestor(&c, &tip).unwrap_or(false) {
                continue;
            }
            let mut o = row.outcome.clone().filter(Value::is_object).unwrap_or_else(|| outcome_base(ctx));
            o["commit"] = json!(c.as_str());
            o["push"] = serde_json::to_value(PushOutcome::Pushed).unwrap_or(Value::Null);
            o["reconciled"] = json!({ "remote": tip.as_str(), "branch": branch });
            match ctx.store.set_publish_phase(row.group.id, PublishPhase::Unknown, PublishPhase::Done, Some(&o), now()) {
                Ok(_) => tracing::info!(publish = row.group.id, commit = c.as_str(), "unknown publish reconciled: the remote has the commit"),
                Err(e) => tracing::warn!(publish = row.group.id, error = %e.message, "unknown publish not reconciled"),
            }
        }
        Ok(())
    });
    if let Err(e) | Ok(Err(e)) = r {
        tracing::warn!(repo = %ctx.repo, error = %e.message, "unknown publishes not reconciled");
    }
}

fn message(ctx: &Ctx, group: &ReleaseGroup) -> String {
    let sessions: BTreeSet<&str> = group.batches.iter().map(|b| b.session.as_str()).collect();
    let paths: usize = group.batches.iter().map(|b| b.paths.len()).sum();
    format!(
        "strata publish {} ({})\n\nsessions: {}\npaths: {paths}\n",
        group.id,
        ctx.repo,
        sessions.into_iter().collect::<Vec<_>>().join(", ")
    )
}

fn record_ref(ctx: &Ctx, change: RefChange) {
    if let Err(e) = ctx.store.record_ref_change(&change) {
        tracing::warn!(ref_name = change.ref_name, error = %e.message, "ref change not recorded");
    }
}

/// Steps 2 (inside the lock) to 7. `written` is set once `commit_tree`
/// returned.
fn locked(
    ctx: &Ctx,
    group: &ReleaseGroup,
    sessions: &BTreeSet<SessionId>,
    confirm: Option<ConfirmToken>,
    shown: Option<&Option<CommitOid>>,
    written: &mut Option<CommitOid>,
) -> Result<Landed> {
    let branch = &ctx.cfg.publish.branch;
    let id = group.id;
    refuse_open(ctx, sessions, Some(id), true)?;
    ctx.git.fetch(REMOTE, branch)?;
    advance_anchor(ctx.git, branch)?;
    let chain = publish_chain(ctx.git, branch)?.ok_or_else(|| {
        StrataError::new(Code::NoSuchBranch, format!("origin/{branch} has not been fetched"))
            .with_detail(json!({ "branch": branch, "remote": REMOTE }))
    })?;
    refuse_state(&chain)?;
    // D17: B comes from the chain read in this lock.
    let base = match (chain.state, &chain.top) {
        (ChainState::Leads, Some(t)) => t.clone(),
        _ => chain.origin.clone(),
    };
    // Step 3: the gate, before any staging write.
    ctx.cfg.validate_commands()?;
    let pushing = ctx.cfg.publish.push;
    let confirmed = if pushing { confirm_gate(ctx.cfg, ctx.repo, confirm)? } else { None };
    if let (Some(_), Some(shown)) = (&confirmed, shown)
        && *shown != chain.top
    {
        return Err(StrataError::new(Code::NotConfirmed, "the publish chain moved after it was shown for the confirmation")
            .with_detail(json!({
                "reason": "chain_moved",
                "repo": ctx.repo.as_str(),
                "confirmed": shown.as_ref().map(|c| c.as_str()),
                "now": chain.top.as_ref().map(|c| c.as_str()),
            })));
    }
    let plan = stage::plan(ctx, group, &base)?;
    // Steps 4–5.
    let staged = stage::build(ctx, group, &base, &plan)?;
    let ran = check(ctx.cfg, &staged.worktree)?;
    // Step 6: the first visible write.
    let expected = chain.top.clone();
    let r = commit_into(ctx.git, branch, &staged, &base, expected.as_ref(), &message(ctx, group), written);
    if let Some(c) = written.as_ref() {
        let head_moved = matches!(&r, Err(e) if e.detail.as_ref().and_then(|d| d.get("reason")) == Some(&json!("head_moved")));
        if r.is_ok() || head_moved {
            record_ref(ctx, RefChange {
                at: now(),
                repo: ctx.repo.clone(),
                op: RefOp::UpdateRef,
                ref_name: chain_ref(branch),
                old: expected.clone(),
                new: c.clone(),
                outcome: if head_moved { RefOutcome::HeadMoved } else { RefOutcome::Applied },
                publish: Some(id),
                confirmed: None,
                detail: None,
            });
        }
    }
    // Review-3 R3: a failure once the object exists (the chain-ref
    // transaction) still says no push was sent and how many checks ran.
    let commit = r.map_err(|e| if written.is_some() { with_landed(e, Value::Null, ran.ran) } else { e })?;
    // Step 7.
    seams::fail(ctx.git, FailPoint::BeforePush).map_err(|e| with_landed(after_write(&commit, e), Value::Null, ran.ran))?;
    let push = if pushing {
        let out = send(ctx.git, branch, &staged.worktree, None);
        let (outcome, detail) = match &out {
            Ok(PushOutcome::Pushed) => (RefOutcome::Pushed, None),
            Ok(PushOutcome::Refused(m)) => (RefOutcome::Refused, Some(m.clone())),
            Ok(PushOutcome::Unknown) => (RefOutcome::Unknown, None),
            Err(e) => (RefOutcome::Unknown, Some(e.message.clone())),
        };
        record_ref(ctx, RefChange {
            at: now(),
            repo: ctx.repo.clone(),
            op: RefOp::Push,
            ref_name: format!("refs/heads/{branch}"),
            old: chain.origin.clone().into(),
            new: commit.clone(),
            outcome,
            publish: Some(id),
            confirmed: confirmed.clone(),
            detail,
        });
        let unknown = || serde_json::to_value(PushOutcome::Unknown).unwrap_or(Value::Null);
        match out.map_err(|e| with_landed(after_write(&commit, e), unknown(), ran.ran))? {
            PushOutcome::Pushed => Some(PushOutcome::Pushed),
            PushOutcome::Refused(m) => {
                let push = serde_json::to_value(PushOutcome::Refused(m.clone())).unwrap_or(Value::Null);
                return Err(StrataError::new(Code::Unknown, "the remote refused the push; the commit stays on the local chain").with_detail(
                    json!({ "kind": "PushRefused", "message": m, "commit": commit.as_str(), "push": push, "checks_ran": ran.ran }),
                ));
            }
            PushOutcome::Unknown => {
                return Err(unknown_with(
                    "push_unknown",
                    "the push did not finish; the chain stays and nothing is replayed",
                    None,
                    json!({ "kind": "Unknown", "commit": commit.as_str(), "push": unknown(), "checks_ran": ran.ran }),
                ));
            }
        }
    } else {
        None
    };
    Ok(Landed { commit, checks_ran: ran.ran, push, staged })
}

/// §3 `publish`: one commit for `ctx.repo`'s batches of `group` (§5.2).
/// The row id (`record_publish`) is the release id; `group.id` is ignored.
pub fn publish(ctx: &Ctx, group: &ReleaseGroup, confirm: Option<ConfirmToken>) -> Result<Published> {
    publish_bound(ctx, group, confirm, None)
}

/// [`publish`] bound to a shown plan (review H2): with `shown =
/// Some(top)` and a push that is confirmed, the chain top read under the
/// repo lock (after the fetch) must be `top` (`None`: no local chain);
/// otherwise `NotConfirmed` (reason `chain_moved`) before any staging or
/// commit. [实现时补的决定] the publish then pushes exactly the shown
/// unpushed commits plus this batch.
pub fn publish_bound(
    ctx: &Ctx,
    group: &ReleaseGroup,
    confirm: Option<ConfirmToken>,
    shown: Option<Option<CommitOid>>,
) -> Result<Published> {
    let mine = ReleaseGroup {
        id: group.id,
        batches: group.batches.iter().filter(|b| b.repo == *ctx.repo).cloned().collect(),
    };
    if mine.batches.is_empty() {
        return Err(StrataError::new(Code::InvalidInput, format!("the release group has no batch for {}", ctx.repo))
            .with_detail(json!({ "reason": "no_batches", "repo": ctx.repo.as_str() })));
    }
    let branch = &ctx.cfg.publish.branch;
    // Step 1: outside the lock, before any write.
    if let Some(c) = publish_chain(ctx.git, branch)?
        && c.state == ChainState::Rewound
    {
        refuse_state(&c)?;
    }
    let sessions: BTreeSet<SessionId> = mine.batches.iter().map(|b| b.session.clone()).collect();
    let queue: Vec<SessionId> = sessions.iter().cloned().collect();

    reconcile_unknown(ctx);
    // Step 2: queue, row, lock.
    let mut confirm = confirm;
    let mut slot: Option<Result<(u64, Landed)>> = None;
    ctx.locks.with_sessions(&queue, &mut || {
        slot = Some((|| {
            refuse_open(ctx, &sessions, None, false)?;
            let id = ctx.store.record_publish(&mine, now())?;
            let group = ReleaseGroup { id, batches: mine.batches.clone() };
            let mut written: Option<CommitOid> = None;
            let mut inner: Option<Result<Landed>> = None;
            let r = ctx.locks.with_repo(ctx.repo, &mut || {
                inner = Some(locked(ctx, &group, &sessions, confirm.take(), shown.as_ref(), &mut written));
                Ok(())
            });
            let r = match r {
                Err(e) => Err(e),
                Ok(Err(e)) => Err(e),
                Ok(Ok(())) => inner.unwrap_or_else(|| Err(unknown_with("lock", "the repo lock never ran", None, json!({})))),
            };
            match r {
                Ok(l) => Ok((id, l)),
                Err(e) => Err(close_failed(ctx, id, written.as_ref(), e)),
            }
        })());
        Ok(())
    })??;
    let (id, landed) = slot.unwrap_or_else(|| Err(unknown_with("queue", "the session queue never ran", None, json!({}))))?;

    // Step 8: lock and queue released; observe main, then the rebase.
    let mut warnings = Vec::new();
    if let Err(e) = strata_deps::observe_main(ctx.deps, &landed.staged.tree, &landed.commit, &landed.staged.worktree) {
        warnings.push(PublishWarning { kind: WarningKind::ObserveFailed, detail: err_json(&e) });
    }
    // The rebase (§5.8), one session at a time in ascending order, each
    // under its queue place and the lock again; waits here are after the
    // first visible write and are not bounded. The first failure stops the
    // rest and the row goes `Unknown` (the commit stays the success).
    let mut rebase: Vec<Value> = Vec::new();
    let mut rebases: Vec<(SessionId, RebaseReport)> = Vec::new();
    let post = (|| -> Result<()> {
        for s in &queue {
            seams::fail(ctx.git, FailPoint::BeforeRebase)?;
            seams::hit(ctx.git, Point::BeforeRebase);
            let r = crate::rebase::after_publish(ctx, s, &landed.staged.tree, &landed.commit)?;
            rebase.push(report_json(s, &r));
            rebases.push((s.clone(), r));
        }
        Ok(())
    })();

    // Step 9.
    let mut o = outcome_base(ctx);
    o["commit"] = json!(landed.commit.as_str());
    o["checks_ran"] = json!(landed.checks_ran);
    o["push"] = serde_json::to_value(&landed.push).unwrap_or(Value::Null);
    o["warnings"] = serde_json::to_value(&warnings).unwrap_or(json!([]));
    o["rebase"] = json!(rebase);
    o["paths"] = json!(landed.staged.paths.iter().map(|p| p.as_str()).collect::<Vec<_>>());
    let fin = post
        .and_then(|()| seams::fail(ctx.git, FailPoint::BeforeDone))
        .and_then(|()| remove_staged_layout(&landed.staged.worktree));
    if let Err(e) = fin {
        let e = with_landed(after_write(&landed.commit, e), o["push"].clone(), landed.checks_ran);
        o["error"] = err_json(&e);
        if let Err(se) = ctx.store.set_publish_phase(id, PublishPhase::Running, PublishPhase::Unknown, Some(&o), now()) {
            tracing::warn!(publish = id, error = %se.message, "publish row could not be marked unknown");
        }
        return Err(e);
    }
    match ctx.store.set_publish_phase(id, PublishPhase::Running, PublishPhase::Done, Some(&o), now()) {
        Ok(true) => {}
        Ok(false) => {
            return Err(unknown_with(
                "row_moved",
                "the publish row was not Running at the end",
                None,
                json!({ "publish": id, "commit": landed.commit.as_str(), "push": o["push"].clone(), "checks_ran": landed.checks_ran }),
            ));
        }
        Err(e) => return Err(with_landed(after_write(&landed.commit, e), o["push"].clone(), landed.checks_ran)),
    }
    Ok(Published { id, commit: landed.commit, checks_ran: landed.checks_ran, push: landed.push, warnings, rebases })
}

//! Approval targets and rollout plans (§5.10).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_store::{NewProposal, ProposalDecision, Store};
use strata_types::{
    BaseGen, Code, CommitOid, DepsSet, ProposalId, SessionId, StrataError, WorkspaceState,
};

use crate::drift::Drift;
use crate::open_files::{OpenFile, scan};
use crate::proposal::{
    Proposal, ProposalOrigin, ProposalState, built, from_record, get_proposal, main_tip,
};
use crate::{Ctx, Result, ensure, find_set, inputs_hash};

/// `ensure_target`'s result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Target {
    /// The proposal's `to` is sealed.
    Ready { proposal: Proposal },
    /// The sealed set is not `to`: `superseded` was replaced by `replacement`
    /// and nothing moves this time (D10).
    Replaced {
        superseded: ProposalId,
        replacement: Proposal,
    },
}

/// One approval's (or one self-switch's) plan: one step per session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutPlan {
    pub proposal: ProposalId,
    pub repo: strata_types::RepoName,
    /// The plan covers every session of the repo; only such a plan can make
    /// the proposal Applied.
    pub complete: bool,
    pub steps: Vec<RolloutStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum RolloutStep {
    /// Hand to `session::move_workspace(session, <RepoId>, new_base, head)`.
    Move {
        session: SessionId,
        new_base: BaseGen,
        head: CommitOid,
    },
    /// Leave the session alone.
    Skip {
        session: SessionId,
        reason: SkipReason,
    },
}

/// Why a session is skipped; decided in §5.10's order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SkipReason {
    Untracked,
    AlreadyThere,
    NotReady,
    OwnChange { proposal: ProposalId },
    Drift { drift: Drift },
    OpenFiles { files: Vec<OpenFile> },
    Error { error: StrataError },
}

fn state_json(s: &ProposalState) -> serde_json::Value {
    serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
}

fn not_pending(p: &Proposal) -> StrataError {
    StrataError::new(
        Code::NoSuchProposal,
        format!("proposal {} is not pending", p.id.get()),
    )
    .with_detail(json!({ "proposal": p.id.get(), "state": state_json(&p.state) }))
}

fn invalid(reason: &str, message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

/// A Pending proposal of `ctx.repo`, or the named refusal.
fn pending_of(ctx: &Ctx, id: ProposalId) -> Result<Proposal> {
    let p = get_proposal(ctx.store, id)?;
    if p.state != ProposalState::Pending {
        return Err(not_pending(&p));
    }
    if p.repo != *ctx.repo {
        return Err(invalid(
            "wrong_repo",
            format!("proposal {} is not of repo {}", id.get(), ctx.repo.as_str()),
        ));
    }
    Ok(p)
}

/// The proposal's head must still be main's tip (§5.10 step 1).
fn check_tip(ctx: &Ctx, p: &Proposal) -> Result<()> {
    let tip = main_tip(ctx.cfg, ctx.git)?;
    if p.head.is_some() && tip == p.head {
        return Ok(());
    }
    Err(StrataError::new(
        Code::HeadMoved,
        "the proposal's head is no longer main's tip",
    )
    .with_detail(json!({
        "reason": "main_moved",
        "proposal": p.id.get(),
        "head": p.head.as_ref().map(|h| h.as_str()),
        "tip": tip.as_ref().map(|t| t.as_str()),
    })))
}

/// §5.10 `ensure_target`: make the proposal's `to` sealed, or file the
/// replacement when the sealed set differs.
pub fn ensure_target(ctx: &Ctx, id: ProposalId, tree_dir: &Path) -> Result<Target> {
    // 1.
    let p = pending_of(ctx, id)?;
    // 2.
    if find_set(ctx.store, &p.repo, &p.to)?.is_some() {
        return Ok(Target::Ready { proposal: p });
    }
    // 3. Outside the repo write lock (may cold-install).
    let set = ensure(ctx, tree_dir)?;
    if set.id == p.to {
        return Ok(Target::Ready { proposal: p });
    }
    // 4. Under the repo write lock.
    let new = NewProposal {
        repo: p.repo.clone(),
        origin: match &p.origin {
            ProposalOrigin::Main => None,
            ProposalOrigin::Session { session } => Some(session.clone()),
        },
        from: p.from.clone(),
        from_inputs: p.from_inputs.clone(),
        to: set.id.clone(),
        to_inputs: set.inputs_hash.clone(),
        tree: p.tree.clone(),
        head: p.head.clone(),
    };
    let mut out: Option<Option<(ProposalId, SystemTime)>> = None;
    ctx.locks.with_write(ctx.repo, &mut || {
        let now = SystemTime::now();
        out = Some(
            ctx.store
                .record_proposal(&new, Some(p.id), now)?
                .map(|q| (q, now)),
        );
        Ok(())
    })??;
    match out.flatten() {
        Some((q, now)) => Ok(Target::Replaced {
            superseded: p.id,
            replacement: built(q, new, now),
        }),
        None => {
            // P was superseded meanwhile: report its current state.
            let cur = get_proposal(ctx.store, p.id)?;
            Err(not_pending(&cur))
        }
    }
}

/// A session waiting for the open-file check.
struct Candidate {
    index: usize,
    dirs: Vec<PathBuf>,
}

/// The directories whose open files block a move: `cfg.deps_dirs`, the
/// pinned set's and the target set's dirs, under the workspace.
fn deps_dirs_of(ctx: &Ctx, ws: &WorkspaceState, pinned: &DepsSet, to: &DepsSet) -> Vec<PathBuf> {
    let mut rel: BTreeSet<&str> = BTreeSet::new();
    for d in ctx.cfg.deps_dirs.iter().chain(&pinned.dirs).chain(&to.dirs) {
        rel.insert(d.as_str());
    }
    rel.into_iter().map(|r| ws.path.join(r)).collect()
}

/// One process scan for every candidate (§5.10 step 4); fills their steps.
fn scan_candidates(
    steps: &mut [Option<RolloutStep>],
    cands: Vec<Candidate>,
    sessions: &[(SessionId, Move)],
) {
    let all: Vec<&Path> = cands
        .iter()
        .flat_map(|c| c.dirs.iter().map(PathBuf::as_path))
        .collect();
    let results = scan(&all);
    let mut at = 0;
    for c in cands {
        let (session, mv) = &sessions[c.index];
        let n = c.dirs.len();
        let step = match &results {
            Err(e) => skip(session, SkipReason::Error { error: e.clone() }),
            Ok(rs) => {
                let mut files: Vec<OpenFile> = Vec::new();
                let mut err: Option<StrataError> = None;
                for r in &rs[at..at + n] {
                    match r {
                        Ok(v) => files.extend(v.iter().cloned()),
                        Err(e) => {
                            err.get_or_insert_with(|| e.clone());
                        }
                    }
                }
                if let Some(error) = err {
                    skip(session, SkipReason::Error { error })
                } else if !files.is_empty() {
                    files.sort_by(|a, b| (a.pid, &a.path).cmp(&(b.pid, &b.path)));
                    files.dedup();
                    skip(session, SkipReason::OpenFiles { files })
                } else {
                    RolloutStep::Move {
                        session: session.clone(),
                        new_base: mv.0.clone(),
                        head: mv.1.clone(),
                    }
                }
            }
        };
        at += n;
        steps[c.index] = Some(step);
    }
}

/// (new base, head) of a Move.
type Move = (BaseGen, CommitOid);

fn skip(session: &SessionId, reason: SkipReason) -> RolloutStep {
    RolloutStep::Skip {
        session: session.clone(),
        reason,
    }
}

fn corrupt_pinned(ws: &WorkspaceState) -> StrataError {
    StrataError::new(
        Code::Corrupt,
        format!(
            "the pinned deps set {} has no record",
            ws.base.deps.as_str()
        ),
    )
    .with_detail(json!({ "repo": ws.base.repo.as_str(), "set": ws.base.deps.as_str() }))
}

/// §5.10 `plan_rollout`: read-only.
pub fn plan_rollout(
    ctx: &Ctx,
    id: ProposalId,
    sessions: &[(SessionId, WorkspaceState)],
    complete: bool,
) -> Result<RolloutPlan> {
    // 1.
    let p = pending_of(ctx, id)?;
    if let Some((s, _)) = sessions.iter().find(|(_, ws)| ws.base.repo != p.repo) {
        return Err(invalid(
            "wrong_repo",
            format!(
                "session {}'s workspace is not of repo {}",
                s.as_str(),
                p.repo.as_str()
            ),
        ));
    }
    check_tip(ctx, &p)?;
    let (Some(tree), Some(head)) = (p.tree.clone(), p.head.clone()) else {
        return Err(invalid("no_target", "a pending proposal without a target"));
    };
    // 2.
    let target = BaseGen {
        repo: p.repo.clone(),
        tree,
        deps: p.to.clone(),
    };
    let to_set = find_set(ctx.store, &p.repo, &p.to)?;
    let drafts: Vec<Proposal> = ctx
        .store
        .proposals(Some(&p.repo), true)?
        .into_iter()
        .map(from_record)
        .filter(|q| q.state == ProposalState::AwaitingPublish && q.id != p.id)
        .collect();
    // 3.
    let moves: Vec<(SessionId, Move)> = sessions
        .iter()
        .map(|(s, _)| (s.clone(), (target.clone(), head.clone())))
        .collect();
    let mut steps: Vec<Option<RolloutStep>> = vec![None; sessions.len()];
    let mut cands: Vec<Candidate> = Vec::new();
    for (i, (s, ws)) in sessions.iter().enumerate() {
        let step = (|| -> std::result::Result<Vec<PathBuf>, SkipReason> {
            if !ws.tracked {
                return Err(SkipReason::Untracked);
            }
            if ws.base.deps == p.to {
                return Err(SkipReason::AlreadyThere);
            }
            let Some(to_set) = &to_set else {
                return Err(SkipReason::NotReady);
            };
            let error = |error: StrataError| SkipReason::Error { error };
            let mut found: Option<String> = None;
            let own = drafts.iter().find(|q| {
                q.origin == ProposalOrigin::Session { session: s.clone() }
                    && q.to_inputs != p.to_inputs
            });
            if let Some(q) = own {
                let f = inputs_hash(ctx.cfg, ctx.git, &ws.path).map_err(error)?;
                let base_inputs =
                    inputs_hash(ctx.cfg, ctx.git, &ctx.store.base_dir(&ws.base)).map_err(error)?;
                if f != base_inputs && f != p.to_inputs {
                    return Err(SkipReason::OwnChange { proposal: q.id });
                }
                found = Some(f);
            }
            let pinned = match find_set(ctx.store, &ws.base.repo, &ws.base.deps) {
                Ok(Some(set)) => set,
                Ok(None) => return Err(error(corrupt_pinned(ws))),
                Err(e) => return Err(error(e)),
            };
            let found = match found {
                Some(f) => f,
                None => inputs_hash(ctx.cfg, ctx.git, &ws.path).map_err(error)?,
            };
            if found != pinned.inputs_hash && found != p.to_inputs {
                return Err(SkipReason::Drift {
                    drift: Drift {
                        pinned: pinned.id.clone(),
                        pinned_inputs: pinned.inputs_hash.clone(),
                        found_inputs: found,
                    },
                });
            }
            Ok(deps_dirs_of(ctx, ws, &pinned, to_set))
        })();
        match step {
            Err(reason) => steps[i] = Some(skip(s, reason)),
            Ok(dirs) => cands.push(Candidate { index: i, dirs }),
        }
    }
    // 4.
    if !cands.is_empty() {
        scan_candidates(&mut steps, cands, &moves);
    }
    // 5.
    Ok(RolloutPlan {
        proposal: id,
        repo: p.repo,
        complete,
        steps: steps
            .into_iter()
            .map(|s| s.expect("every session has a step"))
            .collect(),
    })
}

/// §5.10 `plan_switch`: the proposer's own switch, without confirmation.
pub fn plan_switch(
    ctx: &Ctx,
    id: ProposalId,
    s: &SessionId,
    ws: &WorkspaceState,
) -> Result<RolloutPlan> {
    // 1.
    let p = get_proposal(ctx.store, id)?;
    let refuse = |reason: &str| {
        StrataError::new(
            Code::NoSuchProposal,
            format!(
                "session {} has no open proposal {} to switch to",
                s.as_str(),
                id.get()
            ),
        )
        .with_detail(
            json!({ "proposal": id.get(), "state": state_json(&p.state), "reason": reason }),
        )
    };
    if p.origin != (ProposalOrigin::Session { session: s.clone() }) {
        return Err(refuse("not_origin"));
    }
    if !matches!(
        p.state,
        ProposalState::AwaitingPublish | ProposalState::Pending
    ) {
        return Err(refuse("not_open"));
    }
    if p.repo != *ctx.repo || ws.base.repo != p.repo {
        return Err(invalid(
            "wrong_repo",
            format!("proposal {} is not of this repo", id.get()),
        ));
    }
    // 2.
    let (target, head) = if p.state == ProposalState::Pending {
        check_tip(ctx, &p)?;
        let (Some(tree), Some(head)) = (p.tree.clone(), p.head.clone()) else {
            return Err(invalid("no_target", "a pending proposal without a target"));
        };
        (
            BaseGen {
                repo: p.repo.clone(),
                tree,
                deps: p.to.clone(),
            },
            head,
        )
    } else {
        (
            BaseGen {
                repo: p.repo.clone(),
                tree: ws.base.tree.clone(),
                deps: p.to.clone(),
            },
            ws.head.clone(),
        )
    };
    // 3.
    let error = |error: StrataError| SkipReason::Error { error };
    let decided = (|| -> std::result::Result<Vec<PathBuf>, SkipReason> {
        if !ws.tracked {
            return Err(SkipReason::Untracked);
        }
        if ws.base == target {
            return Err(SkipReason::AlreadyThere);
        }
        let to_set = match find_set(ctx.store, &p.repo, &p.to) {
            Ok(Some(set)) => set,
            Ok(None) => return Err(SkipReason::NotReady),
            Err(e) => return Err(error(e)),
        };
        let found = inputs_hash(ctx.cfg, ctx.git, &ws.path).map_err(error)?;
        if found != p.to_inputs {
            return Err(SkipReason::Drift {
                drift: Drift {
                    pinned: p.to.clone(),
                    pinned_inputs: p.to_inputs.clone(),
                    found_inputs: found,
                },
            });
        }
        let pinned = match find_set(ctx.store, &ws.base.repo, &ws.base.deps) {
            Ok(Some(set)) => set,
            Ok(None) => return Err(error(corrupt_pinned(ws))),
            Err(e) => return Err(error(e)),
        };
        Ok(deps_dirs_of(ctx, ws, &pinned, &to_set))
    })();
    let mut steps = vec![None];
    match decided {
        Err(reason) => steps[0] = Some(skip(s, reason)),
        Ok(dirs) => scan_candidates(
            &mut steps,
            vec![Candidate { index: 0, dirs }],
            &[(s.clone(), (target, head))],
        ),
    }
    // 4.
    Ok(RolloutPlan {
        proposal: id,
        repo: p.repo,
        complete: false,
        steps: steps.into_iter().map(|s| s.expect("one step")).collect(),
    })
}

/// §5.10 `record_rollout`: Applied only for a complete, fully successful plan.
pub fn record_rollout(store: &Store, plan: &RolloutPlan, moved: &[SessionId]) -> Result<Proposal> {
    // 1.
    let p = get_proposal(store, plan.proposal)?;
    if p.state != ProposalState::Pending {
        return Ok(p);
    }
    // 2.
    let done = plan.complete
        && plan.steps.iter().all(|s| match s {
            RolloutStep::Move { session, .. } => moved.contains(session),
            RolloutStep::Skip { reason, .. } => {
                matches!(reason, SkipReason::Untracked | SkipReason::AlreadyThere)
            }
        });
    if done {
        store.decide_proposal(p.id, ProposalDecision::Applied, SystemTime::now())?;
    }
    // 3.
    get_proposal(store, p.id)
}

//! Deps change proposals, the publish chain and main tracking (§4.5, §5.9).

use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_gitio::{Git, remote_ref};
use strata_store::{DepsMain, NewProposal, ProposalRecord, ProposalStatus, Store};
use strata_types::{
    Code, CommitOid, DepsSetId, ProposalId, RepoConfig, RepoName, SessionId, StrataError, TreeOid,
};

use crate::layout::unknown;
use crate::{Ctx, Result, key_of, seams};

/// How many times a compare-and-swap conflict is re-read and retried.
const RETRIES: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub repo: RepoName,
    pub origin: ProposalOrigin,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<CommitOid>,
    pub state: ProposalState,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub created: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalOrigin {
    Main,
    Session { session: SessionId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalState {
    AwaitingPublish,
    Pending,
    Applied,
    Superseded { by: ProposalId },
}

/// `propose`'s arguments; the repo comes from `Ctx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposeArgs {
    pub origin: ProposalOrigin,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    pub tree: Option<TreeOid>,
    pub head: Option<CommitOid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishChain {
    pub branch: String,
    pub origin: CommitOid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<CommitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommitOid>,
    pub state: ChainState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainState {
    Absent,
    Rewound,
    Equal,
    Leads,
    Behind,
    Diverged,
}

fn invalid(reason: &str, message: &str) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

fn busy(reason: &str, message: &str) -> StrataError {
    StrataError::new(Code::Busy, message).with_detail(json!({ "reason": reason }))
}

fn origin_of(o: &ProposalOrigin) -> Option<SessionId> {
    match o {
        ProposalOrigin::Main => None,
        ProposalOrigin::Session { session } => Some(session.clone()),
    }
}

pub(crate) fn from_record(r: ProposalRecord) -> Proposal {
    let state = match r.status {
        ProposalStatus::AwaitingPublish => ProposalState::AwaitingPublish,
        ProposalStatus::Pending => ProposalState::Pending,
        ProposalStatus::Applied => ProposalState::Applied,
        ProposalStatus::Superseded => match r.superseded_by {
            Some(by) => ProposalState::Superseded { by },
            // The schema forbids this; report it as Pending-less history.
            None => ProposalState::Applied,
        },
    };
    Proposal {
        id: r.id,
        repo: r.repo,
        origin: match r.origin {
            Some(session) => ProposalOrigin::Session { session },
            None => ProposalOrigin::Main,
        },
        from: r.from,
        from_inputs: r.from_inputs,
        to: r.to,
        to_inputs: r.to_inputs,
        tree: r.tree,
        head: r.head,
        state,
        created: r.created,
    }
}

/// One proposal by id; `NoSuchProposal` when the store has none.
pub fn get_proposal(store: &Store, id: ProposalId) -> Result<Proposal> {
    match store.proposal(id)? {
        Some(r) => Ok(from_record(r)),
        None => Err(
            StrataError::new(Code::NoSuchProposal, format!("no proposal {}", id.get()))
                .with_detail(json!({ "proposal": id.get() })),
        ),
    }
}

/// The open proposals (Pending, and AwaitingPublish of live sessions), by id.
pub fn open_proposals(store: &Store, repo: Option<&RepoName>) -> Result<Vec<Proposal>> {
    Ok(store
        .proposals(repo, true)?
        .into_iter()
        .map(from_record)
        .collect())
}

/// What one round of `propose` decided.
enum Step {
    Done(Box<Proposal>),
    Retry,
}

/// Register, promote or deduplicate a proposal (§5.9). The caller holds the
/// repo write lock; the store's compare-and-swap is only a backstop.
pub fn propose(ctx: &Ctx, args: ProposeArgs) -> Result<Proposal> {
    if args.from == args.to {
        return Err(invalid(
            "from_equals_to",
            "a proposal's from and to are the same deps set",
        ));
    }
    let target = match (&args.tree, &args.head) {
        (Some(t), Some(h)) => Some((t.clone(), h.clone())),
        (None, None) => None,
        _ => {
            return Err(invalid(
                "tree_head_mismatch",
                "tree and head must be given together",
            ));
        }
    };
    if target.is_none() && args.origin == ProposalOrigin::Main {
        return Err(invalid(
            "no_target",
            "a proposal from main needs a target tree and head",
        ));
    }
    if let Some((tree, head)) = &target
        && ctx.git.tree_of(head)? != *tree
    {
        return Err(invalid(
            "tree_mismatch",
            "the head's tree is not the given tree",
        ));
    }
    for _ in 0..RETRIES {
        match propose_round(ctx, &args, target.as_ref())? {
            Step::Done(p) => return Ok(*p),
            Step::Retry => continue,
        }
    }
    Err(busy(
        "proposal_race",
        "the repo's proposals kept changing under the compare-and-swap",
    ))
}

fn propose_round(
    ctx: &Ctx,
    args: &ProposeArgs,
    target: Option<&(TreeOid, CommitOid)>,
) -> Result<Step> {
    // Every row that occupies a slot (the store's compare-and-swap keys on the
    // occupant), including drafts of removed sessions; see the progress notes.
    let open: Vec<ProposalRecord> = ctx
        .store
        .proposals(Some(ctx.repo), false)?
        .into_iter()
        .filter(|r| {
            matches!(
                r.status,
                ProposalStatus::Pending | ProposalStatus::AwaitingPublish
            )
        })
        .collect();
    let pending = open.iter().find(|r| r.status == ProposalStatus::Pending);
    let session = origin_of(&args.origin);
    let draft = session.as_ref().and_then(|s| {
        open.iter()
            .find(|r| r.status == ProposalStatus::AwaitingPublish && r.origin.as_ref() == Some(s))
    });
    let now = SystemTime::now();
    let fresh =
        |origin: Option<SessionId>, tree: Option<TreeOid>, head: Option<CommitOid>| NewProposal {
            repo: ctx.repo.clone(),
            origin,
            from: args.from.clone(),
            from_inputs: args.from_inputs.clone(),
            to: args.to.clone(),
            to_inputs: args.to_inputs.clone(),
            tree,
            head,
        };
    let record = |new: NewProposal, replaces: Option<ProposalId>| -> Result<Step> {
        Ok(match ctx.store.record_proposal(&new, replaces, now)? {
            Some(id) => Step::Done(Box::new(built(id, new, now))),
            None => Step::Retry,
        })
    };
    let Some((tree, head)) = target else {
        // A session's draft.
        if let Some(d) = draft
            && d.to == args.to
        {
            return Ok(Step::Done(Box::new(from_record(d.clone()))));
        }
        return record(fresh(session, None, None), draft.map(|d| d.id));
    };
    let same_to = |c: &ProposalRecord| -> Result<Step> {
        if c.tree.as_ref() == Some(tree) && c.head.as_ref() == Some(head) {
            return Ok(Step::Done(Box::new(from_record(c.clone()))));
        }
        let copy = NewProposal {
            repo: c.repo.clone(),
            origin: c.origin.clone(),
            from: c.from.clone(),
            from_inputs: c.from_inputs.clone(),
            to: c.to.clone(),
            to_inputs: c.to_inputs.clone(),
            tree: Some(tree.clone()),
            head: Some(head.clone()),
        };
        record(copy, Some(c.id))
    };
    if session.is_some()
        && let Some(d) = draft
    {
        // A session's change landed.
        if d.to == args.to {
            if !ctx.store.promote_proposal(d.id, tree, head, now)? {
                return Ok(Step::Retry);
            }
            let mut p = from_record(d.clone());
            p.tree = Some(tree.clone());
            p.head = Some(head.clone());
            p.state = ProposalState::Pending;
            return Ok(Step::Done(Box::new(p)));
        }
        let new = fresh(session, None, None);
        let Some(id) = ctx.store.record_proposal(&new, Some(d.id), now)? else {
            return Ok(Step::Retry);
        };
        let rekeyed = built(id, new, now);
        let error = match ctx.store.promote_proposal(id, tree, head, now) {
            Ok(true) => {
                let mut p = rekeyed;
                p.tree = Some(tree.clone());
                p.head = Some(head.clone());
                p.state = ProposalState::Pending;
                return Ok(Step::Done(Box::new(p)));
            }
            Ok(false) => busy(
                "proposal_race",
                "the re-keyed draft changed before it was promoted",
            ),
            Err(e) => e,
        };
        return Err(unknown(
            "landing_not_promoted",
            "the landed draft was re-keyed but not promoted",
            json!({ "proposal": rekeyed, "error": error }),
        ));
    }
    // A landed session change without a draft, or main.
    if let Some(c) = pending
        && c.to == args.to
    {
        return same_to(c);
    }
    record(
        fresh(session, Some(tree.clone()), Some(head.clone())),
        pending.map(|c| c.id),
    )
}

fn built(id: ProposalId, new: NewProposal, now: SystemTime) -> Proposal {
    let state = if new.tree.is_some() {
        ProposalState::Pending
    } else {
        ProposalState::AwaitingPublish
    };
    Proposal {
        id,
        repo: new.repo,
        origin: match new.origin {
            Some(session) => ProposalOrigin::Session { session },
            None => ProposalOrigin::Main,
        },
        from: new.from,
        from_inputs: new.from_inputs,
        to: new.to,
        to_inputs: new.to_inputs,
        tree: new.tree,
        head: new.head,
        state,
        created: now,
    }
}

/// The publish chain of `branch` against origin's tracking ref (§4.5). Read-only.
pub fn publish_chain(git: &Git, branch: &str) -> Result<Option<PublishChain>> {
    let Some(origin) = git.read_ref(&remote_ref("origin", branch))? else {
        return Ok(None);
    };
    let top = git.read_ref(&format!("refs/strata/publish/{branch}"))?;
    let anchor = git.read_ref(&format!("refs/strata/anchor/{branch}"))?;
    let state = match (&top, &anchor) {
        (None, _) => ChainState::Absent,
        (Some(_), None) => ChainState::Rewound,
        (Some(c), Some(a)) => {
            if !git.is_ancestor(a, &origin)? {
                ChainState::Rewound
            } else if *c == origin {
                ChainState::Equal
            } else if git.is_ancestor(&origin, c)? {
                ChainState::Leads
            } else if git.is_ancestor(c, &origin)? {
                ChainState::Behind
            } else {
                ChainState::Diverged
            }
        }
    };
    Ok(Some(PublishChain {
        branch: branch.to_owned(),
        origin,
        top,
        anchor,
        state,
    }))
}

/// The commit main is at for deps (§4.5): the publish chain's top while it
/// leads origin under `pin_unpushed`, otherwise origin's tracking ref.
pub fn main_tip(cfg: &RepoConfig, git: &Git) -> Result<Option<CommitOid>> {
    if cfg.pin_unpushed && cfg.publish.branch == cfg.default_branch {
        return Ok(
            publish_chain(git, &cfg.default_branch)?.map(|c| match (c.state, c.top) {
                (ChainState::Leads, Some(top)) => top,
                _ => c.origin,
            }),
        );
    }
    git.read_ref(&remote_ref("origin", &cfg.default_branch))
}

/// Record main's deps set for (tree, head) and propose the change when it
/// moved (§5.9). Takes the repo write lock itself.
pub fn observe_main(
    ctx: &Ctx,
    tree: &TreeOid,
    head: &CommitOid,
    export: &Path,
) -> Result<Option<Proposal>> {
    if ctx.git.tree_of(head)? != *tree {
        return Err(invalid(
            "tree_mismatch",
            "the head's tree is not the given tree",
        ));
    }
    let (inputs, id) = key_of(ctx, export)?;
    seams::before_main_lock();
    let mut out: Option<Result<Option<Proposal>>> = None;
    ctx.locks.with_write(ctx.repo, &mut || {
        let mut got: Option<Proposal> = None;
        let r = observe_locked(ctx, tree, head, &inputs, &id, &mut got);
        out = Some(match (r, got) {
            (Ok(v), _) => Ok(v),
            (Err(e), None) => Err(e),
            (Err(e), Some(p)) => Err(not_recorded(&p, e)),
        });
        Ok(())
    })??;
    out.unwrap_or_else(|| {
        Err(StrataError::new(
            Code::Unknown,
            "observe_main: the write lock never ran",
        ))
    })
}

fn not_recorded(p: &Proposal, error: StrataError) -> StrataError {
    if error.code == Code::Unknown
        && error.detail.as_ref().and_then(|d| d.get("reason"))
            == Some(&Value::from("main_not_recorded"))
    {
        return error;
    }
    unknown(
        "main_not_recorded",
        "a proposal was recorded but main's record did not follow",
        json!({ "proposal": p, "error": error }),
    )
}

fn observe_locked(
    ctx: &Ctx,
    tree: &TreeOid,
    head: &CommitOid,
    inputs: &str,
    id: &DepsSetId,
    got: &mut Option<Proposal>,
) -> Result<Option<Proposal>> {
    let race = || {
        busy(
            "main_race",
            "main's record kept changing under the compare-and-swap",
        )
    };
    let mut old = ctx.store.deps_main(ctx.repo)?;
    for _ in 0..=RETRIES {
        if main_tip(ctx.cfg, ctx.git)?.as_ref() != Some(head) {
            return match got {
                None => Ok(None),
                Some(p) => Err(not_recorded(p, race())),
            };
        }
        let args = |from: DepsSetId, from_inputs: String| ProposeArgs {
            origin: ProposalOrigin::Main,
            from,
            from_inputs,
            to: id.clone(),
            to_inputs: inputs.to_owned(),
            tree: Some(tree.clone()),
            head: Some(head.clone()),
        };
        match &old {
            Some(o) if o.set != *id => {
                *got = Some(propose(ctx, args(o.set.clone(), o.inputs_hash.clone()))?);
            }
            _ => {
                let c = ctx
                    .store
                    .proposals(Some(ctx.repo), true)?
                    .into_iter()
                    .find(|r| r.status == ProposalStatus::Pending);
                if let Some(c) = c
                    && c.to == *id
                    && (c.tree.as_ref() != Some(tree) || c.head.as_ref() != Some(head))
                {
                    *got = Some(propose(ctx, args(c.from, c.from_inputs))?);
                }
            }
        }
        let row = DepsMain {
            repo: ctx.repo.clone(),
            tree: tree.clone(),
            head: head.clone(),
            inputs_hash: inputs.to_owned(),
            set: id.clone(),
            observed: SystemTime::now(),
        };
        seams::before_main_cas();
        match ctx.store.set_deps_main(&row, old.as_ref())? {
            (true, _) => return Ok(got.clone()),
            (false, current) => old = current,
        }
    }
    Err(race())
}

//! The publish chain and its anchor (§2 item 3, §5.4).

use serde_json::json;
use strata_deps::{ChainState, publish_chain};
use strata_gitio::{Git, RefUpdate};
use strata_types::{Code, CommitOid, RepoConfig, StrataError};

use crate::Result;

/// `refs/strata/publish/<branch>`; `branch` is `RepoConfig.publish.branch`.
pub fn chain_ref(branch: &str) -> String {
    format!("refs/strata/publish/{branch}")
}

/// `refs/strata/anchor/<branch>`.
pub fn anchor_ref(branch: &str) -> String {
    format!("refs/strata/anchor/{branch}")
}

/// The planned `merge_base` (D1): the chain top while `publish_chain` is
/// `Leads`, otherwise origin's tip. Never fetched → `NoSuchBranch`. The
/// predicate is deps'; this never compares ancestry itself. Writes nothing.
pub fn merge_base(git: &Git, cfg: &RepoConfig) -> Result<CommitOid> {
    let branch = &cfg.publish.branch;
    let Some(c) = publish_chain(git, branch)? else {
        return Err(StrataError::new(
            Code::NoSuchBranch,
            format!("origin/{branch} has not been fetched"),
        )
        .with_detail(json!({ "branch": branch, "remote": "origin" })));
    };
    Ok(match (c.state, c.top) {
        (ChainState::Leads, Some(top)) => top,
        _ => c.origin,
    })
}

/// §5.4: move the anchor of `branch` to `git.merge_base(origin, top)` when
/// that is a strict descendant of the anchor. One ref transaction that
/// verifies the chain top it read and compare-and-swaps the old anchor.
/// `Absent` and `Rewound` never advance. The caller holds the repo write
/// lock. Returns the new anchor when one was written.
pub fn advance_anchor(git: &Git, branch: &str) -> Result<Option<CommitOid>> {
    let Some(c) = publish_chain(git, branch)? else {
        return Ok(None);
    };
    if matches!(c.state, ChainState::Absent | ChainState::Rewound) {
        return Ok(None);
    }
    let (Some(top), Some(a)) = (c.top, c.anchor) else {
        return Ok(None);
    };
    let Some(m) = git.merge_base(&c.origin, &top)? else {
        return Ok(None);
    };
    if m == a || !git.is_ancestor(&a, &m)? {
        return Ok(None);
    }
    git.ref_transaction(&[
        RefUpdate::verify(chain_ref(branch), Some(top)),
        RefUpdate::set(anchor_ref(branch), m.clone(), Some(a)),
    ])?;
    Ok(Some(m))
}

/// D2: a `Rewound` or `Diverged` chain is refused with `InvalidInput`
/// (`detail.reason` `chain_rewound` / `chain_diverged`); nothing is
/// replayed. Returns the chain read (or `None` when origin was never
/// fetched). Read-only.
pub fn refuse_unpublishable(git: &Git, branch: &str) -> Result<Option<strata_deps::PublishChain>> {
    let c = publish_chain(git, branch)?;
    if let Some(c) = &c {
        refuse_state(c)?;
    }
    Ok(c)
}

pub(crate) fn refuse_state(c: &strata_deps::PublishChain) -> Result<()> {
    let reason = match c.state {
        ChainState::Rewound => "chain_rewound",
        ChainState::Diverged => "chain_diverged",
        _ => return Ok(()),
    };
    Err(StrataError::new(
        Code::InvalidInput,
        format!("the publish chain of {} is {}; it is not replayed automatically", c.branch, &reason[6..]),
    )
    .with_detail(json!({
        "reason": reason,
        "branch": c.branch,
        "origin": c.origin.as_str(),
        "top": c.top.as_ref().map(|t| t.as_str()),
        "anchor": c.anchor.as_ref().map(|a| a.as_str()),
    })))
}

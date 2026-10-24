//! §5.5 `resolve_move` and `chain_resolve`: the two human resolutions.
//!
//! `resolve_move` never asks the `Confirmer` (cli already asked the human)
//! and calls `Sessions::resolve_failed_move` without the repo lock.
//! `chain_resolve` classifies the publish chain under the repo lock before
//! any compare-and-swap, writes the chain and the anchor in one ref
//! transaction, and pushes only for `Restore`, only fast-forward.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error};
use strata_deps::{ChainState, PublishChain};
use strata_gitio::{Git, RefUpdate};
use strata_publish::{PUSH_TIMEOUT, anchor_ref, chain_ref};
use strata_session::{MoveResolution, ResolutionReport};
use strata_store::PublishPhase;
use strata_types::{Action, Code, CommitOid, PushOutcome, RepoName, SessionId, StrataError};

use crate::confirm_flow::{needs_confirm, recheck, require_validated};
use crate::fetch_after::observe_if_moved;
use crate::startup::Core;
use crate::{ChainChoice, Result};

/// Exactly one of `abandon` / `accept`; anything else is no choice (the
/// RPC layer already answers it `Failed`).
pub fn move_choice(abandon: bool, accept: bool) -> Option<MoveResolution> {
    match (abandon, accept) {
        (true, false) => Some(MoveResolution::Abandon),
        (false, true) => Some(MoveResolution::Accept),
        _ => None,
    }
}

/// Whether a publish of `session` is running (its `publishing` flag).
fn publishing(core: &Core, session: &SessionId) -> Result<bool> {
    Ok(core
        .store
        .open_publishes()?
        .iter()
        .any(|r| r.phase == PublishPhase::Running && r.group.batches.iter().any(|b| &b.session == session)))
}

/// Wait for `session`'s publishing flag to clear, holding no queue and no
/// lock, at most the lock wait (then `Busy`, nothing written).
pub fn wait_not_publishing(core: &Core, session: &SessionId) -> Result<()> {
    let start = Instant::now();
    while publishing(core, session)? {
        if start.elapsed() >= core.locks.wait() {
            return Err(StrataError::new(Code::Busy, format!("session {session} is publishing"))
                .with_detail(json!({ "reason": "publishing", "session": session.as_str() })));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// §5.5 `resolve_move`. No TTY check, no `Confirmer`. `resolve` is
/// `Sessions::resolve_failed_move` (it takes the repo lock itself); it runs
/// holding only the session queue. `after_accept` resets the Tracker and
/// watches again; `Abandon` keeps the tree and the Tracker.
pub fn resolve_move(
    core: &Core,
    session: &SessionId,
    repo: &RepoName,
    choice: MoveResolution,
    resolve: &dyn Fn(&SessionId, &RepoName, MoveResolution) -> Result<ResolutionReport>,
    after_accept: &mut dyn FnMut(),
) -> Result<ResolutionReport> {
    wait_not_publishing(core, session)?;
    core.locks.with_sessions(std::slice::from_ref(session), || {
        crate::handlers::writing();
        let r = resolve(session, repo, choice)?;
        if choice == MoveResolution::Accept {
            after_accept();
        }
        Ok(r)
    })
}

/// What `chain_resolve` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChainResolved {
    pub repo: RepoName,
    pub branch: String,
    pub choice: ChainChoice,
    /// The chain tip and the anchor afterwards.
    pub chain: String,
    pub anchor: String,
    pub pushed: bool,
    /// `ObserveFailed` after the lock, if observing `main` failed twice.
    pub warnings: Vec<Value>,
}

fn invalid(reason: &str, message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

fn unknown(reason: &str, e: &StrataError) -> StrataError {
    StrataError::new(Code::Unknown, format!("{}; the refs may have changed", e.message))
        .with_detail(json!({ "reason": reason, "cause": e.code.as_str() }))
}

fn rewound_or_diverged(c: Option<PublishChain>) -> Result<(PublishChain, CommitOid)> {
    match c {
        Some(c) if matches!(c.state, ChainState::Rewound | ChainState::Diverged) => {
            let top = c.top.clone().ok_or_else(|| invalid("not_rewound_or_diverged", "the publish chain is empty"))?;
            Ok((c, top))
        }
        c => Err(invalid(
            "not_rewound_or_diverged",
            format!("the publish chain is {:?}, not Rewound or Diverged", c.map(|c| c.state)),
        )),
    }
}

/// The one chain-and-anchor transaction; once it is called, any failure is
/// `Unknown`.
fn transaction(git: &Git, updates: &[RefUpdate]) -> Result<()> {
    crate::handlers::writing();
    git.ref_transaction(updates).map_err(|e| unknown("ref_transaction", &e))
}

/// The start of `AcceptOriginReplay`'s range: the anchor advanced in
/// memory along a `Diverged` chain, the current anchor for `Rewound`.
pub fn replay_base(git: &Git, chain: &PublishChain, c: &CommitOid) -> Result<Option<CommitOid>> {
    let Some(a) = chain.anchor.clone() else {
        return Ok(None);
    };
    let base = if chain.state == ChainState::Diverged {
        match git.merge_base(&chain.origin, c)? {
            Some(m) if m != a && git.is_ancestor(&a, &m)? => m,
            _ => a,
        }
    } else {
        a
    };
    Ok(git.is_ancestor(&base, c)?.then_some(base))
}

/// A staging tree attached at `tip` under `<root>/daemon/push/`, removed
/// afterwards whatever `f` returned.
pub(crate) fn with_staging<T>(core: &Core, git: &Git, repo: &RepoName, tip: &CommitOid, f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = core.config.paths.root.join("daemon/push").join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| classify_io_error(&e, IoSide::Write, &dir))?;
    let r = (|| {
        let wt = dir.join(repo.as_str());
        let gd = dir.join(format!(".git-{repo}"));
        git.export_tree(&git.tree_of(tip)?, &wt)?;
        git.attach_staging(&wt, &gd, tip)?;
        f(&wt)
    })();
    let _ = fs::remove_dir_all(&dir);
    r
}

/// `deps::observe_main` for `tip` of `repo`: its tree exported to a
/// directory under `<root>/daemon/observe/`, removed afterwards. Takes the
/// repo lock itself (through deps).
pub fn observe_tip(core: &Core, repo: &RepoName, tip: &CommitOid) -> Result<()> {
    let git = core.git(repo)?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = core.config.paths.root.join("daemon/observe").join(format!("{}-{stamp}", std::process::id()));
    let r = (|| {
        let tree = git.tree_of(tip)?;
        git.export_tree(&tree, &dir)?;
        let ctx = strata_deps::Ctx {
            store: &core.store,
            paths: &core.config.paths,
            repo,
            cfg: core.repo_cfg(repo)?,
            git,
            env: &core.env,
            locks: &core.locks,
        };
        strata_deps::observe_main(&ctx, &tree, tip, &dir).map(|_| ())
    })();
    let _ = fs::remove_dir_all(&dir);
    r
}

/// §5.5 `chain_resolve`. `observe` is called (after the lock, at most
/// twice) with the new `main_tip` when it moved.
pub fn chain_resolve(
    core: &Core,
    repo: &RepoName,
    choice: ChainChoice,
    shown: Option<&CommitOid>,
    observe: &mut dyn FnMut(&CommitOid) -> Result<()>,
) -> Result<ChainResolved> {
    let cfg = core.repo_cfg(repo)?;
    let git = core.git(repo)?;
    let branch = cfg.publish.branch.clone();
    let (chain_name, anchor_name) = (chain_ref(&branch), anchor_ref(&branch));

    // 2. Read once without the lock; no fetch when it is not a fork.
    let (_, seen) = rewound_or_diverged(strata_deps::publish_chain(git, &branch)?)?;

    // 3. Restore asks the human like push, holding nothing.
    let action = Action::PushDeploy { repo: repo.clone(), branch: branch.clone() };
    let token = if choice == ChainChoice::Restore && needs_confirm(core.pinned(repo)?) {
        Some(require_validated(&core.confirmer, action.clone())?)
    } else {
        None
    };

    let mut tips = (None, None);
    let out = core.locks.with_repo(repo, || {
        tips.0 = strata_deps::main_tip(cfg, git).ok().flatten();
        // 4. Fetch, read again, classify before any compare-and-swap.
        git.fetch("origin", &branch)?;
        let (chain, c) = rewound_or_diverged(strata_deps::publish_chain(git, &branch)?)?;
        let (o, a) = (chain.origin.clone(), chain.anchor.clone());
        let mut done = ChainResolved {
            repo: repo.clone(),
            branch: branch.clone(),
            choice,
            chain: c.as_str().to_string(),
            anchor: a.as_ref().map(|a| a.as_str().to_string()).unwrap_or_default(),
            pushed: false,
            warnings: Vec::new(),
        };
        match choice {
            ChainChoice::Restore => {
                if !git.is_ancestor(&o, &c)? {
                    return Err(invalid("not_fast_forward", format!("origin/{branch} is not an ancestor of the publish chain")));
                }
                if let Some(t) = &token {
                    recheck(t, &action)?;
                    // Review H2: bound to what the cli showed, and to the
                    // daemon's own read before the prompt.
                    if c != seen || shown != Some(&c) {
                        return Err(StrataError::new(Code::NotConfirmed, "the publish chain moved after the confirmation")
                            .with_detail(json!({
                                "reason": "chain_moved",
                                "confirmed": shown.map(|s| s.as_str()),
                                "now": c.as_str(),
                            })));
                    }
                }
                // 8. Push the chain tip; only `Pushed` reaches the transaction.
                let outcome = with_staging(core, git, repo, &c, |wt| {
                    crate::handlers::writing();
                    git.push(wt, "origin", &chain_name, &branch, PUSH_TIMEOUT)
                })?;
                match outcome {
                    PushOutcome::Pushed => {}
                    PushOutcome::Refused(msg) => {
                        return Err(StrataError::new(Code::PushRefused, format!("origin refused the push: {msg}"))
                            .with_detail(json!({ "reason": "push_refused", "branch": branch })));
                    }
                    PushOutcome::Unknown => {
                        return Err(StrataError::new(Code::Unknown, "the push outcome is unknown; not pushed again")
                            .with_detail(json!({ "reason": "push_unknown", "branch": branch })));
                    }
                }
                transaction(git, &[RefUpdate::verify(&chain_name, Some(c.clone())), RefUpdate::set(&anchor_name, c.clone(), a)])?;
                done.pushed = true;
                done.anchor = c.as_str().to_string();
            }
            ChainChoice::AcceptOriginDrop => {
                transaction(git, &[RefUpdate::set(&chain_name, o.clone(), Some(c.clone())), RefUpdate::set(&anchor_name, o.clone(), a)])?;
                done.chain = o.as_str().to_string();
                done.anchor = o.as_str().to_string();
            }
            ChainChoice::AcceptOriginReplay => {
                let Some(base) = replay_base(git, &chain, &c)? else {
                    return Err(invalid("no_replay_range", "no replay range: the chain has no anchor on it"));
                };
                // A Conflict here comes before any ref write: the refs are unchanged.
                let tip = git.replay_commits(&base, &c, &o)?;
                transaction(git, &[RefUpdate::set(&chain_name, tip.clone(), Some(c.clone())), RefUpdate::set(&anchor_name, o.clone(), a)])?;
                done.chain = tip.as_str().to_string();
                done.anchor = o.as_str().to_string();
            }
        }
        tips.1 = strata_deps::main_tip(cfg, git).ok().flatten();
        Ok(done)
    });
    // 9. After the lock: observe when main moved; failure is a warning.
    let mut done = out?;
    if let Some(after) = tips.1.clone()
        && let Some(e) = observe_if_moved(tips.0.as_ref(), Some(&after), &mut || observe(&after))
    {
        done.warnings.push(json!({ "kind": "ObserveFailed", "repo": repo.as_str(), "code": e.code.as_str(), "message": e.message }));
    }
    Ok(done)
}

//! The check siblings (publish.md D21, config.md §2.3 item 5): the repos a
//! repo's `checks` need at `../<name>` of the staged repo. Publish supplies
//! each one: the layout session's pinned base when the session has the
//! repo, else the repo's current main-tip base, prepared here outside the
//! session queue and every lock but the sibling's own. A sibling that cannot
//! be supplied refuses the publish up front (`InvalidInput`, reason
//! `siblings_missing`); the checks never run without it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use strata_gitio::Git;
use strata_store::Store;
use strata_types::{BaseGen, Code, RepoConfig, RepoName, RepoPath, SessionId, StrataError};

use crate::chain::advance_anchor;
use crate::push::{PublishWarning, WarningKind, err_json, unknown_with};
use crate::seams::{self, FailPoint, Point};
use crate::{Ctx, Result};

const REMOTE: &str = "origin";

/// Why a sibling could not be supplied (`missing[].why`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The fetch failed with a named code, or origin has no default branch.
    MainTipUnavailable,
    /// `ensure_base` refused with a named code.
    BaseUnavailable,
    /// The prepared base generation was collected before it was cloned.
    BaseCollected,
    /// Cloning the base into the layout failed (the layout was removed).
    CloneFailed,
}

impl Why {
    pub fn as_str(self) -> &'static str {
        match self {
            Why::MainTipUnavailable => "main_tip_unavailable",
            Why::BaseUnavailable => "base_unavailable",
            Why::BaseCollected => "base_collected",
            Why::CloneFailed => "clone_failed",
        }
    }
}

/// One sibling that could not be supplied, with the error that said so.
#[derive(Clone, Debug)]
pub struct Missing {
    pub repo: RepoName,
    pub branch: String,
    pub why: Why,
    pub cause: StrataError,
}

/// What `prepare` produced: the main-tip bases of the siblings the layout
/// session does not have, the siblings that could not be supplied (named
/// failures only), and the observe warnings.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    pub ready: BTreeMap<RepoName, BaseGen>,
    pub missing: Vec<Missing>,
    pub warnings: Vec<PublishWarning>,
}

/// One main-tip base to clone into the layout as `<layout>/<repo>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Supply {
    pub repo: RepoName,
    pub src: PathBuf,
}

/// Siblings are supplied only when the repo has checks and names siblings.
pub fn needed(cfg: &RepoConfig) -> bool {
    !cfg.checks.is_empty() && !cfg.check_siblings.is_empty()
}

fn pinned(store: &Store, layout: Option<&SessionId>) -> Result<BTreeMap<RepoName, BaseGen>> {
    let Some(l) = layout else {
        return Ok(BTreeMap::new());
    };
    Ok(store
        .session(l)?
        .map(|rec| rec.manifest.repos.into_iter().map(|(r, ws)| (r, ws.base)).collect())
        .unwrap_or_default())
}

enum Step {
    /// A named failure: the sibling is missing.
    Definite(Why, StrataError),
    /// `Busy`, `Unknown` (or a lock error): returned as is.
    Stop(StrataError),
}

fn with_sibling(mut e: StrataError, s: &RepoName) -> StrataError {
    let d = e.detail.get_or_insert_with(|| json!({}));
    if let Value::Object(m) = d {
        m.insert("sibling".into(), json!(s.as_str()));
    } else {
        let old = d.take();
        *d = json!({ "sibling": s.as_str(), "detail": old });
    }
    e
}

fn classify(e: StrataError, s: &RepoName, why: Why) -> Step {
    match e.code {
        Code::Busy | Code::Unknown => Step::Stop(with_sibling(e, s)),
        _ => Step::Definite(why, e),
    }
}

/// Step 1 (under S's lock): fetch, advance S's anchor in the same critical
/// section, read the main tip. Steps 2–3 (outside it): the base, observe.
fn prepare_one(ctx: &Ctx, s: &RepoName, warnings: &mut Vec<PublishWarning>) -> std::result::Result<BaseGen, Step> {
    let (cfg, git) = ctx.sessions.repo_handles(s).map_err(Step::Stop)?;
    let branch = &cfg.default_branch;
    let mut head = None;
    let r = ctx.locks.with_repo(s, &mut || {
        git.fetch(REMOTE, branch)?;
        seams::fail(git, FailPoint::SiblingAnchor).and_then(|()| advance_anchor(git, branch).map(|_| ())).map_err(
            |e| {
                unknown_with(
                    "anchor_transaction",
                    format!(
                        "the anchor of check sibling {s} could not be advanced after its fetch; the outcome is unknown"
                    ),
                    Some(&e),
                    json!({ "sibling": s.as_str(), "branch": branch }),
                )
            },
        )?;
        head = strata_deps::main_tip(cfg, git)?;
        Ok(())
    });
    match r {
        Err(e) => return Err(Step::Stop(with_sibling(e, s))),
        Ok(Err(e)) => return Err(classify(e, s, Why::MainTipUnavailable)),
        Ok(Ok(())) => {}
    }
    let Some(head) = head else {
        return Err(Step::Definite(
            Why::MainTipUnavailable,
            StrataError::new(Code::NoSuchBranch, format!("repo {s} has no main tip for origin/{branch}"))
                .with_detail(json!({ "repo": s.as_str(), "remote": REMOTE, "branch": branch })),
        ));
    };
    let g = ctx.sessions.ensure_base(s, &head).map_err(|e| classify(e, s, Why::BaseUnavailable))?;
    let dctx = strata_deps::Ctx {
        store: ctx.store,
        paths: ctx.deps.paths,
        repo: s,
        cfg,
        git,
        env: ctx.deps.env,
        locks: ctx.deps.locks,
    };
    if let Err(e) = strata_deps::observe_main(&dctx, &g.tree, &head, &ctx.store.base_dir(&g)) {
        tracing::warn!(repo = %s, error = %e.message, "check sibling: main not observed");
        let mut detail = err_json(&e);
        detail["repo"] = json!(s.as_str());
        warnings.push(PublishWarning { kind: WarningKind::ObserveFailed, detail });
    }
    Ok(g)
}

/// Prepare the check siblings of `ctx.repo` that `layout`'s manifest does
/// not have (all of them with no layout session: the daemon's replay).
/// Called with no lock held; takes each sibling's write lock once, never
/// nested. `Busy` and `Unknown` come back as errors (`detail.sibling`
/// added, nothing else changed); named failures are in `missing`.
pub fn prepare(ctx: &Ctx, layout: Option<&SessionId>) -> Result<Prepared> {
    let mut out = Prepared::default();
    if !needed(ctx.cfg) {
        return Ok(out);
    }
    let pinned = pinned(ctx.store, layout)?;
    for s in &ctx.cfg.check_siblings {
        if pinned.contains_key(s) {
            continue;
        }
        match prepare_one(ctx, s, &mut out.warnings) {
            Ok(g) => {
                out.ready.insert(s.clone(), g);
            }
            Err(Step::Stop(e)) => return Err(e),
            Err(Step::Definite(why, cause)) => {
                let branch = ctx.sessions.repo_handles(s).map(|(c, _)| c.default_branch.clone()).unwrap_or_default();
                out.missing.push(Missing { repo: s.clone(), branch, why, cause });
            }
        }
    }
    Ok(out)
}

/// Re-check right before the layout directory is created: the layout
/// session's manifest is read again (an `add` that landed meanwhile wins:
/// its pin is used), a pinned base whose directory is gone is `Corrupt`,
/// and a prepared generation that was collected is `base_collected`.
/// Returns the main-tip bases to clone; any sibling not suppliable →
/// `siblings_missing`. Creates nothing.
pub fn place(
    store: &Store,
    git: &Git,
    cfg: &RepoConfig,
    repo: &RepoName,
    layout: Option<&SessionId>,
    prep: &Prepared,
) -> Result<Vec<Supply>> {
    if !needed(cfg) {
        return Ok(Vec::new());
    }
    let pinned = pinned(store, layout)?;
    let mut out = Vec::new();
    let mut missing = Vec::new();
    let mut seen = BTreeSet::new();
    for s in &cfg.check_siblings {
        if !seen.insert(s) {
            continue;
        }
        if let Some(g) = pinned.get(s) {
            let dir = store.base_dir(g);
            if !dir.is_dir() {
                return Err(StrataError::new(
                    Code::Corrupt,
                    format!("the pinned base of check sibling {s} is missing from the store"),
                )
                .with_detail(json!({
                    "repo": repo.as_str(),
                    "sibling": s.as_str(),
                    "session": layout.map(|l| l.as_str()),
                    "path": dir.to_string_lossy(),
                    "tree": g.tree.as_str(),
                    "deps": g.deps.as_str(),
                })));
            }
            continue;
        }
        if let Some(g) = prep.ready.get(s) {
            let dir = store.base_dir(g);
            if store.has_base_gen(g)? && dir.is_dir() {
                out.push(Supply { repo: s.clone(), src: dir });
            } else {
                missing.push(Missing {
                    repo: s.clone(),
                    branch: String::new(),
                    why: Why::BaseCollected,
                    cause: StrataError::new(Code::NoSuchPath, format!("the base prepared for {s} was collected"))
                        .with_detail(
                            json!({ "path": dir.to_string_lossy(), "tree": g.tree.as_str(), "deps": g.deps.as_str() }),
                        ),
                });
            }
            continue;
        }
        match prep.missing.iter().find(|m| m.repo == *s) {
            Some(m) => missing.push(m.clone()),
            None => missing.push(Missing {
                repo: s.clone(),
                branch: String::new(),
                why: Why::BaseCollected,
                cause: StrataError::new(Code::NoSuchPath, format!("no base was prepared for {s}")),
            }),
        }
    }
    if !missing.is_empty() {
        return Err(missing_error(repo, layout, &missing));
    }
    seams::hit(git, Point::AfterSiblingPlace);
    Ok(out)
}

/// Clone each supplied base into `<layout>/<repo>` (APFS clone, rooted).
/// A failure is `clone_failed`; the caller removes the layout first.
pub fn clone_supplies(supplies: &[Supply], layout: &Path) -> std::result::Result<(), Missing> {
    for s in supplies {
        let r = RepoPath::try_from(s.repo.as_str().to_string())
            .and_then(|rel| strata_apfs::clone_tree_rooted(&s.src, layout, &rel).map(|_| ()));
        if let Err(cause) = r {
            return Err(Missing { repo: s.repo.clone(), branch: String::new(), why: Why::CloneFailed, cause });
        }
    }
    Ok(())
}

fn next_of(m: &Missing, retry: &str, add: bool) -> String {
    let s = &m.repo;
    let c = &m.cause;
    match m.why {
        Why::MainTipUnavailable => format!(
            "origin/{} of {s} could not be fetched or has no commit ({}): fix the remote, then {retry}{}",
            m.branch,
            c.message,
            if add { " or run the add" } else { "" }
        ),
        Why::BaseUnavailable => format!("the base of {s} could not be built ({}): fix that, then {retry}", c.message),
        Why::BaseCollected => format!("the base prepared for {s} was collected meanwhile: {retry}"),
        Why::CloneFailed if c.code == Code::NoSuchPath => {
            format!("the base prepared for {s} was collected meanwhile: {retry}")
        }
        Why::CloneFailed => {
            format!("cloning the base of {s} into the staging layout failed ({}): fix that, then {retry}", c.message)
        }
    }
}

/// `InvalidInput`, reason `siblings_missing` (publish.md D21). With a
/// layout session the fix is `git-strata add --session <L> --repo …`;
/// without one (the daemon's replay) it is to retry the push.
pub fn missing_error(repo: &RepoName, session: Option<&SessionId>, missing: &[Missing]) -> StrataError {
    let names: Vec<&str> = missing.iter().map(|m| m.repo.as_str()).collect();
    let (retry, fix) = match session {
        Some(l) => (
            "retry the publish".to_string(),
            format!(
                "git-strata add --session {} {}",
                l.as_str(),
                names.iter().map(|n| format!("--repo {n}")).collect::<Vec<_>>().join(" ")
            ),
        ),
        None => (format!("retry `git-strata push {repo}`"), format!("retry `git-strata push {repo}`")),
    };
    let entries: Vec<Value> = missing
        .iter()
        .map(|m| {
            json!({
                "repo": m.repo.as_str(),
                "why": m.why.as_str(),
                "next": next_of(m, &retry, session.is_some()),
                "cause": err_json(&m.cause),
            })
        })
        .collect();
    StrataError::new(
        Code::InvalidInput,
        format!("publish checks of {repo} need sibling repos that could not be supplied: {}", names.join(", ")),
    )
    .with_detail(json!({
        "reason": "siblings_missing",
        "repo": repo.as_str(),
        "session": session.map(|l| l.as_str()),
        "missing": entries,
        "fix": fix,
    }))
}

//! §5.7 after a fetch outside `publish::publish` (after `new` returns): the
//! anchor follows origin along the chain in one ref transaction under the
//! repo lock, and `main` is observed again when `new` reported
//! `ObserveFailed`. §5.10: the attach alert and the first-seen
//! `PrePushGuardDisabled` line. Nothing here writes the main checkout or a
//! shadow repository's config.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use strata_deps::{ChainState, publish_chain};
use strata_gitio::{Git, RefUpdate};
use strata_session::{NewReport, NewWarning, SessionStatus};
use strata_types::{Code, CommitOid, RepoName, SessionId, StrataError};

use crate::startup::{Core, Warnings};
use crate::{DaemonWarning, Result};

/// What §5.7 did with the anchor of one branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorStep {
    /// Not called: origin never fetched (`None`), `Absent` or `Rewound`.
    NotCalled(Option<ChainState>),
    /// Classified as movable but `m` is the anchor or not past it.
    Kept,
    /// The one transaction wrote the anchor to this commit.
    Advanced(CommitOid),
}

/// `refs/strata/publish/<branch>`.
fn chain_ref(branch: &str) -> String {
    format!("refs/strata/publish/{branch}")
}

/// `refs/strata/anchor/<branch>`.
fn anchor_ref(branch: &str) -> String {
    format!("refs/strata/anchor/{branch}")
}

/// §5.7 for `branch`; the caller holds the repo write lock. Classify
/// first; for `Equal`/`Leads`/`Behind`/`Diverged` compute
/// `m = git.merge_base(origin, chain)` (gitio's, not `publish::merge_base`)
/// and write only when `is_ancestor(anchor, m)` and `m != anchor`. The
/// write is one transaction: the chain verified at the value read and the
/// anchor compare-and-swapped to `m`. Once called, any failure is
/// `Unknown` and is not retried.
pub fn advance_anchor_locked(git: &Git, branch: &str) -> Result<AnchorStep> {
    let Some(c) = publish_chain(git, branch)? else {
        return Ok(AnchorStep::NotCalled(None));
    };
    match c.state {
        ChainState::Absent | ChainState::Rewound => return Ok(AnchorStep::NotCalled(Some(c.state))),
        ChainState::Equal | ChainState::Leads | ChainState::Behind | ChainState::Diverged => {}
    }
    let (Some(top), Some(a)) = (c.top, c.anchor) else {
        return Ok(AnchorStep::NotCalled(Some(c.state)));
    };
    let Some(m) = git.merge_base(&c.origin, &top)? else {
        return Ok(AnchorStep::Kept);
    };
    if m == a || !git.is_ancestor(&a, &m)? {
        return Ok(AnchorStep::Kept);
    }
    git.ref_transaction(&[
        RefUpdate::verify(chain_ref(branch), Some(top.clone())),
        RefUpdate::set(anchor_ref(branch), m.clone(), Some(a.clone())),
    ])
    .map_err(|e| {
        StrataError::new(
            Code::Unknown,
            format!("the anchor transaction of {branch} failed; the anchor may or may not have moved: {}", e.message),
        )
        .with_detail(json!({
            "reason": "anchor_transaction",
            "branch": branch,
            "chain": top.as_str(),
            "anchor": a.as_str(),
            "target": m.as_str(),
            "cause": e,
        }))
    })?;
    Ok(AnchorStep::Advanced(m))
}

/// What `after_new` did per repo (for the reply and the tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AfterNew {
    /// Repos whose anchor transaction failed (`Unknown`, reported as
    /// `AnchorNotAdvanced`; `new` stays successful).
    pub anchor_unknown: Vec<(RepoName, StrataError)>,
    /// Repos the daemon observed again because `new` said `ObserveFailed`.
    pub reobserved: Vec<RepoName>,
}

impl Core {
    /// §5.7 for one repo: take the repo write lock and advance its
    /// default branch's anchor. A failed transaction adds
    /// `AnchorNotAdvanced` and returns the `Unknown`.
    pub fn advance_anchor(&self, repo: &RepoName) -> Result<AnchorStep> {
        let git = self.git(repo)?;
        let branch = self.repo_cfg(repo)?.default_branch.clone();
        let r = self.locks.with_repo(repo, || advance_anchor_locked(git, &branch));
        if let Err(e) = &r
            && e.code == Code::Unknown
            && reason_of(e) == Some("anchor_transaction")
        {
            self.warnings.add(DaemonWarning::AnchorNotAdvanced { repo: repo.clone() });
        }
        if matches!(r, Ok(AnchorStep::Advanced(_))) {
            self.warnings.remove(|w| matches!(w, DaemonWarning::AnchorNotAdvanced { repo: r } if r == repo));
        }
        r
    }

    /// The `main_tip` of every configured repo (read before a fetch).
    pub fn main_tips(&self) -> Vec<(RepoName, Option<CommitOid>)> {
        self.config
            .repos
            .iter()
            .map(|(name, cfg)| {
                let tip = self.gits.get(name).and_then(|g| strata_deps::main_tip(cfg, g).ok().flatten());
                (name.clone(), tip)
            })
            .collect()
    }

    /// `deps::observe_main` for the workspace `new` created in `repo`: the
    /// same (tree, head, export) `new` used. Takes the repo lock itself.
    pub fn observe_new(&self, report: &NewReport, repo: &RepoName) -> Result<()> {
        let ws = report
            .manifest
            .repos
            .get(repo)
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, format!("no workspace of {repo}")))?;
        let ctx = strata_deps::Ctx {
            store: &self.store,
            paths: &self.config.paths,
            repo,
            cfg: self.repo_cfg(repo)?,
            git: self.git(repo)?,
            env: &self.env,
            locks: &self.locks,
        };
        strata_deps::observe_main(&ctx, &ws.base.tree, &ws.head, &self.store.base_dir(&ws.base)).map(|_| ())
    }

    /// §5.7 after `new` returned: per repo of the session, advance the
    /// anchor (a failure is a warning, `new` is not failed); where `new`
    /// reported `ObserveFailed`, observe once more with `observe` (the
    /// handler passes `Core::observe_new`). Success drops the warning;
    /// failure replaces its detail. `new` already observed every repo, so
    /// nothing else is observed again.
    pub fn after_new(
        &self,
        report: &mut NewReport,
        observe: &dyn Fn(&NewReport, &RepoName) -> Result<()>,
    ) -> AfterNew {
        let mut out = AfterNew::default();
        let repos: Vec<RepoName> = report.manifest.repos.keys().cloned().collect();
        for repo in &repos {
            if let Err(e) = self.advance_anchor(repo) {
                tracing::warn!(repo = %repo, error = %e.message, "anchor not advanced after new");
                out.anchor_unknown.push((repo.clone(), e));
            }
        }
        let failed: Vec<RepoName> = report
            .warnings
            .iter()
            .map(|w| match w {
                NewWarning::ObserveFailed { repo, .. } => repo.clone(),
            })
            .collect();
        for repo in failed {
            out.reobserved.push(repo.clone());
            let again = observe(report, &repo);
            report.warnings.retain_mut(|w| match w {
                NewWarning::ObserveFailed { repo: r, detail } if *r == repo => match &again {
                    Ok(()) => false,
                    Err(e) => {
                        *detail = e.clone();
                        true
                    }
                },
                _ => true,
            });
        }
        out
    }

    pub(crate) fn git(&self, repo: &RepoName) -> Result<&Git> {
        self.gits.get(repo).ok_or_else(|| StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")))
    }

    pub(crate) fn repo_cfg(&self, repo: &RepoName) -> Result<&strata_types::RepoConfig> {
        self.config
            .repos
            .get(repo)
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")))
    }
}

/// After a fetch outside `new` and `publish`: observe when `main_tip`
/// moved; a failure is tried once more and then is `ObserveFailed` (the
/// returned error), never a failure of the call that fetched.
pub fn observe_if_moved(
    before: Option<&CommitOid>,
    after: Option<&CommitOid>,
    observe: &mut dyn FnMut() -> Result<()>,
) -> Option<StrataError> {
    if before == after {
        return None;
    }
    match observe() {
        Ok(()) => None,
        Err(_) => observe().err(),
    }
}

fn reason_of(e: &StrataError) -> Option<&str> {
    e.detail.as_ref()?.get("reason")?.as_str()
}

/// The attach reasons that may have written outside the staging tree
/// (gitio's spelling, hyphenated).
pub const ATTACH_OUTSIDE_REASONS: [&str; 2] = ["index-read-back-refused", "staging-changed"];

/// The attach reason of an `Unknown` from `move_workspace` or an attach:
/// `detail.reason`, or `detail.cause.detail.reason` (session wraps gitio's
/// error as `cause`).
pub fn attach_outside_reason(e: &StrataError) -> Option<&str> {
    if e.code != Code::Unknown {
        return None;
    }
    let d = e.detail.as_ref()?;
    let direct = d.get("reason").and_then(Value::as_str);
    let cause = d.get("cause").and_then(|c| c.get("detail")).and_then(|c| c.get("reason")).and_then(Value::as_str);
    [direct, cause].into_iter().flatten().find(|r| ATTACH_OUTSIDE_REASONS.contains(r))
}

/// §5.10: an attach that may have written outside its staging raises
/// `AttachOutside` naming `<main>/.git/index` (one stderr line). The error
/// passes through unchanged; the main checkout is not touched.
pub fn alert_attach(warnings: &Warnings, main: &Path, e: &StrataError) -> bool {
    match attach_outside_reason(e) {
        Some(reason) => {
            warnings.add(DaemonWarning::AttachOutside { reason: reason.to_owned(), index: main.join(".git/index") });
            true
        }
        None => false,
    }
}

/// §5.10: `PrePushGuardDisabled` in a status is only reported: one stderr
/// line the first time this daemon sees it for a shadow repository. No
/// re-pinning, no rebuild, no config write. Returns the shadows newly
/// reported.
pub fn report_guard_disabled(warnings: &Warnings, status: &SessionStatus, shadow_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (repo, ws) in &status.workspaces {
        let disabled = ws.warnings.iter().any(|w| match w {
            strata_session::StatusWarning::MirrorDrift { items } => {
                items.iter().any(|i| i.reason == "pre_push_guard_disabled")
            }
            _ => false,
        });
        if !disabled {
            continue;
        }
        let shadow = shadow_dir(shadow_root, &status.manifest.id, repo);
        let line = format!(
            "pre-push guard disabled in {} (session {}, repo {repo}): reported only; nothing was re-pinned",
            shadow.display(),
            status.manifest.id
        );
        if warnings.alert_first(&format!("guard:{}", shadow.display()), &line) {
            out.push(shadow);
        }
    }
    out
}

/// session.md §4.1: `<root>/sessions/.repos/<id>/<repo>.git`.
pub fn shadow_dir(root: &Path, id: &SessionId, repo: &RepoName) -> PathBuf {
    root.join("sessions/.repos").join(id.as_str()).join(format!("{repo}.git"))
}

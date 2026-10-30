//! `add` (session.md D52): give an `Active` session workspaces of more
//! repos, all tracked. Each repo is built in a staging directory under the
//! session root (`.strata-tmp-add-<repo>-<hex>`), pinned, renamed into
//! place, and only then recorded in the store; the session itself never
//! leaves `Active`.
//!
//! A failure before the store row leaves the session as it was: the pin is
//! deleted, and the staging directory (or the renamed workspace, if the
//! store write failed) and the shadow repository are kept, named in the
//! error's `kept`. `rm` of the session retires them with the session root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::RenameMode;
use strata_store::{Lifecycle, SeedRow};
use strata_types::{BaseGen, Code, CommitOid, RepoName, SessionId, StrataError, TreeOid, WorkspaceState};

use super::{NewWarning, Plan, pin_ref};
use crate::seed::{ancestors, seed_files};
use crate::sys::{lstat, random_hex, rp};
use crate::{Result, Sessions, dispatch};

/// Prefix of `add`'s staging directory under the session root.
pub const ADD_STAGING_PREFIX: &str = ".strata-tmp-add-";

/// What `add` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddReport {
    pub session: SessionId,
    /// The workspaces made by this call.
    pub added: BTreeMap<RepoName, WorkspaceState>,
    /// Requested repos the session already had; nothing was done to them.
    pub already: BTreeMap<RepoName, WorkspaceState>,
    /// Observing main for the added repos; empty means no warnings.
    pub warnings: Vec<NewWarning>,
}

impl Sessions<'_> {
    /// Add workspaces of `repos` to the `Active` session `id` (D52). Repos
    /// the session already has are reported in `already`, not changed.
    pub fn add(&self, id: &SessionId, repos: &[RepoName]) -> Result<AddReport> {
        // A. refusals, before any write
        if repos.is_empty() {
            return Err(StrataError::new(Code::InvalidInput, "add needs at least one repo")
                .with_detail(json!({ "field": "repos", "reason": "empty" })));
        }
        let rec = self.store.session(id)?.ok_or_else(|| {
            StrataError::new(Code::NoSuchSession, format!("no session {id}")).with_detail(json!({ "session": id.as_str() }))
        })?;
        if rec.lifecycle != Lifecycle::Active {
            return Err(StrataError::new(
                Code::InvalidInput,
                format!("session {id} is not active; only an active session can get more repos"),
            )
            .with_detail(json!({ "reason": "lifecycle", "session": id.as_str(), "lifecycle": format!("{:?}", rec.lifecycle).to_lowercase() })));
        }
        let mut want = BTreeSet::new();
        for r in repos {
            self.repo_cfg(r)?;
            want.insert(r.clone());
        }
        let already: BTreeMap<RepoName, WorkspaceState> =
            rec.manifest.repos.iter().filter(|(r, _)| want.contains(*r)).map(|(r, w)| (r.clone(), w.clone())).collect();
        let new: BTreeSet<RepoName> = want.into_iter().filter(|r| !already.contains_key(r)).collect();
        if new.is_empty() {
            return Ok(AddReport { session: id.clone(), added: BTreeMap::new(), already, warnings: Vec::new() });
        }
        self.validate_members(&new, &new)?;
        let root = rec.manifest.root.clone();
        for r in &new {
            for p in [root.join(r.as_str()), self.shadow_dir(id, r)] {
                if lstat(&p).map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &p))?.is_some() {
                    return Err(StrataError::new(
                        Code::Exists,
                        format!("{} already exists; a failed add left it (remove the session, or start a new one)", p.display()),
                    )
                    .with_detail(json!({ "reason": "add_leftover", "session": id.as_str(), "repo": r.as_str(), "path": p.to_string_lossy() })));
                }
            }
        }
        let new: Vec<RepoName> = new.into_iter().collect();

        // B. heads, bases, plans
        let heads: BTreeMap<RepoName, (CommitOid, TreeOid)> = self.resolve_heads(&new)?.into_iter().collect();
        let planned = dispatch::each_repo(&new, &|_| Ok(()), &|r| {
            let (_, tree) = &heads[r];
            let bg = self.base_flow(r, tree, None)?;
            let plan = self.plan_repo(r, &bg, tree, true)?;
            Ok((bg, plan))
        })?;
        let plans: BTreeMap<RepoName, (BaseGen, Plan)> = planned.into_iter().collect();

        // C. build each workspace in staging and move it into place
        let built = dispatch::each_repo(&new, &|r| self.seams.hit("add.build", Some(r)), &|r| {
            let ws = WorkspaceState {
                base: plans[r].0.clone(),
                head: heads[r].0.clone(),
                path: root.join(r.as_str()),
                tracked: true,
            };
            let seeds = self.add_build(id, &root, r, &ws, &plans[r].1)?;
            Ok((ws, seeds))
        });
        let built = match built {
            Ok(b) => b,
            Err(e) => {
                // Repos that did build are in place but not recorded; unpin
                // them all and name them in `kept` with the staging dir.
                self.add_unpin(id, &new);
                let mut kept: Vec<String> = e
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("kept"))
                    .and_then(|k| k.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                for r in &new {
                    let p = root.join(r.as_str());
                    if matches!(lstat(&p), Ok(Some(_))) {
                        kept.push(p.display().to_string());
                    }
                }
                return Err(with_kept(e, &kept));
            }
        };

        // D. record
        let added: BTreeMap<RepoName, WorkspaceState> = built.iter().map(|(r, (ws, _))| (r.clone(), ws.clone())).collect();
        let seeds: BTreeMap<RepoName, Vec<SeedRow>> = built.into_iter().map(|(r, (_, s))| (r, s)).collect();
        if let Err(e) = self.store.add_workspaces(id, &added, &seeds) {
            self.add_unpin(id, &new);
            let kept: Vec<String> = added.values().map(|w| w.path.display().to_string()).collect();
            return Err(with_kept(e, &kept));
        }

        // E. observe main; failures are warnings
        let mut warnings = Vec::new();
        for (r, ws) in &added {
            let res = self.deps_ctx(r).and_then(|ctx| {
                strata_deps::observe_main(&ctx, &ws.base.tree, &ws.head, &self.store.base_dir(&ws.base))
            });
            if let Err(detail) = res {
                warnings.push(NewWarning::ObserveFailed { repo: r.clone(), detail });
            }
        }
        Ok(AddReport { session: id.clone(), added, already, warnings })
    }

    /// One repo: clone, caches, git, seed files and pin in staging, then
    /// rename into place. Returns the seed rows to record.
    fn add_build(&self, id: &SessionId, root: &std::path::Path, r: &RepoName, ws: &WorkspaceState, plan: &Plan) -> Result<Vec<SeedRow>> {
        let sname = format!("{ADD_STAGING_PREFIX}{r}-{}", random_hex(8));
        let staging: PathBuf = root.join(&sname);
        let fail = |e: StrataError| with_kept(e, &[staging.display().to_string()]);
        strata_apfs::clone_tree_rooted(&self.store.base_dir(&ws.base), root, &rp(&sname)).map_err(fail)?;
        let cfg = self.repo_cfg(r)?;
        for c in &plan.caches {
            if let Some(parent) = ancestors(c).last() {
                strata_apfs::mkdir_rooted(&staging, parent, 0o755, true).map_err(fail)?;
            }
            strata_apfs::clone_tree_rooted(&cfg.path.join(c.as_str()), &staging, c).map_err(fail)?;
            crate::seed::drop_connector_files(&staging, c).map_err(fail)?;
        }
        let git = self.git(r)?;
        let dir = self.shadow_dir(id, r);
        self.with_lock_unbounded(r, || {
            let shadow = git.ensure_shadow(&dir)?;
            git.attach_worktree(&shadow, 1, &ws.head, &staging, &ws.path)?;
            Ok(())
        })
        .map_err(fail)?;
        let records = seed_files(&plan.files, &staging).map_err(fail)?;
        self.with_lock_unbounded(r, || git.update_ref(&pin_ref(id), &ws.head, None)).map_err(fail)?;
        strata_apfs::rename_rooted(root, &rp(&sname), &rp(r.as_str()), RenameMode::Excl).map_err(fail)?;
        Ok(records.iter().map(SeedRow::from).collect())
    }

    /// Best effort: delete this session's pin in each repo not recorded in it.
    fn add_unpin(&self, id: &SessionId, repos: &[RepoName]) {
        let pin = pin_ref(id);
        for r in repos {
            let Ok(git) = self.git(r) else { continue };
            let _ = self.with_lock_unbounded(r, || {
                if let Some(c) = git.read_ref(&pin)? {
                    git.delete_ref(&pin, &c)?;
                }
                Ok(())
            });
        }
    }
}

fn with_kept(mut e: StrataError, kept: &[String]) -> StrataError {
    let mut d = match e.detail.take() {
        Some(serde_json::Value::Object(m)) => m,
        Some(other) => {
            let mut m = serde_json::Map::new();
            m.insert("cause_detail".into(), other);
            m
        }
        None => serde_json::Map::new(),
    };
    d.insert("op".into(), json!("add"));
    d.insert("kept".into(), json!(kept));
    e.detail = Some(serde_json::Value::Object(d));
    e
}

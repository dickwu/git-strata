//! `remove(id, discard, confirm)` (§5.6): named refusals first, then the
//! internal clean-up and the BeforeRemove checkpoints, then the visible
//! steps (Removing, unpin, retire, Removed); a failure after Removing is an
//! Unknown scene.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_overlay::WorkspaceRef;
use strata_store::{Health, Lifecycle, MoveIntent, MoveState, SessionRecord};
use strata_types::{Action, CheckpointId, CheckpointReason, Code, ConfirmToken, RepoName, SessionId, StrataError, WorkspaceState};

use crate::new::pin_ref;
use crate::scene::{Scene, SceneOp, cause_of, kept_dirs, unknown_error};
use crate::seams::is_stop;
use crate::status::{no_such_session, non_empty_dir};
use crate::sys::lstat;
use crate::{DIRTY_PATHS_CAP, Result, Sessions};

/// What `remove` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    pub session: SessionId,
    pub discarded: bool,
    /// The BeforeRemove checkpoint of each tracked repo, recorded now.
    pub checkpoints: BTreeMap<RepoName, CheckpointId>,
}

fn exists(p: &Path) -> bool {
    matches!(lstat(p), Ok(Some(_)))
}

fn lc_name(l: Lifecycle) -> String {
    format!("{l:?}").to_lowercase()
}

impl Sessions<'_> {
    /// §5.6.
    pub fn remove(&self, id: &SessionId, discard: bool, confirm: Option<ConfirmToken>) -> Result<RemoveReport> {
        // ---- Phase A: named refusals; nothing is written.
        // 1.
        let rec = match self.store.session(id)? {
            Some(r) if r.lifecycle != Lifecycle::Removed => r,
            _ => return Err(no_such_session(id.as_str())),
        };
        let cur = rec.lifecycle;
        if !matches!(cur, Lifecycle::Active | Lifecycle::Unknown) {
            let state = lc_name(cur);
            return Err(StrataError::new(
                Code::InvalidInput,
                format!("session {id} is {state}; another operation is in progress or was interrupted"),
            )
            .with_detail(json!({ "reason": "lifecycle", "state": state, "session": id.as_str() })));
        }
        // 2. The token is consumed by value here.
        if discard {
            let expected = Action::RemoveDiscard { session: id.clone() };
            let why = match &confirm {
                None => Some("missing"),
                Some(t) if t.action != expected => Some("mismatch"),
                Some(_) => None,
            };
            if let Some(why) = why {
                return Err(StrataError::new(
                    Code::NotConfirmed,
                    format!("removing {id} with --discard needs a confirmation for exactly this session ({why})"),
                )
                .with_detail(json!({ "expected": expected, "reason": why })));
            }
        }
        drop(confirm);
        // 3. Open files; discard does not bypass this and there is no force.
        let root = rec.manifest.root.clone();
        let repos_dir = self.config.paths.root.join("sessions/.repos").join(id.as_str());
        let mut open = strata_deps::open_files(&root)?;
        open.extend(strata_deps::open_files(&repos_dir)?);
        if !open.is_empty() {
            let mut pids: Vec<i32> = open.iter().map(|o| o.pid).collect();
            pids.sort_unstable();
            pids.dedup();
            return Err(StrataError::new(
                Code::OpenFiles,
                format!(
                    "processes {} hold files in session {id}; close them and remove again",
                    pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
                ),
            )
            .with_detail(json!({ "session": id.as_str(), "paths": [root, repos_dir], "open": open })));
        }
        let intents = self.store.move_intents(id)?;
        // 4.
        if !discard {
            self.dirty_check(&rec, &intents)?;
        }

        // ---- Phase B: internal clean-up and the BeforeRemove checkpoints.
        // 5.1
        self.collect_session_leftovers(&rec, &intents)?;
        // 5.2
        let mut checkpoints = BTreeMap::new();
        if cur == Lifecycle::Active {
            for (repo, ws) in &rec.manifest.repos {
                if !ws.tracked || !self.healthy(&rec, repo, ws)? {
                    continue;
                }
                let cfg = self.repo_cfg(repo)?;
                let git = self.git(repo)?;
                let base = self.base_of(&ws.base)?;
                let wsref = WorkspaceRef { session: id, repo, root: &ws.path, config: cfg, git, store: self.store };
                let cp = self.before.checkpoint(&wsref, &base, CheckpointReason::BeforeRemove)?;
                checkpoints.insert(repo.clone(), cp.checkpoint.id);
            }
        }

        // ---- Phase C: the visible steps.
        // 6.
        if !self.store.set_lifecycle(id, cur, Lifecycle::Removing, None, SystemTime::now())? {
            return Err(StrataError::new(
                Code::InvalidInput,
                format!("session {id} changed state while it was being removed"),
            )
            .with_detail(json!({ "reason": "lifecycle_changed", "session": id.as_str(), "was": lc_name(cur) })));
        }
        let mut phase = "unpin";
        let r = (|| -> Result<()> {
            // 7.
            self.store.set_session_phase(id, "unpin")?;
            self.seams.hit("remove.unpin", None)?;
            let pin = pin_ref(id);
            for repo in rec.manifest.repos.keys() {
                let git = self.git(repo)?;
                self.with_lock_unbounded(repo, || {
                    if let Some(c) = git.read_ref(&pin)? {
                        git.delete_ref(&pin, &c)?;
                    }
                    Ok(())
                })?;
            }
            // 8.
            phase = "retire";
            self.store.set_session_phase(id, "retire")?;
            self.seams.hit("remove.retire", None)?;
            for d in [&root, &repos_dir] {
                if !exists(d) {
                    continue;
                }
                match self.store.retire_dir(d) {
                    Ok(_) => {}
                    Err(e) if e.code == Code::NoSuchPath => {}
                    Err(e) => return Err(e),
                }
            }
            // 9.
            phase = "record";
            self.store.set_session_phase(id, "record")?;
            self.seams.hit("remove.record", None)?;
            if !self.store.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Removed, None, SystemTime::now())? {
                return Err(StrataError::new(Code::Corrupt, format!("session {id} left Removing during remove"))
                    .with_detail(json!({ "reason": "lifecycle_changed", "session": id.as_str() })));
            }
            Ok(())
        })();
        match r {
            Ok(()) => Ok(RemoveReport { session: id.clone(), discarded: discard, checkpoints }),
            Err(e) if is_stop(&e) => Err(e),
            Err(e) => Err(self.remove_unknown(&rec, phase, &e)),
        }
    }

    /// The scene of a failure in steps 7–9 (§6.3): recorded on the session
    /// row; if that fails too the row stays Removing and `recover` marks it.
    fn remove_unknown(&self, rec: &SessionRecord, phase: &str, e: &StrataError) -> StrataError {
        let id = &rec.manifest.id;
        let kept = kept_dirs(&self.config.paths.root, id, rec.manifest.repos.keys().cloned());
        let err = unknown_error(SceneOp::Remove, id, None, phase, cause_of(e), &kept, Map::new());
        let now = SystemTime::now();
        let scene =
            Scene { op: SceneOp::Remove, session: id.clone(), repo: None, phase: phase.into(), at: now, kept, error: err.clone() };
        if let Ok(v) = serde_json::to_value(&scene) {
            let _ = self.store.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Unknown, Some(&v), now);
        }
        err
    }

    /// A workspace that exists and is healthy: not Unknown in the store, its
    /// directory and `.git` present, its shadow repository there.
    fn healthy(&self, rec: &SessionRecord, repo: &RepoName, ws: &WorkspaceState) -> Result<bool> {
        Ok(self.unhealthy(rec, repo, ws)?.is_none())
    }

    /// Why a workspace is not healthy: `Some("unknown")` or `Some("missing")`.
    fn unhealthy(&self, rec: &SessionRecord, repo: &RepoName, ws: &WorkspaceState) -> Result<Option<&'static str>> {
        match rec.workspaces.get(repo) {
            Some(row) if row.health == Health::Unknown => return Ok(Some("unknown")),
            None => return Ok(Some("missing")),
            Some(_) => {}
        }
        let dir_ok = matches!(lstat(&ws.path), Ok(Some(m)) if m.is_dir());
        if !dir_ok || !exists(&ws.path.join(".git")) {
            return Ok(Some("missing"));
        }
        match self.git(repo)?.shadow_at(&self.shadow_dir(&rec.manifest.id, repo)) {
            Ok(_) => Ok(None),
            Err(e) if e.code == Code::NoSuchPath => Ok(Some("missing")),
            Err(e) => Err(e),
        }
    }

    /// §5.6 step 4: Dirty with one section per rule, or Ok.
    fn dirty_check(&self, rec: &SessionRecord, intents: &[MoveIntent]) -> Result<()> {
        let id = &rec.manifest.id;
        let mut unresolved: Vec<Value> = Vec::new();
        let mut repos = Map::new();
        let mut untracked: Vec<Value> = Vec::new();
        let mut old_trees: Vec<Value> = Vec::new();
        if rec.lifecycle == Lifecycle::Unknown {
            unresolved.push(json!({ "kind": "session_unknown", "scene": rec.scene.clone().unwrap_or(Value::Null) }));
        }
        for (repo, ws) in &rec.manifest.repos {
            let bad = self.unhealthy(rec, repo, ws)?;
            match bad {
                Some("unknown") => unresolved.push(json!({ "kind": "workspace_unknown", "repo": repo.as_str() })),
                Some(_) => unresolved
                    .push(json!({ "kind": "workspace_missing", "repo": repo.as_str(), "path": ws.path })),
                None => {}
            }
            let restore = rec.manifest.root.join(format!("{}{repo}", strata_overlay::RESTORE_TEMP_PREFIX));
            if non_empty_dir(&restore)? {
                unresolved.push(json!({ "kind": "restore_scene", "repo": repo.as_str(), "path": restore }));
            }
            for i in intents.iter().filter(|i| &i.repo == repo && i.state == MoveState::Swapping) {
                unresolved.push(
                    json!({ "kind": "move_awaiting_recover", "repo": repo.as_str(), "n": i.n, "staging": i.staging }),
                );
            }
            // Tracked or not, a healthy workspace of an Active session is
            // reconciled against its base. An untracked one only blocks when
            // that finds something: its edits have no checkpoint, but an
            // untouched sibling loses nothing (D50).
            if bad.is_none() && rec.lifecycle == Lifecycle::Active {
                let cfg = self.repo_cfg(repo)?;
                let git = self.git(repo)?;
                let base = self.base_of(&ws.base)?;
                let wsref = WorkspaceRef { session: id, repo, root: &ws.path, config: cfg, git, store: self.store };
                let o = strata_overlay::reconcile_full(&wsref, &base)?;
                if !o.entries.is_empty() {
                    if !ws.tracked {
                        untracked.push(json!(repo.as_str()));
                    }
                    let truncated = o.entries.len() > DIRTY_PATHS_CAP;
                    let paths: Vec<&str> = o.entries.iter().take(DIRTY_PATHS_CAP).map(|e| e.path.as_str()).collect();
                    repos.insert(
                        repo.as_str().into(),
                        json!({ "paths": paths, "count": o.entries.len(), "truncated": truncated }),
                    );
                }
            }
            // Changed old trees (the rule of §5.5 step 2.9).
            let cfg = self.repo_cfg(repo)?;
            for i in intents.iter().filter(|i| &i.repo == repo && i.state == MoveState::Done) {
                let path = i.old_tree.clone().unwrap_or_else(|| i.staging.clone());
                if !exists(&path) {
                    continue;
                }
                if !self.old_tree_unchanged(i, cfg).unwrap_or(false) {
                    old_trees.push(json!({ "repo": repo.as_str(), "n": i.n, "path": path }));
                }
            }
        }
        if unresolved.is_empty() && repos.is_empty() && untracked.is_empty() && old_trees.is_empty() {
            return Ok(());
        }
        let mut what = Vec::new();
        if !unresolved.is_empty() {
            what.push("unresolved scenes");
        }
        if !repos.is_empty() {
            what.push("unpublished changes");
        }
        if !untracked.is_empty() {
            what.push("untracked repos");
        }
        if !old_trees.is_empty() {
            what.push("changed old trees");
        }
        Err(StrataError::new(
            Code::Dirty,
            format!("session {id} has {}; remove it with --discard to throw them away", what.join(", ")),
        )
        .with_detail(json!({
            "session": id.as_str(),
            "unresolved": unresolved,
            "repos": repos,
            "untracked_repos": untracked,
            "old_trees": old_trees,
        })))
    }

    /// §5.6 step 5.1: close the moves that never reached their swap and
    /// collect stagings an Abandon could not retire. An Unknown session's
    /// stagings leave with its root (step 8).
    fn collect_session_leftovers(&self, rec: &SessionRecord, intents: &[MoveIntent]) -> Result<()> {
        let id = &rec.manifest.id;
        let active = rec.lifecycle == Lifecycle::Active;
        for i in intents {
            let pending = i.state == MoveState::Preparing;
            let stale = i.state == MoveState::Abandoned && exists(&i.staging);
            if !pending && !stale {
                continue;
            }
            let shadow_dir: PathBuf = self.shadow_dir(id, &i.repo);
            self.with_lock(&i.repo, || {
                if active {
                    self.collect_leftover(&i.staging, &shadow_dir, i.n)?;
                }
                if pending {
                    self.store.set_move_state(
                        id,
                        &i.repo,
                        i.n,
                        MoveState::Preparing,
                        MoveState::Abandoned,
                        None,
                        SystemTime::now(),
                    )?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

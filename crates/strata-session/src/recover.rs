//! `leftovers()` (§5.5) and `recover()` (§5.8): what a stopped process
//! left behind, reported read-only or collected once at daemon start.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use strata_apfs::FileKind;
use strata_store::{Lifecycle, MoveState};
use strata_types::{Code, RepoName, RepoPath, SessionId, StrataError};

use crate::scene::{Scene, SceneOp, interrupted, kept_dirs};
use crate::sys::rp;
use crate::{BASE_STAGING_DIR, Result, Sessions};

/// Leftovers that belong to no session (§5.5). The daemon merges them
/// into status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Leftover {
    /// A base staging directory a stopped process (or a failed retirement)
    /// left, with gitio's export temp directories in it.
    BaseStaging { repo: RepoName, path: PathBuf, export_temps: Vec<PathBuf> },
    /// rm-followups R3: `sessions/<sid>` or `sessions/.repos/<sid>` of a
    /// session that is Removed or has no row (R1 moves it to trash).
    RemovedSession { session: SessionId, path: PathBuf },
}

/// What `recover` did (§5.8).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverReport {
    /// Creating and Removing sessions marked Unknown.
    pub unknown: Vec<SessionId>,
    /// Move intents that never reached the swap, closed as Abandoned.
    pub abandoned: Vec<IntentRef>,
    /// Move intents stopped in the swap: the workspace is now Unknown.
    pub failed: Vec<IntentRef>,
    /// Base staging directories retired into the store's trash (their
    /// paths before retirement).
    pub retired: Vec<PathBuf>,
    /// Per-item failures; recover goes on past them.
    pub errors: Vec<StrataError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentRef {
    pub session: SessionId,
    pub repo: RepoName,
    pub n: u64,
}

impl Sessions<'_> {
    /// The entries of `bases/<repo>/.staging/`; a missing directory is none.
    fn staging_entries(&self, repo: &RepoName) -> Result<Vec<(RepoPath, FileKind)>> {
        let dir = rp(format!("bases/{repo}/{BASE_STAGING_DIR}"));
        let mut out = Vec::new();
        for item in strata_apfs::list_dir(&self.config.paths.root, Some(&dir)) {
            match item {
                Ok((p, a)) => out.push((p, a.kind)),
                Err(e) if e.code == Code::NoSuchPath && out.is_empty() => return Ok(Vec::new()),
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn is_live_staging(&self, p: &Path) -> bool {
        self.live_staging.lock().unwrap_or_else(|x| x.into_inner()).contains(p)
    }

    /// §5.5 `leftovers()`: read-only. Staging directories this process is
    /// building now are not leftovers.
    pub fn leftovers(&self) -> Result<Vec<Leftover>> {
        let root = &self.config.paths.root;
        let mut out = Vec::new();
        for repo in self.config.repos.keys() {
            for (rel, kind) in self.staging_entries(repo)? {
                let path = root.join(rel.as_str());
                if self.is_live_staging(&path) {
                    continue;
                }
                let mut export_temps = Vec::new();
                if matches!(kind, FileKind::Dir) {
                    for item in strata_apfs::list_dir(root, Some(&rel)) {
                        let (p, a) = match item {
                            Ok(x) => x,
                            // Retired between the two listings.
                            Err(e) if e.code == Code::NoSuchPath && export_temps.is_empty() => {
                                break;
                            }
                            Err(e) => return Err(e),
                        };
                        let named = Path::new(p.as_str())
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(strata_gitio::EXPORT_TEMP_PREFIX));
                        if named && matches!(a.kind, FileKind::Dir) {
                            export_temps.push(root.join(p.as_str()));
                        }
                    }
                }
                out.push(Leftover::BaseStaging { repo: repo.clone(), path, export_temps });
            }
        }
        // rm-followups R3: directories of removed sessions.
        out.extend(crate::rm_note::removed_leftovers(self.store, root)?);
        Ok(out)
    }

    /// §5.8 `recover()`: called once at daemon start, before gc and any
    /// request. Only the store's codes pass through; per-item failures go
    /// into `errors`.
    pub fn recover(&self) -> Result<RecoverReport> {
        let mut rep = RecoverReport::default();
        let root = &self.config.paths.root;

        // Step 1: Creating and Removing sessions become Unknown.
        for rec in self.store.sessions()? {
            let (op, first) = match rec.lifecycle {
                Lifecycle::Creating => (SceneOp::New, "root"),
                Lifecycle::Removing => (SceneOp::Remove, "unpin"),
                _ => continue,
            };
            let id = rec.manifest.id.clone();
            let phase = rec.phase.clone().unwrap_or_else(|| first.to_string());
            let kept = kept_dirs(root, &id, rec.manifest.repos.keys().cloned());
            let error = interrupted(op, &id, &phase, &kept);
            let now = SystemTime::now();
            let scene = Scene { op, session: id.clone(), repo: None, phase, at: now, kept, error };
            let v = match serde_json::to_value(&scene) {
                Ok(v) => v,
                Err(e) => {
                    rep.errors.push(StrataError::new(Code::Unknown, format!("recover {id}: scene: {e}")));
                    continue;
                }
            };
            match self.store.set_lifecycle(&id, rec.lifecycle, Lifecycle::Unknown, Some(&v), now) {
                Ok(true) => rep.unknown.push(id),
                Ok(false) => rep.errors.push(
                    StrataError::new(
                        Code::Busy,
                        format!("recover {id}: the session left {:?} meanwhile", rec.lifecycle),
                    )
                    .with_detail(serde_json::json!({ "session": id.as_str(), "reason": "lifecycle_changed" })),
                ),
                Err(e) => rep.errors.push(e),
            }
        }

        // Step 2: open move intents. Preparing ones never reached the swap:
        // their staging goes, the intent is Abandoned. Swapping ones may
        // have swapped: the workspace becomes Unknown with the scene.
        let intents = match self.store.open_move_intents() {
            Ok(v) => v,
            Err(e) => {
                rep.errors.push(e);
                Vec::new()
            }
        };
        for i in intents {
            let lifecycle = match self.store.session(&i.session) {
                Ok(Some(r)) => r.lifecycle,
                Ok(None) => continue,
                Err(e) => {
                    rep.errors.push(e);
                    continue;
                }
            };
            if lifecycle == Lifecycle::Removed {
                continue;
            }
            let r = IntentRef { session: i.session.clone(), repo: i.repo.clone(), n: i.n };
            match i.state {
                MoveState::Preparing => {
                    if lifecycle == Lifecycle::Active {
                        let shadow = self.shadow_dir(&i.session, &i.repo);
                        if let Err(e) = self.collect_leftover(&i.staging, &shadow, i.n) {
                            rep.errors.push(e);
                            continue;
                        }
                    }
                    match self.store.set_move_state(
                        &i.session,
                        &i.repo,
                        i.n,
                        MoveState::Preparing,
                        MoveState::Abandoned,
                        None,
                        SystemTime::now(),
                    ) {
                        Ok(true) => rep.abandoned.push(r),
                        Ok(false) => {}
                        Err(e) => rep.errors.push(e),
                    }
                }
                MoveState::Swapping => {
                    let phase = i.phase.clone().unwrap_or_else(|| "swap".to_string());
                    let ws_path = self
                        .store
                        .session(&i.session)
                        .ok()
                        .flatten()
                        .and_then(|rec| rec.manifest.repos.get(&i.repo).map(|w| w.path.clone()));
                    let mut kept = Vec::new();
                    kept.extend(ws_path);
                    kept.push(i.staging.clone());
                    let mut error = interrupted(SceneOp::Move, &i.session, &phase, &kept);
                    if let Some(serde_json::Value::Object(m)) = &mut error.detail {
                        m.insert("repo".into(), serde_json::json!(i.repo.as_str()));
                        m.insert("n".into(), serde_json::json!(i.n));
                    }
                    let now = SystemTime::now();
                    let scene = Scene {
                        op: SceneOp::Move,
                        session: i.session.clone(),
                        repo: Some(i.repo.clone()),
                        phase,
                        at: now,
                        kept,
                        error,
                    };
                    let v = match serde_json::to_value(&scene) {
                        Ok(v) => v,
                        Err(e) => {
                            rep.errors
                                .push(StrataError::new(Code::Unknown, format!("recover {}: scene: {e}", i.session)));
                            continue;
                        }
                    };
                    match self.store.fail_move(&i.session, &i.repo, i.n, &v, now) {
                        Ok(()) => rep.failed.push(r),
                        Err(e) => rep.errors.push(e),
                    }
                }
                _ => {}
            }
        }

        // Step 3: every base staging entry goes to the trash.
        for repo in self.config.repos.keys() {
            let entries = match self.staging_entries(repo) {
                Ok(v) => v,
                Err(e) => {
                    rep.errors.push(e);
                    continue;
                }
            };
            for (rel, _) in entries {
                let path = root.join(rel.as_str());
                if self.is_live_staging(&path) {
                    continue;
                }
                match self.store.retire_dir(&path) {
                    Ok(_) => rep.retired.push(path),
                    Err(e) if e.code == Code::NoSuchPath => {}
                    Err(e) => rep.errors.push(e),
                }
            }
        }
        Ok(rep)
    }
}

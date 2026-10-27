//! `status(id)` (§5.5): read-only, no write lock.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::{IoSide, classify_io_error};
use strata_deps::{Drift, OpenFile, Proposal, ProposalState};
use strata_gitio::MirrorDrift;
use strata_store::{Health, Lifecycle, MoveState, SessionRecord, WorkspaceRecord};
use strata_types::{CheckpointId, Code, RepoName, RepoPath, SessionId, SessionManifest, StrataError};

use crate::scene::{Scene, scene_of};
use crate::seed::{SeedRecord, mtime_of, written_stamp};
use crate::sys::lstat;
use crate::{Result, Sessions};

/// A session's status (design §6.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub manifest: SessionManifest,
    pub state: SessionState,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub created: SystemTime,
    /// The phase `new` or `remove` last recorded (§6.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The scene when state is Unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<Scene>,
    pub workspaces: BTreeMap<RepoName, WorkspaceStatus>,
}

/// The session lifecycle (§4.3); the store's `Lifecycle` mirrors it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Creating,
    Active,
    Removing,
    Removed,
    Unknown,
}

impl From<Lifecycle> for SessionState {
    fn from(l: Lifecycle) -> Self {
        match l {
            Lifecycle::Creating => SessionState::Creating,
            Lifecycle::Active => SessionState::Active,
            Lifecycle::Removing => SessionState::Removing,
            Lifecycle::Removed => SessionState::Removed,
            Lifecycle::Unknown => SessionState::Unknown,
        }
    }
}

/// Missing is read from the filesystem and never stored; Unknown comes from
/// the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceHealth {
    Ok,
    Missing,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    pub health: WorkspaceHealth,
    /// The admin-dir generation (§4.2).
    pub generation: u64,
    /// The scene when health is Unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<Scene>,
    /// The latest checkpoint; none for tracked = false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<Drift>,
    pub pending: Vec<Proposal>,
    pub conflicts: Vec<RepoPath>,
    pub seeds: Vec<SeedRecord>,
    pub warnings: Vec<StatusWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StatusWarning {
    NotCheckpointed,
    SeedStale { path: RepoPath },
    SeedNotIgnored { path: RepoPath },
    MirrorDrift { items: Vec<MirrorDriftItem> },
    PrePushGuardUnknown { detail: StrataError },
    ShadowMissing { path: PathBuf },
    /// The shadow directory exists but is not this repo's shadow
    /// repository (`reason` from gitio, e.g. `no-marker`).
    ShadowInvalid { path: PathBuf, reason: String },
    MoveLeftover { n: u64, staging: PathBuf },
    MoveAwaitingRecover { n: u64, staging: PathBuf },
    OldTreeKept { path: PathBuf, n: u64, holders: Vec<OpenFile>, changed: bool, retained: bool },
    RestoreScene { path: PathBuf },
}

/// gitio's `MirrorDrift` as status reports it (D43).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorDriftItem {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_replaced: Option<bool>,
}

impl From<MirrorDrift> for MirrorDriftItem {
    fn from(d: MirrorDrift) -> Self {
        let item =
            |reason: &str| MirrorDriftItem { reason: reason.into(), key: None, hooks_path: None, hook_replaced: None };
        match d {
            MirrorDrift::Exclude => item("info_exclude"),
            MirrorDrift::Attributes => item("info_attributes"),
            MirrorDrift::Config { key } => MirrorDriftItem { key: Some(key), ..item("config") },
            MirrorDrift::PrePushGuardDisabled { hooks_path, hook_replaced } => {
                MirrorDriftItem { hooks_path, hook_replaced: Some(hook_replaced), ..item("pre_push_guard_disabled") }
            }
        }
    }
}

pub(crate) fn no_such_session(id: &str) -> StrataError {
    StrataError::new(Code::NoSuchSession, format!("no session {id}")).with_detail(json!({ "session": id }))
}

impl Sessions<'_> {
    /// §5.5 steps 1, 2 and 3.
    pub fn status(&self, id: &SessionId) -> Result<SessionStatus> {
        let rec = self.live_session(id)?;
        let state = SessionState::from(rec.lifecycle);
        let scene = match (&rec.scene, rec.lifecycle) {
            (Some(v), Lifecycle::Unknown) => Some(scene_of(v, "session")?),
            _ => None,
        };
        let mut workspaces = BTreeMap::new();
        for (repo, row) in &rec.workspaces {
            workspaces.insert(repo.clone(), self.workspace_status(&rec, repo, row)?);
        }
        Ok(SessionStatus {
            manifest: rec.manifest.clone(),
            state,
            created: rec.created,
            phase: rec.phase.clone(),
            scene,
            workspaces,
        })
    }

    /// The session's row unless it is missing or Removed.
    pub(crate) fn live_session(&self, id: &SessionId) -> Result<SessionRecord> {
        match self.store.session(id)? {
            Some(r) if r.lifecycle != Lifecycle::Removed => Ok(r),
            _ => Err(no_such_session(id.as_str())),
        }
    }

    fn workspace_status(&self, rec: &SessionRecord, repo: &RepoName, row: &WorkspaceRecord) -> Result<WorkspaceStatus> {
        let id = &rec.manifest.id;
        let ws = rec.manifest.repos.get(repo).ok_or_else(|| {
            StrataError::new(Code::Corrupt, format!("session {id} has a workspace row for {repo} outside its manifest"))
                .with_detail(json!({ "session": id.as_str(), "repo": repo.as_str() }))
        })?;
        let git = self.git(repo)?;
        let cfg = self.repo_cfg(repo)?;
        let mut warnings = Vec::new();

        // 2.1 health.
        let (health, scene, shadow) = if row.health == Health::Unknown {
            let scene = row.scene.as_ref().map(|v| scene_of(v, "workspace")).transpose()?;
            (WorkspaceHealth::Unknown, scene, None)
        } else {
            let dir_ok = is_dir(&ws.path)?;
            let git_ok =
                dir_ok && lstat(&ws.path.join(".git")).map_err(|e| read_err(&e, &ws.path.join(".git")))?.is_some();
            let shadow_dir = self.shadow_dir(id, repo);
            let shadow = match git.shadow_at(&shadow_dir) {
                Ok(s) => Some(s),
                Err(e) if e.code == Code::NoSuchPath => {
                    warnings.push(StatusWarning::ShadowMissing { path: shadow_dir });
                    None
                }
                // Not a shadow of this repo (e.g. half-built, no marker):
                // the workspace is Missing; status still answers.
                Err(e) if e.code == Code::InvalidInput => {
                    let reason = e.detail.as_ref().and_then(|d| d.get("reason")).and_then(|r| r.as_str()).unwrap_or("");
                    warnings.push(StatusWarning::ShadowInvalid { path: shadow_dir, reason: reason.to_string() });
                    None
                }
                Err(e) => return Err(e),
            };
            let ok = dir_ok && git_ok && shadow.is_some();
            (if ok { WorkspaceHealth::Ok } else { WorkspaceHealth::Missing }, None, shadow)
        };

        // 2.3 checkpoint.
        let checkpoint = if ws.tracked {
            self.store.latest_checkpoint(id, repo)?.map(|c| c.id)
        } else {
            warnings.push(StatusWarning::NotCheckpointed);
            None
        };

        // 2.4 drift: only on a healthy workspace (see the progress notes).
        let drift = if health == WorkspaceHealth::Ok {
            match strata_deps::find_set(self.store, repo, &ws.base.deps)? {
                Some(set) if !set.dirs.is_empty() => strata_deps::drift(cfg, git, &ws.path, &set)?,
                _ => None,
            }
        } else {
            None
        };

        // 2.5 pending.
        let pending = if ws.tracked {
            strata_deps::open_proposals(self.store, Some(repo))?
                .into_iter()
                .filter(|p| p.state == ProposalState::Pending && p.to != ws.base.deps)
                .collect()
        } else {
            Vec::new()
        };

        // 2.7 seeds.
        let seeds: Vec<SeedRecord> = row.seeds.iter().map(SeedRecord::from).collect();
        if health == WorkspaceHealth::Ok {
            for s in &seeds {
                let stale = match lstat(&s.source).map_err(|e| read_err(&e, &s.source))? {
                    None => true,
                    // Against what strata last wrote at the copy: the
                    // refresh mark, else the row (seed refresh).
                    Some(m) => mtime_of(&m) != written_stamp(&ws.path.join(s.path.as_str()), s).0,
                };
                if stale {
                    warnings.push(StatusWarning::SeedStale { path: s.path.clone() });
                }
                if !git.is_ignored(&ws.path, &s.path)? {
                    warnings.push(StatusWarning::SeedNotIgnored { path: s.path.clone() });
                }
            }
        }

        // 2.8 mirrors and the pre-push guard.
        if let Some(shadow) = &shadow {
            match git.shadow_drift(shadow, &ws.path) {
                Ok(items) if items.is_empty() => {}
                Ok(items) => warnings
                    .push(StatusWarning::MirrorDrift { items: items.into_iter().map(MirrorDriftItem::from).collect() }),
                Err(e) if e.code == Code::Unknown => warnings.push(StatusWarning::PrePushGuardUnknown { detail: e }),
                Err(e) => return Err(e),
            }
        }

        // 2.9 move intents. A move running in this process is not a leftover.
        let live = self.is_live_move(id, repo);
        for i in self.store.move_intents(id)?.into_iter().filter(|i| &i.repo == repo) {
            match i.state {
                MoveState::Preparing if !live => {
                    warnings.push(StatusWarning::MoveLeftover { n: i.n, staging: i.staging.clone() })
                }
                MoveState::Swapping if !live => {
                    warnings.push(StatusWarning::MoveAwaitingRecover { n: i.n, staging: i.staging.clone() })
                }
                MoveState::Abandoned if lstat(&i.staging).map_err(|e| read_err(&e, &i.staging))?.is_some() => {
                    warnings.push(StatusWarning::MoveLeftover { n: i.n, staging: i.staging.clone() })
                }
                MoveState::Done => {
                    let path = i.old_tree.clone().unwrap_or_else(|| i.staging.clone());
                    if lstat(&path).map_err(|e| read_err(&e, &path))?.is_none() {
                        continue;
                    }
                    let holders = strata_deps::open_files(&path)?;
                    // A tree that cannot be compared reads as changed: it
                    // is then never collected automatically.
                    let changed = !self.old_tree_unchanged(&i, cfg).unwrap_or(false);
                    warnings.push(StatusWarning::OldTreeKept {
                        path,
                        n: i.n,
                        holders,
                        changed,
                        retained: i.old_tree.is_some(),
                    });
                }
                _ => {}
            }
        }

        // 2.10 an overlay restore scene.
        let restore = rec.manifest.root.join(format!("{}{repo}", strata_overlay::RESTORE_TEMP_PREFIX));
        if non_empty_dir(&restore)? {
            warnings.push(StatusWarning::RestoreScene { path: restore });
        }

        Ok(WorkspaceStatus {
            health,
            generation: row.generation,
            scene,
            checkpoint,
            drift,
            pending,
            conflicts: row.conflicts.clone(),
            seeds,
            warnings,
        })
    }

    pub(crate) fn shadow_dir(&self, id: &SessionId, repo: &RepoName) -> PathBuf {
        self.config.paths.root.join("sessions/.repos").join(id.as_str()).join(format!("{repo}.git"))
    }
}

fn read_err(e: &io::Error, p: &Path) -> StrataError {
    classify_io_error(e, IoSide::Read, p)
}

fn is_dir(p: &Path) -> Result<bool> {
    Ok(lstat(p).map_err(|e| read_err(&e, p))?.is_some_and(|m| m.is_dir()))
}

/// A directory with at least one entry; anything else (absent, not a
/// directory) is not.
pub(crate) fn non_empty_dir(p: &Path) -> Result<bool> {
    match std::fs::read_dir(p) {
        Ok(mut d) => Ok(d.next().is_some()),
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) => Ok(false),
        Err(e) => Err(read_err(&e, p)),
    }
}

impl From<&strata_store::SeedRow> for SeedRecord {
    fn from(r: &strata_store::SeedRow) -> Self {
        SeedRecord {
            path: r.path.clone(),
            source: r.source.clone(),
            link: r.link.clone(),
            size: r.size,
            source_mtime: r.source_mtime,
            copied: r.copied,
        }
    }
}

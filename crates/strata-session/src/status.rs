//! `status(id)` (§5.5): read-only, no write lock.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::{IoSide, classify_io_error};
use strata_deps::{Drift, OpenFile, Proposal, ProposalState};
use strata_gitio::MirrorDrift;
use strata_store::{Health, Lifecycle, MoveState, SessionRecord, WorkspaceRecord};
use strata_types::{CheckpointId, Code, RepoName, RepoPath, SessionId, SessionManifest, StrataError};

use crate::old_tree::OldTreeMode;
use crate::scene::{Scene, scene_of};
use crate::seed::{SeedRecord, mtime_of, written_stamp};
use crate::sys::lstat;
use crate::{Result, Sessions};

/// A session's status (design §6.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    /// `manifest.id`, at the top level (status-perf D3).
    pub id: SessionId,
    /// `manifest.ticket`, at the top level; null when there is none.
    pub ticket: Option<String>,
    /// `manifest.id` and `manifest.ticket` duplicate the two fields above
    /// and are deprecated; they go in the next major version.
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
    /// `changed: null` + `unchecked: true`: not judged yet (status-perf
    /// A1, D10); `checked_at`: when the background judgement was made
    /// (absent when judged in the request).
    OldTreeKept {
        path: PathBuf,
        n: u64,
        holders: Vec<OpenFile>,
        changed: Option<bool>,
        retained: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchecked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_rfc3339")]
        checked_at: Option<SystemTime>,
    },
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
        self.status_many(std::slice::from_ref(id)).pop().unwrap_or_else(|| Err(no_such_session(id.as_str())))
    }

    /// status-perf A4: `status` of each id, one result per id in the order
    /// given. The workspaces of all of them are read on the shared slots,
    /// at most `[status] parallel` at once in this process; the results are
    /// put together in the serial order, never the order they finish in,
    /// and a session's error is the one the serial walk would have hit
    /// first (its own row before its workspaces, workspaces by repo).
    pub fn status_many(&self, ids: &[SessionId]) -> Vec<Result<SessionStatus>> {
        struct Head {
            rec: SessionRecord,
            scene: Option<Scene>,
        }
        // Ordinal (i, 0): the session's own row, read serially.
        let heads: Vec<Result<Head>> = ids
            .iter()
            .map(|id| {
                let rec = self.live_session(id)?;
                let scene = match (&rec.scene, rec.lifecycle) {
                    (Some(v), Lifecycle::Unknown) => Some(scene_of(v, "session")?),
                    _ => None,
                };
                Ok(Head { rec, scene })
            })
            .collect();
        // Ordinals (i, 1 + k): workspace k of session i, by RepoName.
        let tasks: Vec<(usize, &RepoName, &WorkspaceRecord)> = heads
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.as_ref().ok().map(|h| (i, h)))
            .flat_map(|(i, h)| h.rec.workspaces.iter().map(move |(repo, row)| (i, repo, row)))
            .collect();
        let done = on_slots(&self.status_slots, &tasks, |&(i, repo, row)| {
            heads[i].as_ref().ok().map(|h| self.workspace_status(&h.rec, repo, row))
        });
        let n = tasks.len();
        drop(tasks);
        let mut done = done.into_iter().map(Option::flatten);
        let mut out = Vec::with_capacity(ids.len());
        let mut t = 0;
        for h in heads {
            let h = match h {
                Ok(h) => h,
                Err(e) => {
                    out.push(Err(e));
                    continue;
                }
            };
            let mut workspaces = BTreeMap::new();
            let mut first_err = None;
            for repo in h.rec.workspaces.keys() {
                let r = done.next().flatten();
                t += 1;
                match r {
                    Some(Ok(w)) => {
                        workspaces.insert(repo.clone(), w);
                    }
                    Some(Err(e)) => {
                        first_err.get_or_insert(e);
                    }
                    None => {
                        first_err.get_or_insert_with(|| {
                            StrataError::new(Code::Unknown, format!("status of {repo} was not read"))
                        });
                    }
                }
            }
            let rec = h.rec;
            out.push(match first_err {
                Some(e) => Err(e),
                None => Ok(SessionStatus {
                    id: rec.manifest.id.clone(),
                    ticket: rec.manifest.ticket.clone(),
                    manifest: rec.manifest.clone(),
                    state: SessionState::from(rec.lifecycle),
                    created: rec.created,
                    phase: rec.phase.clone(),
                    scene: h.scene,
                    workspaces,
                }),
            });
        }
        debug_assert_eq!(t, n);
        out
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
                    let retained = i.old_tree.is_some();
                    if self.old_tree_mode() == OldTreeMode::Deferred {
                        // status-perf A1: only the verdict table; what it
                        // lacks is queued for the background thread.
                        let hint = crate::old_tree::ino_of(&path).and_then(|ino| self.old_tree_hint(&i, &path, ino));
                        warnings.push(match hint {
                            Some(v) => StatusWarning::OldTreeKept {
                                path,
                                n: i.n,
                                holders: v.holders,
                                changed: Some(v.changed),
                                retained,
                                unchecked: false,
                                checked_at: Some(v.checked_at),
                            },
                            None => StatusWarning::OldTreeKept {
                                path,
                                n: i.n,
                                holders: Vec::new(),
                                changed: None,
                                retained,
                                unchecked: true,
                                checked_at: None,
                            },
                        });
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
                        changed: Some(changed),
                        retained,
                        unchecked: false,
                        checked_at: None,
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

impl Sessions<'_> {
    /// status-perf A4: `f` of each item on the same shared slots as the
    /// workspace reads of [`Sessions::status_many`] (the daemon's chains
    /// go through here), one result per item in the order given.
    pub fn on_status_slots<T: Sync, R: Send>(&self, items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<Option<R>> {
        on_slots(&self.status_slots, items, f)
    }
}

/// `f` of each item, at most `slots.cap()` workers here and at most the
/// slots' free count across every caller at once; each call holds one
/// slot. Results in item order; `None` only for an item never run.
pub(crate) fn on_slots<T: Sync, R: Send>(slots: &Slots, items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<Option<R>> {
    let done: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let run = || {
        loop {
            let t = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(t) else { break };
            let r = {
                let _slot = slots.take();
                f(item)
            };
            *done[t].lock().unwrap_or_else(|p| p.into_inner()) = Some(r);
        }
    };
    let workers = slots.cap().min(items.len());
    if workers <= 1 {
        run();
    } else {
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(run);
            }
        });
    }
    done.into_iter().map(|m| m.into_inner().unwrap_or_else(|p| p.into_inner())).collect()
}

/// status-perf A4: a counting semaphore. `take` blocks until a slot is
/// free; the guard gives it back. Nothing holds a slot while waiting for
/// another, so the slots cannot deadlock.
pub(crate) struct Slots {
    cap: usize,
    free: Mutex<usize>,
    freed: Condvar,
}

pub(crate) struct Slot<'a>(&'a Slots);

impl Slots {
    pub(crate) fn new(cap: usize) -> Slots {
        let cap = cap.max(1);
        Slots { cap, free: Mutex::new(cap), freed: Condvar::new() }
    }

    pub(crate) fn cap(&self) -> usize {
        self.cap
    }

    fn lock(&self) -> MutexGuard<'_, usize> {
        self.free.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn take(&self) -> Slot<'_> {
        let mut free = self.lock();
        while *free == 0 {
            free = self.freed.wait(free).unwrap_or_else(|p| p.into_inner());
        }
        *free -= 1;
        Slot(self)
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.lock() += 1;
        self.0.freed.notify_one();
    }
}

/// `Option<SystemTime>` as an RFC 3339 string (status-perf A1 `checked_at`).
mod opt_rfc3339 {
    use std::time::SystemTime;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => strata_types::serde_rfc3339::serialize(t, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => strata_types::serde_rfc3339::parse(&text).map(Some).map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{Slots, on_slots};

    /// Two callers at once share the one cap: never more than `cap`
    /// items run together, and each caller gets its results in order.
    #[test]
    fn slots_are_shared_across_concurrent_callers() {
        let slots = Slots::new(2);
        let (now, max) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let items: Vec<usize> = (0..8).collect();
        let work = |&i: &usize| {
            let n = now.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            now.fetch_sub(1, Ordering::SeqCst);
            i * 10
        };
        let (a, b) = std::thread::scope(|s| {
            let a = s.spawn(|| on_slots(&slots, &items, work));
            let b = s.spawn(|| on_slots(&slots, &items, work));
            (a.join().unwrap(), b.join().unwrap())
        });
        let want: Vec<Option<usize>> = items.iter().map(|i| Some(i * 10)).collect();
        assert_eq!(a, want);
        assert_eq!(b, want);
        assert!(max.load(Ordering::SeqCst) <= 2, "max {} at once", max.load(Ordering::SeqCst));
        assert!(max.load(Ordering::SeqCst) >= 2, "the two callers did overlap");
    }
}

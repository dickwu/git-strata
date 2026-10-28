//! `remove(id, discard, confirm)` (§5.6): named refusals first, then the
//! internal clean-up and the BeforeRemove checkpoints, then the visible
//! steps (Removing, unpin, retire, Removed); a failure after Removing is an
//! Unknown scene.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_gitio::{Git, TreeItem};
use strata_overlay::{OverlayWarning, WorkspaceRef};
use strata_store::{Health, Lifecycle, MoveIntent, MoveState, PublishPhase, SessionRecord, Store};
use strata_types::{
    Action, CheckpointId, CheckpointReason, Code, CommitOid, ConfirmToken, EntryKind, OverlayEntry, RepoName, RepoPath,
    SessionId, StrataError, TreeOid, WorkspaceState,
};

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
        let landed = if discard { BTreeMap::new() } else { self.dirty_check(&rec, &intents)? };

        // ---- Phase B: internal clean-up and the BeforeRemove checkpoints.
        // 5.1
        self.collect_session_leftovers(&rec, &intents)?;
        // 5.2
        let mut checkpoints = BTreeMap::new();
        let mut changed = Map::new();
        if cur == Lifecycle::Active {
            for (repo, ws) in &rec.manifest.repos {
                if !ws.tracked {
                    continue;
                }
                if let Some(why) = self.unhealthy(&rec, repo, ws)? {
                    // 5.3: step 4 found it healthy (or Dirty), so without
                    // --discard a lost workspace is a change, not a skip.
                    if !discard {
                        let mut v = list_json(&[]);
                        v["workspace"] = json!(why);
                        changed.insert(repo.as_str().into(), v);
                    }
                    continue;
                }
                let cfg = self.repo_cfg(repo)?;
                let git = self.git(repo)?;
                let base = self.base_of(&ws.base)?;
                let wsref = WorkspaceRef { session: id, repo, root: &ws.path, config: cfg, git, store: self.store };
                let cp = self.before.checkpoint(&wsref, &base, CheckpointReason::BeforeRemove)?;
                checkpoints.insert(repo.clone(), cp.checkpoint.id);
                // 5.3 (D57): what is about to be retired is what step 4 let go.
                if !discard {
                    let judged = landed.get(repo).map(Vec::as_slice).unwrap_or(&[]);
                    let left = recheck(self.store, git, id, repo, &cfg.publish.branch, judged, &cp.checkpoint.entries, &cp.warnings);
                    if !left.is_empty() {
                        changed.insert(repo.as_str().into(), list_json(&left));
                    }
                }
            }
        }
        if !changed.is_empty() {
            return Err(StrataError::new(
                Code::Dirty,
                format!("session {id} changed while it was being removed; remove it again"),
            )
            .with_detail(json!({ "session": id.as_str(), "reason": "changed_during_remove", "repos": changed })));
        }

        // ---- Phase C: the visible steps.
        self.teardown(&rec, cur)?;
        Ok(RemoveReport { session: id.clone(), discarded: discard, checkpoints })
    }

    /// §5.6 steps 6–9 from `cur` (Active or Unknown): Removing, unpin,
    /// retire the session root and its shadow repositories, Removed. A
    /// failure after Removing is an Unknown scene. Also `new`'s rollback.
    pub(crate) fn teardown(&self, rec: &SessionRecord, cur: Lifecycle) -> Result<()> {
        let id = &rec.manifest.id;
        let root = rec.manifest.root.clone();
        let repos_dir = self.config.paths.root.join("sessions/.repos").join(id.as_str());
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
            Ok(()) => Ok(()),
            Err(e) if is_stop(&e) => Err(e),
            Err(e) => Err(self.remove_unknown(rec, phase, &e)),
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

    /// Why a workspace is not healthy (healthy: not Unknown in the store,
    /// its directory and `.git` present, its shadow repository there): `Some("unknown")` or `Some("missing")`.
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
            // A half-built shadow (no marker, e.g. a `new` that stopped in
            // `git init`) is as good as none.
            Err(e) if matches!(e.code, Code::NoSuchPath | Code::InvalidInput) => Ok(Some("missing")),
            Err(e) => Err(e),
        }
    }

    /// §5.6 step 4: Dirty with one section per rule, or the entries of each
    /// tracked repo that are not counted because they landed (D57).
    fn dirty_check(&self, rec: &SessionRecord, intents: &[MoveIntent]) -> Result<BTreeMap<RepoName, Vec<OverlayEntry>>> {
        let id = &rec.manifest.id;
        let mut unresolved: Vec<Value> = Vec::new();
        let mut repos = Map::new();
        let mut landed_all: BTreeMap<RepoName, Vec<OverlayEntry>> = BTreeMap::new();
        let mut landed = Map::new();
        let mut landed_errors = Map::new();
        let mut counts = Counts::default();
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
            // reconciled against its base (a cold scan, as `reconcile_full`,
            // keeping the warnings). An untracked one only blocks when that
            // finds something: its edits have no checkpoint, but an untouched
            // sibling loses nothing (D50). A tracked one does not count the
            // entries that landed on origin (D57).
            if bad.is_none() && rec.lifecycle == Lifecycle::Active {
                let cfg = self.repo_cfg(repo)?;
                let git = self.git(repo)?;
                let base = self.base_of(&ws.base)?;
                let wsref = WorkspaceRef { session: id, repo, root: &ws.path, config: cfg, git, store: self.store };
                let mut t = strata_overlay::Tracker::new(&ws.path, strata_overlay::DEFAULT_DIRTY_CAPACITY);
                let scan = strata_overlay::compute(&wsref, &base, &mut t, None)?;
                let entries = scan.overlay.entries;
                let remaining = if !ws.tracked || entries.is_empty() {
                    entries
                } else {
                    match split_landed(self.store, git, id, repo, &cfg.publish.branch, entries.clone(), &unstable(&scan.warnings))
                    {
                        Ok(sp) => {
                            if !sp.landed.is_empty() {
                                counts.add(&sp);
                                landed.insert(repo.as_str().into(), sp.summary());
                                landed_all.insert(repo.clone(), sp.landed);
                            }
                            sp.remaining
                        }
                        Err(e) => {
                            landed_errors.insert(repo.as_str().into(), json!(e.message));
                            entries
                        }
                    }
                };
                if !remaining.is_empty() {
                    if !ws.tracked {
                        untracked.push(json!(repo.as_str()));
                    }
                    counts.remaining += remaining.len();
                    repos.insert(repo.as_str().into(), paths_json(&remaining));
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
            return Ok(landed_all);
        }
        let mut what = Vec::new();
        if !unresolved.is_empty() {
            what.push("unresolved scenes");
        }
        let unpublished = counts.what();
        if !repos.is_empty() {
            what.push(&unpublished);
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
        .with_detail({
            let mut d = json!({
                "session": id.as_str(),
                "unresolved": unresolved,
                "repos": repos,
                "untracked_repos": untracked,
                "old_trees": old_trees,
                "landed": landed,
            });
            if !landed_errors.is_empty() {
                d["landed_errors"] = Value::Object(landed_errors);
            }
            d
        }))
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

/// Most publish rows one repo's landed check reads (D57).
const LANDED_MAX_COMMITS: usize = 32;
/// A blob above this many bytes is never read, so its entry stays Dirty (D57).
const LANDED_MAX_BYTES: u64 = 64 << 20;

/// `{paths, count, truncated}` of one repo's Dirty entries.
fn paths_json(entries: &[OverlayEntry]) -> Value {
    let paths: Vec<RepoPath> = entries.iter().map(|e| e.path.clone()).collect();
    list_json(&paths)
}

/// `{paths, count, truncated}` of these paths.
fn list_json(paths: &[RepoPath]) -> Value {
    let shown: Vec<&str> = paths.iter().take(DIRTY_PATHS_CAP).map(RepoPath::as_str).collect();
    json!({ "paths": shown, "count": paths.len(), "truncated": paths.len() > DIRTY_PATHS_CAP })
}

/// The paths a scan could not read steadily: their digests prove nothing.
fn unstable(warnings: &[OverlayWarning]) -> BTreeSet<RepoPath> {
    warnings
        .iter()
        .filter_map(|w| match w {
            OverlayWarning::Unstable { path } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

fn touches(e: &OverlayEntry, set: &BTreeSet<RepoPath>) -> bool {
    set.contains(&e.path) || matches!(&e.kind, EntryKind::Renamed { from, .. } if set.contains(from))
}

/// The git mode publish writes for an overlay mode (publish `stage.rs` `git_mode`).
fn git_mode(mode: u32) -> u32 {
    if mode & 0o170000 == 0o120000 {
        0o120000
    } else if mode & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    }
}

/// One Done publish of this session whose commit origin has (D57).
struct Candidate {
    publish: u64,
    commit: CommitOid,
    proposals: Vec<u64>,
}

/// One repo's overlay split by the landed rule (D57).
#[derive(Default)]
struct Split {
    remaining: Vec<OverlayEntry>,
    landed: Vec<OverlayEntry>,
    /// The candidates something landed at, newest first.
    used: Vec<Candidate>,
}

impl Split {
    fn summary(&self) -> Value {
        let mut proposals: Vec<u64> = self.used.iter().flat_map(|c| c.proposals.iter().copied()).collect();
        proposals.sort_unstable();
        proposals.dedup();
        // `used` is newest first; the detail lists publishes ascending (§5.6 step 4.2).
        let mut used: Vec<&Candidate> = self.used.iter().collect();
        used.sort_by_key(|c| c.publish);
        json!({
            "count": self.landed.len(),
            "publishes": used.iter().map(|c| c.publish).collect::<Vec<_>>(),
            "commits": used.iter().map(|c| c.commit.as_str()).collect::<Vec<_>>(),
            "proposals": proposals,
        })
    }
}

/// The counts of the Dirty message's "changes that are not published" item.
#[derive(Default)]
struct Counts {
    remaining: usize,
    landed: usize,
    publishes: Vec<u64>,
    proposals: Vec<u64>,
}

impl Counts {
    fn add(&mut self, sp: &Split) {
        self.landed += sp.landed.len();
        for c in &sp.used {
            self.publishes.push(c.publish);
            self.proposals.extend(c.proposals.iter().copied());
        }
    }

    fn what(&self) -> String {
        let base = "changes that are not published".to_string();
        if self.landed == 0 {
            return base;
        }
        let mut publishes = self.publishes.clone();
        publishes.sort_unstable();
        publishes.dedup();
        let mut proposals = self.proposals.clone();
        proposals.sort_unstable();
        proposals.dedup();
        let join = |v: &[u64], sep: &str| v.iter().map(u64::to_string).collect::<Vec<_>>().join(sep);
        let path = |n: usize| if n == 1 { "path" } else { "paths" };
        let mut s = format!(
            "{base} ({} {}; {} more are already published in publish {}",
            self.remaining,
            path(self.remaining),
            self.landed,
            join(&publishes, ", ")
        );
        if !proposals.is_empty() {
            s.push_str(&format!(", which left the session waiting on deps proposal #{}", join(&proposals, ", #")));
        }
        s.push(')');
        s
    }
}

/// §5.6 step 4.2 (D57): Done publishes this session took part in for
/// `repo`, whose commit origin's tracking ref already has; newest first.
fn candidates(store: &Store, git: &Git, session: &SessionId, repo: &RepoName, branch: &str) -> Result<Vec<Candidate>> {
    let Some(tip) = git.read_ref(&format!("refs/strata/remotes/origin/{branch}"))? else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<_> = store
        .publishes()?
        .into_iter()
        .filter(|r| r.phase == PublishPhase::Done)
        .filter(|r| r.group.batches.iter().any(|b| &b.session == session && &b.repo == repo))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.group.id));
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for r in rows {
        if seen.len() >= LANDED_MAX_COMMITS {
            break;
        }
        let Some(o) = r.outcome.as_ref() else { continue };
        if o.get("repo").and_then(Value::as_str) != Some(repo.as_str()) {
            continue;
        }
        let Some(commit) = o.get("commit").and_then(Value::as_str).and_then(|c| CommitOid::try_from(c.to_string()).ok())
        else {
            continue;
        };
        if !seen.insert(commit.clone()) {
            continue;
        }
        if !git.is_ancestor(&commit, &tip)? {
            continue;
        }
        let proposals = o
            .get("rebase")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|x| x.get("kind").and_then(Value::as_str) == Some("Waiting"))
            .filter(|x| x.get("session").and_then(Value::as_str) == Some(session.as_str()))
            .filter_map(|x| x.get("proposal").and_then(Value::as_u64))
            .collect();
        out.push(Candidate { publish: r.group.id, commit, proposals });
    }
    Ok(out)
}

/// The path holds nothing a checkout would write as a file there. Judged
/// on the validated tree table, not on `ls-tree`'s literal spelling: a
/// decomposed name, or another ASCII case where the repository ignores
/// case, is the same path on checkout.
fn absent(git: &Git, tree: &TreeOid, p: &RepoPath) -> Result<bool> {
    Ok(!git.tree_has_file(tree, p)?)
}

/// Whether the blob at `p` has these exact bytes and this mode.
fn same_blob(git: &Git, items: &BTreeMap<String, TreeItem>, p: &RepoPath, blob: &strata_types::BlobId, mode: u32) -> Result<bool> {
    let Some(i) = items.get(p.as_str()) else { return Ok(false) };
    if !blob_fits(i, git_mode(mode)) {
        return Ok(false);
    }
    let bytes = git.object_bytes(&i.oid)?;
    Ok(blake3::hash(&bytes).to_hex().as_str() == blob.as_str())
}

/// A blob item of this mode and of a size the check will read.
fn blob_fits(i: &TreeItem, mode: u32) -> bool {
    i.kind == "blob" && i.mode == mode && i.size <= LANDED_MAX_BYTES
}

/// The per-kind content rule of D57.
fn landed_at(git: &Git, tree: &TreeOid, items: &BTreeMap<String, TreeItem>, e: &OverlayEntry) -> Result<bool> {
    match &e.kind {
        EntryKind::Deleted => absent(git, tree, &e.path),
        EntryKind::Added { blob } | EntryKind::Modified { blob } | EntryKind::Untracked { blob } => {
            same_blob(git, items, &e.path, blob, e.mode)
        }
        EntryKind::Renamed { from, blob } => Ok(absent(git, tree, from)? && same_blob(git, items, &e.path, blob, e.mode)?),
    }
}

/// §5.6 step 4.2 (D57): which of `entries` landed at a candidate. Any
/// error is the caller's to turn conservative.
fn split_landed(
    store: &Store,
    git: &Git,
    session: &SessionId,
    repo: &RepoName,
    branch: &str,
    entries: Vec<OverlayEntry>,
    unstable: &BTreeSet<RepoPath>,
) -> Result<Split> {
    let (mut pending, never): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| !touches(e, unstable));
    let mut sp = Split::default();
    for c in candidates(store, git, session, repo, branch)? {
        if pending.is_empty() {
            break;
        }
        let mut paths: Vec<RepoPath> = Vec::new();
        for e in &pending {
            paths.push(e.path.clone());
            if let EntryKind::Renamed { from, .. } = &e.kind {
                paths.push(from.clone());
            }
        }
        let tree = git.tree_of(&c.commit)?;
        let items = git.tree_items(&tree, &paths)?;
        let mut rest = Vec::new();
        let mut hit = false;
        for e in pending {
            if landed_at(git, &tree, &items, &e)? {
                sp.landed.push(e);
                hit = true;
            } else {
                rest.push(e);
            }
        }
        pending = rest;
        if hit {
            sp.used.push(c);
        }
    }
    sp.remaining = pending;
    sp.remaining.extend(never);
    sp.remaining.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(sp)
}

/// §5.6 step 5.3 (D57): the paths of the BeforeRemove checkpoint that
/// step 4 did not let go and that are not landed now either. Every
/// Unstable warning counts, with or without an entry: a path that changed
/// while it was read can sample as the base and leave no entry at all.
#[allow(clippy::too_many_arguments)]
fn recheck(
    store: &Store,
    git: &Git,
    session: &SessionId,
    repo: &RepoName,
    branch: &str,
    judged: &[OverlayEntry],
    entries: &[OverlayEntry],
    warnings: &[OverlayWarning],
) -> Vec<RepoPath> {
    let unstable = unstable(warnings);
    let mut left = unstable.clone();
    let mut new = Vec::new();
    for e in entries {
        if touches(e, &unstable) {
            left.insert(e.path.clone());
        } else if !judged.contains(e) {
            new.push(e.clone());
        }
    }
    if !new.is_empty() {
        let rest = match split_landed(store, git, session, repo, branch, new.clone(), &BTreeSet::new()) {
            Ok(sp) => sp.remaining,
            Err(_) => new,
        };
        left.extend(rest.into_iter().map(|e| e.path));
    }
    left.into_iter().collect()
}

#[cfg(test)]
mod landed_tests {
    use super::*;

    fn item(kind: &str, mode: u32, size: u64) -> TreeItem {
        TreeItem { mode, kind: kind.into(), oid: "0".repeat(40), size }
    }

    #[test]
    fn git_mode_matches_publish() {
        assert_eq!(git_mode(0o100644), 0o100644);
        assert_eq!(git_mode(0o100600), 0o100644);
        assert_eq!(git_mode(0o100755), 0o100755);
        assert_eq!(git_mode(0o100744), 0o100755);
        assert_eq!(git_mode(0o120777), 0o120000);
    }

    #[test]
    fn blob_fits_checks_kind_mode_and_size() {
        assert!(blob_fits(&item("blob", 0o100644, LANDED_MAX_BYTES), 0o100644));
        assert!(!blob_fits(&item("blob", 0o100644, LANDED_MAX_BYTES + 1), 0o100644));
        assert!(!blob_fits(&item("blob", 0o100755, 1), 0o100644));
        assert!(!blob_fits(&item("commit", 0o160000, 0), 0o160000));
        assert!(!blob_fits(&item("tree", 0o040000, 0), 0o040000));
    }

    #[test]
    fn message_names_publishes_and_proposals() {
        let mut c = Counts { remaining: 2, ..Counts::default() };
        assert_eq!(c.what(), "changes that are not published");
        c.landed = 1;
        c.publishes = vec![16];
        assert_eq!(c.what(), "changes that are not published (2 paths; 1 more are already published in publish 16)");
        c.remaining = 1;
        c.landed = 18;
        c.publishes = vec![17, 16, 17];
        c.proposals = vec![22, 21, 22];
        assert_eq!(
            c.what(),
            "changes that are not published (1 path; 18 more are already published in publish 16, 17, \
             which left the session waiting on deps proposal #21, #22)"
        );
    }
}

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
use crate::rm_note::{interrupted, interrupted_line};
use crate::scene::{Scene, SceneOp, cause_of, kept_dirs, unknown_error};
use crate::seams::is_stop;
use crate::status::{no_such_session, non_empty_dir};
use crate::sys::lstat;
use crate::{Result, Sessions};

/// What `remove` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveReport {
    pub session: SessionId,
    pub discarded: bool,
    /// The BeforeRemove checkpoint of each tracked repo, recorded now.
    pub checkpoints: BTreeMap<RepoName, CheckpointId>,
    /// rm-followups §1.8: the unpublished `rm.regenerable` paths of each
    /// tracked repo that did not block the remove (they are in its
    /// checkpoint).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub regenerable: BTreeMap<RepoName, Vec<RepoPath>>,
}

/// What step 4 let go without blocking: the entries that landed (D57) and
/// the `rm.regenerable` ones (§1.8), per tracked repo.
#[derive(Default)]
struct Judged {
    landed: BTreeMap<RepoName, Vec<OverlayEntry>>,
    regenerable: BTreeMap<RepoName, Vec<OverlayEntry>>,
}

/// rm-followups §1.8: the effective `rm.regenerable` rules of one repo.
pub(crate) struct Regenerable<'c> {
    rules: strata_overlay::LwwRules,
    cfg: &'c strata_types::RepoConfig,
}

impl<'c> Regenerable<'c> {
    pub(crate) fn new(patterns: &[String], cfg: &'c strata_types::RepoConfig) -> Self {
        Regenerable { rules: strata_overlay::LwwRules::parse(&patterns.join("\n")), cfg }
    }

    /// Modified/Added/Untracked only (a deletion or a rename is never
    /// regenerable) and never an install input, whatever its case or
    /// normalization.
    pub(crate) fn matches(&self, e: &OverlayEntry) -> bool {
        matches!(e.kind, EntryKind::Added { .. } | EntryKind::Modified { .. } | EntryKind::Untracked { .. })
            && !self.rules.is_empty()
            && strata_overlay::lww_matches(&self.rules, &e.path)
            && !strata_deps::is_input_folded(self.cfg, &e.path)
    }
}

fn exists(p: &Path) -> bool {
    matches!(lstat(p), Ok(Some(_)))
}

fn lc_name(l: Lifecycle) -> String {
    format!("{l:?}").to_lowercase()
}

/// `Active`, `Unknown`, … as `detail.lifecycle` of a refusal (R2).
fn lc_title(l: Lifecycle) -> String {
    format!("{l:?}")
}

/// Paths shown inline per repo in a Dirty refusal; `detail` has them all.
const INLINE_PATHS: usize = 5;

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
                    "{} Processes {} hold files in it; close them and run the same command again.",
                    self.refused_prefix(&rec),
                    pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
                ),
            )
            .with_detail(json!({
                "session": id.as_str(),
                "paths": [root, repos_dir],
                "open": open,
                "removed": false,
                "lifecycle": lc_title(cur),
            })));
        }
        let intents = self.store.move_intents(id)?;
        // 4.
        let judged = if discard { Judged::default() } else { self.dirty_check(&rec, &intents)? };

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
                    let mut let_go: Vec<OverlayEntry> = judged.landed.get(repo).cloned().unwrap_or_default();
                    let_go.extend(judged.regenerable.get(repo).cloned().unwrap_or_default());
                    let regen = Regenerable::new(&self.regenerable(repo), cfg);
                    let left = recheck(
                        self.store,
                        git,
                        id,
                        repo,
                        &cfg.publish.branch,
                        &let_go,
                        &cp.checkpoint.entries,
                        &cp.warnings,
                        &|e| regen.matches(e),
                    );
                    if !left.is_empty() {
                        changed.insert(repo.as_str().into(), list_json(&left));
                    }
                }
            }
        }
        if !changed.is_empty() {
            return Err(StrataError::new(
                Code::Dirty,
                format!(
                    "{} It changed while it was being removed ({}); run the same command again.",
                    self.refused_prefix(&rec),
                    changed.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
                ),
            )
            .with_detail(json!({
                "session": id.as_str(),
                "reason": "changed_during_remove",
                "repos": changed,
                "removed": false,
                "lifecycle": lc_title(cur),
            })));
        }

        // ---- Phase C: the visible steps.
        self.teardown(&rec, cur)?;
        let regenerable = judged
            .regenerable
            .into_iter()
            .map(|(r, v)| (r, v.into_iter().map(|e| e.path).collect()))
            .collect();
        Ok(RemoveReport { session: id.clone(), discarded: discard, checkpoints, regenerable })
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

    /// R2: the start of every refusal before `Removing`, naming the
    /// session's actual lifecycle at the start of this rm.
    fn refused_prefix(&self, rec: &SessionRecord) -> String {
        let id = &rec.manifest.id;
        let head = "rm refused: nothing was removed;";
        match rec.lifecycle {
            Lifecycle::Unknown => match interrupted(&self.config.paths.root, rec) {
                Some(i) => format!("{head} session {id} is still Unknown ({}).", interrupted_line(id, &i)),
                None => format!("{head} session {id} is still Unknown (an earlier operation was interrupted)."),
            },
            l => format!("{head} session {id} is still {} and listed (`git-strata ls`).", lc_title(l)),
        }
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
    /// A healthy tracked repo's `rm.regenerable` entries do not count
    /// either (rm-followups §1.8); they are listed as `regenerable`.
    fn dirty_check(&self, rec: &SessionRecord, intents: &[MoveIntent]) -> Result<Judged> {
        let id = &rec.manifest.id;
        let mut unresolved: Vec<Value> = Vec::new();
        let mut repos = Map::new();
        let mut landed_all: BTreeMap<RepoName, Vec<OverlayEntry>> = BTreeMap::new();
        let mut landed = Map::new();
        let mut landed_errors = Map::new();
        let mut untracked: Vec<Value> = Vec::new();
        let mut regen_all: BTreeMap<RepoName, Vec<OverlayEntry>> = BTreeMap::new();
        let mut regen_json = Map::new();
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
                let mut remaining = if !ws.tracked || entries.is_empty() {
                    entries
                } else {
                    match split_landed(self.store, git, id, repo, &cfg.publish.branch, entries.clone(), &unstable(&scan.warnings))
                    {
                        Ok(sp) => {
                            if !sp.landed.is_empty() {
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
                // §1.8: tracked and healthy only (an untracked repo has no
                // checkpoint to keep them).
                if ws.tracked {
                    let regen = Regenerable::new(&self.regenerable(repo), cfg);
                    let (r, keep): (Vec<OverlayEntry>, Vec<OverlayEntry>) =
                        std::mem::take(&mut remaining).into_iter().partition(|e| regen.matches(e));
                    remaining = keep;
                    if !r.is_empty() {
                        regen_json.insert(repo.as_str().into(), paths_json(&r));
                        regen_all.insert(repo.clone(), r);
                    }
                }
                if !remaining.is_empty() {
                    if !ws.tracked {
                        untracked.push(json!(repo.as_str()));
                    }
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
            return Ok(Judged { landed: landed_all, regenerable: regen_all });
        }
        let msg = dirty_message(&self.refused_prefix(rec), id, rec.lifecycle, &repos, &landed, &untracked, &unresolved, &old_trees);
        Err(StrataError::new(Code::Dirty, msg).with_detail({
            let mut d = json!({
                "session": id.as_str(),
                "unresolved": unresolved,
                "repos": repos,
                "untracked_repos": untracked,
                "old_trees": old_trees,
                "landed": landed,
                "removed": false,
                "lifecycle": lc_title(rec.lifecycle),
            });
            if !landed_errors.is_empty() {
                d["landed_errors"] = Value::Object(landed_errors);
            }
            if !regen_json.is_empty() {
                d["regenerable"] = Value::Object(regen_json);
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

/// `{paths, count, truncated}` of these paths. rm-followups R2: `detail`
/// keeps the full list (only the message is capped, at 5 per repo);
/// `truncated` stays for compatibility and is always false.
fn list_json(paths: &[RepoPath]) -> Value {
    let shown: Vec<&str> = paths.iter().map(RepoPath::as_str).collect();
    json!({ "paths": shown, "count": paths.len(), "truncated": false })
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

fn join_u64(v: &[u64], sep: &str) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(sep)
}

fn u64s(v: Option<&Value>) -> Vec<u64> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default()
}

/// "REPO: N paths not published: p1, … (+k more); M already published
/// (publish 22, waiting on deps proposal #21)" from the Dirty detail.
fn repo_line(repo: &str, entry: &Value, landed: Option<&Value>, untracked: bool) -> String {
    let n = entry.get("count").and_then(Value::as_u64).unwrap_or(0);
    let shown: Vec<&str> =
        entry.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let inline: Vec<&str> = shown.iter().take(INLINE_PATHS).copied().collect();
    let more = n.saturating_sub(inline.len() as u64);
    let mut s = format!("{repo}: {n} {} not published", if n == 1 { "path" } else { "paths" });
    if !inline.is_empty() {
        s.push_str(&format!(": {}", inline.join(", ")));
        if more > 0 {
            s.push_str(&format!(" (+{more} more)"));
        }
    }
    if untracked {
        s.push_str(" (untracked repo: no checkpoint keeps them)");
    }
    if let Some(l) = landed {
        let m = l.get("count").and_then(Value::as_u64).unwrap_or(0);
        if m > 0 {
            s.push_str(&format!("; {m} already published (publish {}", join_u64(&u64s(l.get("publishes")), ", ")));
            let props = u64s(l.get("proposals"));
            if !props.is_empty() {
                s.push_str(&format!(", which left the session waiting on deps proposal #{}", join_u64(&props, ", #")));
            }
            s.push(')');
        }
    }
    s
}

/// The R2 Dirty refusal (design §2.2).
#[allow(clippy::too_many_arguments)]
fn dirty_message(
    prefix: &str,
    id: &SessionId,
    lifecycle: Lifecycle,
    repos: &Map<String, Value>,
    landed: &Map<String, Value>,
    untracked: &[Value],
    unresolved: &[Value],
    old_trees: &[Value],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (repo, entry) in repos {
        let u = untracked.iter().any(|v| v.as_str() == Some(repo.as_str()));
        parts.push(repo_line(repo, entry, landed.get(repo), u));
    }
    let scenes: Vec<String> = unresolved
        .iter()
        .filter(|v| v.get("kind").and_then(Value::as_str) != Some("session_unknown"))
        .map(|v| {
            let kind = v.get("kind").and_then(Value::as_str).unwrap_or("scene").replace('_', " ");
            match v.get("repo").and_then(Value::as_str) {
                Some(r) => format!("{kind} in {r}"),
                None => kind,
            }
        })
        .collect();
    if !scenes.is_empty() {
        parts.push(format!("unresolved: {}", scenes.join(", ")));
    }
    if !old_trees.is_empty() {
        let t: Vec<String> = old_trees
            .iter()
            .map(|v| {
                format!(
                    "{} #{}",
                    v.get("repo").and_then(Value::as_str).unwrap_or("?"),
                    v.get("n").and_then(Value::as_u64).unwrap_or(0)
                )
            })
            .collect();
        parts.push(format!("changed old trees: {}", t.join(", ")));
    }
    let mut s = prefix.to_string();
    if !parts.is_empty() {
        s.push(' ');
        s.push_str(&parts.join("; "));
        s.push('.');
    }
    if lifecycle == Lifecycle::Unknown {
        s.push_str(&format!(" Run `git-strata rm --discard {id}` (Touch ID) to finish removing it."));
    } else if !repos.is_empty() {
        let is_untracked = |r: &String| untracked.iter().any(|v| v.as_str() == Some(r.as_str()));
        let lost: Vec<&str> = repos.keys().filter(|r| is_untracked(r)).map(String::as_str).collect();
        let kept = repos.keys().any(|r| !is_untracked(r));
        s.push_str(&format!(" Publish them, or run `git-strata rm --discard {id}` (Touch ID) to throw them away"));
        match (kept, lost.is_empty()) {
            (true, true) => s.push_str("; the BeforeRemove checkpoint then keeps them until gc."),
            (true, false) => s.push_str(&format!(
                "; the BeforeRemove checkpoint then keeps the tracked repos' edits until gc, \
                 but the edits in untracked {} are lost (no checkpoint).",
                lost.join(", ")
            )),
            (false, _) => s.push_str(&format!("; the edits in untracked {} are then lost (no checkpoint).", lost.join(", "))),
        }
    } else {
        s.push_str(&format!(" Resolve them, or run `git-strata rm --discard {id}` (Touch ID) to remove it anyway."));
    }
    s
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
    regenerable: &dyn Fn(&OverlayEntry) -> bool,
) -> Vec<RepoPath> {
    let unstable = unstable(warnings);
    let mut left = unstable.clone();
    let mut new = Vec::new();
    for e in entries {
        if touches(e, &unstable) {
            left.insert(e.path.clone());
        } else if !judged.contains(e) && !regenerable(e) {
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
    fn dirty_message_says_nothing_removed_and_caps_paths() {
        let id = SessionId::try_from("s-abcdefgh".to_string()).unwrap();
        let paths: Vec<String> = (0..7).map(|i| format!("p{i}")).collect();
        let mut repos = Map::new();
        repos.insert("admin".into(), json!({ "paths": paths, "count": 36, "truncated": false }));
        let mut landed = Map::new();
        landed.insert("admin".into(), json!({ "count": 2, "publishes": [16, 17], "proposals": [21] }));
        let m = dirty_message("PFX", &id, Lifecycle::Active, &repos, &landed, &[], &[], &[]);
        assert_eq!(
            m,
            "PFX admin: 36 paths not published: p0, p1, p2, p3, p4 (+31 more); 2 already published (publish 16, 17, \
             which left the session waiting on deps proposal #21). Publish them, or run \
             `git-strata rm --discard s-abcdefgh` (Touch ID) to throw them away; the BeforeRemove checkpoint then \
             keeps them until gc."
        );
        let one = json!({ "paths": ["a"], "count": 1, "truncated": false });
        assert_eq!(repo_line("app", &one, None, true), "app: 1 path not published: a (untracked repo: no checkpoint keeps them)");
        let m = dirty_message("PFX", &id, Lifecycle::Unknown, &Map::new(), &Map::new(), &[], &[json!({"kind": "session_unknown"})], &[]);
        assert_eq!(m, "PFX Run `git-strata rm --discard s-abcdefgh` (Touch ID) to finish removing it.");
        // Untracked edits get no checkpoint: never promised one.
        let mut only = Map::new();
        only.insert("web".into(), one.clone());
        let m = dirty_message("PFX", &id, Lifecycle::Active, &only, &Map::new(), &[json!("web")], &[], &[]);
        assert!(!m.contains("checkpoint then keeps"), "{m}");
        assert!(m.ends_with("throw them away; the edits in untracked web are then lost (no checkpoint)."), "{m}");
        let mut mixed = only.clone();
        mixed.insert("app".into(), one);
        let m = dirty_message("PFX", &id, Lifecycle::Active, &mixed, &Map::new(), &[json!("web")], &[], &[]);
        assert!(
            m.ends_with("keeps the tracked repos' edits until gc, but the edits in untracked web are lost (no checkpoint)."),
            "{m}"
        );
    }

    #[test]
    fn dirty_detail_keeps_every_path() {
        let paths: Vec<RepoPath> = (0..250).map(|i| RepoPath::try_from(format!("d/p{i:03}")).unwrap()).collect();
        let v = list_json(&paths);
        assert_eq!(v["count"], 250);
        assert_eq!(v["paths"].as_array().unwrap().len(), 250);
        assert_eq!(v["truncated"], false);
    }
}

//! `move_workspace` (§5.7): move a workspace to a new base generation and
//! head, source and dependencies together, with one atomic directory swap.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_apfs::RenameMode;
use strata_overlay::{Base, Overlay, RebaseOptions, Rebased, WorkspaceRef};
use strata_store::{Health, Lifecycle, MoveIntent, MoveState};
use strata_types::{
    BaseGen, CheckpointId, CheckpointReason, Code, CommitOid, OverlayEntry, RepoConfig, RepoId, RepoName, RepoPath,
    SessionId, StrataError,
};

use crate::carry::{Carried, Carry};
use crate::new::pin_ref;
use crate::scene::{Scene, SceneOp, cause_of, unknown_error};
use crate::seams::is_stop;
use crate::status::{no_such_session, non_empty_dir};
use crate::sys::{lstat, random_hex, rp};
use crate::{MOVE_STAGING_PREFIX, Result, Sessions};

/// What a move did (§5.7 step 21).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveReport {
    pub session: SessionId,
    pub repo: RepoName,
    pub from: BaseGen,
    pub from_head: CommitOid,
    pub to: BaseGen,
    pub head: CommitOid,
    /// The `BeforeRollout` checkpoint; None when nothing moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    pub dropped: Vec<RepoPath>,
    pub conflicts: Vec<RepoPath>,
    pub lww: Vec<RepoPath>,
    /// Ignored entries left in the old tree (§5.7.1).
    pub uncarried: Vec<RepoPath>,
    /// The old tree, kept in place (in use, changed, or holding uncarried
    /// content).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_tree: Option<PathBuf>,
}

fn invalid(reason: &str, message: String, mut detail: Map<String, Value>) -> StrataError {
    detail.insert("reason".into(), json!(reason));
    StrataError::new(Code::InvalidInput, message).with_detail(Value::Object(detail))
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn set_detail(e: &mut StrataError, key: &str, v: Value) {
    match &mut e.detail {
        Some(Value::Object(m)) => {
            m.insert(key.into(), v);
        }
        _ => e.detail = Some(json!({ key: v })),
    }
}

fn exists(p: &Path) -> bool {
    matches!(lstat(p), Ok(Some(_)))
}

/// Removes the in-progress mark on return, including on a panic.
struct LiveMove<'s> {
    set: &'s std::sync::Mutex<BTreeSet<(SessionId, RepoName)>>,
    key: (SessionId, RepoName),
}

impl Drop for LiveMove<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|x| x.into_inner()).remove(&self.key);
    }
}

/// One move in progress: what the failure paths need.
struct Mv<'m> {
    session: &'m SessionId,
    name: &'m RepoName,
    n: u64,
    staging: PathBuf,
    shadow_dir: PathBuf,
}

impl Sessions<'_> {
    /// The configured repo whose main checkout has this RepoId.
    pub fn repo_name(&self, repo: &RepoId) -> Result<RepoName> {
        for (name, git) in self.gits {
            if git.repo_id() == *repo {
                return Ok(name.clone());
            }
        }
        Err(StrataError::new(Code::NoSuchRepo, format!("no configured repo has id {repo}"))
            .with_detail(json!({ "repo_id": repo })))
    }

    /// The set of in-process moves, for status (§5.5 step 2.9).
    pub(crate) fn is_live_move(&self, session: &SessionId, repo: &RepoName) -> bool {
        self.live_moves.lock().unwrap_or_else(|x| x.into_inner()).contains(&(session.clone(), repo.clone()))
    }

    /// §5.7. The caller holds `repo`'s write lock and built `new_base`.
    pub fn move_workspace(
        &self,
        session: &SessionId,
        repo: &RepoId,
        new_base: BaseGen,
        head: CommitOid,
    ) -> Result<MoveReport> {
        // ---- Phase A: checks; nothing is written.
        // 1.
        let name = self.repo_name(repo)?;
        if new_base.repo != name {
            return Err(invalid(
                "repo_mismatch",
                format!("the new base is of repo {}, not {name}", new_base.repo),
                obj(json!({ "repo_id": repo, "name": name.as_str(), "base_repo": new_base.repo.as_str() })),
            ));
        }
        let git = self.git(&name)?;
        let cfg = self.repo_cfg(&name)?;
        // 2.
        let rec = match self.store.session(session)? {
            Some(r) if r.lifecycle != Lifecycle::Removed => r,
            _ => return Err(no_such_session(session.as_str())),
        };
        match rec.lifecycle {
            Lifecycle::Active => {}
            Lifecycle::Unknown => {
                return Err(StrataError::new(
                    Code::Unknown,
                    format!("session {session} is Unknown; see git-strata status {session}"),
                )
                .with_detail(json!({
                    "reason": "session_unknown",
                    "session": session.as_str(),
                    "scene": rec.scene.clone().unwrap_or(Value::Null),
                })));
            }
            other => {
                let lc = format!("{other:?}").to_lowercase();
                return Err(invalid(
                    "lifecycle",
                    format!("session {session} is {lc}; a move needs an active session"),
                    obj(json!({ "session": session.as_str(), "lifecycle": lc })),
                ));
            }
        }
        // 3.
        let Some(ws) = rec.manifest.repos.get(&name) else {
            return Err(StrataError::not_in_session(session.as_str(), name.as_str()));
        };
        if !ws.tracked {
            return Err(invalid(
                "untracked_repo",
                format!("{name} is not tracked in session {session}; it never moves"),
                obj(json!({ "session": session.as_str(), "repo": name.as_str() })),
            ));
        }
        let row = rec.workspaces.get(&name).ok_or_else(|| {
            StrataError::new(Code::Corrupt, format!("session {session} has no workspace row for {name}"))
                .with_detail(json!({ "session": session.as_str(), "repo": name.as_str() }))
        })?;
        if row.health == Health::Unknown {
            return Err(StrataError::new(
                Code::Unknown,
                format!("workspace {session}/{name} is Unknown; see git-strata status {session}"),
            )
            .with_detail(json!({
                "reason": "workspace_unknown",
                "session": session.as_str(),
                "repo": name.as_str(),
                "scene": row.scene.clone().unwrap_or(Value::Null),
            })));
        }
        for p in [ws.path.clone(), ws.path.join(".git")] {
            if !exists(&p) {
                return Err(StrataError::new(Code::NoSuchPath, format!("{} is missing", p.display()))
                    .with_detail(json!({ "path": p, "session": session.as_str(), "repo": name.as_str() })));
            }
        }
        let shadow_dir = self.shadow_dir(session, &name);
        let shadow = match git.shadow_at(&shadow_dir) {
            Ok(s) => s,
            Err(e) if e.code == Code::NoSuchPath => {
                return Err(StrataError::new(
                    Code::NoSuchPath,
                    format!("the shadow repository {} is missing", shadow_dir.display()),
                )
                .with_detail(json!({ "reason": "shadow_missing", "path": shadow_dir })));
            }
            Err(e) => return Err(e),
        };
        let root = rec.manifest.root.clone();
        let restore = root.join(format!("{}{name}", strata_overlay::RESTORE_TEMP_PREFIX));
        if non_empty_dir(&restore)? {
            return Err(StrataError::new(
                Code::Unknown,
                format!("an interrupted restore left {}; see git-strata status {session}", restore.display()),
            )
            .with_detail(json!({ "reason": "restore_scene", "path": restore })));
        }
        // 4.
        let old_gen = ws.base.clone();
        let from_head = ws.head.clone();
        if old_gen == new_base && from_head == head {
            return Ok(MoveReport {
                session: session.clone(),
                repo: name,
                from: old_gen,
                from_head,
                to: new_base,
                head,
                checkpoint: None,
                dropped: vec![],
                conflicts: vec![],
                lww: vec![],
                uncarried: vec![],
                old_tree: None,
            });
        }
        // 5.
        let tree = git.tree_of(&head)?;
        if tree != new_base.tree {
            return Err(invalid(
                "head_tree",
                format!("{head} has tree {tree}, not the new base's {}", new_base.tree),
                obj(json!({ "head": head.as_str(), "tree": tree.as_str(), "base_tree": new_base.tree.as_str() })),
            ));
        }
        // 6.
        if git.merge_base(&from_head, &head)?.is_none() {
            return Err(StrataError::new(Code::NoMergeBase, format!("{from_head} and {head} have no common ancestor"))
                .with_detail(json!({ "a": from_head.as_str(), "b": head.as_str() })));
        }
        // 7.
        let Some(new_set) = strata_deps::find_set(self.store, &name, &new_base.deps)? else {
            return Err(invalid(
                "deps_unsealed",
                format!("dependency set {} of {name} is not sealed", new_base.deps),
                obj(json!({ "deps": new_base.deps.as_str() })),
            ));
        };
        let Some(old_set) = strata_deps::find_set(self.store, &name, &old_gen.deps)? else {
            return Err(StrataError::new(
                Code::Corrupt,
                format!("the workspace's dependency set {} of {name} is missing", old_gen.deps),
            )
            .with_detail(json!({ "reason": "deps_set_missing", "deps": old_gen.deps.as_str() })));
        };
        if !self.store.has_base_gen(&new_base)? {
            return Err(invalid(
                "base_unbuilt",
                format!("base generation {}/{} of {name} is not built", new_base.tree, new_base.deps),
                obj(json!({ "base": new_base })),
            ));
        }
        // 8.
        let mut dset: BTreeSet<RepoPath> = BTreeSet::new();
        dset.extend(cfg.deps_dirs.iter().cloned());
        dset.extend(old_set.dirs.iter().cloned());
        dset.extend(new_set.dirs.iter().cloned());
        let d: Vec<RepoPath> = dset.into_iter().collect();
        let holders = held_dirs(&ws.path, &d)?;
        if !holders.is_empty() {
            return Err(open_files_error(&ws.path, holders));
        }
        // 9.
        let intents: Vec<MoveIntent> =
            self.store.move_intents(session)?.into_iter().filter(|i| i.repo == name).collect();
        for i in &intents {
            if i.state == MoveState::Swapping {
                return Err(invalid(
                    "move_awaiting_recover",
                    format!("move {} of {session}/{name} stopped in its swap; it waits for recover", i.n),
                    obj(json!({ "n": i.n, "staging": i.staging })),
                ));
            }
        }
        for i in &intents {
            match i.state {
                MoveState::Preparing => {
                    self.collect_leftover(&i.staging, &shadow_dir, i.n)?;
                    self.store.set_move_state(
                        session,
                        &name,
                        i.n,
                        MoveState::Preparing,
                        MoveState::Abandoned,
                        None,
                        SystemTime::now(),
                    )?;
                }
                MoveState::Abandoned if exists(&i.staging) => {
                    self.collect_leftover(&i.staging, &shadow_dir, i.n)?;
                }
                _ => {}
            }
        }

        // ---- Phase B: the bases.
        let old = self.base_of(&old_gen)?;
        let new = self.base_of(&new_base)?;

        // ---- Phase C: the BeforeRollout checkpoint.
        let wsref = WorkspaceRef { session, repo: &name, root: &ws.path, config: cfg, git, store: self.store };
        let cp = self.before.checkpoint(&wsref, &old, CheckpointReason::BeforeRollout)?.checkpoint;

        // ---- Phase D: the intent and the staging.
        // 12.
        self.live_moves.lock().unwrap_or_else(|x| x.into_inner()).insert((session.clone(), name.clone()));
        let _live = LiveMove { set: &self.live_moves, key: (session.clone(), name.clone()) };
        let sname = format!("{MOVE_STAGING_PREFIX}{name}-{}", random_hex(8));
        let staging = root.join(&sname);
        let started = SystemTime::now();
        let n = self.store.begin_move(session, &name, &new_base, &head, &staging, started)?;
        let mv = Mv { session, name: &name, n, staging: staging.clone(), shadow_dir: shadow_dir.clone() };

        // 13. prepare.
        let old_admin = shadow_dir.join("worktrees").join(row.generation.to_string());
        let prepared = (|| -> Result<(Rebased, Vec<RepoPath>, Carried, bool)> {
            self.store.set_move_phase(session, &name, n, "prepare")?;
            self.seams.hit("move.prepare", Some(&name))?;
            strata_apfs::clone_tree_rooted(&self.store.base_dir(&new_base), &root, &rp(sname.clone()))?;
            let ignorecase = git.read_index(&ws.path)?.ignorecase;
            let opts = RebaseOptions::for_move(&new, ignorecase)?;
            let rebased = strata_overlay::rebase(&Overlay::from_checkpoint(&cp), &old, &new, self.store, &opts)?;
            strata_overlay::project(self.store, &rebased.overlay, &staging)?;
            let out = self.carry(&ws.path, &staging, &old, &cp.entries, &d, cfg, ignorecase, &name)?;
            // Review P1-14: what the carry took and when, next to the old
            // tree's admin dir, for later judgements of the kept old tree.
            write_carried(&old_admin, n, &out.carried);
            git.attach_worktree(&shadow, n, &head, &staging, &ws.path)?;
            strata_apfs::full_fsync(&strata_apfs::open_rooted(&root, &rp(sname.clone()))?)?;
            strata_apfs::full_fsync(&strata_apfs::open_root(&root)?)?;
            Ok((rebased, out.uncarried, out.carried, ignorecase))
        })();
        let (rebased, uncarried, carried, ignorecase) = match prepared {
            Ok(x) => x,
            Err(e) => return Err(self.prepare_failed(&mv, "prepare", e)),
        };

        // 14. recheck.
        let rechecked = (|| -> Result<Vec<strata_deps::OpenFile>> {
            self.store.set_move_phase(session, &name, n, "recheck")?;
            self.seams.hit("move.recheck", Some(&name))?;
            let mut h = held_dirs(&ws.path, &d)?;
            h.extend(held_dirs(&staging, &d)?);
            Ok(h)
        })();
        match rechecked {
            Ok(h) if h.is_empty() => {}
            Ok(h) => {
                return Err(self.prepare_failed(&mv, "recheck", open_files_error(&ws.path, h)));
            }
            Err(e) => return Err(self.prepare_failed(&mv, "recheck", e)),
        }
        // 15.
        match self.store.set_move_state(
            session,
            &name,
            n,
            MoveState::Preparing,
            MoveState::Swapping,
            Some(&rebased.conflicts),
            SystemTime::now(),
        ) {
            Ok(true) => {}
            Ok(false) => {
                let e =
                    StrataError::new(Code::Unknown, format!("move {n} of {session}/{name} left Preparing meanwhile"))
                        .with_detail(json!({ "reason": "intent_changed", "n": n }));
                return Err(self.prepare_failed(&mv, "recheck", e));
            }
            Err(e) => return Err(self.prepare_failed(&mv, "recheck", e)),
        }

        // ---- Phase E: the swap.
        let mut phase = "swap";
        let swapped = (|| -> Result<()> {
            self.store.set_move_phase(session, &name, n, "swap")?;
            self.seams.hit("move.swap", Some(&name))?;
            strata_apfs::rename_rooted(&root, &rp(sname.clone()), &rp(name.as_str()), RenameMode::Swap)?;
            strata_apfs::full_fsync(&strata_apfs::open_root(&root)?)?;
            phase = "pin";
            self.store.set_move_phase(session, &name, n, "pin")?;
            self.seams.hit("move.pin", Some(&name))?;
            git.update_ref(&pin_ref(session), &head, Some(&from_head))?;
            phase = "record";
            self.store.set_move_phase(session, &name, n, "record")?;
            self.seams.hit("move.record", Some(&name))?;
            self.store.finish_move(session, &name, n, &rebased.conflicts, SystemTime::now())
        })();
        if let Err(e) = swapped {
            if is_stop(&e) {
                return Err(e);
            }
            let kept = vec![ws.path.clone(), staging.clone()];
            let mut extra = Map::new();
            extra.insert("repo".into(), json!(name.as_str()));
            extra.insert("n".into(), json!(n));
            let err = unknown_error(SceneOp::Move, session, Some(&name), phase, cause_of(&e), &kept, extra);
            let scene = Scene {
                op: SceneOp::Move,
                session: session.clone(),
                repo: Some(name.clone()),
                phase: phase.to_string(),
                at: SystemTime::now(),
                kept,
                error: err.clone(),
            };
            if let Ok(v) = serde_json::to_value(&scene) {
                let _ = self.store.fail_move(session, &name, n, &v, scene.at);
            }
            return Err(err);
        }

        // ---- Phase F: the old tree (now at S). Failures here only show in
        // `old_tree` and status.
        let old_tree = match self.seams.hit("move.old_tree", Some(&name)) {
            Err(e) if is_stop(&e) => return Err(e),
            Err(_) => Some(staging.clone()),
            Ok(()) => {
                let held = strata_deps::open_files(&staging).map(|h| !h.is_empty()).unwrap_or(true);
                // Review P1-14: the overlay entries, and every carried
                // ignored entry as the carry took it.
                let unchanged = self.tree_matches(session, &name, cfg, &staging, &old, &cp.entries)
                    && untouched(&staging, &old, &cp.entries, &d, cfg, ignorecase, Some(&carried), started);
                if !held && unchanged && uncarried.is_empty() {
                    let gone = self.store.retire_dir(&staging).is_ok() && {
                        let _ = self.retire_present(&old_admin);
                        true
                    };
                    if gone { None } else { Some(staging.clone()) }
                } else {
                    if !uncarried.is_empty() || !unchanged {
                        let _ = self.store.set_old_tree(session, &name, n, &staging);
                    }
                    Some(staging.clone())
                }
            }
        };
        // 20. earlier trees kept only because they were held.
        if let Ok(all) = self.store.move_intents(session) {
            let mine: Vec<&MoveIntent> = all.iter().filter(|i| i.repo == name).collect();
            for i in &mine {
                if i.n == n || i.state != MoveState::Done || i.old_tree.is_some() || !exists(&i.staging) {
                    continue;
                }
                if !matches!(strata_deps::open_files(&i.staging), Ok(h) if h.is_empty()) {
                    continue;
                }
                if !self.old_tree_unchanged(i, cfg).unwrap_or(false) {
                    continue;
                }
                let admin = shadow_dir.join("worktrees").join(prior_generation(&all, i).to_string());
                if self.store.retire_dir(&i.staging).is_ok() {
                    let _ = self.retire_present(&admin);
                }
            }
        }

        Ok(MoveReport {
            session: session.clone(),
            repo: name,
            from: old_gen,
            from_head,
            to: new_base,
            head,
            checkpoint: Some(cp.id),
            dropped: rebased.dropped,
            conflicts: rebased.conflicts,
            lww: rebased.lww,
            uncarried,
            old_tree,
        })
    }

    /// A `Base` for a registered generation (§5.7 step 10).
    pub(crate) fn base_of(&self, g: &BaseGen) -> Result<Base> {
        let manifest = self.store.manifest(&g.repo, &g.tree)?.ok_or_else(|| {
            StrataError::new(Code::Corrupt, format!("base {}/{} of {} has no manifest", g.tree, g.deps, g.repo))
                .with_detail(json!({ "reason": "manifest_missing", "base": g }))
        })?;
        Ok(Base { r#gen: g.clone(), manifest: Arc::new(manifest), dir: self.store.base_dir(g) })
    }

    #[allow(clippy::too_many_arguments)]
    fn carry(
        &self,
        ws: &Path,
        staging: &Path,
        old: &Base,
        entries: &[OverlayEntry],
        d: &[RepoPath],
        cfg: &RepoConfig,
        fold: bool,
        name: &RepoName,
    ) -> Result<crate::carry::CarryOut> {
        let mut owned: Vec<&str> = old.manifest.entries.keys().map(|p| p.as_str()).collect();
        for e in entries {
            owned.push(e.path.as_str());
            if let strata_types::EntryKind::Renamed { from, .. } = &e.kind {
                owned.push(from.as_str());
            }
        }
        let c = Carry {
            ws,
            dst: staging,
            owned,
            deps_dirs: d.iter().map(|p| p.as_str()).collect(),
            nested: strata_deps::nested_seed_caches(cfg),
            fold,
        };
        c.run(&|| self.seams.hit("move.carry", Some(name)))
    }

    /// Whether the tree at `dir` still reads as `entries` against `base`.
    fn tree_matches(
        &self,
        session: &SessionId,
        name: &RepoName,
        cfg: &RepoConfig,
        dir: &Path,
        base: &Base,
        entries: &[OverlayEntry],
    ) -> bool {
        let Ok(git) = self.git(name) else {
            return false;
        };
        let wsref = WorkspaceRef { session, repo: name, root: dir, config: cfg, git, store: self.store };
        match strata_overlay::reconcile_full(&wsref, base) {
            Ok(o) => sorted(o.entries) == sorted(entries.to_vec()),
            Err(_) => false,
        }
    }

    /// The BeforeRollout checkpoint of a done move: the latest checkpoint of
    /// the workspace at or before the move started (§5.5 step 2.9).
    pub(crate) fn rollout_checkpoint(&self, i: &MoveIntent) -> Result<Option<strata_types::Checkpoint>> {
        let infos = self.store.checkpoints(&i.session, Some(&i.repo))?;
        let Some(info) = infos.iter().filter(|c| c.at <= i.started).max_by_key(|c| (c.at, c.id)) else {
            return Ok(None);
        };
        Ok(Some(self.store.checkpoint(&i.session, info.id)?))
    }

    /// Whether a done move's old tree still matches its BeforeRollout
    /// checkpoint, and (review P1-14) still holds every carried ignored
    /// entry as the carry took it: judged against the record the move left
    /// in the old tree's admin dir, or, without one, against the move's
    /// start.
    pub(crate) fn old_tree_unchanged(&self, i: &MoveIntent, cfg: &RepoConfig) -> Result<bool> {
        let path = i.old_tree.clone().unwrap_or_else(|| i.staging.clone());
        let Some(cp) = self.rollout_checkpoint(i)? else {
            return Ok(false);
        };
        let base = self.base_of(&cp.base)?;
        if !self.tree_matches(&i.session, &i.repo, cfg, &path, &base, &cp.entries) {
            return Ok(false);
        }
        let mut dset: BTreeSet<RepoPath> = cfg.deps_dirs.iter().cloned().collect();
        for deps in [&cp.base.deps, &i.to.deps] {
            if let Some(set) = strata_deps::find_set(self.store, &i.repo, deps)? {
                dset.extend(set.dirs.iter().cloned());
            }
        }
        let d: Vec<RepoPath> = dset.into_iter().collect();
        let fold = self.git(&i.repo)?.read_index(&path).map(|x| x.ignorecase).unwrap_or(false);
        let all = self.store.move_intents(&i.session)?;
        let admin =
            self.shadow_dir(&i.session, &i.repo).join("worktrees").join(prior_generation(&all, i).to_string());
        let carried = read_carried(&admin, i.n);
        Ok(untouched(&path, &base, &cp.entries, &d, cfg, fold, carried.as_ref(), i.started))
    }

    /// Retire `p` unless it is already gone.
    pub(crate) fn retire_present(&self, p: &Path) -> Result<()> {
        if !exists(p) {
            return Ok(());
        }
        match self.store.retire_dir(p) {
            Ok(_) => Ok(()),
            Err(e) if e.code == Code::NoSuchPath => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Take a move's internal staging away: retire the staging and the
    /// admin dir `worktrees/<n>`; a non-directory there (or at `worktrees`)
    /// is quarantined, never followed. Returns the quarantine path, if any.
    pub(crate) fn collect_leftover(&self, staging: &Path, shadow_dir: &Path, n: u64) -> Result<Option<PathBuf>> {
        self.retire_present(staging)?;
        self.collect_admin(shadow_dir, n)
    }

    /// The admin-dir half of `collect_leftover`: retire `worktrees/<n>`,
    /// quarantining a non-directory there (or at `worktrees`).
    pub(crate) fn collect_admin(&self, shadow_dir: &Path, n: u64) -> Result<Option<PathBuf>> {
        let wt = shadow_dir.join("worktrees");
        match lstat(&wt).map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &wt))? {
            None => return Ok(None),
            Some(m) if !m.is_dir() => {
                return quarantine(shadow_dir, "worktrees", "worktrees").map(Some);
            }
            Some(_) => {}
        }
        let admin = wt.join(n.to_string());
        match lstat(&admin).map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &admin))? {
            None => Ok(None),
            Some(m) if m.is_dir() => self.retire_present(&admin).map(|_| None),
            Some(_) => quarantine(shadow_dir, &format!("worktrees/{n}"), &n.to_string()).map(Some),
        }
    }

    /// §5.7 "第 13 步的失败": a named code refuses the move (staging taken
    /// away, intent Refused); an Unknown, or a failed take-away, leaves the
    /// staging and the Preparing intent for a later collection.
    fn prepare_failed(&self, mv: &Mv<'_>, phase: &str, e: StrataError) -> StrataError {
        if is_stop(&e) {
            return e;
        }
        if e.code != Code::Unknown {
            match self.collect_leftover(&mv.staging, &mv.shadow_dir, mv.n) {
                Ok(q) => {
                    let mut e = e;
                    if let Some(q) = q {
                        set_detail(&mut e, "quarantined", json!(q));
                    }
                    let _ = self.store.set_move_state(
                        mv.session,
                        mv.name,
                        mv.n,
                        MoveState::Preparing,
                        MoveState::Refused,
                        None,
                        SystemTime::now(),
                    );
                    return e;
                }
                Err(re) => {
                    let mut extra = Map::new();
                    extra.insert("repo".into(), json!(mv.name.as_str()));
                    extra.insert("n".into(), json!(mv.n));
                    extra.insert("refused".into(), cause_of(&e));
                    return unknown_error(
                        SceneOp::Move,
                        mv.session,
                        Some(mv.name),
                        phase,
                        cause_of(&re),
                        &self.prepare_kept(mv),
                        extra,
                    );
                }
            }
        }
        let mut extra = Map::new();
        extra.insert("repo".into(), json!(mv.name.as_str()));
        extra.insert("n".into(), json!(mv.n));
        unknown_error(SceneOp::Move, mv.session, Some(mv.name), phase, cause_of(&e), &self.prepare_kept(mv), extra)
    }

    /// `kept` of a prepare-time Unknown: the staging (the intent names it),
    /// plus `worktrees/<n>` if it was made.
    fn prepare_kept(&self, mv: &Mv<'_>) -> Vec<PathBuf> {
        let mut kept = vec![mv.staging.clone()];
        let admin = mv.shadow_dir.join("worktrees").join(mv.n.to_string());
        if exists(&admin) {
            kept.push(admin);
        }
        kept
    }
}

/// The generation a done move `i` replaced: the previous done move's n, or
/// 1 (the generation `new` attaches).
pub(crate) fn prior_generation(all: &[MoveIntent], i: &MoveIntent) -> u64 {
    all.iter().filter(|x| x.repo == i.repo && x.state == MoveState::Done && x.n < i.n).map(|x| x.n).max().unwrap_or(1)
}

/// Move a non-directory entry of the shadow repository into
/// `<shadow>/strata-quarantine/<stem>-<nonce>`: only the entry itself, never
/// followed; a failure is Unknown.
fn quarantine(shadow_dir: &Path, from: &str, stem: &str) -> Result<PathBuf> {
    let to = format!("strata-quarantine/{stem}-{}", random_hex(8));
    let r = (|| -> Result<()> {
        match strata_apfs::mkdir_rooted(shadow_dir, &rp("strata-quarantine"), 0o755, false) {
            Ok(()) => {}
            Err(e) if e.code == Code::Exists => {}
            Err(e) => return Err(e),
        }
        strata_apfs::rename_rooted(shadow_dir, &rp(from), &rp(to.clone()), RenameMode::Excl)
    })();
    match r {
        Ok(()) => Ok(shadow_dir.join(&to)),
        Err(e) => {
            let mut u = StrataError::new(
                Code::Unknown,
                format!("could not quarantine {}: {}", shadow_dir.join(from).display(), e.message),
            )
            .with_detail(
                json!({ "reason": "quarantine_failed", "path": shadow_dir.join(from), "cause": cause_of(&e) }),
            );
            u.code = Code::Unknown;
            Err(u)
        }
    }
}

/// The carry record's file in an old tree's admin dir (review P1-14).
const CARRIED_FILE: &str = "strata-carried.json";

#[derive(Serialize, Deserialize)]
struct CarriedRecord {
    n: u64,
    carried: Carried,
}

/// Best effort: without the record, later judgements hold every ignored
/// entry to the move's start (stricter, never looser).
fn write_carried(admin: &Path, n: u64, carried: &Carried) {
    if let Ok(body) = serde_json::to_vec(&CarriedRecord { n, carried: carried.clone() }) {
        let _ = std::fs::write(admin.join(CARRIED_FILE), body);
    }
}

fn read_carried(admin: &Path, n: u64) -> Option<Carried> {
    let body = std::fs::read(admin.join(CARRIED_FILE)).ok()?;
    let r: CarriedRecord = serde_json::from_slice(&body).ok()?;
    (r.n == n).then_some(r.carried)
}

/// Review P1-14: no ignored entry of the old tree at `tree` holds content
/// the carry did not take (`Carry::touched`). An error reads as touched.
#[allow(clippy::too_many_arguments)]
fn untouched(
    tree: &Path,
    old: &Base,
    entries: &[OverlayEntry],
    d: &[RepoPath],
    cfg: &RepoConfig,
    fold: bool,
    carried: Option<&Carried>,
    since: SystemTime,
) -> bool {
    let mut owned: Vec<&str> = old.manifest.entries.keys().map(|p| p.as_str()).collect();
    for e in entries {
        owned.push(e.path.as_str());
        if let strata_types::EntryKind::Renamed { from, .. } = &e.kind {
            owned.push(from.as_str());
        }
    }
    let c = Carry {
        ws: tree,
        dst: tree,
        owned,
        deps_dirs: d.iter().map(|p| p.as_str()).collect(),
        nested: strata_deps::nested_seed_caches(cfg),
        fold,
    };
    matches!(c.touched(carried, since), Ok(None))
}

fn sorted(mut v: Vec<OverlayEntry>) -> Vec<OverlayEntry> {
    v.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    v
}

/// Open files under each existing `dir/<d>`.
fn held_dirs(dir: &Path, d: &[RepoPath]) -> Result<Vec<strata_deps::OpenFile>> {
    let mut out = Vec::new();
    for p in d {
        let full = dir.join(p.as_str());
        if matches!(lstat(&full), Ok(Some(m)) if m.is_dir()) {
            out.extend(strata_deps::open_files(&full)?);
        }
    }
    Ok(out)
}

fn open_files_error(ws: &Path, holders: Vec<strata_deps::OpenFile>) -> StrataError {
    let pids: BTreeSet<i32> = holders.iter().map(|h| h.pid).collect();
    StrataError::new(
        Code::OpenFiles,
        format!(
            "processes {} hold files in the dependency directories of {}",
            pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "),
            ws.display()
        ),
    )
    .with_detail(json!({ "path": ws, "holders": holders }))
}

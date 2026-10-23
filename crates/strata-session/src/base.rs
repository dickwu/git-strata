//! Base generations (§5.2): `ensure_base` and `ensure_base_for`. Cache fill,
//! never a visible write. Export, deps and the manifest run outside the
//! repo lock; only registration takes it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_apfs::{RemoveKind, RenameMode};
use strata_types::{BaseGen, BaseManifest, Code, CommitOid, DepsSet, DepsSetId, RepoName, StrataError, TreeOid};

use crate::sys::{random_hex, rp};
use crate::{BASE_STAGING_DIR, Result, Sessions, seams};

/// How many consecutive deps-set misses (step 5 and 7.1) before `Busy`.
const MISS_LIMIT: u32 = 3;

/// What one pass over steps 3–8 ended with.
enum Pass {
    Built(BaseGen),
    /// Step 5 (after `ensure`) or 7.1 missed: back to step 2.
    Miss,
    /// `B` was taken by `gc_repo` before registration: rebuild once.
    StagingGone,
}

/// A failure in steps 3–8, with what is already known about it.
struct Fail {
    e: StrataError,
    phase: &'static str,
    /// Paths kept besides `B` (an unregistered base_dir that could not be retired).
    kept: Vec<PathBuf>,
    /// `B` was already handled (empty and removed): do not retire it again.
    staging_done: bool,
}

impl Fail {
    fn at(phase: &'static str) -> impl FnOnce(StrataError) -> Fail {
        move |e| Fail { e, phase, kept: Vec::new(), staging_done: false }
    }
}

/// Removes `B` from the in-process "live staging" set on drop (step 3).
struct Live<'s> {
    set: &'s std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
    path: PathBuf,
}

impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.path);
    }
}

impl Sessions<'_> {
    /// `head`'s tree with the deps set its install inputs need (§5.2).
    /// Called without holding `repo`'s write lock.
    pub fn ensure_base(&self, repo: &RepoName, head: &CommitOid) -> Result<BaseGen> {
        let git = self.git(repo)?;
        let tree = git.tree_of(head)?;
        self.base_flow(repo, &tree, None)
    }

    /// `tree` with the given, already sealed deps set; never `deps::ensure`
    /// (§5.2). Called without holding `repo`'s write lock.
    pub fn ensure_base_for(&self, repo: &RepoName, tree: &TreeOid, deps: &DepsSetId) -> Result<BaseGen> {
        self.git(repo)?;
        let Some(set) = strata_deps::find_set(self.store, repo, deps)? else {
            return Err(deps_unsealed(repo, deps));
        };
        self.base_flow(repo, tree, Some(&set))
    }

    pub(crate) fn deps_ctx<'c>(&'c self, repo: &'c RepoName) -> Result<strata_deps::Ctx<'c>> {
        Ok(strata_deps::Ctx {
            store: self.store,
            paths: &self.config.paths,
            repo,
            cfg: self.repo_cfg(repo)?,
            git: self.git(repo)?,
            env: self.env,
            locks: self.locks,
        })
    }

    pub(crate) fn base_flow(&self, repo: &RepoName, tree: &TreeOid, given: Option<&DepsSet>) -> Result<BaseGen> {
        let mut misses = 0u32;
        let mut gone = 0u32;
        loop {
            // Step 2: the hot path.
            if let Some(g) = self.hot_base(repo, tree, given)? {
                return Ok(g);
            }
            match self.base_pass(repo, tree, given)? {
                Pass::Built(g) => return Ok(g),
                Pass::Miss => {
                    misses += 1;
                    if let Some(set) = given {
                        // 7.1 for ensure_base_for: it never builds a deps set.
                        return Err(deps_unsealed(repo, &set.id));
                    }
                    if misses >= MISS_LIMIT {
                        return Err(StrataError::new(
                            Code::Busy,
                            format!("base {repo} {tree}: the deps set was collected {MISS_LIMIT} times in a row"),
                        )
                        .with_detail(json!({ "reason": "deps_set_race", "repo": repo.as_str(), "tree": tree.as_str() })));
                    }
                }
                Pass::StagingGone => {
                    gone += 1;
                    if gone >= 2 {
                        return Err(StrataError::new(
                            Code::Busy,
                            format!("base {repo} {tree}: the staging directory was collected twice"),
                        )
                        .with_detail(json!({ "reason": "base_staging_retired", "repo": repo.as_str(), "tree": tree.as_str() })));
                    }
                }
            }
        }
    }

    /// §5.2 step 2.
    fn hot_base(&self, repo: &RepoName, tree: &TreeOid, given: Option<&DepsSet>) -> Result<Option<BaseGen>> {
        if let Some(set) = given {
            let g = BaseGen { repo: repo.clone(), tree: tree.clone(), deps: set.id.clone() };
            return Ok(self.store.has_base_gen(&g)?.then_some(g));
        }
        let root = &self.config.paths.root;
        let dir = rp(format!("bases/{repo}/{tree}"));
        for item in strata_apfs::list_dir(root, Some(&dir)) {
            let (p, _) = match item {
                Ok(x) => x,
                Err(e) if matches!(e.code, Code::NoSuchPath) => return Ok(None),
                Err(e) => return Err(e),
            };
            let Some(name) = Path::new(p.as_str()).file_name().and_then(|n| n.to_str()) else { continue };
            let Ok(d) = DepsSetId::try_from(name.to_string()) else { continue };
            let found = BaseGen { repo: repo.clone(), tree: tree.clone(), deps: d };
            if !self.store.has_base_gen(&found)? {
                continue;
            }
            let ctx = self.deps_ctx(repo)?;
            let (_, id) = strata_deps::key_of(&ctx, &self.store.base_dir(&found))?;
            let g = BaseGen { repo: repo.clone(), tree: tree.clone(), deps: id };
            if self.store.has_base_gen(&g)? {
                return Ok(Some(g));
            }
            return Ok(None);
        }
        Ok(None)
    }

    /// §5.2 steps 3–8, one staging directory.
    fn base_pass(&self, repo: &RepoName, tree: &TreeOid, given: Option<&DepsSet>) -> Result<Pass> {
        let root = &self.config.paths.root;
        let rel_b = format!("bases/{repo}/{BASE_STAGING_DIR}/{}", random_hex(16));
        let b = root.join(&rel_b);
        // Step 3: mark live before the directory exists.
        self.live_staging.lock().unwrap_or_else(|p| p.into_inner()).insert(b.clone());
        let _live = Live { set: &self.live_staging, path: b.clone() };
        strata_apfs::mkdir_rooted(root, &rp(format!("bases/{repo}/{BASE_STAGING_DIR}")), 0o755, true)?;
        strata_apfs::mkdir_rooted(root, &rp(rel_b.clone()), 0o755, false)?;

        match self.base_steps(repo, tree, given, &rel_b, &b) {
            Ok(p) => Ok(p),
            Err(f) if seams::is_stop(&f.e) => Err(f.e),
            Err(f) => self.base_failed(repo, tree, &b, f),
        }
    }

    fn base_steps(
        &self,
        repo: &RepoName,
        tree: &TreeOid,
        given: Option<&DepsSet>,
        rel_b: &str,
        b: &Path,
    ) -> std::result::Result<Pass, Fail> {
        let root = &self.config.paths.root;
        let cfg = self.repo_cfg(repo).map_err(Fail::at("export"))?;
        let git = self.git(repo).map_err(Fail::at("export"))?;
        let b_tree = b.join("tree");

        // Step 4: export outside the lock.
        self.seams.hit("base.export", Some(repo)).map_err(Fail::at("export"))?;
        git.export_tree(tree, &b_tree).map_err(Fail::at("export"))?;

        // Step 5: the deps set.
        self.seams.hit("base.deps", Some(repo)).map_err(Fail::at("deps"))?;
        let set = match given {
            Some(s) => s.clone(),
            None => {
                let ctx = self.deps_ctx(repo).map_err(Fail::at("deps"))?;
                let (_, id) = strata_deps::key_of(&ctx, &b_tree).map_err(Fail::at("deps"))?;
                match strata_deps::find_set(self.store, repo, &id).map_err(Fail::at("deps"))? {
                    Some(s) => s,
                    None => {
                        // Claim or cold install: B is only ensure's input, then retired.
                        strata_deps::ensure(&ctx, &b_tree).map_err(Fail::at("deps"))?;
                        return self.retire_staging_after(b).map(|_| Pass::Miss);
                    }
                }
            }
        };

        // Step 6: the manifest, before the deps dirs are cloned in.
        self.seams.hit("base.manifest", Some(repo)).map_err(Fail::at("manifest"))?;
        let manifest = strata_overlay::build_manifest(&b_tree, tree, cfg).map_err(Fail::at("manifest"))?;

        // Step 7: register, one critical section.
        self.seams.hit("base.register", Some(repo)).map_err(Fail::at("register"))?;
        let mut out: Option<std::result::Result<Pass, Fail>> = None;
        let lock = self.locks.with_write(repo, &mut || {
            out = Some(self.register(repo, tree, &set, &manifest, rel_b, b));
            Ok(())
        });
        match lock {
            Err(e) => return Err(Fail::at("register")(e)),
            Ok(Err(e)) => return Err(Fail::at("register")(e)),
            Ok(Ok(())) => {}
        }
        let pass = out.expect("with_write ran the closure")?;
        match pass {
            Pass::Built(g) => {
                // Step 8: B should be empty now.
                match strata_apfs::remove_rooted(root, &rp(rel_b), RemoveKind::EmptyDir) {
                    Ok(()) => {}
                    Err(e) if e.code == Code::NotEmpty => {
                        // The gen is registered; a staging that cannot be
                        // retired is a leftover, reported by leftovers().
                        let _ = self.store.retire_dir(b);
                    }
                    Err(_) => {}
                }
                Ok(Pass::Built(g))
            }
            Pass::Miss => self.retire_staging_after(b).map(|_| Pass::Miss),
            Pass::StagingGone => Ok(Pass::StagingGone),
        }
    }

    /// Step 7 inside the lock.
    fn register(
        &self,
        repo: &RepoName,
        tree: &TreeOid,
        set: &DepsSet,
        manifest: &BaseManifest,
        rel_b: &str,
        b: &Path,
    ) -> std::result::Result<Pass, Fail> {
        let root = &self.config.paths.root;
        let f = Fail::at("register");
        // 7.1
        if strata_deps::find_set(self.store, repo, &set.id).map_err(Fail::at("register"))?.is_none() {
            return Ok(Pass::Miss);
        }
        // 7.2
        let bg = BaseGen { repo: repo.clone(), tree: tree.clone(), deps: set.id.clone() };
        if self.store.has_base_gen(&bg).map_err(Fail::at("register"))? {
            // Another call registered it first: retire our staging, return it.
            match self.store.retire_dir(b) {
                Ok(_) => {}
                Err(e) if e.code == Code::NoSuchPath => {}
                Err(_) => {} // left for leftovers()/recover
            }
            return Ok(Pass::Built(bg));
        }
        // 7.3
        let b_tree = b.join("tree");
        if let Err(e) = strata_deps::clone_into(self.store, set, &b_tree) {
            if e.code == Code::NoSuchPath && !b.exists() {
                return Ok(Pass::StagingGone);
            }
            return Err(f(e));
        }
        // 7.4
        let tree_dir = format!("bases/{repo}/{tree}");
        strata_apfs::mkdir_rooted(root, &rp(tree_dir.clone()), 0o755, true).map_err(Fail::at("register"))?;
        let from = rp(format!("{rel_b}/tree"));
        let to = rp(format!("{tree_dir}/{}", set.id));
        let base_dir = self.store.base_dir(&bg);
        match strata_apfs::rename_rooted(root, &from, &to, RenameMode::Excl) {
            Ok(()) => {}
            Err(e) if e.code == Code::Exists => {
                // An unregistered leftover of an earlier crash.
                self.store.retire_dir(&base_dir).map_err(Fail::at("register"))?;
                strata_apfs::rename_rooted(root, &from, &to, RenameMode::Excl).map_err(Fail::at("register"))?;
            }
            Err(e) if e.code == Code::NoSuchPath && !b.exists() => return Ok(Pass::StagingGone),
            Err(e) => return Err(Fail::at("register")(e)),
        }
        // From here an unregistered base_dir exists: on failure, retire it
        // inside this critical section (review M-6).
        let after = |e: StrataError| -> Fail {
            let kept = match self.store.retire_dir(&base_dir) {
                Ok(_) => Vec::new(),
                Err(_) => vec![base_dir.clone()],
            };
            Fail { e, phase: "register", kept, staging_done: false }
        };
        // 7.5
        let sync = |rel: &str| -> Result<()> {
            let fd = strata_apfs::open_rooted(root, &rp(rel.to_string()))?;
            strata_apfs::full_fsync(&fd)
        };
        sync(&tree_dir).map_err(after)?;
        sync(rel_b).map_err(after)?;
        // 7.6
        match self.store.record_base_gen(&bg, manifest, SystemTime::now()) {
            Ok(()) => Ok(Pass::Built(bg)),
            Err(e) if e.code == Code::Unknown => {
                // Registered or not, unknowable: keep base_dir, remove the
                // (empty) staging, report Unknown.
                let _ = strata_apfs::remove_rooted(root, &rp(rel_b), RemoveKind::EmptyDir);
                Err(Fail { e, phase: "register", kept: vec![base_dir.clone()], staging_done: true })
            }
            Err(e) => Err(after(e)),
        }
    }

    /// Retire `B` after `ensure` or a 7.1 miss; `NoSuchPath` means gc took it.
    fn retire_staging_after(&self, b: &Path) -> std::result::Result<(), Fail> {
        match self.store.retire_dir(b) {
            Ok(_) => Ok(()),
            Err(e) if e.code == Code::NoSuchPath => Ok(()),
            Err(e) => {
                let mut f = Fail::at("deps")(e.clone());
                f.kept.push(b.to_path_buf());
                f.staging_done = true;
                // Report as a failed retirement: Unknown, kept [B].
                Err(Fail { e: unknown_base(&self.repo_hint(b), None, "deps", b, &e, &f.kept, &[]), ..f })
            }
        }
    }

    fn repo_hint(&self, b: &Path) -> String {
        // bases/<repo>/.staging/<x>
        b.parent().and_then(|p| p.parent()).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The failure path (§5.2 "失败"): retire `B` first, then pass a named
    /// code through or report a half-done write as `Unknown`.
    fn base_failed(&self, repo: &RepoName, tree: &TreeOid, b: &Path, f: Fail) -> Result<Pass> {
        if f.staging_done {
            if f.e.code == Code::Unknown && f.e.detail.as_ref().and_then(|d| d.get("op")) == Some(&json!("base")) {
                return Err(f.e);
            }
            return Err(unknown_base(repo.as_str(), Some(tree), f.phase, b, &f.e, &f.kept, &[]));
        }
        let mut kept = f.kept.clone();
        let mut retired = Vec::new();
        match self.store.retire_dir(b) {
            Ok(_) => retired.push(b.to_path_buf()),
            Err(e) if e.code == Code::NoSuchPath => return Ok(Pass::StagingGone),
            Err(_) => kept.push(b.to_path_buf()),
        }
        if f.e.code != Code::Unknown && kept.is_empty() {
            return Err(f.e);
        }
        Err(unknown_base(repo.as_str(), Some(tree), f.phase, b, &f.e, &kept, &retired))
    }
}

fn deps_unsealed(repo: &RepoName, deps: &DepsSetId) -> StrataError {
    StrataError::new(Code::InvalidInput, format!("deps set {deps} of {repo} is not sealed"))
        .with_detail(json!({ "reason": "deps_unsealed", "repo": repo.as_str(), "deps": deps.as_str() }))
}

/// `Unknown` for `op = "base"` (§6.3).
fn unknown_base(
    repo: &str,
    tree: Option<&TreeOid>,
    phase: &str,
    b: &Path,
    cause: &StrataError,
    kept: &[PathBuf],
    retired: &[PathBuf],
) -> StrataError {
    let tree_s = tree.map(|t| t.as_str().to_string()).unwrap_or_default();
    let tail = if kept.is_empty() {
        "the staging was retired; retry when the cause is fixed".to_string()
    } else {
        format!(
            "kept {}; the next daemon restart collects it",
            kept.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        )
    };
    let paths = |v: &[PathBuf]| Value::from(v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>());
    StrataError::new(Code::Unknown, format!("base {repo} {tree_s}: {phase} failed before anything visible changed; {tail}"))
        .with_detail(json!({
            "op": "base",
            "repo": repo,
            "tree": tree_s,
            "phase": phase,
            "path": b.display().to_string(),
            "cause": cause,
            "kept": paths(kept),
            "retired": paths(retired),
        }))
}

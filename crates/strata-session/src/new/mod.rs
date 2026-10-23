//! `new` (§5.1): validate (phase A), resolve heads and bases (phase B),
//! register (phase C), build the layout (phase D), observe main (phase E).

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_types::{Code, CommitOid, RepoName, SessionManifest, StrataError, TreeOid};

use std::collections::BTreeMap;
use std::time::SystemTime;

use strata_apfs::{FileKind, IoSide, classify_io_error};
use strata_store::{Lifecycle, SeedRow};
use strata_types::{BaseGen, RepoPath, SessionId, WorkspaceState};

use crate::seed::{SeedSource, ancestors, seed_files, seed_plan};
use crate::sys::{lstat, rp};
use crate::{Result, Sessions, dispatch};

/// What phase B settled for one repo (§5.1 step 10).
pub(crate) struct Plan {
    pub(crate) files: Vec<SeedSource>,
    /// Seed caches to clone (tracked repos only).
    pub(crate) caches: Vec<RepoPath>,
}

/// `refs/strata/pins/<id>` in the main checkout (§4.2).
pub(crate) fn pin_ref(id: &SessionId) -> String {
    format!("refs/strata/pins/{id}")
}

/// Add `repo` to a seed refusal's detail.
fn with_repo(mut e: StrataError, repo: &RepoName) -> StrataError {
    if let Some(serde_json::Value::Object(m)) = &mut e.detail
        && m.get("field").and_then(|v| v.as_str()) == Some("seed_files")
    {
        m.insert("repo".into(), json!(repo.as_str()));
    }
    e
}

pub(crate) mod validate;

/// `new`'s result: the manifest, plus warnings about observing main after
/// the session was built (§5.1 step 15) [D15].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewReport {
    pub manifest: SessionManifest,
    /// Empty means no warnings.
    pub warnings: Vec<NewWarning>,
}

/// Serialised with a `kind` tag carrying the variant name (types.md §2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NewWarning {
    /// `deps::observe_main` failed for this repo; `detail` is deps' error
    /// as returned. The session is Active all the same.
    ObserveFailed { repo: RepoName, detail: StrataError },
}

impl Sessions<'_> {
    /// Create a session of `repos` (§5.1).
    #[allow(clippy::new_ret_no_self)] // `new` is the spec's operation name, not a constructor
    pub fn new(&self, ticket: Option<&str>, repos: &[RepoName]) -> Result<NewReport> {
        let v = self.validate_new(ticket, repos)?;
        let heads: BTreeMap<RepoName, (CommitOid, TreeOid)> = self.resolve_heads(&v.tracked)?.into_iter().collect();
        let all: Vec<RepoName> = heads.keys().cloned().collect();

        // Phase B, steps 9-10: base generations and seed plans (cache fill).
        let planned = dispatch::each_repo(&all, &|_| Ok(()), &|r| {
            let (_, tree) = &heads[r];
            let bg = self.base_flow(r, tree, None)?;
            let plan = self.plan_repo(r, &bg, tree, v.tracked.contains(r))?;
            Ok((bg, plan))
        })?;
        let plans: BTreeMap<RepoName, (BaseGen, Plan)> = planned.into_iter().collect();

        // Phase C, step 11: the first visible write.
        let paths_root = self.config.paths.root.clone();
        let root = paths_root.join("sessions").join(v.id.as_str());
        let manifest = SessionManifest {
            id: v.id.clone(),
            ticket: ticket.map(str::to_string),
            root: root.clone(),
            repos: all
                .iter()
                .map(|r| {
                    let ws = WorkspaceState {
                        base: plans[r].0.clone(),
                        head: heads[r].0.clone(),
                        path: root.join(r.as_str()),
                        tracked: v.tracked.contains(r),
                    };
                    (r.clone(), ws)
                })
                .collect(),
        };
        self.store.create_session(&manifest, SystemTime::now())?;

        // Phase D, steps 12-14.
        self.build_layout(&manifest, &plans)?;

        // Step 15: observe main; failures are warnings only.
        let warnings = self.observe_all(&manifest);
        Ok(NewReport { manifest, warnings })
    }

    /// §5.1 step 10: the seed-file plan and, for tracked repos, the seed caches.
    fn plan_repo(&self, repo: &RepoName, bg: &BaseGen, tree: &TreeOid, tracked: bool) -> Result<Plan> {
        let cfg = self.repo_cfg(repo)?;
        let git = self.git(repo)?;
        let base = self.store.base_dir(bg);
        let files = seed_plan(cfg, git, &cfg.path, &base, tree).map_err(|e| with_repo(e, repo))?;
        let mut caches = Vec::new();
        if tracked {
            'next: for c in &cfg.seed_caches {
                let src = cfg.path.join(c.as_str());
                match lstat(&src).map_err(|e| classify_io_error(&e, IoSide::Read, &src))? {
                    Some(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                    _ => continue,
                }
                for a in ancestors(c) {
                    match strata_apfs::attrs_of(&base, &a) {
                        Ok(at) if matches!(at.kind, FileKind::Dir) => {}
                        Ok(_) => continue 'next,
                        Err(e) if e.code == Code::NoSuchPath => break,
                        Err(e) if e.code == Code::NotDirectory => continue 'next,
                        Err(e) => return Err(e),
                    }
                }
                match strata_apfs::attrs_of(&base, c) {
                    Err(e) if e.code == Code::NoSuchPath => caches.push(c.clone()),
                    Err(e) if e.code == Code::NotDirectory => {}
                    Err(e) => return Err(e),
                    Ok(_) => {}
                }
            }
        }
        Ok(Plan { files, caches })
    }

    /// §5.1 steps 12-14. In this version a failure is returned as is.
    fn build_layout(&self, m: &SessionManifest, plans: &BTreeMap<RepoName, (BaseGen, Plan)>) -> Result<()> {
        let id = &m.id;
        let proot = &self.config.paths.root;
        let repos: Vec<RepoName> = m.repos.keys().cloned().collect();
        let seam = |phase: &'static str| move |r: &RepoName| self.seams.hit(&format!("new.{phase}"), Some(r));

        // 12. root
        self.store.set_session_phase(id, "root")?;
        self.seams.hit("new.root", None)?;
        strata_apfs::mkdir_rooted(proot, &rp("sessions"), 0o755, true)?;
        strata_apfs::mkdir_rooted(proot, &rp(format!("sessions/{id}")), 0o755, false)?;
        strata_apfs::mkdir_rooted(proot, &rp(format!("sessions/.repos/{id}")), 0o755, true)?;

        // 13.1 workspace
        self.store.set_session_phase(id, "workspace")?;
        dispatch::each_repo(&repos, &seam("workspace"), &|r| {
            strata_apfs::clone_tree_rooted(&self.store.base_dir(&plans[r].0), &m.root, &rp(r.as_str()))
        })?;

        // 13.2 git
        self.store.set_session_phase(id, "git")?;
        dispatch::each_repo(&repos, &seam("git"), &|r| {
            let git = self.git(r)?;
            let ws = &m.repos[r];
            let dir = self.shadow_dir(id, r);
            self.with_lock_unbounded(r, || {
                let shadow = git.ensure_shadow(&dir)?;
                git.attach_worktree(&shadow, 1, &ws.head, &ws.path, &ws.path)?;
                Ok(())
            })
        })?;

        // 13.3 pin
        self.store.set_session_phase(id, "pin")?;
        dispatch::each_repo(&repos, &seam("pin"), &|r| {
            let git = self.git(r)?;
            self.with_lock_unbounded(r, || git.update_ref(&pin_ref(id), &m.repos[r].head, None))
        })?;

        // 13.4 seed_caches (tracked repos only)
        self.store.set_session_phase(id, "seed_caches")?;
        let tracked: Vec<RepoName> = repos.iter().filter(|r| m.repos[*r].tracked).cloned().collect();
        dispatch::each_repo(&tracked, &seam("seed_caches"), &|r| {
            let cfg = self.repo_cfg(r)?;
            let ws = &m.repos[r].path;
            for c in &plans[r].1.caches {
                if let Some(parent) = ancestors(c).last() {
                    strata_apfs::mkdir_rooted(ws, parent, 0o755, true)?;
                }
                strata_apfs::clone_tree_rooted(&cfg.path.join(c.as_str()), ws, c)?;
            }
            Ok(())
        })?;

        // 13.5 seed_files
        self.store.set_session_phase(id, "seed_files")?;
        dispatch::each_repo(&repos, &seam("seed_files"), &|r| {
            let records = seed_files(&plans[r].1.files, &m.repos[r].path)?;
            let rows: Vec<SeedRow> = records.iter().map(SeedRow::from).collect();
            self.store.record_seeds(id, r, &rows)
        })?;

        // 14. finalize
        self.store.set_session_phase(id, "finalize")?;
        self.seams.hit("new.finalize", None)?;
        for rel in [format!("sessions/{id}"), format!("sessions/.repos/{id}")] {
            let fd = strata_apfs::open_rooted(proot, &rp(rel))?;
            strata_apfs::full_fsync(&fd)?;
        }
        self.store.set_lifecycle(id, Lifecycle::Creating, Lifecycle::Active, None, SystemTime::now())?;
        Ok(())
    }

    /// §5.1 step 15: every repo is observed; a failure is a warning.
    fn observe_all(&self, m: &SessionManifest) -> Vec<NewWarning> {
        let repos: Vec<RepoName> = m.repos.keys().cloned().collect();
        let out: Vec<std::sync::Mutex<Option<StrataError>>> = repos.iter().map(|_| std::sync::Mutex::new(None)).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..dispatch::WORKERS.min(repos.len()).max(1) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if i >= repos.len() {
                            return;
                        }
                        let r = &repos[i];
                        let res = self.seams.hit("new.observe", Some(r)).and_then(|_| {
                            let ws = &m.repos[r];
                            let ctx = self.deps_ctx(r)?;
                            let tree = &ws.base.tree;
                            strata_deps::observe_main(&ctx, tree, &ws.head, &self.store.base_dir(&ws.base))
                        });
                        if let Err(e) = res {
                            *out[i].lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                        }
                    }
                });
            }
        });
        repos
            .into_iter()
            .zip(out)
            .filter_map(|(repo, e)| {
                e.into_inner().unwrap_or_else(|p| p.into_inner()).map(|detail| NewWarning::ObserveFailed { repo, detail })
            })
            .collect()
    }

    /// §5.1 step 8 for every configured repo: fetch the default branch and
    /// read the chain top in one critical section, then the tree.
    pub(crate) fn resolve_heads(&self, _tracked: &[RepoName]) -> Result<Vec<(RepoName, (CommitOid, TreeOid))>> {
        let all: Vec<RepoName> = self.config.repos.keys().cloned().collect();
        dispatch::each_repo(&all, &|r| self.seams.hit("head", Some(r)), &|r| self.head_of(r))
    }

    fn head_of(&self, repo: &RepoName) -> Result<(CommitOid, TreeOid)> {
        let cfg = self.repo_cfg(repo)?;
        let git = self.git(repo)?;
        let head = self.with_lock(repo, || {
            git.fetch("origin", &cfg.default_branch)?;
            strata_deps::main_tip(cfg, git)
        })?;
        let Some(head) = head else {
            return Err(StrataError::new(
                Code::NoSuchBranch,
                format!("repo {repo} has no main tip for origin/{}", cfg.default_branch),
            )
            .with_detail(json!({ "repo": repo.as_str(), "remote": "origin", "branch": cfg.default_branch })));
        };
        let tree = git.tree_of(&head)?;
        Ok((head, tree))
    }
}

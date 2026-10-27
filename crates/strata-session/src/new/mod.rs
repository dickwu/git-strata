//! `new` (§5.1): validate (phase A), resolve heads and bases (phase B),
//! register (phase C), build the layout (phase D), observe main (phase E).

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_types::{Code, CommitOid, RepoName, SessionManifest, StrataError, TreeOid};

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::SystemTime;

use strata_apfs::{FileKind, IoSide, classify_io_error};
use strata_store::{Lifecycle, SeedRow};
use strata_types::{BaseGen, RepoPath, SessionId, WorkspaceState};

use crate::scene::{Scene, SceneOp, cause_of, kept_dirs, unknown_error};
use crate::seed::{SeedSource, ancestors, seed_files, seed_plan};
use crate::sys::{lstat, rp};
use crate::{Result, Sessions, dispatch};

/// What phase B settled for one repo (§5.1 step 10).
pub(crate) struct Plan {
    pub(crate) files: Vec<SeedSource>,
    /// Seed caches to clone (tracked repos only).
    pub(crate) caches: Vec<RepoPath>,
}

/// Background workers cloning seed caches during `new`'s layout (D54).
/// APFS runs clonefile calls on one volume one at a time (measured, D54),
/// so more workers would not make the caches land sooner.
const CACHE_WORKERS: usize = 1;

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

pub(crate) mod add;
pub(crate) mod validate;

/// Which repos a new session gets a workspace of (session.md D51).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Every configured repo, the requested ones tracked (design §3 sibling
    /// layout, §6.1). The default.
    #[default]
    Siblings,
    /// Only the requested repos, all tracked. More can be added later with
    /// `Sessions::add`.
    Only,
}

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
        self.new_with(ticket, repos, Layout::Siblings)
    }

    /// `new` with an explicit layout (D51): `Layout::Only` builds workspaces
    /// of the requested repos only and fetches only those.
    pub fn new_with(&self, ticket: Option<&str>, repos: &[RepoName], layout: Layout) -> Result<NewReport> {
        let v = self.validate_new(ticket, repos, layout)?;
        let heads: BTreeMap<RepoName, (CommitOid, TreeOid)> = self.resolve_heads(&v.members)?.into_iter().collect();
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

    /// §5.1 steps 12-14. Any failure here is after the first visible write:
    /// the session becomes `Unknown` with its scene, nothing is undone
    /// (§6.1 rule 3). A `StopAt` seam returns as is (a crash).
    fn build_layout(&self, m: &SessionManifest, plans: &BTreeMap<RepoName, (BaseGen, Plan)>) -> Result<()> {
        let at = Mutex::new(("root", None::<RepoName>));
        match self.layout_steps(m, plans, &at) {
            Ok(()) => Ok(()),
            Err(e) if crate::seams::is_stop(&e) => Err(e),
            Err(e) => {
                let (phase, repo) = at.into_inner().unwrap_or_else(|p| p.into_inner());
                Err(self.new_unknown(m, phase, repo.as_ref(), &e))
            }
        }
    }

    /// Record the scene and build the error (§6.3). If `set_lifecycle`
    /// fails too the row stays `Creating`; `recover` marks it next start.
    fn new_unknown(&self, m: &SessionManifest, phase: &str, repo: Option<&RepoName>, e: &StrataError) -> StrataError {
        let kept = kept_dirs(&self.config.paths.root, &m.id, m.repos.keys().cloned());
        let mut extra = serde_json::Map::new();
        if let Some(r) = repo {
            extra.insert("repo".into(), json!(r.as_str()));
            if let Some(ws) = m.repos.get(r) {
                extra.insert("path".into(), json!(ws.path.display().to_string()));
            }
        }
        let err = unknown_error(SceneOp::New, &m.id, None, phase, cause_of(e), &kept, extra);
        let now = SystemTime::now();
        let scene = Scene { op: SceneOp::New, session: m.id.clone(), repo: repo.cloned(), phase: phase.into(), at: now, kept, error: err.clone() };
        if let Ok(v) = serde_json::to_value(&scene) {
            let _ = self.store.set_lifecycle(&m.id, Lifecycle::Creating, Lifecycle::Unknown, Some(&v), now);
        }
        err
    }

    fn layout_steps(
        &self,
        m: &SessionManifest,
        plans: &BTreeMap<RepoName, (BaseGen, Plan)>,
        at: &Mutex<(&'static str, Option<RepoName>)>,
    ) -> Result<()> {
        let id = &m.id;
        let proot = &self.config.paths.root;
        let repos: Vec<RepoName> = m.repos.keys().cloned().collect();
        let enter = |phase: &'static str| -> Result<()> {
            *at.lock().unwrap_or_else(|p| p.into_inner()) = (phase, None);
            self.store.set_session_phase(id, phase)
        };
        // One phase over `repos`; the first failing repo in RepoName order
        // (the one dispatch returns) is the scene's repo.
        let each = |phase: &'static str, repos: &[RepoName], f: &(dyn Fn(&RepoName) -> Result<()> + Sync)| -> Result<()> {
            let failed = Mutex::new(std::collections::BTreeSet::new());
            let mark = |r: &RepoName| {
                failed.lock().unwrap_or_else(|p| p.into_inner()).insert(r.clone());
            };
            let seam = |r: &RepoName| self.seams.hit(&format!("new.{phase}"), Some(r)).inspect_err(|_| mark(r));
            let run = |r: &RepoName| f(r).inspect_err(|_| mark(r));
            let out = dispatch::each_repo(repos, &seam, &run);
            if out.is_err() {
                let first = failed.lock().unwrap_or_else(|p| p.into_inner()).iter().next().cloned();
                at.lock().unwrap_or_else(|p| p.into_inner()).1 = first;
            }
            out.map(|_| ())
        };

        // 12. root
        enter("root")?;
        self.seams.hit("new.root", None)?;
        strata_apfs::mkdir_rooted(proot, &rp("sessions"), 0o755, true)?;
        strata_apfs::mkdir_rooted(proot, &rp(format!("sessions/{id}")), 0o755, false)?;
        strata_apfs::mkdir_rooted(proot, &rp(format!("sessions/.repos/{id}")), 0o755, true)?;

        // 13.1-13.4. Each tracked repo's seed caches are cloned by a
        // background worker as soon as that repo's workspace is cloned
        // (D54), overlapping the other repos' clones and the git and pin
        // phases; the phase is entered (and its result taken) after pin.
        let wants_cache = |r: &RepoName| m.repos[r].tracked;
        let (tx, rx) = std::sync::mpsc::channel::<RepoName>();
        let tx = Mutex::new(Some(tx));
        let rx = Mutex::new(rx);
        let cache_errs: Mutex<BTreeMap<RepoName, StrataError>> = Mutex::new(BTreeMap::new());
        let cache_one = |r: &RepoName| -> Result<()> {
            self.seams.hit("new.seed_caches", Some(r))?;
            let cfg = self.repo_cfg(r)?;
            let ws = &m.repos[r].path;
            for c in &plans[r].1.caches {
                if let Some(parent) = ancestors(c).last() {
                    strata_apfs::mkdir_rooted(ws, parent, 0o755, true)?;
                }
                strata_apfs::clone_tree_rooted(&cfg.path.join(c.as_str()), ws, c)?;
            }
            Ok(())
        };
        let steps = std::thread::scope(|sc| {
            for _ in 0..CACHE_WORKERS {
                sc.spawn(|| {
                    loop {
                        let next = rx.lock().unwrap_or_else(|p| p.into_inner()).recv();
                        let Ok(r) = next else { return };
                        // Once one repo's caches failed no further repo starts (as dispatch).
                        if !cache_errs.lock().unwrap_or_else(|p| p.into_inner()).is_empty() {
                            continue;
                        }
                        if let Err(e) = cache_one(&r) {
                            cache_errs.lock().unwrap_or_else(|p| p.into_inner()).insert(r, e);
                        }
                    }
                });
            }
            let steps = (|| -> Result<()> {
                // 13.1 workspace
                enter("workspace")?;
                each("workspace", &repos, &|r| {
                    strata_apfs::clone_tree_rooted(&self.store.base_dir(&plans[r].0), &m.root, &rp(r.as_str()))?;
                    if wants_cache(r)
                        && let Some(tx) = tx.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
                    {
                        let _ = tx.send(r.clone());
                    }
                    Ok(())
                })?;

                // 13.2 git
                enter("git")?;
                each("git", &repos, &|r| {
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
                enter("pin")?;
                each("pin", &repos, &|r| {
                    let git = self.git(r)?;
                    self.with_lock_unbounded(r, || git.update_ref(&pin_ref(id), &m.repos[r].head, None))
                })?;

                // 13.4 seed_caches (tracked repos only): wait for the worker.
                enter("seed_caches")
            })();
            // Closing the channel lets the workers finish what was sent; the
            // scope joins them.
            tx.lock().unwrap_or_else(|p| p.into_inner()).take();
            steps
        });
        steps?;
        if let Some((r, e)) = cache_errs.into_inner().unwrap_or_else(|p| p.into_inner()).into_iter().next() {
            at.lock().unwrap_or_else(|p| p.into_inner()).1 = Some(r);
            return Err(e);
        }

        // 13.5 seed_files
        enter("seed_files")?;
        each("seed_files", &repos, &|r| {
            let records = seed_files(&plans[r].1.files, &m.repos[r].path)?;
            let rows: Vec<SeedRow> = records.iter().map(SeedRow::from).collect();
            self.store.record_seeds(id, r, &rows)
        })?;

        // 14. finalize
        enter("finalize")?;
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

    /// §5.1 step 8 for the session's repos: fetch the default branch and
    /// read the chain top in one critical section, then the tree. Fetches
    /// run `FETCH_WORKERS` wide (D53); a repo fetched successfully within
    /// the fetch window is not fetched again.
    pub(crate) fn resolve_heads(&self, members: &[RepoName]) -> Result<Vec<(RepoName, (CommitOid, TreeOid))>> {
        dispatch::each_repo_n(dispatch::FETCH_WORKERS, members, &|r| self.seams.hit("head", Some(r)), &|r| self.head_of(r))
    }

    fn head_of(&self, repo: &RepoName) -> Result<(CommitOid, TreeOid)> {
        let cfg = self.repo_cfg(repo)?;
        let git = self.git(repo)?;
        let fresh = self.fetch_is_fresh(repo);
        let mut fetched_at = None;
        let head = self.with_lock(repo, || {
            if !fresh {
                let t = std::time::Instant::now();
                git.fetch("origin", &cfg.default_branch)?;
                fetched_at = Some(t);
            }
            strata_deps::main_tip(cfg, git)
        })?;
        if let Some(t) = fetched_at {
            self.fetched.lock().unwrap_or_else(|p| p.into_inner()).insert(repo.clone(), t);
        }
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

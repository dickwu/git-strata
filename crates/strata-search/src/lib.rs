//! git-strata search (search.md): per-session `grep` / `files` views.
//!
//! A view answers for one (session, repo): the shared base index of the
//! workspace's pinned tree (tgrep `Generation`, shared across sessions by
//! (main-checkout identity, tree, profile)) shadowed by this session's
//! overlay. Views live only in memory; nothing here writes the workspace.
//!
//! Locks (§5.2): one table lock (`views`) and one lock per view. Neither is
//! held across `ensure`, and the table lock is never held while reading
//! workspace files.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

mod base;

use serde_json::json;
use tgrep_core::generations::Generation;
use strata_overlay::Overlay;
use strata_session::{SessionState, Sessions, WorkspaceHealth};
use strata_types::{BaseGen, Code, CommitOid, Config, RepoName, RepoPath, SessionId, StrataError, TreeOid, ViewId};

/// types.md §8 `Result<T>`.
pub type Result<T> = std::result::Result<T, StrataError>;

/// Where the latest overlay of a (session, repo) comes from (the daemon's
/// tracker). Search never computes an overlay itself.
pub trait OverlaySource: Send + Sync {
    /// The latest overlay, or None when it has not been scanned yet.
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay>;
    /// True when the source lost events and the view must redo §5.4.
    fn needs_full(&self, session: &SessionId, repo: &RepoName) -> bool;
}

/// `One` queries this view; `All` every attached view, by session then repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Which {
    One(ViewId),
    All,
}

/// Case of the pattern only (D1). Path case is the volume's (§4.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GrepOpts {
    pub case_insensitive: bool,
}

/// One hit: the whole line, `\n` / `\r\n` stripped, lines from 1 (D13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub session: SessionId,
    pub repo: RepoName,
    pub path: RepoPath,
    pub line: u32,
    pub text: String,
}

/// Test seams (增补 4): compiled only with `test-seams`.
#[cfg(feature = "test-seams")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchSeam {
    /// `grep` stops after taking its reader snapshot, before reading any
    /// workspace file, until `release`.
    PauseBeforeVerify,
    /// The next `ensure` fails after a `.stage-*` exists, before publishing.
    FailEnsureAfterStage,
}

pub(crate) fn err(code: Code, msg: impl Into<String>, reason: &str) -> StrataError {
    StrataError::new(code, msg).with_detail(json!({ "reason": reason }))
}

pub(crate) fn no_such_view(id: &ViewId) -> StrataError {
    StrataError::new(Code::NoSuchView, format!("no view {id}")).with_detail(json!({ "view": id.get() }))
}

/// (st_dev, st_ino) of a workspace root, through `open_root` (no symlink
/// at the root itself is followed).
pub(crate) fn root_identity(path: &Path) -> Result<(u64, u64)> {
    let f = strata_apfs::open_root(path)?;
    let m = f
        .metadata()
        .map_err(|e| StrataError::new(Code::Unknown, format!("fstat {}: {e}", path.display())))?;
    Ok((m.dev(), m.ino()))
}

/// One attached view (§4.2).
#[allow(dead_code)] // read by the index code of later tasks
pub(crate) struct View {
    pub(crate) session: SessionId,
    pub(crate) repo: RepoName,
    pub(crate) path: PathBuf,
    pub(crate) root: (u64, u64),
    pub(crate) base: BaseGen,
    #[allow(dead_code)]
    pub(crate) head: CommitOid,
    /// Set once the root or base no longer matches: only a new `attach`
    /// helps; every call on this id answers `Busy` with this reason.
    pub(crate) reattach: Option<&'static str>,
    /// The shared base of `base.tree` (§4.1).
    pub(crate) generation: Arc<Generation>,
}

/// The search handle the daemon holds (§3).
pub struct Search<'a> {
    config: &'a Config,
    sessions: &'a Sessions<'a>,
    overlays: &'a dyn OverlaySource,
    views: Mutex<BTreeMap<ViewId, Arc<Mutex<View>>>>,
    bases: base::Bases,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn rand_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("getrandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn new_view_id() -> ViewId {
    ViewId::new(format!("v-{}", rand_hex(8)))
}

impl<'a> Search<'a> {
    /// No I/O, no views.
    pub fn open(config: &'a Config, sessions: &'a Sessions<'_>, overlays: &'a dyn OverlaySource) -> Search<'a> {
        Search { config, sessions, overlays, views: Mutex::new(BTreeMap::new()), bases: base::Bases::default() }
    }

    fn view(&self, id: &ViewId) -> Result<Arc<Mutex<View>>> {
        lock(&self.views).get(id).cloned().ok_or_else(|| no_such_view(id))
    }

    /// §5.1. Only a fully built view gets an id.
    pub fn attach(&self, session: &SessionId, repo: &RepoName) -> Result<ViewId> {
        // 1–2: the session must be Active.
        let st = self.sessions.status(session)?;
        match st.state {
            SessionState::Active => {}
            SessionState::Removed => {
                return Err(StrataError::new(Code::NoSuchSession, format!("session {session} was removed"))
                    .with_detail(json!({ "session": session.as_str() })));
            }
            SessionState::Creating | SessionState::Removing => {
                return Err(err(Code::Busy, format!("session {session} is not active ({:?})", st.state), "not_active"));
            }
            SessionState::Unknown => {
                let scene = st.scene.as_ref().and_then(|s| serde_json::to_value(s).ok());
                return Err(StrataError::new(Code::Unknown, format!("session {session} is in an unknown state"))
                    .with_detail(json!({ "reason": "session_unknown", "scene": scene })));
            }
        }
        // 3: the repo must be in this session.
        let Some(ws) = st.manifest.repos.get(repo) else {
            return Err(StrataError::new(Code::NoSuchRepo, format!("session {session} has no repo {repo}"))
                .with_detail(json!({ "repo": repo.as_str() })));
        };
        // 4: workspace health.
        if let Some(w) = st.workspaces.get(repo) {
            match w.health {
                WorkspaceHealth::Ok => {}
                // session reports a non-directory at the path as Missing too;
                // open_root (step 7) names that one NotDirectory.
                WorkspaceHealth::Missing if std::fs::symlink_metadata(&ws.path).is_ok() => {}
                WorkspaceHealth::Missing => {
                    return Err(StrataError::new(Code::NoSuchPath, format!("workspace {} is missing", ws.path.display()))
                        .with_detail(json!({ "path": ws.path.to_string_lossy() })));
                }
                WorkspaceHealth::Unknown => {
                    let scene = w.scene.as_ref().and_then(|s| serde_json::to_value(s).ok());
                    return Err(StrataError::new(Code::Unknown, format!("workspace {session}/{repo} is in an unknown state"))
                        .with_detail(json!({ "reason": "workspace_unknown", "scene": scene })));
                }
            }
        }
        // 5–6: the overlay.
        let overlay = match self.overlays.last(session, repo) {
            Some(o) => o,
            None if ws.tracked => {
                return Err(err(Code::Busy, format!("overlay of {session}/{repo} not scanned yet"), "overlay_not_ready"));
            }
            None => Overlay::empty(ws.base.clone()),
        };
        if overlay.base != ws.base {
            return Err(err(Code::Busy, format!("overlay of {session}/{repo} is on another base"), "base_moved"));
        }
        // 7: the root.
        let root = root_identity(&ws.path)?;
        // 8–12: the shared base. No search lock is held across ensure.
        let generation = self.bases.ensure(self.config, repo, &ws.head, &ws.base.tree)?;
        let v = View {
            session: session.clone(),
            repo: repo.clone(),
            path: ws.path.clone(),
            root,
            base: ws.base.clone(),
            head: ws.head.clone(),
            reattach: None,
            generation,
        };
        // 14: issue the id.
        let mut views = lock(&self.views);
        let mut id = new_view_id();
        while views.contains_key(&id) {
            id = new_view_id();
        }
        views.insert(id.clone(), Arc::new(Mutex::new(v)));
        Ok(id)
    }

    /// §5.7. The shared generations stay.
    pub fn detach(&self, view: ViewId) -> Result<()> {
        lock(&self.views).remove(&view).map(|_| ()).ok_or_else(|| no_such_view(&view))
    }

    /// §5.3.
    pub fn refresh(&self, view: ViewId, _changed: &[RepoPath]) -> Result<()> {
        let v = self.view(&view)?;
        let v = lock(&v);
        if let Some(r) = v.reattach {
            return Err(err(Code::Busy, format!("view {view} must be re-attached"), r));
        }
        Ok(())
    }

    /// §5.5.
    pub fn grep(&self, which: Which, pattern: &str, _opts: &GrepOpts) -> Result<Vec<Hit>> {
        check_pattern(pattern)?;
        if let Which::One(id) = &which {
            self.view(id)?;
        }
        Ok(Vec::new())
    }

    /// §5.6.
    pub fn files(&self, view: ViewId, glob: &str) -> Result<Vec<RepoPath>> {
        let _v = self.view(&view)?;
        check_glob(glob, false)?;
        Ok(Vec::new())
    }
}

/// §5.5 step 1: an empty pattern or one tgrep cannot plan is `pattern`.
pub(crate) fn check_pattern(pattern: &str) -> Result<()> {
    if pattern.is_empty() {
        return Err(err(Code::InvalidInput, "empty pattern", "pattern"));
    }
    tgrep_core::query::build_query_plan(pattern, false)
        .map_err(|e| err(Code::InvalidInput, format!("invalid pattern: {e}"), "pattern"))?;
    Ok(())
}

/// §5.6 step 1.
pub(crate) fn check_glob(glob: &str, fold: bool) -> Result<globset::GlobMatcher> {
    if glob.is_empty() {
        return Err(err(Code::InvalidInput, "empty glob", "glob"));
    }
    let g = globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .backslash_escape(false)
        .case_insensitive(fold)
        .build()
        .map_err(|e| err(Code::InvalidInput, format!("invalid glob: {e}"), "glob"))?;
    Ok(g.compile_matcher())
}

#[cfg(feature = "test-seams")]
impl<'a> Search<'a> {
    /// `Arc::as_ptr` of the reader of this (repo, tree)'s generation: one
    /// generation has one reader, so equal pointers are `Arc::ptr_eq` of
    /// the generation too, and comparable with `paused_reader_ptr`.
    pub fn generation_ptr(&self, repo: &RepoName, tree: &TreeOid) -> Option<usize> {
        self.bases.opened(repo, tree.as_str()).map(|g| Arc::as_ptr(g.base().reader()) as usize)
    }

    /// `<root>/search/<repo>/<identity>/`, once the repo has a manager.
    pub fn storage_dir(&self, repo: &RepoName) -> Option<PathBuf> {
        self.bases.existing(repo).map(|r| r.manager.directory().to_path_buf())
    }

    pub fn arm(&self, seam: SearchSeam) {
        match seam {
            SearchSeam::FailEnsureAfterStage => {
                self.bases.fail_after_stage.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            SearchSeam::PauseBeforeVerify => {}
        }
    }

    pub fn release(&self) {}
}

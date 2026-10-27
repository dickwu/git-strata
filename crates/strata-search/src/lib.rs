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
mod view;

use serde_json::json;
use tgrep_core::generations::Generation;
use strata_overlay::Overlay;
use strata_session::{SessionState, Sessions, WorkspaceHealth};
use strata_types::{BaseGen, Code, CommitOid, Config, RepoName, RepoPath, SessionId, StrataError, ViewId};
#[cfg(feature = "test-seams")]
use strata_types::TreeOid;

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
    /// Paths compare folded (the volume is case-insensitive).
    pub(crate) fold: bool,
    pub(crate) excl: view::Excl,
    /// The overlay the private ops were computed from.
    pub(crate) overlay: Overlay,
    /// The private `live` content, replayed onto the reader at publish.
    pub(crate) ops: BTreeMap<String, view::Op>,
    /// The published state; `None` while the view is not ready.
    pub(crate) snap: Option<Arc<view::Snap>>,
}

impl View {
    fn fill(&self) -> view::Fill<'_> {
        view::Fill { root: &self.path, overlay: &self.overlay, generation: &self.generation, excl: &self.excl }
    }

    /// Publish `ops` with the current generation and root identity.
    fn publish(&mut self) {
        let members = self.fill().members();
        let snap = view::Snap::build(&self.generation, &self.ops, members, (&self.session, &self.repo, &self.path, self.root));
        self.snap = Some(Arc::new(snap));
    }

    fn refused(&self) -> Result<()> {
        match self.reattach {
            Some(r) => Err(err(Code::Busy, format!("view of {}/{} must be re-attached", self.session, self.repo), r)),
            None => Ok(()),
        }
    }
}

/// `grep` pause (test seam): armed once, holds the reader pointer seen.
#[cfg(feature = "test-seams")]
#[derive(Default)]
struct Pause {
    state: Mutex<(bool, Option<usize>, bool)>,
    cv: std::sync::Condvar,
}

/// The search handle the daemon holds (§3).
pub struct Search<'a> {
    config: &'a Config,
    sessions: &'a Sessions<'a>,
    overlays: &'a dyn OverlaySource,
    views: Mutex<BTreeMap<ViewId, Arc<Mutex<View>>>>,
    bases: base::Bases,
    #[cfg(feature = "test-seams")]
    pause: Pause,
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

fn root_changed(what: &str) -> StrataError {
    err(Code::Busy, format!("the workspace root of {what} was replaced"), "root_changed")
}

impl<'a> Search<'a> {
    /// No I/O, no views.
    pub fn open(config: &'a Config, sessions: &'a Sessions<'_>, overlays: &'a dyn OverlaySource) -> Search<'a> {
        Search {
            config,
            sessions,
            overlays,
            views: Mutex::new(BTreeMap::new()),
            bases: base::Bases::default(),
            #[cfg(feature = "test-seams")]
            pause: Pause::default(),
        }
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
            return Err(StrataError::not_in_session(session.as_str(), repo.as_str()));
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
        // 13: fill the private live (§5.4). A failure issues no id.
        let fold = !strata_apfs::volume_caps(&ws.path)?.case_sensitive;
        let excl = view::Excl::new(&self.config.repos[repo], fold);
        let mut v = View {
            session: session.clone(),
            repo: repo.clone(),
            path: ws.path.clone(),
            root,
            base: ws.base.clone(),
            head: ws.head.clone(),
            reattach: None,
            generation,
            fold,
            excl,
            overlay,
            ops: BTreeMap::new(),
            snap: None,
        };
        let mut ops = BTreeMap::new();
        let f = v.fill();
        f.update(&mut ops, &f.all_paths())?;
        if root_identity(&v.path).ok() != Some(v.root) {
            return Err(root_changed(&format!("{session}/{repo}")));
        }
        v.ops = ops;
        v.publish();
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
    pub fn refresh(&self, view: ViewId, changed: &[RepoPath]) -> Result<()> {
        let v = self.view(&view)?;
        let mut v = lock(&v);
        self.refresh_locked(&mut v, changed)
    }

    /// §5.3 steps 1–7 with the view lock held (never the table lock).
    fn refresh_locked(&self, v: &mut View, changed: &[RepoPath]) -> Result<()> {
        v.refused()?;
        let what = format!("{}/{}", v.session, v.repo);
        // 2: the workspace and its root still are the attached ones.
        let st = self.sessions.status(&v.session)?;
        let Some(ws) = st.manifest.repos.get(&v.repo) else {
            return Err(StrataError::not_in_session(v.session.as_str(), v.repo.as_str()));
        };
        if ws.path != v.path || root_identity(&v.path).ok() != Some(v.root) {
            v.reattach = Some("root_changed");
            return Err(root_changed(&what));
        }
        if ws.base != v.base {
            v.reattach = Some("base_moved");
            return Err(err(Code::Busy, format!("{what} moved to another base"), "base_moved"));
        }
        // 3: the latest overlay.
        let overlay = match self.overlays.last(&v.session, &v.repo) {
            Some(o) => o,
            None if ws.tracked => {
                return Err(err(Code::Busy, format!("overlay of {what} not scanned yet"), "overlay_not_ready"));
            }
            None => Overlay::empty(ws.base.clone()),
        };
        if overlay.base != v.base {
            v.reattach = Some("base_moved");
            return Err(err(Code::Busy, format!("overlay of {what} is on another base"), "base_moved"));
        }
        // 4–5: what to reread.
        let full = v.snap.is_none() || self.overlays.needs_full(&v.session, &v.repo);
        if !full && changed.is_empty() {
            return Ok(());
        }
        let old = std::mem::replace(&mut v.overlay, overlay);
        let mut ops;
        let paths = if full {
            ops = BTreeMap::new();
            v.fill().all_paths()
        } else {
            ops = v.ops.clone();
            let mut s = std::collections::BTreeSet::new();
            for p in changed {
                s.insert(p.as_str().to_string());
                for q in old.paired(p).into_iter().chain(v.overlay.paired(p)) {
                    s.insert(q.as_str().to_string());
                }
            }
            s
        };
        // 6: read; a failure leaves the view not ready.
        if let Err(e) = v.fill().update(&mut ops, &paths) {
            v.snap = None;
            return Err(e);
        }
        // 7: publish only with the root still the attached one.
        if root_identity(&v.path).ok() != Some(v.root) {
            v.reattach = Some("root_changed");
            return Err(root_changed(&what));
        }
        v.ops = ops;
        v.publish();
        Ok(())
    }

    /// §5.5 step 3: the snapshot of a view, refreshing a not ready one once.
    fn snapshot(&self, v: &Mutex<View>) -> Result<Arc<view::Snap>> {
        let mut g = lock(v);
        g.refused()?;
        if g.snap.is_none() {
            match self.refresh_locked(&mut g, &[]) {
                Ok(()) => {}
                Err(e) if e.code == Code::Busy => return Err(e),
                Err(e) => {
                    return Err(err(Code::Unknown, format!("view of {}/{} is not ready: {}", g.session, g.repo, e.message), "not_ready"));
                }
            }
        }
        g.snap.clone().ok_or_else(|| err(Code::Unknown, format!("view of {}/{} is not ready", g.session, g.repo), "not_ready"))
    }

    /// §5.5 step 7: the root of a finished query is still the snapshot's.
    fn recheck(&self, v: &Mutex<View>, snap: &view::Snap) -> Result<()> {
        if root_identity(&snap.path).ok() == Some(snap.root) {
            return Ok(());
        }
        lock(v).reattach = Some("root_changed");
        Err(root_changed(&format!("{}/{}", snap.session, snap.repo)))
    }

    #[cfg(feature = "test-seams")]
    fn pause_hook(&self) -> impl Fn(usize) + '_ {
        |ptr| {
            let mut s = lock(&self.pause.state);
            if !s.0 {
                return;
            }
            s.0 = false;
            s.1 = Some(ptr);
            while !s.2 {
                s = self.pause.cv.wait(s).unwrap_or_else(|p| p.into_inner());
            }
        }
    }

    #[cfg(not(feature = "test-seams"))]
    fn pause_hook(&self) -> impl Fn(usize) + '_ {
        |_| {}
    }

    /// §5.5.
    pub fn grep(&self, which: Which, pattern: &str, opts: &GrepOpts) -> Result<Vec<Hit>> {
        check_pattern(pattern)?;
        let views: Vec<Arc<Mutex<View>>> = match &which {
            Which::One(id) => vec![self.view(id)?],
            Which::All => lock(&self.views).values().cloned().collect(),
        };
        let mut snaps = Vec::with_capacity(views.len());
        for v in &views {
            snaps.push(self.snapshot(v)?);
        }
        let pause = self.pause_hook();
        let mut hits = Vec::new();
        for s in &snaps {
            hits.extend(s.grep(pattern, opts.case_insensitive, &pause)?);
        }
        for (v, s) in views.iter().zip(&snaps) {
            self.recheck(v, s)?;
        }
        hits.sort_by(|a, b| (&a.session, &a.repo, &a.path, a.line).cmp(&(&b.session, &b.repo, &b.path, b.line)));
        Ok(hits)
    }

    /// §5.6.
    pub fn files(&self, view: ViewId, glob: &str) -> Result<Vec<RepoPath>> {
        let v = self.view(&view)?;
        let fold = lock(&v).fold;
        let m = check_glob(glob, fold)?;
        let snap = self.snapshot(&v)?;
        let out: Vec<RepoPath> = snap
            .members
            .iter()
            .filter(|p| m.is_match(p.as_str()))
            .filter_map(|p| RepoPath::try_from(p.clone()).ok())
            .collect();
        self.recheck(&v, &snap)?;
        Ok(out)
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
            SearchSeam::PauseBeforeVerify => *lock(&self.pause.state) = (true, None, false),
        }
    }

    pub fn release(&self) {
        lock(&self.pause.state).2 = true;
        self.pause.cv.notify_all();
    }

    /// The reader pointer the paused (or last paused) `grep` holds.
    pub fn paused_reader_ptr(&self) -> Option<usize> {
        lock(&self.pause.state).1
    }

    /// Republish the view on another already opened generation of its repo,
    /// leaving its root and base alone (a stand-in for a base swap).
    pub fn swap_view_reader(&self, view: ViewId, tree: &TreeOid) -> Result<()> {
        let v = self.view(&view)?;
        let mut v = lock(&v);
        let g = self
            .bases
            .opened(&v.repo, tree.as_str())
            .ok_or_else(|| err(Code::InvalidInput, format!("no generation of {tree} is open"), "no_generation"))?;
        v.generation = g;
        v.publish();
        Ok(())
    }

    /// Paths whose workspace bytes this view holds privately (an upsert in
    /// its `live`), ascending. Not a §3 seam: see impl-progress/search.md.
    #[doc(hidden)]
    pub fn private_paths(&self, view: ViewId) -> Result<Vec<String>> {
        let v = self.view(&view)?;
        let v = lock(&v);
        Ok(v.ops.iter().filter(|(_, op)| matches!(op, view::Op::Upsert(_))).map(|(p, _)| p.clone()).collect())
    }
}

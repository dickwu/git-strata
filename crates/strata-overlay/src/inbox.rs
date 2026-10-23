//! §5.4 the dirty set: the watcher callback notes events into it in constant
//! time (never touching the disk); `compute` takes a snapshot of it.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use strata_apfs::{FileKind, WatchEvent, attrs_of};
use strata_types::RepoPath;

use crate::context::Fold;
use crate::hash::is_shape_error;
use crate::scan::XSet;

#[derive(Debug)]
struct Pending {
    root: PathBuf,
    capacity: usize,
    fold: Fold,
    /// The X of the latest scan (`None` before it): events under it are dropped.
    x: Option<XSet>,
    /// Changed paths by their volume-fold key (one item per canonical path).
    paths: BTreeMap<String, RepoPath>,
    /// Subtrees to walk again, by key.
    subtrees: BTreeMap<String, RepoPath>,
    full: bool,
}

impl Pending {
    fn insert(&mut self, sub: bool, p: RepoPath) {
        if self.full {
            return;
        }
        let k = self.fold.key(p.as_str());
        let set = if sub {
            &mut self.subtrees
        } else {
            &mut self.paths
        };
        set.entry(k).or_insert(p);
        if self.paths.len() + self.subtrees.len() > self.capacity {
            self.require_full();
        }
    }

    /// A path known to be written (hint): dropped under X, `.gitignore` asks
    /// for a full scan (the same rules as `note`).
    fn add(&mut self, p: RepoPath) {
        if self.x.as_ref().is_some_and(|x| x.covers(p.as_str())) {
            return;
        }
        if p.as_str().rsplit('/').next() == Some(".gitignore") {
            self.require_full();
        } else {
            self.insert(false, p);
        }
    }

    fn require_full(&mut self) {
        self.paths.clear();
        self.subtrees.clear();
        self.full = true;
    }

    /// `abs` as a repository path: `Ok(None)` to drop it (outside the root
    /// or under X), `Err(())` when only a full scan can account for it.
    fn rel(&self, abs: &Path) -> Result<Option<RepoPath>, ()> {
        let Ok(rest) = abs.strip_prefix(&self.root) else {
            return Ok(None);
        };
        let mut parts: Vec<&str> = Vec::new();
        for c in rest.components() {
            match c {
                Component::CurDir => {}
                Component::Normal(n) => parts.push(n.to_str().ok_or(())?),
                _ => return Err(()),
            }
        }
        if parts.is_empty() {
            return Err(());
        }
        let p = RepoPath::try_from(parts.join("/")).map_err(|_| ())?;
        if self.x.as_ref().is_some_and(|x| x.covers(p.as_str())) {
            return Ok(None);
        }
        Ok(Some(p))
    }
}

/// What `compute` took out of the inbox (merged back when it fails).
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub paths: Vec<RepoPath>,
    pub subtrees: Vec<RepoPath>,
    pub full: bool,
}

/// The tracker's dirty set, shared with the watcher callback (§3).
#[derive(Clone, Debug)]
pub struct Inbox {
    inner: Arc<Mutex<Pending>>,
}

/// What the inbox holds right now (test seam).
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InboxView {
    pub paths: Vec<RepoPath>,
    pub subtrees: Vec<RepoPath>,
    pub full: bool,
}

impl Inbox {
    pub(crate) fn new(root: &Path, capacity: usize, fold: Fold) -> Inbox {
        Inbox {
            inner: Arc::new(Mutex::new(Pending {
                root: root.to_path_buf(),
                capacity,
                fold,
                x: None,
                paths: BTreeMap::new(),
                subtrees: BTreeMap::new(),
                full: false,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record one watcher event (constant time per path; never touches the disk).
    pub fn note(&self, ev: &WatchEvent) {
        let mut g = self.lock();
        if g.full {
            return;
        }
        match ev {
            WatchEvent::Changed(paths) => {
                for abs in paths {
                    match g.rel(abs) {
                        Ok(None) => {}
                        Ok(Some(p)) if p.as_str().rsplit('/').next() == Some(".gitignore") => {
                            g.require_full();
                        }
                        Ok(Some(p)) => g.insert(false, p),
                        Err(()) => g.require_full(),
                    }
                    if g.full {
                        return;
                    }
                }
            }
            WatchEvent::MustRescan(dir) => match g.rel(dir) {
                Ok(None) => {}
                Ok(Some(p)) => g.insert(true, p),
                Err(()) => g.require_full(),
            },
            WatchEvent::Dropped => g.require_full(),
        }
    }

    /// Record paths known to be written (§5.4 hint): each `p`, and where a
    /// prefix of `p` is a symlink, the path it leads to (one hop only; what
    /// cannot be resolved that way asks for a full scan). Reads attributes:
    /// call it on the daemon's thread, never from the watcher callback.
    pub fn hint(&self, paths: &[RepoPath]) {
        let root = self.lock().root.clone();
        for p in paths {
            let via = resolve_hint(&root, p);
            let mut g = self.lock();
            if g.full {
                return;
            }
            g.add(p.clone());
            match via {
                Ok(Some(q)) => g.add(q),
                Ok(None) => {}
                Err(()) => g.require_full(),
            }
        }
    }

    /// The next scan walks the whole tree (a tool call without a path list).
    pub fn require_full(&self) {
        self.lock().require_full();
    }

    pub(crate) fn needs_full(&self) -> bool {
        self.lock().full
    }

    /// Give the inbox the X (and fold) of the scan about to run.
    pub(crate) fn set_x(&self, x: &XSet) {
        let mut g = self.lock();
        if g.x.as_ref() != Some(x) {
            let fold = x.fold();
            if fold != g.fold {
                g.fold = fold;
                let paths = std::mem::take(&mut g.paths);
                let subs = std::mem::take(&mut g.subtrees);
                for p in paths.into_values() {
                    g.insert(false, p);
                }
                for p in subs.into_values() {
                    g.insert(true, p);
                }
            }
            g.x = Some(x.clone());
        }
    }

    /// Swap the contents out under the lock.
    pub(crate) fn take(&self) -> Snapshot {
        let mut g = self.lock();
        let s = Snapshot {
            paths: std::mem::take(&mut g.paths).into_values().collect(),
            subtrees: std::mem::take(&mut g.subtrees).into_values().collect(),
            full: g.full,
        };
        g.full = false;
        s
    }

    /// Put a snapshot back after a failed scan.
    pub(crate) fn merge(&self, s: Snapshot) {
        let mut g = self.lock();
        if s.full {
            g.require_full();
            return;
        }
        for p in s.paths {
            g.insert(false, p);
        }
        for p in s.subtrees {
            g.insert(true, p);
        }
    }

    /// Empty the set and ask for a full scan (`Tracker::reset`).
    pub(crate) fn clear_full(&self) {
        let mut g = self.lock();
        g.x = None;
        g.require_full();
    }

    #[cfg(feature = "test-seams")]
    pub fn pending(&self) -> InboxView {
        let g = self.lock();
        InboxView {
            paths: g.paths.values().cloned().collect(),
            subtrees: g.subtrees.values().cloned().collect(),
            full: g.full,
        }
    }
}

/// Where a write to `p` lands when a prefix of `p` is a symlink (§5.4 hint):
/// `Ok(None)` = no symlink on the way (or a component is gone: `p` alone is
/// enough), `Ok(Some(q))` = the one-hop resolution, `Err(())` = only a full
/// scan can tell (absolute target, `..` after another component or past the
/// root, a second symlink, attributes unreadable).
fn resolve_hint(root: &Path, p: &RepoPath) -> Result<Option<RepoPath>, ()> {
    let parts: Vec<&str> = p.as_str().split('/').collect();
    for i in 0..parts.len() {
        let prefix = RepoPath::try_from(parts[..=i].join("/")).map_err(|_| ())?;
        let a = match attrs_of(root, &prefix) {
            Ok(a) => a,
            Err(e) if is_shape_error(&e) => return Ok(None),
            Err(_) => return Err(()),
        };
        match a.kind {
            FileKind::Dir => continue,
            FileKind::Symlink { target } => {
                let q = join_target(&parts[..i], &target, &parts[i + 1..])?;
                no_symlink_on(root, &q)?;
                return Ok(Some(q));
            }
            // A file (or special file) above the last component: `p` cannot exist.
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// `parent` + `target` + `rest`, `.` and `..` folded literally; `..` only at
/// the start of the target, cancelling components already seen as directories.
fn join_target(parent: &[&str], target: &Path, rest: &[&str]) -> Result<RepoPath, ()> {
    let t = target.to_str().ok_or(())?;
    if t.starts_with('/') {
        return Err(());
    }
    let mut out: Vec<&str> = parent.to_vec();
    let mut leading = true;
    for c in t.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if !leading || out.pop().is_none() {
                    return Err(());
                }
            }
            n => {
                leading = false;
                out.push(n);
            }
        }
    }
    out.extend_from_slice(rest);
    if out.is_empty() {
        return Err(());
    }
    RepoPath::try_from(out.join("/")).map_err(|_| ())
}

/// Walking `q` from the root passes no symlink (a missing component ends the walk).
fn no_symlink_on(root: &Path, q: &RepoPath) -> Result<(), ()> {
    let parts: Vec<&str> = q.as_str().split('/').collect();
    for i in 0..parts.len() {
        let prefix = RepoPath::try_from(parts[..=i].join("/")).map_err(|_| ())?;
        match attrs_of(root, &prefix) {
            Ok(a) if matches!(a.kind, FileKind::Symlink { .. }) => return Err(()),
            Ok(a) if a.kind == FileKind::Dir => {}
            Ok(_) => return Ok(()),
            Err(e) if is_shape_error(&e) => return Ok(()),
            Err(_) => return Err(()),
        }
    }
    Ok(())
}

//! §4.1 `Tracker`: the in-process state between scans (attribute cache, raw
//! difference, last overlay, index, X, warnings). Never persisted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use strata_types::{BaseGen, RepoPath};

use strata_apfs::{WatchOptions, Watcher, watch_with};

use crate::classify::Raw;
use crate::context::Fold;
use crate::hash::Cached;
use crate::inbox::Inbox;
use crate::overlay::Overlay;
use crate::scan::{Keys, XSet};
use crate::{OverlayWarning, Result};

/// Everything a scan replaces at once (compute works on a copy and commits
/// it only on success, §5.5 step 7).
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    pub cache: BTreeMap<RepoPath, Cached>,
    pub raw: BTreeMap<RepoPath, Raw>,
    pub last: Option<Overlay>,
    pub base: Option<BaseGen>,
    pub tracked: Option<BTreeSet<RepoPath>>,
    pub ignorecase: Option<bool>,
    /// The volume's case sensitivity, read on the first scan.
    pub volume_cs: Option<bool>,
    pub keys: Option<Keys>,
    pub x: Option<XSet>,
    pub unstable: BTreeSet<RepoPath>,
    /// Warnings by the path they are about.
    pub warn: BTreeMap<RepoPath, Vec<OverlayWarning>>,
    pub warnings: Vec<OverlayWarning>,
    /// A scan has succeeded since `new` or `reset`.
    pub scanned: bool,
}

impl State {
    pub fn flatten_warnings(&mut self) {
        self.warnings = self.warn.values().flatten().cloned().collect();
    }
}

/// The daemon's per-workspace scan state (held under its own mutex).
pub struct Tracker {
    pub(crate) root: PathBuf,
    pub(crate) st: State,
    pub(crate) inbox: Inbox,
    watcher: Option<Watcher>,
}

impl Tracker {
    /// Empty cache, `needs_full()` true: the first scan is cold.
    pub fn new(root: &Path, capacity: usize) -> Tracker {
        Tracker {
            root: root.to_path_buf(),
            st: State::default(),
            inbox: Inbox::new(root, capacity, Fold::of(root)),
            watcher: None,
        }
    }

    /// The dirty set the watcher callback (or the daemon) feeds.
    pub fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }

    /// Start (or restart) watching the root; events go to `inbox()`. Only
    /// `.git` is excluded at the source, X is filtered in `Inbox::note`.
    pub fn watch(&mut self) -> Result<()> {
        self.watcher = None;
        let inbox = self.inbox.clone();
        let opts = WatchOptions {
            exclude: vec![RepoPath::try_from(".git".to_owned()).expect("valid path")],
        };
        self.watcher = Some(watch_with(&self.root, &opts, move |ev| inbox.note(&ev))?);
        Ok(())
    }

    /// The base of the latest scan; `None` before the first one.
    pub fn base(&self) -> Option<&BaseGen> {
        self.st.base.as_ref()
    }

    /// The overlay of the latest scan.
    pub fn last(&self) -> Option<&Overlay> {
        self.st.last.as_ref()
    }

    /// The current warnings (status reads them).
    pub fn warnings(&self) -> &[OverlayWarning] {
        &self.st.warnings
    }

    /// The next scan must walk the whole tree.
    pub fn needs_full(&self) -> bool {
        !self.st.scanned || self.inbox.needs_full()
    }

    /// After the workspace became a new tree: drop cache, raw difference and
    /// the watcher; the next scan is cold.
    pub fn reset(&mut self) {
        self.watcher = None;
        self.st = State::default();
        self.inbox.clear_full();
    }
}

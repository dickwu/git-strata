//! §4.1 `Tracker`: the in-process state between scans (attribute cache, raw
//! difference, last overlay, index, X, warnings). Never persisted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use strata_types::{BaseGen, RepoPath};

use crate::OverlayWarning;
use crate::classify::Raw;
use crate::hash::Cached;
use crate::overlay::Overlay;
use crate::scan::{Keys, XSet};

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
    #[allow(dead_code)] // the dirty-set bound (Task 6)
    pub(crate) capacity: usize,
    pub(crate) st: State,
}

impl Tracker {
    /// Empty cache, `needs_full()` true: the first scan is cold.
    pub fn new(root: &Path, capacity: usize) -> Tracker {
        Tracker {
            root: root.to_path_buf(),
            capacity,
            st: State::default(),
        }
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
        !self.st.scanned
    }

    /// After the workspace became a new tree: drop cache and raw difference;
    /// the next scan is cold.
    pub fn reset(&mut self) {
        self.st = State::default();
    }
}

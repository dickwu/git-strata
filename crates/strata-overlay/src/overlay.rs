//! `Overlay` and its shape rules (§4.3 V1–V4) and grouping (§5.3).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_types::{
    BaseGen, BaseManifest, BlobId, Checkpoint, Code, EntryKind, OverlayEntry, RepoPath, StrataError,
};

use crate::Result;

pub(crate) const MODE_FILE: u32 = 0o100644;
pub(crate) const MODE_EXEC: u32 = 0o100755;
pub(crate) const MODE_LINK: u32 = 0o120000;

/// All whole-file differences of a workspace against one base (types.md §2.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlay {
    pub base: BaseGen,
    /// Ascending by path (bytewise), satisfying §4.3 V1–V5.
    pub entries: Vec<OverlayEntry>,
}

/// The proper ancestors of `p` ("a", "a/b" for "a/b/c"), shallow first.
pub(crate) fn ancestors(p: &str) -> impl Iterator<Item = &str> {
    p.match_indices('/').map(move |(i, _)| &p[..i])
}

pub(crate) fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).expect("internal path is a valid RepoPath")
}

/// The content an entry puts at its path (`None` for `Deleted`).
pub(crate) fn entry_blob(e: &OverlayEntry) -> Option<&BlobId> {
    match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Untracked { blob }
        | EntryKind::Renamed { blob, .. } => Some(blob),
        EntryKind::Deleted => None,
    }
}

pub(crate) fn entry_from(e: &OverlayEntry) -> Option<&RepoPath> {
    match &e.kind {
        EntryKind::Renamed { from, .. } => Some(from),
        _ => None,
    }
}

fn shape(rule: &str, path: &RepoPath, msg: &str) -> StrataError {
    StrataError::new(
        Code::InvalidInput,
        format!("overlay violates {rule}: {msg} ({})", path.as_str()),
    )
    .with_detail(json!({ "rule": rule, "path": path.as_str() }))
}

impl Overlay {
    pub fn empty(base: BaseGen) -> Overlay {
        Overlay {
            base,
            entries: Vec::new(),
        }
    }

    pub fn from_checkpoint(cp: &Checkpoint) -> Overlay {
        Overlay {
            base: cp.base.clone(),
            entries: cp.entries.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, path: &RepoPath) -> Option<&OverlayEntry> {
        match self.entries.binary_search_by(|e| e.path.cmp(path)) {
            Ok(i) => Some(&self.entries[i]),
            Err(_) => self.entries.iter().find(|e| &e.path == path),
        }
    }

    pub fn hides(&self, path: &RepoPath) -> bool {
        self.get(path).is_some() || self.entries.iter().any(|e| entry_from(e) == Some(path))
    }

    pub fn tombstones(&self) -> Vec<RepoPath> {
        let mut v: Vec<RepoPath> = self
            .entries
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Deleted => Some(e.path.clone()),
                EntryKind::Renamed { from, .. } => Some(from.clone()),
                _ => None,
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn paired(&self, path: &RepoPath) -> Vec<RepoPath> {
        groups(self.entries.iter()).remove(path).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<()> {
        for w in self.entries.windows(2) {
            if w[0].path >= w[1].path {
                return Err(shape(
                    "V1",
                    &w[1].path,
                    "entries not strictly ascending or duplicated",
                ));
            }
        }
        for e in &self.entries {
            let ok = match e.kind {
                EntryKind::Deleted => e.mode == 0,
                _ => matches!(e.mode, MODE_FILE | MODE_EXEC | MODE_LINK),
            };
            if !ok {
                return Err(shape(
                    "V2",
                    &e.path,
                    &format!("mode {:o} not allowed for this kind", e.mode),
                ));
            }
        }
        let paths: BTreeSet<&RepoPath> = self.entries.iter().map(|e| &e.path).collect();
        let mut froms = BTreeSet::new();
        for e in &self.entries {
            if let Some(from) = entry_from(e) {
                if from == &e.path {
                    return Err(shape("V3", &e.path, "rename from itself"));
                }
                if paths.contains(from) {
                    return Err(shape(
                        "V3",
                        &e.path,
                        "rename source is another entry's path",
                    ));
                }
                if !froms.insert(from) {
                    return Err(shape("V3", &e.path, "rename source used twice"));
                }
            }
        }
        let live: BTreeSet<&str> = self
            .entries
            .iter()
            .filter(|e| !matches!(e.kind, EntryKind::Deleted))
            .map(|e| e.path.as_str())
            .collect();
        for p in &live {
            if let Some(a) = ancestors(p).find(|a| live.contains(a)) {
                return Err(shape(
                    "V4",
                    &rp(p),
                    &format!("{a} is both a file and a directory"),
                ));
            }
        }
        Ok(())
    }
}

impl Overlay {
    /// §4.3 V5 against base manifest `m` (rebase and restore check it).
    pub(crate) fn check_base(&self, m: &BaseManifest) -> Result<()> {
        for e in &self.entries {
            let at = m.entries.get(&e.path);
            let ok = match &e.kind {
                EntryKind::Modified { .. } | EntryKind::Deleted => at.is_some(),
                EntryKind::Added { .. } | EntryKind::Untracked { .. } => at.is_none(),
                EntryKind::Renamed { from, .. } => {
                    if !m.entries.contains_key(from) {
                        return Err(shape("V5", &e.path, "rename source not in the base"));
                    }
                    true
                }
            };
            if !ok {
                return Err(shape("V5", &e.path, "kind does not match the base"));
            }
            if let (Some(b), Some(blob)) = (at, entry_blob(e))
                && b.blob == *blob
                && b.mode == e.mode
            {
                return Err(shape("V5", &e.path, "entry equals the base"));
            }
        }
        Ok(())
    }
}

/// Union-find groups (§5.3 "成组") over entry paths and rename sources:
/// a rename's two halves, and paths where one is a directory prefix of
/// another. Every member maps to its whole sorted group.
pub(crate) fn groups<'a>(
    entries: impl Iterator<Item = &'a OverlayEntry>,
) -> BTreeMap<RepoPath, Vec<RepoPath>> {
    let mut idx: BTreeMap<RepoPath, usize> = BTreeMap::new();
    let mut parent: Vec<usize> = Vec::new();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let add =
        |idx: &mut BTreeMap<RepoPath, usize>, parent: &mut Vec<usize>, k: &RepoPath| -> usize {
            *idx.entry(k.clone()).or_insert_with(|| {
                parent.push(parent.len());
                parent.len() - 1
            })
        };
    let mut pairs = Vec::new();
    for e in entries {
        let a = add(&mut idx, &mut parent, &e.path);
        if let Some(f) = entry_from(e) {
            let b = add(&mut idx, &mut parent, f);
            pairs.push((a, b));
        }
    }
    for (k, &i) in &idx {
        for a in ancestors(k.as_str()) {
            if let Some(&j) = idx.get(&rp(a)) {
                pairs.push((i, j));
            }
        }
    }
    for (a, b) in pairs {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut by_root: BTreeMap<usize, Vec<RepoPath>> = BTreeMap::new();
    for (k, &i) in &idx {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(k.clone());
    }
    let mut out = BTreeMap::new();
    for g in by_root.into_values() {
        for k in &g {
            out.insert(k.clone(), g.clone());
        }
    }
    out
}

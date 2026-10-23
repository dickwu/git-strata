//! §5.1 classification: the raw difference against the base manifest, and the
//! entries it determines (a pure function of the raw difference).

use std::collections::{BTreeMap, BTreeSet};

use strata_types::{BaseGen, BaseManifest, BlobId, EntryKind, OverlayEntry, RepoPath};

use crate::overlay::Overlay;

/// The present state of one path that differs from the base (§4.1 `raw`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Raw {
    /// In the base, not on disk (or occupied by a special file).
    Absent,
    /// On disk with content or mode other than the base's (or not in it);
    /// `staged` = the path is in the index.
    Present {
        blob: BlobId,
        mode: u32,
        staged: bool,
    },
}

/// The raw-difference value of a read path, or `None` when it equals the base.
pub(crate) fn present(
    m: &BaseManifest,
    path: &RepoPath,
    blob: BlobId,
    mode: u32,
    staged: bool,
) -> Option<Raw> {
    match m.entries.get(path) {
        Some(e) if e.blob == blob && e.mode == mode => None,
        _ => Some(Raw::Present { blob, mode, staged }),
    }
}

/// How names compare for case-only pairing (§5.3 step 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Folds {
    pub volume_case_sensitive: bool,
    pub ignorecase: bool,
}

/// Entries of the raw difference: Present & in M → Modified, Present & not in
/// M → Added (staged) or Untracked, Absent → Deleted; then §5.3 pairing.
pub(crate) fn classify(
    base: &BaseGen,
    m: &BaseManifest,
    raw: &BTreeMap<RepoPath, Raw>,
    folds: Folds,
) -> Overlay {
    // §5.3 pairing arrives with Task 5 (rename.rs).
    let _ = folds;
    let renames: BTreeMap<RepoPath, RepoPath> = BTreeMap::new();
    let froms: BTreeSet<&RepoPath> = renames.values().collect();
    let mut entries = Vec::with_capacity(raw.len());
    for (p, r) in raw {
        match r {
            Raw::Absent => {
                if !froms.contains(p) {
                    entries.push(OverlayEntry {
                        path: p.clone(),
                        kind: EntryKind::Deleted,
                        mode: 0,
                    });
                }
            }
            Raw::Present { blob, mode, staged } => {
                let kind = if let Some(from) = renames.get(p) {
                    EntryKind::Renamed {
                        from: from.clone(),
                        blob: blob.clone(),
                    }
                } else if m.entries.contains_key(p) {
                    EntryKind::Modified { blob: blob.clone() }
                } else if *staged {
                    EntryKind::Added { blob: blob.clone() }
                } else {
                    EntryKind::Untracked { blob: blob.clone() }
                };
                entries.push(OverlayEntry {
                    path: p.clone(),
                    kind,
                    mode: *mode,
                });
            }
        }
    }
    Overlay {
        base: base.clone(),
        entries,
    }
}

/// Paths whose entry differs between `old` and `new` (Renamed `from` included).
pub(crate) fn changed(old: Option<&Overlay>, new: &Overlay) -> Vec<RepoPath> {
    let index = |o: &Overlay| -> BTreeMap<RepoPath, OverlayEntry> {
        o.entries
            .iter()
            .map(|e| (e.path.clone(), e.clone()))
            .collect()
    };
    let a = old.map(index).unwrap_or_default();
    let b = index(new);
    let mut out = BTreeSet::new();
    let mut note = |e: &OverlayEntry| {
        out.insert(e.path.clone());
        if let EntryKind::Renamed { from, .. } = &e.kind {
            out.insert(from.clone());
        }
    };
    for (p, e) in &a {
        if b.get(p) != Some(e) {
            note(e);
        }
    }
    for (p, e) in &b {
        if a.get(p) != Some(e) {
            note(e);
        }
    }
    out.into_iter().collect()
}

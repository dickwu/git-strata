//! §5.3 rename pairing: deterministic, a function of the final raw difference
//! only (never of operation or event order).

use std::collections::{BTreeMap, BTreeSet};

use strata_apfs::case_fold_key;
use strata_types::{BaseManifest, BlobId, RepoPath};

use crate::classify::{Folds, Raw};
use crate::overlay::{MODE_EXEC, MODE_FILE, MODE_LINK};

/// The pairs `n → v` (new path → the base path it was renamed from).
///
/// 1. Case-only: for each v ascending, the smallest unpaired n not in M whose
///    key equals v's under the volume fold or the git fold.
/// 2. Exact content: for each remaining v ascending, the smallest unpaired n
///    (in M or not) with blob = M[v].blob and the same type (file/symlink).
///    A pair where one path is a directory prefix of the other is a type swap
///    (§5.3 "类型互换"), never a rename, so it is skipped.
pub(crate) fn pair(
    m: &BaseManifest,
    raw: &BTreeMap<RepoPath, Raw>,
    folds: Folds,
) -> BTreeMap<RepoPath, RepoPath> {
    let vs: Vec<&RepoPath> = raw
        .iter()
        .filter(|(p, r)| matches!(r, Raw::Absent) && m.entries.contains_key(*p))
        .map(|(p, _)| p)
        .collect();
    let mut out: BTreeMap<RepoPath, RepoPath> = BTreeMap::new();
    if vs.is_empty() {
        return out;
    }
    let mut taken: BTreeSet<&RepoPath> = BTreeSet::new();
    let mut paired_v: BTreeSet<&RepoPath> = BTreeSet::new();

    // Step 1: case-only, under either fold. Each N is grouped once per fold
    // (once when the folds coincide), never compared pairwise.
    // Both folds that fold use the same key (`case_fold_key`); a byte fold never
    // pairs here (v is in M and n is not, so their bytes differ).
    if !folds.volume_case_sensitive || folds.ignorecase {
        let mut groups: BTreeMap<String, BTreeSet<&RepoPath>> = BTreeMap::new();
        for (p, r) in raw {
            if matches!(r, Raw::Present { .. }) && !m.entries.contains_key(p) {
                groups
                    .entry(case_fold_key(p.as_str()))
                    .or_default()
                    .insert(p);
            }
        }
        for v in &vs {
            let Some(g) = groups.get(&case_fold_key(v.as_str())) else {
                continue;
            };
            if let Some(n) = g.iter().find(|n| !taken.contains(*n)) {
                taken.insert(n);
                paired_v.insert(v);
                out.insert((*n).clone(), (*v).clone());
            }
        }
    }

    // Step 2: exact content and type.
    let mut by_content: BTreeMap<(&BlobId, bool), BTreeSet<&RepoPath>> = BTreeMap::new();
    for (p, r) in raw {
        if let Raw::Present { blob, mode, .. } = r
            && !taken.contains(p)
        {
            by_content
                .entry((blob, *mode == MODE_LINK))
                .or_default()
                .insert(p);
        }
    }
    for v in &vs {
        if paired_v.contains(v) {
            continue;
        }
        let e = &m.entries[*v];
        let link = match e.mode {
            MODE_LINK => true,
            MODE_FILE | MODE_EXEC => false,
            _ => continue,
        };
        let Some(g) = by_content.get(&(&e.blob, link)) else {
            continue;
        };
        let found = g
            .iter()
            .find(|n| !taken.contains(*n) && !prefix_related(n.as_str(), v.as_str()));
        if let Some(n) = found {
            taken.insert(n);
            out.insert((*n).clone(), (*v).clone());
        }
    }
    out
}

/// One path is a directory prefix of the other.
fn prefix_related(a: &str, b: &str) -> bool {
    let under =
        |x: &str, d: &str| x.len() > d.len() && x.starts_with(d) && x.as_bytes()[d.len()] == b'/';
    under(a, b) || under(b, a)
}

#[cfg(test)]
mod tests {
    use super::prefix_related;

    #[test]
    fn prefix_relation_is_by_component() {
        assert!(prefix_related("d", "d/f"));
        assert!(prefix_related("d/f", "d"));
        assert!(!prefix_related("d", "dx/f"));
        assert!(!prefix_related("d", "d"));
    }
}

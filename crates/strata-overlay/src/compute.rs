//! §5.5 `compute` and `reconcile_full`.

use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;
use strata_types::{Code, RepoPath, StrataError};

use crate::classify::{Folds, Raw, changed, classify, present};
use crate::context::{Base, Fold, WorkspaceRef};
use crate::hash::{Cached, Got, read_path, special_bits};
use crate::overlay::Overlay;
use crate::scan::{Ctx, Keys, XSet, walk_tree};
use crate::tracker::{State, Tracker};
use crate::{DEFAULT_DIRTY_CAPACITY, OverlayWarning, Result, ScanStats};

/// What `compute` returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan {
    pub overlay: Overlay,
    /// Paths whose entry differs from `tracker.last()` (Renamed `from` included).
    pub changed: Vec<RepoPath>,
    pub warnings: Vec<OverlayWarning>,
    pub stats: ScanStats,
}

fn push_warn(w: &mut BTreeMap<RepoPath, Vec<OverlayWarning>>, p: &RepoPath, x: OverlayWarning) {
    let v = w.entry(p.clone()).or_default();
    if !v.contains(&x) {
        v.push(x);
    }
}

/// The scan facts every compute needs (§5.5 step 2).
pub(crate) struct Frame {
    pub info: strata_gitio::IndexInfo,
    pub keys: Keys,
    pub x: XSet,
    pub volume_cs: bool,
}

pub(crate) fn frame(ws: &WorkspaceRef, base: &Base, st: &State) -> Result<Frame> {
    let info = ws.git.read_index(ws.root)?;
    let volume_cs = match st.volume_cs {
        Some(v) => v,
        None => strata_apfs::volume_caps(ws.root)?.case_sensitive,
    };
    let keys = Keys::build(
        Fold {
            case_sensitive: volume_cs,
        },
        &base.manifest,
        &info.tracked,
    );
    let x = XSet::new(ws.config, &keys);
    Ok(Frame {
        info,
        keys,
        x,
        volume_cs,
    })
}

/// §5.5 step 4, full: walk the whole tree, read every candidate (the cache
/// may vouch) and rebuild the raw difference, cache, warnings and `unstable`.
fn full_scan(
    ws: &WorkspaceRef,
    base: &Base,
    st: &mut State,
    f: &Frame,
    stats: &mut ScanStats,
) -> Result<()> {
    let ctx = Ctx {
        root: ws.root,
        git: ws.git,
        keys: &f.keys,
        x: &f.x,
    };
    let found = walk_tree(&ctx, None)?;
    stats.dirs_listed += found.dirs;
    stats.entries_seen += found.seen;
    let old = &st.cache;
    let reads: Vec<(RepoPath, Result<Got>)> = found
        .files
        .into_par_iter()
        .map(|(p, a)| {
            let got = read_path(ws.root, &p, a, old.get(&p));
            (p, got)
        })
        .collect();
    let m = &base.manifest;
    let mut cache = BTreeMap::new();
    let mut raw = BTreeMap::new();
    let mut warn = BTreeMap::new();
    let mut unstable = BTreeSet::new();
    let mut on_disk = BTreeSet::new();
    for (p, got) in reads {
        match got? {
            Got::File(o) => {
                stats.files_hashed += o.files_hashed;
                stats.bytes_hashed += o.hashed;
                if o.vouched {
                    stats.vouched += 1;
                }
                if let Some(a) = &o.cache {
                    cache.insert(
                        p.clone(),
                        Cached {
                            attrs: a.clone(),
                            blob: o.blob.clone(),
                        },
                    );
                }
                if o.unstable {
                    unstable.insert(p.clone());
                    push_warn(&mut warn, &p, OverlayWarning::Unstable { path: p.clone() });
                }
                if let Some(mode) = special_bits(&o.attrs) {
                    push_warn(
                        &mut warn,
                        &p,
                        OverlayWarning::SpecialModeBits {
                            path: p.clone(),
                            mode,
                        },
                    );
                }
                if let Some(r) = present(m, &p, o.blob.clone(), o.mode, f.info.is_tracked(&p)) {
                    raw.insert(p.clone(), r);
                }
                on_disk.insert(p);
            }
            // Gone between the walk and the read: absent now, looked at again next time.
            Got::Gone => {
                unstable.insert(p);
            }
            Got::Special => {
                push_warn(
                    &mut warn,
                    &p,
                    OverlayWarning::SpecialFile { path: p.clone() },
                );
            }
        }
    }
    for p in found.special {
        push_warn(
            &mut warn,
            &p,
            OverlayWarning::SpecialFile { path: p.clone() },
        );
    }
    for d in found.embedded {
        push_warn(
            &mut warn,
            &d,
            OverlayWarning::EmbeddedRepo { path: d.clone() },
        );
    }
    for p in m.entries.keys() {
        if !on_disk.contains(p) && !f.x.covers(p.as_str()) {
            raw.insert(p.clone(), Raw::Absent);
        }
    }
    st.cache = cache;
    st.raw = raw;
    st.warn = warn;
    st.unstable = unstable;
    Ok(())
}

/// §5.5: the overlay of the workspace against `base`. Never writes the Store
/// or the workspace; on failure `tracker` is unchanged.
pub fn compute(
    ws: &WorkspaceRef,
    base: &Base,
    tracker: &mut Tracker,
    dirty: Option<&[RepoPath]>,
) -> Result<Scan> {
    // Task 4: every scan is full; the dirty set arrives with Task 6.
    let _ = dirty;
    if base.r#gen.repo != *ws.repo {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!(
                "base belongs to repo {}, workspace is {}",
                base.r#gen.repo.as_str(),
                ws.repo.as_str()
            ),
        )
        .with_detail(serde_json::json!({ "reason": "wrong_repo" })));
    }
    if tracker.root != ws.root {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!(
                "tracker is for {}, workspace is {}",
                tracker.root.display(),
                ws.root.display()
            ),
        )
        .with_detail(serde_json::json!({ "reason": "wrong_tracker" })));
    }
    let mut st = tracker.st.clone();
    let mut stats = ScanStats {
        full: true,
        cold: st.cache.is_empty() && !st.scanned,
        ..Default::default()
    };
    let f = frame(ws, base, &st)?;
    full_scan(ws, base, &mut st, &f, &mut stats)?;
    let overlay = classify(
        &base.r#gen,
        &base.manifest,
        &st.raw,
        Folds {
            volume_case_sensitive: f.volume_cs,
            ignorecase: f.info.ignorecase,
        },
    );
    let changed = changed(st.last.as_ref(), &overlay);
    st.last = Some(overlay.clone());
    st.base = Some(base.r#gen.clone());
    st.ignorecase = Some(f.info.ignorecase);
    st.volume_cs = Some(f.volume_cs);
    st.tracked = Some(f.info.tracked.clone());
    st.keys = Some(f.keys);
    st.x = Some(f.x);
    st.scanned = true;
    st.flatten_warnings();
    tracker.st = st;
    Ok(Scan {
        overlay,
        changed,
        warnings: tracker.st.warnings.clone(),
        stats,
    })
}

/// §5.5 cold scan: a fresh tracker, every candidate's content read.
pub fn reconcile_full(ws: &WorkspaceRef, base: &Base) -> Result<Overlay> {
    let mut t = Tracker::new(ws.root, DEFAULT_DIRTY_CAPACITY);
    Ok(compute(ws, base, &mut t, None)?.overlay)
}

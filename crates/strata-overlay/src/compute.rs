//! §5.5 `compute` and `reconcile_full`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use rayon::prelude::*;
use strata_apfs::{Attrs, FileKind};
use strata_gitio::IndexInfo;
use strata_types::{BaseManifest, Code, RepoPath, StrataError};

use crate::classify::{Folds, Raw, changed, classify, present};
use crate::context::{Base, Fold, WorkspaceRef, under};
use crate::hash::{Cached, Got, is_shape_error, read_path, special_bits};
use crate::inbox::Snapshot;
use crate::overlay::Overlay;
use crate::scan::{Ctx, Keys, XSet, file_candidate, restricted_at, walk_tree};
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

pub(crate) fn push_warn(
    w: &mut BTreeMap<RepoPath, Vec<OverlayWarning>>,
    p: &RepoPath,
    x: OverlayWarning,
) {
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

/// Fold the results of §5.2 reads into `st` (whose entries for these paths
/// are already gone); returns the paths found on disk as files or symlinks.
fn absorb(
    st: &mut State,
    m: &BaseManifest,
    info: &IndexInfo,
    reads: Vec<(RepoPath, Result<Got>)>,
    stats: &mut ScanStats,
) -> Result<BTreeSet<RepoPath>> {
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
                    st.cache.insert(
                        p.clone(),
                        Cached {
                            attrs: a.clone(),
                            blob: o.blob.clone(),
                        },
                    );
                }
                if o.unstable {
                    st.unstable.insert(p.clone());
                    push_warn(
                        &mut st.warn,
                        &p,
                        OverlayWarning::Unstable { path: p.clone() },
                    );
                }
                if let Some(mode) = special_bits(&o.attrs) {
                    push_warn(
                        &mut st.warn,
                        &p,
                        OverlayWarning::SpecialModeBits {
                            path: p.clone(),
                            mode,
                        },
                    );
                }
                if let Some(r) = present(m, &p, o.blob.clone(), o.mode, info.is_tracked(&p)) {
                    st.raw.insert(p.clone(), r);
                }
                on_disk.insert(p);
            }
            // Gone between the walk and the read: absent now, looked at again
            // next time. Unstable, since a path that became a directory hides
            // whatever the walk never saw beneath it (session.md §5.6 step 5.3).
            Got::Gone => {
                push_warn(
                    &mut st.warn,
                    &p,
                    OverlayWarning::Unstable { path: p.clone() },
                );
                st.unstable.insert(p);
            }
            Got::Special => {
                push_warn(
                    &mut st.warn,
                    &p,
                    OverlayWarning::SpecialFile { path: p.clone() },
                );
            }
        }
    }
    Ok(on_disk)
}

fn read_all(
    root: &Path,
    files: Vec<(RepoPath, Attrs)>,
    old: &BTreeMap<RepoPath, Cached>,
) -> Vec<(RepoPath, Result<Got>)> {
    files
        .into_par_iter()
        .map(|(p, a)| {
            let got = read_path(root, &p, a, old.get(&p));
            (p, got)
        })
        .collect()
}

fn warn_found(st: &mut State, special: Vec<RepoPath>, embedded: Vec<RepoPath>) {
    for p in special {
        push_warn(
            &mut st.warn,
            &p,
            OverlayWarning::SpecialFile { path: p.clone() },
        );
    }
    for d in embedded {
        push_warn(
            &mut st.warn,
            &d,
            OverlayWarning::EmbeddedRepo { path: d.clone() },
        );
    }
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
    let old = std::mem::take(&mut st.cache);
    let reads = read_all(ws.root, found.files, &old);
    st.raw.clear();
    st.warn.clear();
    st.unstable.clear();
    let m = &base.manifest;
    let on_disk = absorb(st, m, &f.info, reads, stats)?;
    warn_found(st, found.special, found.embedded);
    for p in m.entries.keys() {
        if !on_disk.contains(p) && !f.x.covers(p.as_str()) {
            st.raw.insert(p.clone(), Raw::Absent);
        }
    }
    Ok(())
}

/// The incremental scan found something only a full walk can settle.
struct NeedFull;

/// Directory listings of one incremental scan (each directory listed once).
struct Lister<'a> {
    root: &'a Path,
    dirs: HashMap<String, Vec<(RepoPath, Attrs)>>,
}

impl Lister<'_> {
    fn list(
        &mut self,
        dir: Option<&RepoPath>,
        stats: &mut ScanStats,
    ) -> Result<&[(RepoPath, Attrs)]> {
        let key = dir.map(|d| d.as_str().to_owned()).unwrap_or_default();
        if !self.dirs.contains_key(&key) {
            let mut v = Vec::new();
            for item in strata_apfs::list_dir(self.root, dir) {
                match item {
                    Ok(e) => v.push(e),
                    // The directory went away (or is not one): nothing under it.
                    Err(e) if is_shape_error(&e) => {
                        v.clear();
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
            stats.dirs_listed += 1;
            stats.entries_seen += v.len() as u64;
            self.dirs.insert(key.clone(), v);
        }
        Ok(&self.dirs[&key])
    }
}

fn last_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Where the dirty path is on disk (§5.4 spelling resolution).
enum Spelled {
    /// Two names at one level fold to the same key.
    Ambiguous,
    /// `at`: the entry there by the volume's fold (`None`: nothing);
    /// `blocked`: an ancestor that is not a directory any more.
    Found {
        at: Option<(RepoPath, Attrs)>,
        blocked: Option<RepoPath>,
    },
}

fn spell(lister: &mut Lister, fold: Fold, p: &RepoPath, stats: &mut ScanStats) -> Result<Spelled> {
    let parts: Vec<&str> = p.as_str().split('/').collect();
    let mut dir: Option<RepoPath> = None;
    for (i, name) in parts.iter().enumerate() {
        let kn = fold.key(name);
        let entries = lister.list(dir.as_ref(), stats)?;
        let mut hits = entries
            .iter()
            .filter(|(e, _)| fold.key(last_name(e.as_str())) == kn);
        let Some((ep, a)) = hits.next().cloned() else {
            return Ok(Spelled::Found {
                at: None,
                blocked: None,
            });
        };
        if hits.next().is_some() {
            return Ok(Spelled::Ambiguous);
        }
        if i + 1 == parts.len() {
            return Ok(Spelled::Found {
                at: Some((ep, a)),
                blocked: None,
            });
        }
        if a.kind != FileKind::Dir {
            return Ok(Spelled::Found {
                at: None,
                blocked: Some(ep),
            });
        }
        dir = Some(ep);
    }
    unreachable!("a repository path has at least one segment")
}

/// Known paths (base, cache, raw) by volume-fold key.
type Known = BTreeMap<String, BTreeSet<RepoPath>>;

/// The known paths at or under the path whose key is `kp`.
fn region(known: &Known, kp: &str) -> Vec<RepoPath> {
    known
        .range(kp.to_owned()..)
        .take_while(|(k, _)| k.starts_with(kp))
        .filter(|(k, _)| under(k, kp))
        .flat_map(|(_, v)| v.iter().cloned())
        .collect()
}

/// §5.5 step 4, incremental: judge again only what lies at or under the
/// dirty paths (by the volume's fold), starting from the tracker's state.
fn incremental(
    ws: &WorkspaceRef,
    base: &Base,
    st: &mut State,
    f: &Frame,
    dirty: BTreeSet<RepoPath>,
    stats: &mut ScanStats,
) -> Result<std::result::Result<(), NeedFull>> {
    let ctx = Ctx {
        root: ws.root,
        git: ws.git,
        keys: &f.keys,
        x: &f.x,
    };
    let fold = f.keys.fold;
    let m = &base.manifest;
    let mut known: Known = BTreeMap::new();
    for p in m.entries.keys().chain(st.cache.keys()).chain(st.raw.keys()) {
        known
            .entry(fold.key(p.as_str()))
            .or_default()
            .insert(p.clone());
    }
    let mut lister = Lister {
        root: ws.root,
        dirs: HashMap::new(),
    };
    let mut queue: Vec<RepoPath> = dirty.into_iter().rev().collect();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while let Some(p) = queue.pop() {
        if f.x.covers(p.as_str()) {
            continue;
        }
        if last_name(p.as_str()) == ".gitignore" {
            return Ok(Err(NeedFull));
        }
        let kp = fold.key(p.as_str());
        if !done.insert(kp.clone()) {
            continue;
        }
        let (at, blocked) = match spell(&mut lister, fold, &p, stats)? {
            Spelled::Ambiguous => return Ok(Err(NeedFull)),
            Spelled::Found { at, blocked } => (at, blocked),
        };
        if let Some(c) = blocked {
            queue.push(c);
        }
        let mut files = Vec::new();
        let mut special = Vec::new();
        let mut embedded = Vec::new();
        if let Some((q, a)) = at {
            if a.kind == FileKind::Dir {
                let found = walk_tree(&ctx, Some(&q))?;
                stats.dirs_listed += found.dirs;
                stats.entries_seen += found.seen;
                files = found.files;
                special = found.special;
                embedded = found.embedded;
            } else {
                let pr = match q.as_str().rsplit_once('/') {
                    Some((d, _)) => restricted_at(&ctx, &RepoPath::try_from(d.to_owned())?)?,
                    None => false,
                };
                if matches!(a.kind, FileKind::File | FileKind::Symlink { .. }) {
                    if file_candidate(&ctx, &q, pr)? {
                        files.push((q, a));
                    }
                } else if f.keys.has(q.as_str()) || !pr {
                    special.push(q);
                }
            }
        }
        // Forget what the state knew about the region, then judge it again.
        let old = region(&known, &kp);
        let mut prior = BTreeMap::new();
        for o in old.iter().chain(files.iter().map(|(q, _)| q)) {
            if let Some(c) = st.cache.remove(o) {
                prior.insert(o.clone(), c);
            }
            st.raw.remove(o);
        }
        st.warn.retain(|w, _| !under(&fold.key(w.as_str()), &kp));
        st.unstable.retain(|u| !under(&fold.key(u.as_str()), &kp));
        let reads = read_all(ws.root, files, &prior);
        let on_disk = absorb(st, m, &f.info, reads, stats)?;
        warn_found(st, special, embedded);
        for o in &old {
            if !on_disk.contains(o) && m.entries.contains_key(o) && !f.x.covers(o.as_str()) {
                st.raw.insert(o.clone(), Raw::Absent);
            }
        }
        for q in on_disk {
            known.entry(fold.key(q.as_str())).or_default().insert(q);
        }
    }
    Ok(Ok(()))
}

/// §5.5: the overlay of the workspace against `base`. Never writes the Store
/// or the workspace; on failure `tracker` is unchanged (and the dirty set it
/// took is put back).
pub fn compute(
    ws: &WorkspaceRef,
    base: &Base,
    tracker: &mut Tracker,
    dirty: Option<&[RepoPath]>,
) -> Result<Scan> {
    let mut pass = begin(ws, base, tracker, dirty)?;
    let overlay = settle(base, &mut pass);
    let changed = changed(tracker.st.last.as_ref(), &overlay);
    tracker.st = pass.st;
    Ok(Scan {
        overlay,
        changed,
        warnings: tracker.st.warnings.clone(),
        stats: pass.stats,
    })
}

fn check_args(ws: &WorkspaceRef, base: &Base, tracker: &Tracker) -> Result<()> {
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
    Ok(())
}

/// A scan that has run but is not committed to the tracker yet (checkpoint
/// stores blobs in between, and may judge raced paths again).
pub(crate) struct Pass {
    pub st: State,
    pub f: Frame,
    pub stats: ScanStats,
    pub snap: Snapshot,
}

/// §5.5 steps 1-4 on a copy of the tracker's state. On failure the snapshot
/// is merged back and the tracker is unchanged.
pub(crate) fn begin(
    ws: &WorkspaceRef,
    base: &Base,
    tracker: &mut Tracker,
    dirty: Option<&[RepoPath]>,
) -> Result<Pass> {
    check_args(ws, base, tracker)?;
    let f = frame(ws, base, &tracker.st)?;
    tracker.inbox.set_x(&f.x);
    let snap = tracker.inbox.take();
    crate::fault::after_snapshot(ws.root);
    match run(ws, base, &tracker.st, dirty, &f, &snap) {
        Ok((st, stats)) => Ok(Pass { st, f, stats, snap }),
        Err(e) => {
            tracker.inbox.merge(snap);
            Err(e)
        }
    }
}

fn run(
    ws: &WorkspaceRef,
    base: &Base,
    old: &State,
    dirty: Option<&[RepoPath]>,
    f: &Frame,
    snap: &Snapshot,
) -> Result<(State, ScanStats)> {
    let full = dirty.is_none()
        || snap.full
        || !old.scanned
        || old.base.as_ref() != Some(&base.r#gen)
        || old.ignorecase != Some(f.info.ignorecase)
        || old.x.as_ref() != Some(&f.x);
    let cold = old.cache.is_empty() && !old.scanned;
    let mut stats = ScanStats {
        full,
        cold,
        ..Default::default()
    };
    let mut st = old.clone();
    let mut go_full = full;
    if !full {
        let mut set: BTreeSet<RepoPath> = BTreeSet::new();
        set.extend(dirty.unwrap_or_default().iter().cloned());
        set.extend(snap.paths.iter().cloned());
        set.extend(snap.subtrees.iter().cloned());
        set.extend(old.unstable.iter().cloned());
        if let Some(t) = &old.tracked {
            set.extend(t.symmetric_difference(&f.info.tracked).cloned());
        }
        if incremental(ws, base, &mut st, f, set, &mut stats)?.is_err() {
            go_full = true;
            st = old.clone();
            stats = ScanStats {
                full: true,
                cold,
                ..Default::default()
            };
        }
    }
    if go_full {
        full_scan(ws, base, &mut st, f, &mut stats)?;
    }
    Ok((st, stats))
}

/// Judge `paths` again in `pass` (§5.4 spelling steps 1-3: a path, the
/// shallowest non-directory above it, and what it hides); a full walk when
/// only that can settle them.
pub(crate) fn rejudge(
    ws: &WorkspaceRef,
    base: &Base,
    pass: &mut Pass,
    paths: BTreeSet<RepoPath>,
) -> Result<()> {
    let mut st = pass.st.clone();
    let mut stats = pass.stats;
    if incremental(ws, base, &mut st, &pass.f, paths, &mut stats)?.is_err() {
        st = pass.st.clone();
        stats.full = true;
        full_scan(ws, base, &mut st, &pass.f, &mut stats)?;
    }
    pass.st = st;
    pass.stats = stats;
    Ok(())
}

/// §5.5 steps 5-6: classify the raw difference and record the scan facts in
/// the pass's state (committed by the caller).
pub(crate) fn settle(base: &Base, pass: &mut Pass) -> Overlay {
    let f = &pass.f;
    let st = &mut pass.st;
    let overlay = classify(
        &base.r#gen,
        &base.manifest,
        &st.raw,
        Folds {
            volume_case_sensitive: f.volume_cs,
            ignorecase: f.info.ignorecase,
        },
    );
    st.last = Some(overlay.clone());
    st.base = Some(base.r#gen.clone());
    st.ignorecase = Some(f.info.ignorecase);
    st.volume_cs = Some(f.volume_cs);
    st.tracked = Some(f.info.tracked.clone());
    st.keys = Some(f.keys.clone());
    st.x = Some(f.x.clone());
    st.scanned = true;
    st.flatten_warnings();
    overlay
}

/// §5.5 cold scan: a fresh tracker, every candidate's content read.
pub fn reconcile_full(ws: &WorkspaceRef, base: &Base) -> Result<Overlay> {
    let mut t = Tracker::new(ws.root, DEFAULT_DIRTY_CAPACITY);
    Ok(compute(ws, base, &mut t, None)?.overlay)
}

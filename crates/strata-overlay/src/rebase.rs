//! §5.8 `rebase`: whole-file three-way projection of an overlay from one base
//! generation onto another. Never writes a workspace; writes blobs only for
//! merge results and the §5.8 step 5a invariant.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_gitio::{Merge3, merge3};
use strata_store::Store;
use strata_types::{BaseManifest, BlobId, Code, EntryKind, RepoPath, StrataError};

use crate::classify::{Folds, Raw, classify};
use crate::context::Base;
use crate::hash::{hash_bytes, hash_file};
use crate::lww::{LwwRules, lww_matches};
use crate::overlay::{MODE_EXEC, MODE_FILE, Overlay, ancestors, entry_blob, entry_from, rp};
use crate::{MERGE_MAX_BYTES, Result};

/// rebase 的选项（§3）。Default: no `.lww`, no content merge, ignorecase =
/// false, case-insensitive volume (the APFS default).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebaseOptions {
    /// Which `.lww` rules apply; empty rules = none.
    pub lww: LwwRules,
    /// The repository's core.ignorecase: the git fold of case-only pairing.
    pub ignorecase: bool,
    /// Whether the workspace volume is case-sensitive: the volume fold.
    pub volume_case_sensitive: bool,
    /// true: regular files changed on both sides are tried with merge3.
    /// false: every both-sides change conflicts; rebase writes no blob.
    pub merge: bool,
}

impl RebaseOptions {
    /// move_workspace and post-publish rebase: merge, `.lww` rules from the
    /// new base's directory, volume case sensitivity of that directory.
    pub fn for_move(new: &Base, ignorecase: bool) -> Result<RebaseOptions> {
        Ok(RebaseOptions {
            lww: LwwRules::load(&new.dir)?,
            ignorecase,
            volume_case_sensitive: strata_apfs::volume_caps(&new.dir)?.case_sensitive,
            merge: true,
        })
    }

    /// restore across bases: no merge, no `.lww`.
    pub fn for_restore(ignorecase: bool, volume_case_sensitive: bool) -> RebaseOptions {
        RebaseOptions {
            lww: LwwRules::default(),
            ignorecase,
            volume_case_sensitive,
            merge: false,
        }
    }
}

/// The result of `rebase` (§3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rebased {
    /// The overlay against the new base; the session's version on conflicts.
    pub overlay: Overlay,
    /// Both-sides changes that did not merge (session's version kept), and
    /// both sides of every §5.8 step 4a prefix collision.
    pub conflicts: Vec<RepoPath>,
    /// Paths that left the overlay: the new base already equals the session.
    pub dropped: Vec<RepoPath>,
    /// `.lww` paths changed on both sides: the session's version, no merge.
    pub lww: Vec<RepoPath>,
}

type Side = Option<(BlobId, u32)>;

/// Where the projection reads the session's blobs and keeps what it makes
/// (rm-followups §1.4 G3). [`Store`] is the real one; [`PreviewBlobs`]
/// writes nothing.
pub trait Blobs {
    /// All of blob `id`'s bytes, if at most `max` (`TooLarge` otherwise).
    fn read(&self, id: &BlobId, max: u64) -> Result<Vec<u8>>;
    /// Keep a clean merge result; its id.
    fn put(&self, bytes: &[u8]) -> Result<BlobId>;
    /// §5.8 step 5a: keep the new base's file `p` under `dir`; its id.
    fn put_base(&self, dir: &Path, p: &RepoPath) -> Result<BlobId>;
}

impl Blobs for Store {
    fn read(&self, id: &BlobId, max: u64) -> Result<Vec<u8>> {
        self.read_blob(id, max)
    }
    fn put(&self, bytes: &[u8]) -> Result<BlobId> {
        self.put_bytes(bytes)
    }
    fn put_base(&self, dir: &Path, p: &RepoPath) -> Result<BlobId> {
        self.put_blob_rooted(dir, p)
    }
}

/// `reason` of the error [`PreviewBlobs`] returns when a workspace file no
/// longer has the bytes the scan saw.
pub const CHANGED_SINCE_SCAN: &str = "changed_since_scan";

/// Read-only [`Blobs`] for [`rebase_preview`] over an overlay that was never
/// checkpointed: a blob comes from the store when it is there, else from the
/// workspace file the overlay names for it (no symlink followed, at most
/// `max` bytes), whose hash must still be the overlay's id. `put` only hashes.
pub struct PreviewBlobs<'a> {
    store: &'a Store,
    root: &'a Path,
    by_id: BTreeMap<BlobId, RepoPath>,
}

impl<'a> PreviewBlobs<'a> {
    pub fn new(store: &'a Store, root: &'a Path, overlay: &Overlay) -> PreviewBlobs<'a> {
        let mut by_id = BTreeMap::new();
        for e in &overlay.entries {
            if let Some(b) = entry_blob(e) {
                by_id.entry(b.clone()).or_insert_with(|| e.path.clone());
            }
        }
        PreviewBlobs { store, root, by_id }
    }
}

fn changed_since_scan(root: &Path, p: &RepoPath, why: &str) -> StrataError {
    StrataError::new(Code::Conflict, format!("{} changed since the scan ({why})", root.join(p.as_str()).display()))
        .with_detail(json!({ "reason": CHANGED_SINCE_SCAN, "path": p.as_str() }))
}

impl Blobs for PreviewBlobs<'_> {
    fn read(&self, id: &BlobId, max: u64) -> Result<Vec<u8>> {
        match self.store.read_blob(id, max) {
            Err(e) if e.code == Code::Corrupt
                && e.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some("blob_missing") => {}
            r => return r,
        }
        let Some(p) = self.by_id.get(id) else {
            return Err(StrataError::new(Code::Corrupt, format!("blob {id} is neither stored nor in the overlay")));
        };
        let f = strata_apfs::open_rooted_file(self.root, p).map_err(|e| changed_since_scan(self.root, p, &e.message))?;
        let mut out = Vec::new();
        (&f).take(max + 1).read_to_end(&mut out).map_err(|e| changed_since_scan(self.root, p, &e.to_string()))?;
        if out.len() as u64 > max {
            return Err(StrataError::new(Code::TooLarge, format!("{} is more than {max} bytes", p.as_str()))
                .with_detail(json!({ "limit": "read_max", "max": max })));
        }
        if hash_bytes(&out) != *id {
            return Err(changed_since_scan(self.root, p, "its hash differs"));
        }
        Ok(out)
    }
    fn put(&self, bytes: &[u8]) -> Result<BlobId> {
        Ok(hash_bytes(bytes))
    }
    fn put_base(&self, dir: &Path, p: &RepoPath) -> Result<BlobId> {
        let f = strata_apfs::open_rooted_file(dir, p)?;
        Ok(hash_file(&f, &dir.join(p.as_str()))?.0)
    }
}

/// What [`rebase_preview`] would do; nothing was written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub conflicts: Vec<RepoPath>,
    pub dropped: Vec<RepoPath>,
    pub lww: Vec<RepoPath>,
    /// Changed on both sides and merged cleanly.
    pub merged: Vec<RepoPath>,
}

fn invalid(reason: &str, msg: &str) -> StrataError {
    StrataError::new(Code::InvalidInput, msg.to_owned()).with_detail(json!({ "reason": reason }))
}

fn corrupt(
    base: &Base,
    p: &RepoPath,
    expected: &BlobId,
    actual: Option<&BlobId>,
    why: &str,
) -> StrataError {
    StrataError::new(
        Code::Corrupt,
        format!("base file {} {why}", base.dir.join(p.as_str()).display()),
    )
    .with_detail(json!({
        "path": base.dir.join(p.as_str()).to_string_lossy(),
        "expected": expected.as_str(),
        "actual": actual.map(|b| b.as_str()),
    }))
}

fn side(m: &BaseManifest, p: &RepoPath) -> Side {
    m.entries.get(p).map(|e| (e.blob.clone(), e.mode))
}

fn regular(s: &Side) -> bool {
    matches!(s, Some((_, MODE_FILE | MODE_EXEC)))
}

/// A base file's bytes, checked against the manifest. Missing or different
/// is `Corrupt`, never a new file (Addendum 4).
fn read_base(base: &Base, p: &RepoPath) -> Result<Vec<u8>> {
    let want = &base.manifest.entries[p].blob;
    let mut f = strata_apfs::open_rooted_file(&base.dir, p).map_err(|e| {
        let mut c = corrupt(base, p, want, None, "is missing or not a regular file");
        c.message = format!("{} ({})", c.message, e.message);
        c
    })?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).map_err(|e| {
        strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &base.dir.join(p.as_str()))
    })?;
    let got = hash_bytes(&out);
    if &got != want {
        return Err(corrupt(
            base,
            p,
            want,
            Some(&got),
            "differs from the manifest",
        ));
    }
    Ok(out)
}

enum Merged {
    Take(BlobId),
    Conflict,
}

/// §5.8 step 3.4: content of a regular file changed on both sides.
fn merge_content(
    p: &RepoPath,
    b: &Side,
    o: &(BlobId, u32),
    t: &(BlobId, u32),
    old: &Base,
    new: &Base,
    store: &dyn Blobs,
) -> Result<Merged> {
    if let Some((bb, _)) = b {
        if *bb == o.0 {
            return Ok(Merged::Take(t.0.clone()));
        }
        if *bb == t.0 {
            return Ok(Merged::Take(o.0.clone()));
        }
    }
    let too_big = |m: &BaseManifest| m.entries.get(p).is_some_and(|e| e.size > MERGE_MAX_BYTES);
    if (b.is_some() && too_big(&old.manifest)) || too_big(&new.manifest) {
        return Ok(Merged::Conflict);
    }
    let ours = match store.read(&o.0, MERGE_MAX_BYTES) {
        Ok(v) => v,
        Err(e) if e.code == Code::TooLarge => return Ok(Merged::Conflict),
        Err(e) => return Err(e),
    };
    let base = match b {
        Some(_) => read_base(old, p)?,
        None => Vec::new(),
    };
    let theirs = read_base(new, p)?;
    match merge3(&base, &ours, &theirs) {
        Merge3::Clean(bytes) => {
            let id = hash_bytes(&bytes);
            if id == o.0 || id == t.0 {
                Ok(Merged::Take(id))
            } else {
                Ok(Merged::Take(store.put(&bytes)?))
            }
        }
        Merge3::Conflict(_) => Ok(Merged::Conflict),
    }
}

fn merge_mode(b: &Side, o: u32, t: u32) -> u32 {
    match b {
        Some((_, bm)) if *bm != o => o,
        Some(_) => t,
        None if o == MODE_EXEC || t == MODE_EXEC => MODE_EXEC,
        None => MODE_FILE,
    }
}

/// Where a live path of the final state comes from (§5.8 step 4a).
#[derive(Clone, Copy, PartialEq, Eq)]
enum From {
    Session,
    Base,
}

/// 整文件三方投影（§5.8）。
pub fn rebase(
    overlay: &Overlay,
    old: &Base,
    new: &Base,
    store: &Store,
    opts: &RebaseOptions,
) -> Result<Rebased> {
    Ok(rebase_with(overlay, old, new, store, opts)?.0)
}

/// rm-followups §1.4 G3: [`rebase`]'s classification with no store write,
/// over an overlay that need not be checkpointed ([`PreviewBlobs`]). A
/// workspace file that changed since the scan is `Conflict` with
/// `detail.reason = "changed_since_scan"`.
pub fn rebase_preview(
    overlay: &Overlay,
    old: &Base,
    new: &Base,
    blobs: &PreviewBlobs<'_>,
    opts: &RebaseOptions,
) -> Result<Preview> {
    let (r, merged) = rebase_with(overlay, old, new, blobs, opts)?;
    Ok(Preview { conflicts: r.conflicts, dropped: r.dropped, lww: r.lww, merged })
}

/// [`rebase`] over any [`Blobs`]; also the paths merged cleanly.
pub fn rebase_with(
    overlay: &Overlay,
    old: &Base,
    new: &Base,
    store: &dyn Blobs,
    opts: &RebaseOptions,
) -> Result<(Rebased, Vec<RepoPath>)> {
    // 1. Arguments and V1-V5.
    if overlay.base != old.r#gen {
        return Err(invalid(
            "base_mismatch",
            "overlay is not against the old base",
        ));
    }
    if old.r#gen.repo != new.r#gen.repo {
        return Err(invalid(
            "repo_mismatch",
            "old and new bases are of different repos",
        ));
    }
    overlay.validate()?;
    overlay.check_base(&old.manifest)?;

    // 2. U: entry paths and rename sources; ours per path.
    let mut ours: BTreeMap<RepoPath, Side> = BTreeMap::new();
    let mut untracked: BTreeSet<RepoPath> = BTreeSet::new();
    let mut input_blobs: BTreeSet<BlobId> = BTreeSet::new();
    for e in &overlay.entries {
        if let Some(from) = entry_from(e) {
            ours.insert(from.clone(), None);
        }
        let blob = entry_blob(e).cloned();
        if let Some(b) = &blob {
            input_blobs.insert(b.clone());
        }
        if matches!(e.kind, EntryKind::Untracked { .. }) {
            untracked.insert(e.path.clone());
        }
        ours.insert(e.path.clone(), blob.map(|b| (b, e.mode)));
    }

    // 3. Per-path three-way judgement.
    let mut fin: BTreeMap<RepoPath, (Side, From)> = BTreeMap::new();
    let mut conflicts: BTreeSet<RepoPath> = BTreeSet::new();
    let mut lww: BTreeSet<RepoPath> = BTreeSet::new();
    let mut merged: BTreeSet<RepoPath> = BTreeSet::new();
    for (p, o) in &ours {
        let b = side(&old.manifest, p);
        let t = side(&new.manifest, p);
        let r = if *o == t {
            (t, From::Base)
        } else if t == b {
            (o.clone(), From::Session)
        } else if lww_matches(&opts.lww, p) {
            lww.insert(p.clone());
            (o.clone(), From::Session)
        } else if opts.merge && regular(o) && regular(&t) && (b.is_none() || regular(&b)) {
            let (oo, tt) = (o.clone().unwrap(), t.clone().unwrap());
            match merge_content(p, &b, &oo, &tt, old, new, store)? {
                Merged::Take(blob) => {
                    merged.insert(p.clone());
                    let side = Some((blob, merge_mode(&b, oo.1, tt.1)));
                    let from = if side == t { From::Base } else { From::Session };
                    (side, from)
                }
                Merged::Conflict => {
                    conflicts.insert(p.clone());
                    (o.clone(), From::Session)
                }
            }
        } else {
            conflicts.insert(p.clone());
            (o.clone(), From::Session)
        };
        fin.insert(p.clone(), r);
    }

    // 4a. Prefix collisions between a session path and a new-base path: the
    // session side stays, the base side becomes absent (a tombstone).
    let mut live: BTreeMap<&str, From> = new
        .manifest
        .entries
        .keys()
        .filter(|p| !fin.contains_key(*p))
        .map(|p| (p.as_str(), From::Base))
        .collect();
    for (p, (s, f)) in &fin {
        if s.is_some() {
            live.insert(p.as_str(), *f);
        }
    }
    let mut kill: BTreeSet<String> = BTreeSet::new();
    for (p, f) in &live {
        if *f != From::Session {
            continue;
        }
        for a in ancestors(p) {
            if live.get(a) == Some(&From::Base) {
                kill.insert(a.to_owned());
                conflicts.insert(rp(a));
                conflicts.insert(rp(p));
            }
        }
        let under = format!("{p}/");
        for (q, g) in live.range(under.as_str()..) {
            if !q.starts_with(&under) {
                break;
            }
            if *g == From::Base {
                kill.insert((*q).to_owned());
                conflicts.insert(rp(q));
                conflicts.insert(rp(p));
            }
        }
    }
    for k in kill {
        fin.insert(rp(&k), (None, From::Session));
    }

    // 5. Reclassify against the new manifest only.
    let mut raw: BTreeMap<RepoPath, Raw> = BTreeMap::new();
    for (p, (s, _)) in &fin {
        match s {
            None => {
                if new.manifest.entries.contains_key(p) {
                    raw.insert(p.clone(), Raw::Absent);
                }
            }
            Some((blob, mode)) => {
                if side(&new.manifest, p).as_ref() != Some(&(blob.clone(), *mode)) {
                    raw.insert(
                        p.clone(),
                        Raw::Present {
                            blob: blob.clone(),
                            mode: *mode,
                            staged: !untracked.contains(p),
                        },
                    );
                }
            }
        }
    }
    let folds = Folds {
        volume_case_sensitive: opts.volume_case_sensitive,
        ignorecase: opts.ignorecase,
    };
    let mut out = classify(&new.r#gen, &new.manifest, &raw, folds);

    // 5a. Blob invariant: an entry carrying the new base's blob that the
    // input overlay never had is stored from the base file, checked first.
    for e in &mut out.entries {
        let Some(blob) = entry_blob(e) else { continue };
        let Some(m) = new.manifest.entries.get(&e.path) else {
            continue;
        };
        if m.blob != *blob || input_blobs.contains(blob) {
            continue;
        }
        let got = store.put_base(&new.dir, &e.path).map_err(|err| {
            if matches!(
                err.code,
                Code::NoSuchPath | Code::IsDirectory | Code::NotFile | Code::NotDirectory
            ) {
                corrupt(new, &e.path, &m.blob, None, "is missing or not a file")
            } else {
                err
            }
        })?;
        if got != m.blob {
            return Err(corrupt(
                new,
                &e.path,
                &m.blob,
                Some(&got),
                "differs from the manifest",
            ));
        }
    }

    // 6. dropped.
    let mut still: BTreeSet<&RepoPath> = BTreeSet::new();
    for e in &out.entries {
        still.insert(&e.path);
        if let Some(f) = entry_from(e) {
            still.insert(f);
        }
    }
    let dropped: Vec<RepoPath> = ours
        .keys()
        .filter(|p| !lww.contains(*p) && !still.contains(p))
        .cloned()
        .collect();
    let merged = merged.into_iter().filter(|p| !conflicts.contains(p)).collect();
    Ok((
        Rebased { overlay: out, conflicts: conflicts.into_iter().collect(), dropped, lww: lww.into_iter().collect() },
        merged,
    ))
}

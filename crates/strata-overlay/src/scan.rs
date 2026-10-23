//! §5.1 membership: the exclusion set X, the fold keys of M ∪ I.tracked and
//! the walk that yields the candidates (read-only; errors fail the scan).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Mutex;

use strata_apfs::{Attrs, FileKind, WalkOptions, attrs_of, walk_with};
use strata_gitio::Git;
use strata_types::{BaseManifest, RepoConfig, RepoPath, StrataError};

use crate::Result;
use crate::context::{Excluded, Fold, under};
use crate::hash::is_shape_error;

/// The volume-fold keys of M ∪ I.tracked (§4.1 `fold`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Keys {
    pub fold: Fold,
    set: BTreeSet<String>,
}

impl Keys {
    pub fn build(fold: Fold, m: &BaseManifest, tracked: &BTreeSet<RepoPath>) -> Keys {
        let set = m
            .entries
            .keys()
            .chain(tracked.iter())
            .map(|p| fold.key(p.as_str()))
            .collect();
        Keys { fold, set }
    }

    /// The key of `p` is the key of a base or tracked path.
    pub fn has(&self, p: &str) -> bool {
        self.set.contains(&self.fold.key(p))
    }

    /// Some base or tracked path lies strictly under directory `d`.
    pub fn under(&self, d: &str) -> bool {
        let prefix = format!("{}/", self.fold.key(d));
        self.set
            .range(prefix.clone()..)
            .next()
            .is_some_and(|k| k.starts_with(&prefix))
    }
}

/// The exclusion set X of §5.1 (X1-X2 from the config, X3 the untracked seed files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct XSet {
    ex: Excluded,
    fold: Fold,
    seeds: Vec<String>,
}

impl XSet {
    pub fn new(config: &RepoConfig, keys: &Keys) -> XSet {
        let fold = keys.fold;
        let (files, _) = config.seed_files();
        let mut seeds: Vec<String> = files
            .iter()
            .filter(|p| !keys.has(p.as_str()))
            .map(|p| fold.key(p.as_str()))
            .collect();
        seeds.sort();
        seeds.dedup();
        XSet {
            ex: Excluded::new(config, fold),
            fold,
            seeds,
        }
    }

    /// `p` lies in X (at or under one of its paths), compared with the volume's fold.
    pub fn covers(&self, p: &str) -> bool {
        if self.ex.covers(p) {
            return true;
        }
        if self.seeds.is_empty() {
            return false;
        }
        let k = self.fold.key(p);
        self.seeds.iter().any(|s| under(&k, s))
    }
}

pub(crate) struct Ctx<'a> {
    pub root: &'a Path,
    pub git: &'a Git,
    pub keys: &'a Keys,
    pub x: &'a XSet,
}

/// What one walk found.
#[derive(Default)]
pub(crate) struct Found {
    /// Candidate files and symlinks with their walk attrs (A1 of §5.2).
    pub files: Vec<(RepoPath, Attrs)>,
    /// FIFOs, sockets and devices to warn about (never opened).
    pub special: Vec<RepoPath>,
    /// Nested repositories entered as restricted.
    pub embedded: Vec<RepoPath>,
    /// Directories walked through (the root or start included).
    pub dirs: u64,
    /// Entries whose attrs the walk read and kept.
    pub seen: u64,
}

fn rpath(s: &str) -> Result<RepoPath> {
    RepoPath::try_from(s.to_owned()).map_err(|_| {
        StrataError::new(
            strata_types::Code::InvalidInput,
            format!("not a repository path: {s}"),
        )
        .with_detail(serde_json::json!({ "path": s }))
    })
}

/// Directory `d` (never the root) is a nested repository: it holds `.git`.
fn embedded(ctx: &Ctx, d: &str) -> Result<bool> {
    match attrs_of(ctx.root, &rpath(&format!("{d}/.git"))?) {
        Ok(_) => Ok(true),
        Err(e) if is_shape_error(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Directory `d` restricts what lies below it: nested repo or ignored.
fn restricts(ctx: &Ctx, d: &str, emb: &Mutex<Vec<RepoPath>>) -> Result<bool> {
    if embedded(ctx, d)? {
        emb.lock().unwrap().push(rpath(d)?);
        return Ok(true);
    }
    ctx.git.is_ignored(ctx.root, &rpath(d)?)
}

/// Whether `d` or one of its ancestors restricts (start of a subtree walk).
pub(crate) fn restricted_at(ctx: &Ctx, d: &RepoPath) -> Result<bool> {
    let sink = Mutex::new(Vec::new());
    let s = d.as_str();
    for (i, _) in s.match_indices('/').chain(std::iter::once((s.len(), ""))) {
        if restricts(ctx, &s[..i], &sink)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether file `p` (outside X) is a candidate, given its parent's restriction.
pub(crate) fn file_candidate(ctx: &Ctx, p: &RepoPath, parent_restricted: bool) -> Result<bool> {
    if ctx.keys.has(p.as_str()) {
        return Ok(true);
    }
    if parent_restricted {
        return Ok(false);
    }
    Ok(!ctx.git.is_ignored(ctx.root, p)?)
}

/// Walk the tree (or the subtree at `start`) by the §5.1 rules.
pub(crate) fn walk_tree(ctx: &Ctx, start: Option<&RepoPath>) -> Result<Found> {
    let start_restricted = match start {
        None => false,
        Some(s) => restricted_at(ctx, s)?,
    };
    let memo: Mutex<HashMap<String, bool>> = Mutex::new(HashMap::new());
    let err: Mutex<Option<StrataError>> = Mutex::new(None);
    let emb: Mutex<Vec<RepoPath>> = Mutex::new(Vec::new());
    let fail = |e: StrataError| {
        let mut g = err.lock().unwrap();
        if g.is_none() {
            *g = Some(e);
        }
        false
    };
    let filter = |p: &Path, a: &Attrs| -> bool {
        if err.lock().unwrap().is_some() {
            return false;
        }
        let Some(s) = p.to_str() else {
            return false;
        };
        if ctx.x.covers(s) {
            return false;
        }
        let parent_r = match s.rsplit_once('/') {
            Some((pp, _)) => memo
                .lock()
                .unwrap()
                .get(pp)
                .copied()
                .unwrap_or(start_restricted),
            None => start_restricted,
        };
        match &a.kind {
            FileKind::Dir => {
                let r = if parent_r {
                    true
                } else {
                    match restricts(ctx, s, &emb) {
                        Ok(r) => r,
                        Err(e) => return fail(e),
                    }
                };
                memo.lock().unwrap().insert(s.to_owned(), r);
                !r || ctx.keys.under(s)
            }
            FileKind::File | FileKind::Symlink { .. } => {
                let rp = match rpath(s) {
                    Ok(r) => r,
                    Err(e) => return fail(e),
                };
                match file_candidate(ctx, &rp, parent_r) {
                    Ok(b) => b,
                    Err(e) => fail(e),
                }
            }
            // Special files: warned unless a restricted dir hides the name.
            _ => ctx.keys.has(s) || !parent_r,
        }
    };
    let opts = WalkOptions {
        start: start.cloned(),
        ..Default::default()
    };
    let mut found = Found {
        dirs: 1,
        ..Default::default()
    };
    for item in walk_with(ctx.root, &opts, &filter) {
        if let Some(e) = err.lock().unwrap().take() {
            return Err(e);
        }
        let (p, a) = item?;
        found.seen += 1;
        match a.kind {
            FileKind::Dir => found.dirs += 1,
            FileKind::File | FileKind::Symlink { .. } => found.files.push((p, a)),
            _ => found.special.push(p),
        }
    }
    if let Some(e) = err.lock().unwrap().take() {
        return Err(e);
    }
    found.embedded = emb.into_inner().unwrap();
    Ok(found)
}

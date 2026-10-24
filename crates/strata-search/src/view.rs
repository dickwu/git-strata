//! A view's private index: the shared reader shadowed by this session's
//! overlay (search.md §4.4, §5.4), and the query over one snapshot (§4.5,
//! §5.5, §5.6).
//!
//! A published state is an immutable `Snap`. A query clones the `Arc<Snap>`
//! under the view lock and then reads without any lock, so a later publish
//! or reader swap never changes a query already running. Each publish
//! builds a fresh `HybridIndex` from the generation's reader and replays
//! the view's `ops`; the bytes of overlaid files are kept in `ops`, so a
//! publish does not re-read files it did not have to.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use strata_overlay::{MERGE_MAX_BYTES, Overlay};
use strata_types::{Code, EntryKind, RepoConfig, RepoName, RepoPath, SessionId, StrataError};
use tgrep_core::generations::{EntryMode, Generation};
use tgrep_core::hybrid::HybridIndex;
use tgrep_core::reader::IndexReader;

use crate::{Hit, Result, err};

/// What the private `live` holds for one path.
#[derive(Clone)]
pub(crate) enum Op {
    /// Workspace bytes that differ from the base (or have no base).
    Upsert(Arc<Vec<u8>>),
    /// Mask the base postings of this path (tombstone, excluded, or content
    /// search does not index: oversized, binary, symlink, gone).
    Delete,
}

/// The exclusion set of §4.4, compared folded on a case-insensitive volume.
pub(crate) struct Excl {
    prefixes: Vec<String>,
    fold: bool,
}

impl Excl {
    pub(crate) fn new(cfg: &RepoConfig, fold: bool) -> Excl {
        let mut v = vec![".git".to_string(), ".strata".to_string()];
        v.extend(cfg.deps_dirs.iter().map(|p| p.as_str().to_string()));
        v.extend(cfg.seed_caches.iter().map(|p| p.as_str().to_string()));
        let prefixes = v
            .into_iter()
            .map(|p| p.trim_end_matches('/').to_string())
            .map(|p| if fold { strata_apfs::case_fold_key(&p) } else { p })
            .collect();
        Excl { prefixes, fold }
    }

    pub(crate) fn excluded(&self, path: &str) -> bool {
        let key;
        let p = if self.fold {
            key = strata_apfs::case_fold_key(path);
            key.as_str()
        } else {
            path
        };
        self.prefixes.iter().any(|x| p == x || (p.starts_with(x.as_str()) && p.as_bytes()[x.len()] == b'/'))
    }
}

fn is_regular(mode: u32) -> bool {
    mode & 0o170000 == 0o100000
}

/// One read of a workspace file through `open_rooted_file`.
pub(crate) enum Bytes {
    Read(Vec<u8>),
    TooLarge,
    Gone,
}

fn stamp(f: &File) -> std::io::Result<(u64, u64, i64, i64, i64, i64)> {
    let m = f.metadata()?;
    Ok((m.ino(), m.size(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()))
}

fn read_once(f: &mut File) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    f.take(MERGE_MAX_BYTES + 1).read_to_end(&mut buf)?;
    Ok((buf.len() as u64 <= MERGE_MAX_BYTES).then_some(buf))
}

fn not_ready(path: &str, why: impl std::fmt::Display) -> StrataError {
    err(Code::Unknown, format!("cannot read {path}: {why}"), "not_ready")
}

/// §5.4 / §5.3 step 6: a stable read (fstat before and after, one retry).
/// A missing file is `Gone`, not a failure. Anything that is not a regular
/// file now (symlink, FIFO, directory) is a failure: the view stays not
/// ready rather than guess.
pub(crate) fn read_stable(root: &Path, rel: &RepoPath) -> Result<Bytes> {
    for _ in 0..2 {
        let mut f = match strata_apfs::open_rooted_file(root, rel) {
            Ok(f) => f,
            Err(e) if e.code == Code::NoSuchPath => return Ok(Bytes::Gone),
            Err(e) => return Err(not_ready(rel.as_str(), e.message)),
        };
        let before = stamp(&f).map_err(|e| not_ready(rel.as_str(), e))?;
        if before.1 > MERGE_MAX_BYTES {
            return Ok(Bytes::TooLarge);
        }
        let got = read_once(&mut f).map_err(|e| not_ready(rel.as_str(), e))?;
        let after = stamp(&f).map_err(|e| not_ready(rel.as_str(), e))?;
        if before == after {
            return Ok(match got {
                Some(b) => Bytes::Read(b),
                None => Bytes::TooLarge,
            });
        }
    }
    Err(not_ready(rel.as_str(), "it changed while being read"))
}

/// The private op for bytes that search does index or not.
fn op_for(bytes: Bytes) -> Op {
    match bytes {
        Bytes::Read(b) if !b.contains(&0) => Op::Upsert(Arc::new(b)),
        _ => Op::Delete,
    }
}

/// Inputs of one fill.
pub(crate) struct Fill<'a> {
    pub(crate) root: &'a Path,
    pub(crate) overlay: &'a Overlay,
    pub(crate) generation: &'a Generation,
    pub(crate) excl: &'a Excl,
}

impl Fill<'_> {
    /// §5.4 for one path. `None`: nothing private, the base stands (or the
    /// path is in neither).
    pub(crate) fn compute(&self, path: &str) -> Result<Option<Op>> {
        if self.excl.excluded(path) {
            return Ok(Some(Op::Delete));
        }
        let Ok(rp) = RepoPath::try_from(path.to_string()) else {
            return Ok(Some(Op::Delete));
        };
        if let Some(e) = self.overlay.get(&rp) {
            if matches!(e.kind, EntryKind::Deleted) || !is_regular(e.mode) {
                return Ok(Some(Op::Delete));
            }
            return Ok(Some(op_for(read_stable(self.root, &rp)?)));
        }
        if self.overlay.hides(&rp) {
            return Ok(Some(Op::Delete));
        }
        let Some(entry) = self.generation.entry(path) else { return Ok(None) };
        if !matches!(entry.mode, EntryMode::File | EntryMode::Executable) {
            return Ok(None);
        }
        match read_stable(self.root, &rp)? {
            Bytes::Gone => Ok(None),
            Bytes::Read(b) if entry.matches_worktree_bytes(&b) => Ok(None),
            other => Ok(Some(op_for(other))),
        }
    }

    /// Every path a full fill looks at: base entries, overlay paths and
    /// tombstones.
    pub(crate) fn all_paths(&self) -> BTreeSet<String> {
        let mut s: BTreeSet<String> = self.generation.entries().iter().map(|e| e.path.clone()).collect();
        for e in &self.overlay.entries {
            s.insert(e.path.as_str().to_string());
            if let EntryKind::Renamed { from, .. } = &e.kind {
                s.insert(from.as_str().to_string());
            }
        }
        s
    }

    /// Apply `compute` to `paths` over `ops`.
    pub(crate) fn update(&self, ops: &mut BTreeMap<String, Op>, paths: &BTreeSet<String>) -> Result<()> {
        for p in paths {
            match self.compute(p)? {
                Some(op) => {
                    ops.insert(p.clone(), op);
                }
                None => {
                    ops.remove(p);
                }
            }
        }
        Ok(())
    }

    /// The file-name members of §4.4.
    pub(crate) fn members(&self) -> BTreeSet<String> {
        let mut m = BTreeSet::new();
        for e in self.generation.entries() {
            if matches!(e.mode, EntryMode::Gitlink) || self.excl.excluded(&e.path) {
                continue;
            }
            match RepoPath::try_from(e.path.clone()) {
                Ok(rp) if !self.overlay.hides(&rp) => {
                    m.insert(e.path.clone());
                }
                _ => {}
            }
        }
        for e in &self.overlay.entries {
            if !matches!(e.kind, EntryKind::Deleted) && !self.excl.excluded(e.path.as_str()) {
                m.insert(e.path.as_str().to_string());
            }
        }
        m
    }
}

/// One published state of a view (§4.5).
pub(crate) struct Snap {
    pub(crate) session: SessionId,
    pub(crate) repo: RepoName,
    pub(crate) path: PathBuf,
    pub(crate) root: (u64, u64),
    pub(crate) index: HybridIndex,
    pub(crate) members: BTreeSet<String>,
}

impl Snap {
    pub(crate) fn build(
        generation: &Generation,
        ops: &BTreeMap<String, Op>,
        members: BTreeSet<String>,
        who: (&SessionId, &RepoName, &Path, (u64, u64)),
    ) -> Snap {
        let reader: Arc<IndexReader> = generation.base().reader().clone();
        let mut index = HybridIndex::from_reader(reader.clone(), who.2);
        for (p, op) in ops {
            match op {
                Op::Upsert(b) => index.live.upsert_file(p, b),
                // A tombstone for a path the base never had changes nothing.
                Op::Delete if reader.contains_path(p) => index.live.delete_file(p),
                Op::Delete => {}
            }
        }
        Snap { session: who.0.clone(), repo: who.1.clone(), path: who.2.to_path_buf(), root: who.3, index, members }
    }

    /// §5.5 steps 4–6. The reader used is the one returned with the ids;
    /// `pause` runs between taking it and reading any workspace file.
    pub(crate) fn grep(&self, pattern: &str, ci: bool, pause: &dyn Fn(usize)) -> Result<Vec<Hit>> {
        let plan = tgrep_core::query::build_query_plan(pattern, ci)
            .map_err(|e| err(Code::InvalidInput, format!("invalid pattern: {e}"), "pattern"))?;
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(ci)
            .build()
            .map_err(|e| err(Code::InvalidInput, format!("invalid pattern: {e}"), "pattern"))?;
        let (ids, reader) = self.index.execute_query_with_masks(&plan);
        let mut paths = BTreeSet::new();
        for id in ids {
            if let Some(p) = self.index.resolve_path(id, &reader)
                && self.members.contains(&p)
            {
                paths.insert(p);
            }
        }
        pause(Arc::as_ptr(&reader) as usize);
        let mut hits = Vec::new();
        for p in paths {
            let Ok(rp) = RepoPath::try_from(p.clone()) else { continue };
            let mut f = match strata_apfs::open_rooted_file(&self.path, &rp) {
                Ok(f) => f,
                // Gone, or no longer a regular file (never followed).
                Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotFile | Code::IsDirectory) => continue,
                Err(e) => return Err(e),
            };
            let Some(bytes) = read_once(&mut f)
                .map_err(|e| StrataError::new(Code::Unknown, format!("read {p}: {e}")))?
            else {
                continue;
            };
            for (i, line) in bytes.split(|b| *b == b'\n').enumerate() {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let Ok(text) = std::str::from_utf8(line) else { continue };
                if re.is_match(text) {
                    hits.push(Hit {
                        session: self.session.clone(),
                        repo: self.repo.clone(),
                        path: rp.clone(),
                        line: (i + 1) as u32,
                        text: text.to_string(),
                    });
                }
            }
        }
        Ok(hits)
    }
}

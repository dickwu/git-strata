//! Carrying the session's ignored content into the move's new tree
//! (§5.7.1): seed files, seed caches, build output, empty directories.
//! Entries are cloned (directories whole) or, for symlinks, recreated from
//! their target; nothing is ever captured into the blob store, nothing in
//! the new tree is ever overwritten.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use strata_apfs::{Attrs, CloneKind, FileKind, RenameMode, WalkOptions};
use strata_types::{Code, RepoPath};

use crate::Result;
use crate::sys::{random_hex, rp};

/// The private temp name a carried entry is cloned to under `S`'s root.
pub(crate) const CARRY_TEMP_PREFIX: &str = ".strata-carry-";

/// What one carry run needs.
pub(crate) struct Carry<'a> {
    /// The old workspace (source).
    pub ws: &'a Path,
    /// The new tree (destination, the move staging `S`).
    pub dst: &'a Path,
    /// Paths the projection owns: the old base manifest's paths, the
    /// checkpoint's entry paths and every Renamed `from`.
    pub owned: Vec<&'a str>,
    /// `D`: dependency directories, pruned from the walk.
    pub deps_dirs: Vec<&'a str>,
    /// Seed caches nested inside a dependency directory, carried separately.
    pub nested: Vec<RepoPath>,
    /// Names compare case-folded (a case-insensitive workspace).
    pub fold: bool,
}

struct Keys {
    owned: BTreeSet<String>,
    /// Proper ancestors of the owned paths: walked into.
    above: BTreeSet<String>,
    prune: BTreeSet<String>,
    fold: bool,
}

impl Keys {
    fn key(&self, s: &str) -> String {
        if self.fold { strata_apfs::case_fold_key(s) } else { s.to_string() }
    }
}

enum Outcome {
    Carried,
    Uncarried,
    Skipped,
    Descend,
}

impl Carry<'_> {
    /// Carry everything foreign; `after_walk` runs once after the first walk
    /// (the `move.carry` seam). Returns the `uncarried` paths, sorted.
    pub(crate) fn run(&self, after_walk: &dyn Fn() -> Result<()>) -> Result<Vec<RepoPath>> {
        let mut keys = Keys { owned: BTreeSet::new(), above: BTreeSet::new(), prune: BTreeSet::new(), fold: self.fold };
        for p in &self.owned {
            let k = keys.key(p);
            let mut acc = String::new();
            let comps: Vec<&str> = k.split('/').collect();
            for c in &comps[..comps.len().saturating_sub(1)] {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(c);
                keys.above.insert(acc.clone());
            }
            keys.owned.insert(k);
        }
        for d in &self.deps_dirs {
            let k = keys.key(d);
            keys.prune.insert(k);
        }

        let mut uncarried = BTreeSet::new();
        let found = self.collect(&keys, None)?;
        // Seed caches nested in dependency directories: found after the walk
        // too, so the same race rules hold.
        let mut nested = Vec::new();
        for c in &self.nested {
            match strata_apfs::attrs_of(self.ws, c) {
                Ok(a) => nested.push((c.clone(), a)),
                Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory) => {}
                Err(e) => return Err(e),
            }
        }
        after_walk()?;
        self.process(&keys, found, &mut uncarried)?;
        self.process(&keys, nested, &mut uncarried)?;
        Ok(uncarried.into_iter().collect())
    }

    /// The foreign entries under `start` (`None` = the workspace root).
    fn collect(&self, keys: &Keys, start: Option<&RepoPath>) -> Result<Vec<(RepoPath, Attrs)>> {
        let found: Mutex<Vec<(String, Attrs)>> = Mutex::new(Vec::new());
        let at_root = start.is_none();
        let filter = |p: &Path, a: &Attrs| -> bool {
            let Some(s) = p.to_str() else {
                return false;
            };
            if at_root && s == ".git" {
                return false;
            }
            let k = keys.key(s);
            if keys.prune.contains(&k) {
                return false;
            }
            if keys.above.contains(&k) {
                return matches!(a.kind, FileKind::Dir);
            }
            if keys.owned.contains(&k) {
                return false;
            }
            found.lock().unwrap_or_else(|x| x.into_inner()).push((s.to_string(), a.clone()));
            false
        };
        let opts = WalkOptions { start: start.cloned(), ..Default::default() };
        for item in strata_apfs::walk_with(self.ws, &opts, &filter) {
            item?;
        }
        let v = found.into_inner().unwrap_or_else(|x| x.into_inner());
        Ok(v.into_iter().map(|(s, a)| (rp(s), a)).collect())
    }

    fn process(&self, keys: &Keys, items: Vec<(RepoPath, Attrs)>, uncarried: &mut BTreeSet<RepoPath>) -> Result<()> {
        for (p, a) in items {
            match self.one(&p, &a)? {
                Outcome::Carried | Outcome::Skipped => {}
                Outcome::Uncarried => {
                    uncarried.insert(p);
                }
                Outcome::Descend => {
                    let below = match self.collect(keys, Some(&p)) {
                        Ok(v) => v,
                        // Changed since the walk: gone, or no longer a directory.
                        Err(e) if e.code == Code::NoSuchPath => continue,
                        Err(e) if e.code == Code::NotDirectory => {
                            uncarried.insert(p);
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    self.process(keys, below, uncarried)?;
                }
            }
        }
        Ok(())
    }

    fn one(&self, p: &RepoPath, a: &Attrs) -> Result<Outcome> {
        match strata_apfs::attrs_of(self.dst, p) {
            Ok(there) => {
                return Ok(if matches!(there.kind, FileKind::Dir) && matches!(a.kind, FileKind::Dir) {
                    Outcome::Descend
                } else {
                    Outcome::Uncarried
                });
            }
            Err(e) if e.code == Code::NoSuchPath => {}
            // An ancestor in the new tree is a symlink or not a directory.
            Err(e) if e.code == Code::NotDirectory => return Ok(Outcome::Uncarried),
            Err(e) => return Err(e),
        }
        let kind = match &a.kind {
            FileKind::File => CloneKind::File,
            FileKind::Dir => CloneKind::Dir,
            FileKind::Symlink { .. } => return self.relink(p),
            FileKind::Fifo | FileKind::Socket | FileKind::CharDevice | FileKind::BlockDevice => {
                return Ok(Outcome::Uncarried);
            }
        };
        if a.mode & 0o400 == 0 || (kind == CloneKind::Dir && a.mode & 0o200 == 0) {
            return Ok(Outcome::Uncarried);
        }
        self.parents(p)?;
        let tmp = format!("{CARRY_TEMP_PREFIX}{}", random_hex(8));
        match strata_apfs::clone_from_rooted(self.ws, p, &self.dst.join(&tmp), kind) {
            Ok(()) => {}
            Err(e) if e.code == Code::NoSuchPath => return Ok(Outcome::Skipped),
            Err(e) if matches!(e.code, Code::IsDirectory | Code::NotFile | Code::NotDirectory) => {
                return Ok(Outcome::Uncarried);
            }
            Err(e) => return Err(e),
        }
        if let Err(mut e) = strata_apfs::rename_rooted(self.dst, &rp(tmp.clone()), p, RenameMode::Excl) {
            // `detail.path` names the entry being carried (§5.7.1: a `uchg`
            // entry fails here); the temp name is kept alongside.
            let mut d = match e.detail.take() {
                Some(serde_json::Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            d.insert("temp".into(), serde_json::Value::String(self.dst.join(&tmp).display().to_string()));
            d.insert("path".into(), serde_json::Value::String(p.as_str().to_string()));
            e.detail = Some(serde_json::Value::Object(d));
            return Err(e);
        }
        Ok(Outcome::Carried)
    }

    /// A symlink is recreated from its target, never cloned.
    fn relink(&self, p: &RepoPath) -> Result<Outcome> {
        let target = match strata_apfs::attrs_of(self.ws, p) {
            Ok(Attrs { kind: FileKind::Symlink { target }, .. }) => target,
            Ok(_) => return Ok(Outcome::Uncarried),
            Err(e) if e.code == Code::NoSuchPath => return Ok(Outcome::Skipped),
            Err(e) if e.code == Code::NotDirectory => return Ok(Outcome::Uncarried),
            Err(e) => return Err(e),
        };
        self.parents(p)?;
        strata_apfs::symlink_rooted(self.dst, p, target.as_os_str())?;
        Ok(Outcome::Carried)
    }

    fn parents(&self, p: &RepoPath) -> Result<()> {
        if let Some((parent, _)) = p.as_str().rsplit_once('/') {
            strata_apfs::mkdir_rooted(self.dst, &rp(parent), 0o755, true)?;
        }
        Ok(())
    }
}

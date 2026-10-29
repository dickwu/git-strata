//! rm-followups §1.4 G1/G2: is anybody using a workspace right now? Used by
//! the `Auto` post-publish move, which only moves an idle workspace.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::Result;

/// The whole-root lstat walk (G2): the newest ctime and where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Activity {
    /// When the walk started (ns since the epoch).
    pub t0_ns: i128,
    /// The newest ctime seen, root included (ns since the epoch).
    pub m0_ns: i128,
    pub newest: PathBuf,
    /// Entries walked, root included.
    pub entries: u64,
    /// An entry whose ctime is after `t0`: written during the walk.
    pub late: Option<PathBuf>,
    /// The root's entry names, sorted (G4 compares them, not the root's
    /// ctime, which the swap itself advances).
    pub root_names: Vec<String>,
}

fn ns(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

/// Now, in ns since the epoch (the clean-pass stamps).
pub(crate) fn now_ns() -> i64 {
    i64::try_from(ns(SystemTime::now())).unwrap_or(i64::MAX)
}

fn ctime_ns(md: &fs::Metadata) -> i128 {
    md.ctime() as i128 * 1_000_000_000 + md.ctime_nsec() as i128
}

/// lstat every entry under `root` (and `root`), never following a symlink,
/// `.git` and deps directories included. Any walk error is returned: the
/// caller counts it as "changed", never as "unchanged".
pub fn walk_activity(root: &Path) -> Result<Activity> {
    let t0_ns = ns(SystemTime::now());
    let md = fs::symlink_metadata(root).map_err(|e| walk_error(root, &e))?;
    let mut a = Activity {
        t0_ns,
        m0_ns: ctime_ns(&md),
        newest: root.to_path_buf(),
        entries: 1,
        late: None,
        root_names: Vec::new(),
    };
    let note = |a: &mut Activity, p: &Path, md: &fs::Metadata| {
        let c = ctime_ns(md);
        a.entries += 1;
        if c > a.m0_ns {
            a.m0_ns = c;
            a.newest = p.to_path_buf();
        }
        if c > a.t0_ns && a.late.is_none() {
            a.late = Some(p.to_path_buf());
        }
    };
    if ctime_ns(&md) > t0_ns {
        a.late = Some(root.to_path_buf());
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for ent in fs::read_dir(&dir).map_err(|e| walk_error(&dir, &e))? {
            let ent = ent.map_err(|e| walk_error(&dir, &e))?;
            let p = ent.path();
            if dir == root {
                a.root_names.push(ent.file_name().to_string_lossy().into_owned());
            }
            let md = fs::symlink_metadata(&p).map_err(|e| walk_error(&p, &e))?;
            note(&mut a, &p, &md);
            if md.file_type().is_dir() {
                stack.push(p);
            }
        }
    }
    a.root_names.sort();
    Ok(a)
}

/// The store form of a G2 snapshot (§1.4 G4, r8).
pub fn baseline_of(a: &Activity) -> strata_store::MoveBaseline {
    strata_store::MoveBaseline {
        m0_ns: i64::try_from(a.m0_ns).unwrap_or(i64::MAX),
        t0_ns: i64::try_from(a.t0_ns).unwrap_or(i64::MAX),
        root_names: a.root_names.clone(),
        entries: a.entries,
    }
}

/// What changed in the tree at `root` since baseline `b` (§1.4 G4 step 14,
/// Phase F, the old-tree check): every entry (the root itself excepted)
/// whose ctime is after `b.m0_ns`, relative to `root`, then any root entry
/// name added or removed. Empty = unchanged. A walk error is returned (the
/// caller counts it as changed).
pub fn changed_since(root: &Path, b: &strata_store::MoveBaseline) -> Result<Vec<String>> {
    let m0 = b.m0_ns as i128;
    let mut out: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for ent in fs::read_dir(&dir).map_err(|e| walk_error(&dir, &e))? {
            let ent = ent.map_err(|e| walk_error(&dir, &e))?;
            let p = ent.path();
            if dir == root {
                names.push(ent.file_name().to_string_lossy().into_owned());
            }
            let md = fs::symlink_metadata(&p).map_err(|e| walk_error(&p, &e))?;
            if ctime_ns(&md) > m0 {
                out.push(rel(root, &p));
            }
            if md.file_type().is_dir() {
                stack.push(p);
            }
        }
    }
    names.sort();
    let (old, new): (std::collections::BTreeSet<&String>, std::collections::BTreeSet<&String>) =
        (b.root_names.iter().collect(), names.iter().collect());
    for n in old.symmetric_difference(&new) {
        if !out.contains(n) {
            out.push((*n).clone());
        }
    }
    out.sort();
    Ok(out)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned()
}

fn walk_error(p: &Path, e: &std::io::Error) -> strata_types::StrataError {
    strata_apfs::classify_io_error(e, strata_apfs::IoSide::Read, p)
}

/// Why an `Auto` move is not made now (§1.4); `detail` is for people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Idle {
    pub reason: &'static str,
    pub detail: Value,
}

/// G1 then G2 over the whole workspace `root`: `None` when nobody holds
/// anything under it and nothing under it changed for `quiet`.
pub fn busy_reason(root: &Path, quiet: Duration) -> Result<Option<Idle>> {
    Ok(idle_activity(root, quiet)?.err())
}

/// [`busy_reason`] that returns G2's snapshot when the workspace is idle:
/// the baseline an `Auto` move persists and proves again (§1.4 G4).
pub fn idle_activity(root: &Path, quiet: Duration) -> Result<std::result::Result<Activity, Idle>> {
    let holders = strata_deps::open_files(root)?;
    if !holders.is_empty() {
        let shown: Vec<Value> = holders
            .iter()
            .take(20)
            .map(|h| json!({ "pid": h.pid, "command": h.command, "path": h.path }))
            .collect();
        return Ok(Err(Idle { reason: "open_files", detail: json!({ "holders": shown, "count": holders.len() }) }));
    }
    let a = match walk_activity(root) {
        Ok(a) => a,
        Err(e) => {
            return Ok(Err(Idle {
                reason: "recent_writes",
                detail: json!({ "walk_error": e.message, "code": e.code.as_str() }),
            }));
        }
    };
    if let Some(p) = &a.late {
        return Ok(Err(Idle { reason: "recent_writes", detail: json!({ "path": p, "during_scan": true }) }));
    }
    let age_ns = ns(SystemTime::now()) - a.m0_ns;
    if age_ns < quiet.as_nanos() as i128 {
        return Ok(Err(Idle {
            reason: "recent_writes",
            detail: json!({ "path": a.newest, "age_secs": (age_ns.max(0) / 1_000_000_000) as u64, "quiet_secs": quiet.as_secs() }),
        }));
    }
    Ok(Ok(a))
}

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
}

fn ns(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
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
    let mut a = Activity { t0_ns, m0_ns: ctime_ns(&md), newest: root.to_path_buf(), entries: 1, late: None };
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
            let md = fs::symlink_metadata(&p).map_err(|e| walk_error(&p, &e))?;
            note(&mut a, &p, &md);
            if md.file_type().is_dir() {
                stack.push(p);
            }
        }
    }
    Ok(a)
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
    let holders = strata_deps::open_files(root)?;
    if !holders.is_empty() {
        let shown: Vec<Value> = holders
            .iter()
            .take(20)
            .map(|h| json!({ "pid": h.pid, "command": h.command, "path": h.path }))
            .collect();
        return Ok(Some(Idle { reason: "open_files", detail: json!({ "holders": shown, "count": holders.len() }) }));
    }
    let a = match walk_activity(root) {
        Ok(a) => a,
        Err(e) => {
            return Ok(Some(Idle {
                reason: "recent_writes",
                detail: json!({ "walk_error": e.message, "code": e.code.as_str() }),
            }));
        }
    };
    if let Some(p) = &a.late {
        return Ok(Some(Idle { reason: "recent_writes", detail: json!({ "path": p, "during_scan": true }) }));
    }
    let age_ns = ns(SystemTime::now()) - a.m0_ns;
    if age_ns < quiet.as_nanos() as i128 {
        return Ok(Some(Idle {
            reason: "recent_writes",
            detail: json!({ "path": a.newest, "age_secs": (age_ns.max(0) / 1_000_000_000) as u64, "quiet_secs": quiet.as_secs() }),
        }));
    }
    Ok(None)
}

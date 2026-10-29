//! rm-followups R2/R3: what a session's last refused or failed `rm`, an
//! interrupted teardown and a deferred auto-rebase look like in `status`,
//! `ls` and `status --brief` (store reads plus two `lstat`s; no walk), and
//! the leftovers of removed sessions (one `read_dir` each of `sessions/`
//! and `sessions/.repos/`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strata_store::{Lifecycle, MoveState, RebasePending, RmAttempt, RmOutcome, SessionRecord, Store};
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::recover::Leftover;
use crate::scene::SceneOp;
use crate::sys::lstat;
use crate::Result;

/// The R2/R3 additions of one session (additive response fields; all
/// absent when there is nothing to say).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmNotes {
    /// The last `rm` that did not remove the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rm: Option<LastRm>,
    /// An Unknown session whose teardown (`rm`, or `new`'s rollback)
    /// stopped part way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rm_interrupted: Option<RmInterrupted>,
    /// Deferred post-publish rebases, by repo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rebase_pending: Vec<RebaseNote>,
    /// rm-followups §1.4 G4: pre-move trees of `Auto` moves kept because
    /// they changed after the move (or their baseline is unreadable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_trees: Vec<KeptTree>,
    /// The lines above as a person reads them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl RmNotes {
    pub fn is_empty(&self) -> bool {
        self.last_rm.is_none() && self.rm_interrupted.is_none() && self.rebase_pending.is_empty() && self.kept_trees.is_empty()
    }
}

/// A kept pre-move tree with its late-write notice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptTree {
    pub repo: RepoName,
    pub path: PathBuf,
    /// RFC 3339, when the move finished.
    pub moved_at: String,
    /// How many paths changed (0 for `baseline_missing`).
    pub count: u64,
    /// The first 20 of them, relative to `path`.
    pub paths: Vec<String>,
    /// `late_writes`, `walk_error` or `baseline_missing`.
    pub reason: String,
}

/// The kept trees of `session`: done `Auto` moves with a late-write notice
/// whose tree is still there (store reads plus one `lstat` each).
pub(crate) fn kept_trees(store: &Store, session: &SessionId) -> Result<Vec<KeptTree>> {
    let rows = store.move_activities(session)?;
    if rows.iter().all(|r| r.late_writes.is_none()) {
        return Ok(Vec::new());
    }
    let intents = store.move_intents(session)?;
    let mut out = Vec::new();
    for r in rows {
        let Some(n) = &r.late_writes else { continue };
        let Some(i) = intents.iter().find(|i| i.repo == r.repo && i.n == r.n && i.state == MoveState::Done) else {
            continue;
        };
        let path = i.old_tree.clone().unwrap_or_else(|| i.staging.clone());
        if !present(&path) {
            continue;
        }
        out.push(KeptTree {
            repo: r.repo.clone(),
            path,
            moved_at: n.get("moved_at").and_then(Value::as_str).unwrap_or("").to_string(),
            count: n.get("count").and_then(Value::as_u64).unwrap_or(0),
            paths: n
                .get("paths")
                .and_then(Value::as_array)
                .map(|v| v.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default(),
            reason: n.get("reason").and_then(Value::as_str).unwrap_or("late_writes").to_string(),
        });
    }
    Ok(out)
}

/// "auto-rebase moved REPO at T; N files changed in the pre-move tree
/// afterwards, kept at PATH".
pub(crate) fn kept_line(k: &KeptTree) -> String {
    let at = strata_types::serde_rfc3339::parse(&k.moved_at).map(local_hhmm).unwrap_or_else(|_| "?".into());
    let p = k.path.display();
    if k.reason == "baseline_missing" {
        return format!("auto-rebase moved {} at {at}; its activity baseline is unreadable, the pre-move tree is kept at {p}", k.repo);
    }
    let files = if k.count == 1 { "file" } else { "files" };
    format!("auto-rebase moved {} at {at}; {} {files} changed in the pre-move tree afterwards, kept at {p}", k.repo, k.count)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRm {
    #[serde(with = "strata_types::serde_rfc3339")]
    pub at: SystemTime,
    pub outcome: RmOutcome,
    /// The error code the caller got (`Dirty`, `OpenFiles`, `Unknown`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// e.g. `Dirty (36 paths in admin)`.
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RmInterrupted {
    pub phase: String,
    /// Which of `sessions/<sid>` and `sessions/.repos/<sid>` are still there.
    pub remaining: Vec<PathBuf>,
    pub root_absent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebaseNote {
    pub repo: RepoName,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<u64>,
    pub attempts: u64,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub since: SystemTime,
}

/// `sessions/<sid>` and `sessions/.repos/<sid>`.
pub(crate) fn session_dirs(root: &Path, rec: &SessionRecord) -> [PathBuf; 2] {
    let id = rec.manifest.id.as_str();
    [rec.manifest.root.clone(), root.join("sessions/.repos").join(id)]
}

fn present(p: &Path) -> bool {
    matches!(lstat(p), Ok(Some(_)))
}

/// The interrupted teardown of an Unknown session with a Remove scene.
pub(crate) fn interrupted(root: &Path, rec: &SessionRecord) -> Option<RmInterrupted> {
    if rec.lifecycle != Lifecycle::Unknown {
        return None;
    }
    let scene = rec.scene.as_ref()?;
    let op: SceneOp = serde_json::from_value(scene.get("op")?.clone()).ok()?;
    if op != SceneOp::Remove {
        return None;
    }
    let phase = scene.get("phase").and_then(Value::as_str).unwrap_or("?").to_string();
    let [r, repos] = session_dirs(root, rec);
    let root_absent = !present(&r);
    let remaining = [r, repos].into_iter().filter(|p| present(p)).collect();
    Some(RmInterrupted { phase, remaining, root_absent })
}

/// "rm interrupted at phase X; remaining: …; needs `git-strata rm --discard <sid>`".
pub(crate) fn interrupted_line(id: &SessionId, i: &RmInterrupted) -> String {
    let rel = |p: &PathBuf| -> String {
        let s = p.to_string_lossy();
        match s.find("/sessions/") {
            Some(k) => s[k + 1..].to_string(),
            None => s.into_owned(),
        }
    };
    let mut remaining: Vec<String> = i.remaining.iter().map(rel).collect();
    if remaining.is_empty() {
        remaining.push("none".into());
    }
    let absent = if i.root_absent { " (root absent)" } else { "" };
    format!(
        "rm interrupted at phase {}; remaining: {}{absent}; needs `git-strata rm --discard {id}`",
        i.phase,
        remaining.join(", ")
    )
}

/// `HH:MM TZ` in the local zone (the daemon and the CLI share the machine).
pub fn local_hhmm(t: SystemTime) -> String {
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as libc::time_t,
        Err(_) => return "?".into(),
    };
    // SAFETY: localtime_r/strftime write into the buffers given and keep no
    // pointers; `tm` is plain data.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return "?".into();
        }
        let mut buf = [0u8; 64];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%H:%M %Z".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// The short form of a stored rm error: `Dirty (36 paths in admin)`.
pub(crate) fn attempt_summary(a: &RmAttempt) -> (Option<String>, String) {
    let code = a.detail.get("code").and_then(Value::as_str).map(str::to_string);
    let d = a.detail.get("detail").cloned().unwrap_or(Value::Null);
    let c = code.clone().unwrap_or_else(|| "error".into());
    let extra = match (c.as_str(), d.get("reason").and_then(Value::as_str)) {
        ("Dirty", Some("changed_during_remove")) => "changed while it was being removed".to_string(),
        ("Dirty", _) => {
            let mut parts = Vec::new();
            if let Some(m) = d.get("repos").and_then(Value::as_object) {
                for (repo, v) in m {
                    let n = v.get("count").and_then(Value::as_u64).unwrap_or(0);
                    parts.push(format!("{n} {} in {repo}", if n == 1 { "path" } else { "paths" }));
                }
            }
            if d.get("unresolved").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
                parts.push("unresolved scenes".into());
            }
            if d.get("old_trees").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
                parts.push("changed old trees".into());
            }
            parts.join(", ")
        }
        ("OpenFiles", _) => {
            let pids: BTreeSet<i64> = d
                .get("open")
                .and_then(Value::as_array)
                .map(|v| v.iter().filter_map(|o| o.get("pid").and_then(Value::as_i64)).collect())
                .unwrap_or_default();
            if pids.is_empty() {
                String::new()
            } else {
                format!("held by pid {}", pids.iter().map(i64::to_string).collect::<Vec<_>>().join(", "))
            }
        }
        _ => d.get("phase").and_then(Value::as_str).map(|p| format!("at phase {p}")).unwrap_or_default(),
    };
    let s = if extra.is_empty() { c } else { format!("{c} ({extra})") };
    (code, s)
}

/// The R2/R3 notes of a live session (`root`: the strata root).
pub fn rm_notes(store: &Store, root: &Path, rec: &SessionRecord) -> Result<RmNotes> {
    let id = &rec.manifest.id;
    let mut out = RmNotes::default();
    if let Some(a) = store.rm_attempts(Some(id))?.into_iter().next() {
        let (code, summary) = attempt_summary(&a);
        let tail = match a.outcome {
            RmOutcome::Refused => "; nothing removed",
            RmOutcome::Failed => "",
        };
        let verb = match a.outcome {
            RmOutcome::Refused => "refused",
            RmOutcome::Failed => "failed",
        };
        out.notes.push(format!("last rm {} {verb}: {summary}{tail}", local_hhmm(a.at)));
        out.last_rm = Some(LastRm { at: a.at, outcome: a.outcome, code, summary });
    }
    if let Some(i) = interrupted(root, rec) {
        out.notes.push(interrupted_line(id, &i));
        out.rm_interrupted = Some(i);
    }
    for p in store.rebase_pending(Some(id))? {
        out.notes.push(rebase_line(&p));
        out.rebase_pending.push(RebaseNote {
            repo: p.repo.clone(),
            reason: p.reason.clone(),
            publish: publish_of(&p),
            attempts: p.attempts,
            since: p.first_at,
        });
    }
    for k in kept_trees(store, id)? {
        out.notes.push(kept_line(&k));
        out.kept_trees.push(k);
    }
    Ok(out)
}

/// The publish a row is behind: its committed target's, else the planned
/// one.
fn publish_of(p: &RebasePending) -> Option<u64> {
    let committed = p.detail.as_ref().and_then(|d| d.get("publish")).and_then(serde_json::Value::as_u64);
    if p.head.is_some() { committed.or(p.planned_publish) } else { p.planned_publish.or(committed) }
}

/// rm-followups §1.6 status: "behind publish N of REPO: auto-rebase
/// deferred (reason, since HH:MM, attempts k)" for a committed target,
/// "waiting for publish N of REPO" when only a publish is planned.
pub(crate) fn rebase_line(p: &RebasePending) -> String {
    let behind = match publish_of(p) {
        Some(n) => format!("behind publish {n} of {}", p.repo),
        None => format!("behind origin of {}", p.repo),
    };
    if p.head.is_none() {
        return match (p.planned_publish, p.planned_head.is_some()) {
            (Some(n), _) => format!("waiting for publish {n} of {}", p.repo),
            (None, true) => format!("waiting for the replay of {}", p.repo),
            (None, false) => format!("{behind}: {}", p.reason),
        };
    }
    let since = local_hhmm(p.first_at);
    if p.attempts == 0 {
        return format!("{behind}: auto-rebase pending ({}, since {since})", p.reason);
    }
    format!("{behind}: auto-rebase deferred ({}, since {since}, attempts {})", p.reason, p.attempts)
}

/// R3: the top-level `sessions/<sid>` and `sessions/.repos/<sid>`
/// directories whose session is Removed or has no row (R1's orphans). One
/// `read_dir` of each (the entry type from `d_type`, no per-entry stat),
/// then one store read per distinct candidate id (a session's two
/// directories share it); non-session names and symlinks are skipped.
pub fn removed_leftovers(store: &Store, root: &Path) -> Result<Vec<Leftover>> {
    let mut candidates: std::collections::BTreeMap<SessionId, Vec<PathBuf>> = std::collections::BTreeMap::new();
    for dir in [root.join("sessions"), root.join("sessions/.repos")] {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            let Ok(id) = SessionId::try_from(name) else { continue };
            candidates.entry(id).or_default().push(e.path());
        }
    }
    let mut out = Vec::new();
    for (id, paths) in candidates {
        let gone = match store.session(&id)? {
            None => true,
            Some(r) => r.lifecycle == Lifecycle::Removed,
        };
        if gone {
            out.extend(paths.into_iter().map(|path| Leftover::RemovedSession { session: id.clone(), path }));
        }
    }
    out.sort_by(|a, b| match (a, b) {
        (Leftover::RemovedSession { path: x, .. }, Leftover::RemovedSession { path: y, .. }) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(out)
}

/// The leftover directories of `id` when it names a removed (or unknown)
/// session; empty otherwise.
pub fn removed_leftover_of(store: &Store, root: &Path, id: &str) -> Result<Vec<PathBuf>> {
    let Ok(sid) = SessionId::try_from(id.to_string()) else { return Ok(Vec::new()) };
    Ok(removed_leftovers(store, root)?
        .into_iter()
        .filter_map(|l| match l {
            Leftover::RemovedSession { session, path } if session == sid => Some(path),
            _ => None,
        })
        .collect())
}

/// "leftover of removed session s-xxxx: <path> (moved to trash by the
/// next gc or daemon start)".
pub fn leftover_line(session: &str, path: &Path) -> String {
    format!(
        "leftover of removed session {session}: {} (moved to trash by the next gc or daemon start)",
        path.display()
    )
}

/// R3: a `NoSuchSession` for `selector` whose id still has directories
/// under `sessions/` gets them as `detail.leftover` and in the message.
/// Any other error comes back as it was.
pub fn with_leftover_detail(store: &Store, root: &Path, selector: &str, mut e: StrataError) -> StrataError {
    if e.code != Code::NoSuchSession {
        return e;
    }
    let Ok(paths) = removed_leftover_of(store, root, selector) else { return e };
    if paths.is_empty() {
        return e;
    }
    let lines: Vec<String> = paths.iter().map(|p| leftover_line(selector, p)).collect();
    e.message = format!("{}; {}", e.message, lines.join("; "));
    let mut d = match e.detail.take() {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    d.insert("session".into(), Value::String(selector.to_string()));
    d.insert("leftover".into(), serde_json::to_value(&paths).unwrap_or(Value::Null));
    e.detail = Some(Value::Object(d));
    e
}

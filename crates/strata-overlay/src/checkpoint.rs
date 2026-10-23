//! §5.6 `checkpoint`: scan, store the blobs the latest checkpoint does not
//! already reference (with `put_blob_rooted` as the storing hasher and the
//! write-race rules), record one row.

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::time::SystemTime;

use strata_apfs::{Attrs, FileKind, attrs_of};
use strata_store::Lifecycle;
use strata_types::{
    BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, EntryKind, OverlayEntry, RepoPath,
    StrataError,
};

use crate::classify::{changed, present};
use crate::compute::{Pass, begin, push_warn, rejudge, settle};
use crate::context::{Base, WorkspaceRef};
use crate::hash::{Cached, git_mode, is_shape_error};
use crate::overlay::Overlay;
use crate::tracker::Tracker;
use crate::{DEFAULT_DIRTY_CAPACITY, OverlayWarning, Result, ScanStats};

/// Rounds of "put, judge the raced paths again" before a checkpoint gives up
/// with `Busy` (the workspace kept changing under every put).
const MAX_PUT_ROUNDS: u32 = 8;

/// What `checkpoint` returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpointed {
    /// `recorded`: the checkpoint written now (its id assigned by the store).
    /// Otherwise base and entries equal the latest checkpoint, which this is;
    /// nothing was written.
    pub checkpoint: Checkpoint,
    pub recorded: bool,
    /// Paths whose entry differs from the latest checkpoint before this one.
    pub changed: Vec<RepoPath>,
    pub warnings: Vec<OverlayWarning>,
    pub stats: ScanStats,
}

fn invalid(reason: &str, msg: String) -> StrataError {
    StrataError::new(Code::InvalidInput, msg).with_detail(serde_json::json!({ "reason": reason }))
}

fn io_error(e: &std::io::Error, p: &std::path::Path) -> StrataError {
    let code = if e.kind() == std::io::ErrorKind::NotFound {
        Code::NoSuchPath
    } else {
        Code::Unknown
    };
    StrataError::new(code, format!("{}: {e}", p.display())).with_detail(serde_json::json!({
        "path": p.to_string_lossy(),
        "errno": e.raw_os_error(),
    }))
}

/// §5.6 step 1 (reads the Store only).
fn validate(ws: &WorkspaceRef, base: &Base) -> Result<()> {
    let rec = match ws.store.session(ws.session)? {
        Some(r) if r.lifecycle != Lifecycle::Removed => r,
        _ => {
            return Err(StrataError::new(
                Code::NoSuchSession,
                format!("no such session: {}", ws.session.as_str()),
            )
            .with_detail(serde_json::json!({ "session": ws.session.as_str() })));
        }
    };
    let Some(w) = rec.manifest.repos.get(ws.repo) else {
        return Err(StrataError::new(
            Code::NoSuchRepo,
            format!(
                "session {} has no repo {}",
                ws.session.as_str(),
                ws.repo.as_str()
            ),
        )
        .with_detail(serde_json::json!({
            "session": ws.session.as_str(),
            "repo": ws.repo.as_str(),
        })));
    };
    if !w.tracked {
        return Err(invalid(
            "untracked",
            format!(
                "workspace {}/{} is not tracked",
                ws.session.as_str(),
                ws.repo.as_str()
            ),
        ));
    }
    if w.base != base.r#gen {
        return Err(invalid(
            "stale_base",
            format!(
                "workspace {}/{} is on another base generation",
                ws.session.as_str(),
                ws.repo.as_str()
            ),
        ));
    }
    let a = std::fs::metadata(ws.root).map_err(|e| io_error(&e, ws.root))?;
    let b = std::fs::metadata(&w.path).map_err(|e| io_error(&e, &w.path))?;
    if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
        return Err(invalid(
            "wrong_root",
            format!(
                "{} is not the workspace directory {}",
                ws.root.display(),
                w.path.display()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn blob_of(e: &OverlayEntry) -> Option<&BlobId> {
    match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Untracked { blob }
        | EntryKind::Renamed { blob, .. } => Some(blob),
        EntryKind::Deleted => None,
    }
}

fn sorted(mut v: Vec<OverlayEntry>) -> Vec<OverlayEntry> {
    v.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
    v
}

fn same_state(latest: &Checkpoint, base: &Base, overlay: &Overlay) -> bool {
    latest.base == base.r#gen && sorted(latest.entries.clone()) == sorted(overlay.entries.clone())
}

/// The outcome of storing one entry's path.
enum Put {
    /// The stored content is the scanned blob, unchanged around the put.
    Same,
    /// The file changed after the scan: `id` is its content now (a complete
    /// clone); `stable` = unchanged around the last put.
    Changed {
        id: BlobId,
        attrs: Attrs,
        stable: bool,
    },
    /// Gone, a directory, a special file, or an ancestor is no directory:
    /// judge the path again.
    Race,
}

fn stable(a1: &Attrs, a2: &Attrs) -> bool {
    match a2.kind {
        // A symlink's content is its target, part of the kind.
        FileKind::Symlink { .. } => a1.kind == a2.kind && a1.inode == a2.inode,
        _ => a2.unchanged_since(a1),
    }
}

fn shape<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_shape_error(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// §5.6 step 5 for one path: A1 → put → A2, once more when it raced.
fn put_one(ws: &WorkspaceRef, p: &RepoPath, blob: &BlobId) -> Result<Put> {
    let mut last = None;
    for attempt in 0..2 {
        let Some(a1) = shape(attrs_of(ws.root, p))? else {
            return Ok(Put::Race);
        };
        let Some(id) = shape(ws.store.put_blob_rooted(ws.root, p))? else {
            return Ok(Put::Race);
        };
        let Some(a2) = shape(attrs_of(ws.root, p))? else {
            return Ok(Put::Race);
        };
        if git_mode(&a2).is_none() {
            return Ok(Put::Race);
        }
        let ok = stable(&a1, &a2);
        if attempt == 0 && ok && id == *blob {
            return Ok(Put::Same);
        }
        let symlink = matches!(a2.kind, FileKind::Symlink { .. });
        if !symlink && !a2.gen_count_known() {
            // A writable mapping: read once, never retried, Unstable (§5.2 step 6).
            return Ok(if id == *blob {
                Put::Same
            } else {
                Put::Changed {
                    id,
                    attrs: a2,
                    stable: false,
                }
            });
        }
        if attempt == 1 && ok {
            return Ok(Put::Changed {
                id,
                attrs: a2,
                stable: true,
            });
        }
        last = Some((id, a2));
    }
    let (id, attrs) = last.expect("two attempts ran");
    Ok(Put::Changed {
        id,
        attrs,
        stable: false,
    })
}

/// The put's content replaces the scanned one in the raw difference.
fn take_put(base: &Base, pass: &mut Pass, p: &RepoPath, id: BlobId, attrs: Attrs, stable: bool) {
    let mode = git_mode(&attrs).expect("checked by put_one");
    let staged = pass.f.info.is_tracked(p);
    let st = &mut pass.st;
    st.raw.remove(p);
    if let Some(r) = present(&base.manifest, p, id.clone(), mode, staged) {
        st.raw.insert(p.clone(), r);
    }
    if stable {
        st.cache.insert(p.clone(), Cached { attrs, blob: id });
        st.unstable.remove(p);
        if let Some(v) = st.warn.get_mut(p) {
            v.retain(|w| !matches!(w, OverlayWarning::Unstable { .. }));
        }
    } else {
        st.cache.remove(p);
        st.unstable.insert(p.clone());
        push_warn(
            &mut st.warn,
            p,
            OverlayWarning::Unstable { path: p.clone() },
        );
    }
}

/// §5.6 step 5: store every blob `stored` lacks; raced paths are judged
/// again and the overlay reclassified until every entry's blob is stored.
fn put_blobs(
    ws: &WorkspaceRef,
    base: &Base,
    pass: &mut Pass,
    mut stored: BTreeSet<BlobId>,
) -> Result<Overlay> {
    for _ in 0..MAX_PUT_ROUNDS {
        let overlay = settle(base, pass);
        let todo: Vec<(RepoPath, BlobId)> = overlay
            .entries
            .iter()
            .filter_map(|e| blob_of(e).map(|b| (e.path.clone(), b.clone())))
            .filter(|(_, b)| !stored.contains(b))
            .collect();
        if todo.is_empty() {
            return Ok(overlay);
        }
        let mut again = BTreeSet::new();
        for (p, blob) in todo {
            match put_one(ws, &p, &blob)? {
                Put::Same => {
                    stored.insert(blob);
                }
                Put::Changed { id, attrs, stable } => {
                    stored.insert(id.clone());
                    take_put(base, pass, &p, id, attrs, stable);
                }
                Put::Race => {
                    again.insert(p);
                }
            }
        }
        if !again.is_empty() {
            rejudge(ws, base, pass, again)?;
        }
    }
    Err(StrataError::new(
        Code::Busy,
        format!(
            "{} kept changing under every put ({MAX_PUT_ROUNDS} rounds)",
            ws.root.display()
        ),
    )
    .with_detail(serde_json::json!({ "reason": "workspace_changing" })))
}

/// §5.6: record the workspace's overlay as a checkpoint, storing only blobs
/// the latest checkpoint does not reference. Never writes the workspace.
pub fn checkpoint(
    ws: &WorkspaceRef,
    base: &Base,
    tracker: Option<&mut Tracker>,
    reason: CheckpointReason,
) -> Result<Checkpointed> {
    validate(ws, base)?;
    let mut own;
    let tracker = match tracker {
        Some(t) => t,
        None => {
            own = Tracker::new(ws.root, DEFAULT_DIRTY_CAPACITY);
            &mut own
        }
    };
    let dirty: Option<&[RepoPath]> = match reason {
        CheckpointReason::ToolCall { .. } => Some(&[]),
        _ => None,
    };
    let mut pass = begin(ws, base, tracker, dirty)?;
    match finish(ws, base, &mut pass, reason) {
        Ok(out) => {
            tracker.st = pass.st;
            Ok(out)
        }
        Err(failed) => {
            let (e, scanned) = *failed;
            // The tracker keeps its old state; what the scan saw goes back
            // into the inbox with the snapshot (§5.6, 3c:43).
            let mut snap = pass.snap;
            if let Some(o) = scanned {
                let mut seen: BTreeSet<RepoPath> =
                    changed(tracker.st.last.as_ref(), &o).into_iter().collect();
                for e in &o.entries {
                    seen.insert(e.path.clone());
                    if let EntryKind::Renamed { from, .. } = &e.kind {
                        seen.insert(from.clone());
                    }
                }
                snap.paths.extend(seen);
            }
            tracker.inbox.merge(snap);
            Err(e)
        }
    }
}

/// The error plus what the scan saw (boxed: it is large).
type Failed = Box<(StrataError, Option<Overlay>)>;

/// §5.6 steps 3-7 on a scanned pass.
fn finish(
    ws: &WorkspaceRef,
    base: &Base,
    pass: &mut Pass,
    reason: CheckpointReason,
) -> std::result::Result<Checkpointed, Failed> {
    let scanned = settle(base, pass);
    let fail = |e: StrataError, o: &Overlay| Box::new((e, Some(o.clone())));
    let latest = ws
        .store
        .latest_checkpoint(ws.session, ws.repo)
        .map_err(|e| fail(e, &scanned))?;
    let may_skip = matches!(
        reason,
        CheckpointReason::ToolCall { .. } | CheckpointReason::Manual { .. }
    );
    let out =
        |cp: Checkpoint, recorded: bool, o: &Overlay, pass: &Pass, prev: Option<&Checkpoint>| {
            let prev = prev.map(Overlay::from_checkpoint);
            Checkpointed {
                checkpoint: cp,
                recorded,
                changed: changed(prev.as_ref(), o),
                warnings: pass.st.warnings.clone(),
                stats: pass.stats,
            }
        };
    if may_skip
        && let Some(l) = &latest
        && same_state(l, base, &scanned)
    {
        return Ok(out(l.clone(), false, &scanned, pass, Some(l)));
    }
    crate::fault::before_put(ws.root);
    let stored: BTreeSet<BlobId> = latest
        .iter()
        .flat_map(|l| l.entries.iter().filter_map(blob_of).cloned())
        .collect();
    let overlay = put_blobs(ws, base, pass, stored).map_err(|e| fail(e, &scanned))?;
    // A raced put may have brought the overlay back to the latest checkpoint.
    if may_skip
        && let Some(l) = &latest
        && same_state(l, base, &overlay)
    {
        return Ok(out(l.clone(), false, &overlay, pass, Some(l)));
    }
    let placeholder = CheckpointId::from(0_i64);
    let cp = Checkpoint {
        id: placeholder,
        session: ws.session.clone(),
        repo: ws.repo.clone(),
        base: base.r#gen.clone(),
        at: SystemTime::now(),
        reason,
        entries: overlay.entries.clone(),
    };
    let id = ws
        .store
        .record_checkpoint(cp.clone())
        .map_err(|e| fail(e, &overlay))?;
    if let Some(l) = &latest
        && id <= l.id
    {
        // The store's dedup of one ToolCall id: the row already there.
        let existing = ws
            .store
            .checkpoint(ws.session, id)
            .map_err(|e| fail(e, &overlay))?;
        return Ok(out(existing, false, &overlay, pass, Some(l)));
    }
    let cp = Checkpoint { id, ..cp };
    Ok(out(cp, true, &overlay, pass, latest.as_ref()))
}

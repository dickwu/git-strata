//! Path-level merge of a release group onto B and the staging directory
//! (publish.md §5.3, D4, D5, D12). The merge is decided in memory first;
//! no directory exists until it is clean and the tree differs from B.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use strata_apfs::RemoveKind;
use strata_gitio::{Git, Merge3, TreeItem, is_binary, merge3};
use strata_overlay::{LwwRules, MERGE_MAX_BYTES, lww_matches};
use strata_store::Store;
use strata_types::{
    Batch, Checkpoint, CheckpointId, Code, CommitOid, EntryKind, OverlayEntry, ReleaseGroup, RepoPath, SessionId,
    StrataError, TreeOid,
};

use crate::{Ctx, Result};

/// Git modes publish writes (§5.3): nothing else reaches a commit.
pub(crate) const MODE_FILE: u32 = 0o100644;
pub(crate) const MODE_EXEC: u32 = 0o100755;
pub(crate) const MODE_LINK: u32 = 0o120000;
/// D5: hunk text cap per side.
pub const HUNK_TEXT_CAP: usize = 4096;
/// Symlink targets are read whole; anything longer is not a link target.
const LINK_MAX: u64 = 64 * 1024;
const LWW_FILE: &str = ".strata/lww";
const LWW_MAX_FILES: usize = 16;
const LWW_MAX_BYTES: u64 = 1024 * 1024;

/// A staged release (§3): the layout's repo directory, its private git dir,
/// the tree `gitio::stage` wrote and the paths it staged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub worktree: PathBuf,
    pub gitdir: PathBuf,
    pub tree: TreeOid,
    pub paths: Vec<RepoPath>,
}

impl Staged {
    /// The layout root (`<session root>/.strata-publish-<repo>-<id>`).
    pub fn layout_root(&self) -> &Path {
        self.worktree.parent().expect("layout root")
    }
}

/// One version of a path: a file or symlink with its git mode and
/// whichever identities are known (BLAKE3 from the store, git oid from a
/// tree); bytes are read lazily, never above `MERGE_MAX_BYTES`.
#[derive(Clone, Debug)]
pub(crate) struct Ver {
    pub mode: u32,
    pub blake: Option<String>,
    pub oid: Option<String>,
    pub size: Option<u64>,
    /// A pinned base directory and the path below it (the base side).
    pub file: Option<(PathBuf, RepoPath)>,
    pub data: Option<Arc<Vec<u8>>>,
}

impl Ver {
    fn from_bytes(mode: u32, bytes: Vec<u8>) -> Ver {
        let blake = blake3::hash(&bytes).to_hex().to_string();
        Ver { mode, blake: Some(blake), oid: None, size: Some(bytes.len() as u64), file: None, data: Some(Arc::new(bytes)) }
    }
    pub(crate) fn is_link(&self) -> bool {
        self.mode == MODE_LINK
    }
}

/// Only `100644`, `100755` and `120000` (§5.3; special bits never written).
pub(crate) fn git_mode(mode: u32) -> u32 {
    if mode & 0o170000 == 0o120000 {
        MODE_LINK
    } else if mode & 0o111 != 0 {
        MODE_EXEC
    } else {
        MODE_FILE
    }
}

/// Where the bytes come from.
pub(crate) struct Src<'a> {
    pub store: &'a Store,
    pub git: &'a Git,
}

impl Src<'_> {
    /// The bytes of `v`, or `None` when it is larger than `MERGE_MAX_BYTES`
    /// (never read).
    pub(crate) fn load(&self, v: &Ver) -> Result<Option<Arc<Vec<u8>>>> {
        if let Some(d) = &v.data {
            return Ok(Some(d.clone()));
        }
        if v.size.is_some_and(|s| s > MERGE_MAX_BYTES) {
            return Ok(None);
        }
        if let Some((root, p)) = &v.file {
            return read_rooted(root, p).map(|b| Some(Arc::new(b)));
        }
        if let Some(b) = &v.blake {
            let id = strata_types::BlobId::try_from(b.clone())?;
            return match self.store.read_blob(&id, MERGE_MAX_BYTES) {
                Ok(bytes) => Ok(Some(Arc::new(bytes))),
                Err(e) if e.code == Code::TooLarge => Ok(None),
                Err(e) => Err(e),
            };
        }
        if let Some(o) = &v.oid {
            return Ok(Some(Arc::new(self.git.object_bytes(o)?)));
        }
        Ok(None)
    }

    /// Same content? `None` when it cannot be told without reading an
    /// oversize side (treated as different by callers).
    pub(crate) fn same_content(&self, a: &Ver, b: &Ver) -> Result<Option<bool>> {
        if let (Some(x), Some(y)) = (&a.blake, &b.blake) {
            return Ok(Some(x == y));
        }
        if let (Some(x), Some(y)) = (&a.oid, &b.oid) {
            return Ok(Some(x == y));
        }
        if let (Some(x), Some(y)) = (a.size, b.size)
            && x != y
        {
            return Ok(Some(false));
        }
        match (self.load(a)?, self.load(b)?) {
            (Some(x), Some(y)) => Ok(Some(x == y)),
            _ => Ok(None),
        }
    }

    pub(crate) fn same(&self, a: Option<&Ver>, b: Option<&Ver>) -> Result<bool> {
        Ok(match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a.mode == b.mode && self.same_content(a, b)? == Some(true),
            _ => false,
        })
    }
}

/// The bytes of `root/p` (a pinned base), never following a symlink; a
/// symlink reads as its target.
pub(crate) fn read_rooted(root: &Path, p: &RepoPath) -> Result<Vec<u8>> {
    let full = root.join(p.as_str());
    let meta = std::fs::symlink_metadata(&full)
        .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &full))?;
    if meta.file_type().is_symlink() {
        return std::fs::read_link(&full)
            .map(|t| t.as_os_str().as_bytes().to_vec())
            .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &full));
    }
    let mut f = strata_apfs::open_rooted_file(root, p)?;
    let mut out = Vec::with_capacity(meta.len() as usize);
    std::io::Read::read_to_end(&mut f, &mut out)
        .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &full))?;
    Ok(out)
}

/// Who wrote the current result of a path (for `.lww`).
#[derive(Clone, Debug)]
struct Prov {
    session: SessionId,
    checkpoint: CheckpointId,
}

#[derive(Clone, Debug)]
struct Slot {
    ver: Option<Ver>,
    prov: Option<Prov>,
}

/// The in-memory merge result (§5.3): every path whose version differs
/// from B, and the session whose root holds the layout (D4).
#[derive(Debug)]
pub(crate) struct Plan {
    pub b_tree: TreeOid,
    pub changed: BTreeMap<RepoPath, Option<Ver>>,
    pub layout_session: SessionId,
}

fn invalid(reason: &str, msg: String, mut detail: Value) -> StrataError {
    detail["reason"] = json!(reason);
    StrataError::new(Code::InvalidInput, msg).with_detail(detail)
}

fn is_under(p: &str, dir: &str) -> bool {
    p.len() > dir.len() && p.starts_with(dir) && p.as_bytes()[dir.len()] == b'/'
}

fn related(a: &str, b: &str) -> bool {
    is_under(a, b) || is_under(b, a)
}

/// The selected entries of one batch (§5.3 selection rules).
fn select<'c>(cp: &'c Checkpoint, batch: &Batch) -> Result<Vec<&'c OverlayEntry>> {
    let from_of = |e: &OverlayEntry| match &e.kind {
        EntryKind::Renamed { from, .. } => Some(from.as_str().to_string()),
        _ => None,
    };
    let mut chosen: BTreeSet<usize> = BTreeSet::new();
    for p in &batch.paths {
        let p = p.as_str();
        let exact: Vec<usize> = cp
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.path.as_str() == p || from_of(e).as_deref() == Some(p))
            .map(|(i, _)| i)
            .collect();
        let hits = if exact.is_empty() {
            cp.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| is_under(e.path.as_str(), p) || from_of(e).is_some_and(|f| is_under(&f, p)))
                .map(|(i, _)| i)
                .collect()
        } else {
            exact
        };
        if hits.is_empty() {
            return Err(StrataError::new(
                Code::NoMatch,
                format!("{p} matches no entry of checkpoint {} of session {}", cp.id.get(), cp.session.as_str()),
            )
            .with_detail(json!({
                "path": p, "session": cp.session.as_str(), "checkpoint": cp.id.get(), "repo": cp.repo.as_str(),
            })));
        }
        chosen.extend(hits);
    }
    let selected: Vec<&OverlayEntry> = chosen.iter().map(|&i| &cp.entries[i]).collect();
    // A directory↔file switch is a pair that must travel together.
    let names = |e: &OverlayEntry| -> Vec<String> {
        let mut v = vec![e.path.as_str().to_string()];
        v.extend(from_of(e));
        v
    };
    let sel_names: Vec<String> = selected.iter().flat_map(|e| names(e)).collect();
    let mut missing = Vec::new();
    for (i, e) in cp.entries.iter().enumerate() {
        if chosen.contains(&i) {
            continue;
        }
        for n in names(e) {
            if let Some(s) = sel_names.iter().find(|s| related(s, &n)) {
                missing.push(json!({ "selected": s, "missing": n }));
            }
        }
    }
    if !missing.is_empty() {
        return Err(StrataError::new(
            Code::Conflict,
            "a directory/file switch must be published as a pair in one batch",
        )
        .with_detail(json!({
            "reason": "type_pair", "session": cp.session.as_str(), "checkpoint": cp.id.get(), "pairs": missing,
        })));
    }
    Ok(selected)
}

/// `.strata/lww` and its `%include`s, read from B through `blob_bytes`.
fn lww_rules(git: &Git, tree: &TreeOid) -> Result<LwwRules> {
    let mut text = String::new();
    let mut budget = LWW_MAX_FILES;
    lww_into(git, tree, LWW_FILE, &mut budget, &mut text)?;
    Ok(LwwRules::parse(&text))
}

fn lww_into(git: &Git, tree: &TreeOid, rel: &str, budget: &mut usize, out: &mut String) -> Result<()> {
    if *budget == 0 {
        return Ok(());
    }
    *budget -= 1;
    let Ok(path) = RepoPath::try_from(rel.to_string()) else { return Ok(()) };
    let items = git.tree_items(tree, std::slice::from_ref(&path))?;
    let Some(item) = items.get(path.as_str()) else { return Ok(()) };
    if item.kind != "blob" || item.mode == MODE_LINK || item.size > LWW_MAX_BYTES {
        return Ok(());
    }
    let Ok(text) = String::from_utf8(git.object_bytes(&item.oid)?) else { return Ok(()) };
    for line in text.lines() {
        let t = line.trim_end_matches('\r').trim();
        if let Some(rest) = t.strip_prefix("%include")
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            let inc = rest.trim();
            if !inc.is_empty() {
                lww_into(git, tree, inc.trim_start_matches('/'), budget, out)?;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(())
}

fn tree_ver(i: &TreeItem) -> Option<Ver> {
    (i.kind == "blob").then(|| Ver {
        mode: git_mode(i.mode),
        blake: None,
        oid: Some(i.oid.clone()),
        size: Some(i.size),
        file: None,
        data: None,
    })
}

/// D5: at most `HUNK_TEXT_CAP` bytes, cut at a UTF-8 boundary.
pub(crate) fn cap_text(bytes: &[u8]) -> (String, bool) {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= HUNK_TEXT_CAP {
        return (s.into_owned(), false);
    }
    let mut end = HUNK_TEXT_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

fn hunk_json(h: &strata_gitio::Hunk) -> Value {
    let (base, tb) = cap_text(&h.base);
    let (ours, to) = cap_text(&h.ours);
    let (theirs, tt) = cap_text(&h.theirs);
    json!({
        "base_start": h.base_start, "ours_start": h.ours_start, "theirs_start": h.theirs_start,
        "base": base, "ours": ours, "theirs": theirs, "truncated": tb || to || tt,
    })
}

fn whole_file(ours: Option<&Ver>, theirs: Option<&Ver>) -> Value {
    json!({
        "whole_file": true, "base_start": 1, "ours_start": 1, "theirs_start": 1,
        "base": "", "ours": "", "theirs": "", "truncated": false,
        "ours_size": ours.and_then(|v| v.size), "theirs_size": theirs.and_then(|v| v.size),
    })
}

enum Step {
    Keep,
    Take(Option<Ver>),
    Conflict(&'static str, Vec<Value>),
}

/// Three-way merge of one path (§5.3 byte rules). `lww` → the later side
/// (`ours`, unless `theirs_wins`) takes the whole file.
fn merge_path(src: &Src, base: Option<&Ver>, ours: Option<&Ver>, theirs: Option<&Ver>, lww: Option<bool>) -> Result<Step> {
    let ours_changed = !src.same(base, ours)?;
    if !ours_changed {
        return Ok(Step::Keep);
    }
    let theirs_changed = !src.same(base, theirs)?;
    if !theirs_changed {
        return Ok(Step::Take(ours.cloned()));
    }
    if src.same(ours, theirs)? {
        return Ok(Step::Keep);
    }
    if let Some(theirs_wins) = lww
        && ours.is_some()
        && theirs.is_some()
    {
        return Ok(if theirs_wins { Step::Keep } else { Step::Take(ours.cloned()) });
    }
    let (o, t) = match (ours, theirs) {
        (Some(o), Some(t)) => (o, t),
        _ => return Ok(Step::Conflict("modify_delete", vec![])),
    };
    let Some(b) = base else {
        return Ok(Step::Conflict("add_add", vec![whole_file(ours, theirs)]));
    };
    if o.is_link() || t.is_link() || b.is_link() {
        return Ok(Step::Conflict("link", vec![whole_file(ours, theirs)]));
    }
    // The exec bit and the content are merged separately.
    let mode = match (o.mode != b.mode, t.mode != b.mode) {
        (true, true) if o.mode != t.mode => return Ok(Step::Conflict("mode", vec![])),
        (true, _) => o.mode,
        (false, _) => t.mode,
    };
    let oc = src.same_content(b, o)? != Some(true);
    let tc = src.same_content(b, t)? != Some(true);
    let content = if !oc {
        t.clone()
    } else if !tc || src.same_content(o, t)? == Some(true) {
        o.clone()
    } else {
        let (Some(bb), Some(ob), Some(tb)) = (src.load(b)?, src.load(o)?, src.load(t)?) else {
            return Ok(Step::Conflict("too_large", vec![whole_file(ours, theirs)]));
        };
        if is_binary(&bb) || is_binary(&ob) || is_binary(&tb) {
            return Ok(Step::Conflict("binary", vec![whole_file(ours, theirs)]));
        }
        match merge3(&bb, &ob, &tb) {
            Merge3::Clean(bytes) => Ver::from_bytes(mode, bytes),
            Merge3::Conflict(hunks) => return Ok(Step::Conflict("text", hunks.iter().map(hunk_json).collect())),
        }
    };
    Ok(Step::Take(Some(Ver { mode, ..content })))
}

type Changes = Vec<(RepoPath, Option<Ver>)>;

/// Selection and merge of `group` onto `base` (= B), all in memory. Errors:
/// `NoMatch`, `InvalidInput` (`link_target`), `Conflict` (`type_pair` or
/// `merge`), `NoCommit`. Nothing is written.
pub(crate) fn plan(ctx: &Ctx, group: &ReleaseGroup, base: &CommitOid) -> Result<Plan> {
    if group.batches.is_empty() {
        return Err(StrataError::new(Code::InvalidInput, "a release group needs at least one batch")
            .with_detail(json!({ "reason": "empty_group" })));
    }
    let src = Src { store: ctx.store, git: ctx.git };
    let b_tree = ctx.git.tree_of(base)?;
    // Per batch: its checkpoint and selected changes.
    let mut loaded: Vec<(Checkpoint, Changes)> = Vec::new();
    for b in &group.batches {
        if &b.repo != ctx.repo {
            return Err(StrataError::new(
                Code::InvalidInput,
                format!("batch of session {} is for {}, not {}", b.session.as_str(), b.repo.as_str(), ctx.repo.as_str()),
            )
            .with_detail(json!({ "reason": "repo_mismatch", "repo": b.repo.as_str() })));
        }
        let cp = ctx.store.checkpoint(&b.session, b.checkpoint)?;
        if &cp.repo != ctx.repo {
            return Err(StrataError::new(Code::NoSuchCheckpoint, "the checkpoint belongs to another repo").with_detail(
                json!({ "session": b.session.as_str(), "checkpoint": b.checkpoint.get(), "repo": cp.repo.as_str() }),
            ));
        }
        let selected = select(&cp, b)?;
        let own: BTreeSet<&str> = cp.entries.iter().map(|e| e.path.as_str()).collect();
        let mut changes = Vec::new();
        for e in selected {
            let ver = match &e.kind {
                EntryKind::Deleted => None,
                EntryKind::Added { blob }
                | EntryKind::Modified { blob }
                | EntryKind::Untracked { blob }
                | EntryKind::Renamed { blob, .. } => Some(Ver {
                    mode: git_mode(e.mode),
                    blake: Some(blob.as_str().to_string()),
                    oid: None,
                    size: None,
                    file: None,
                    data: None,
                }),
            };
            if let Some(v) = &ver
                && v.is_link()
            {
                let id = strata_types::BlobId::try_from(v.blake.clone().unwrap())?;
                let target = ctx.store.read_blob(&id, LINK_MAX)?;
                if std::str::from_utf8(&target).is_err() || target.is_empty() {
                    return Err(invalid(
                        "link_target",
                        format!("the symlink target of {} is not UTF-8", e.path.as_str()),
                        json!({ "path": e.path.as_str(), "session": cp.session.as_str() }),
                    ));
                }
            }
            if let EntryKind::Renamed { from, .. } = &e.kind
                && !own.contains(from.as_str())
            {
                changes.push((from.clone(), None));
            }
            changes.push((e.path.clone(), ver));
        }
        loaded.push((cp, changes));
    }
    // §5.7: a path named by `assign` goes only with the named session.
    let named = crate::rebase::assignments(ctx.store, ctx.repo)?;
    let mut refused: Vec<Value> = Vec::new();
    for (cp, changes) in &loaded {
        for (p, _) in changes {
            if let Some(who) = named.get(p.as_str())
                && *who != cp.session
            {
                refused.push(json!({
                    "path": p.as_str(), "kind": "assigned", "session": cp.session.as_str(),
                    "checkpoint": cp.id.get(), "assigned": who.as_str(), "hunks": [],
                }));
            }
        }
    }
    if !refused.is_empty() {
        let n = refused.len();
        return Err(StrataError::new(Code::Conflict, format!("{n} path(s) are assigned to another session; nothing was published"))
            .with_detail(json!({ "reason": "assigned", "conflicts": refused })));
    }
    // Every touched path, read from B (and descendants: `ls-tree` is recursive).
    let all: Vec<RepoPath> = {
        let s: BTreeSet<RepoPath> = loaded.iter().flat_map(|(_, c)| c.iter().map(|(p, _)| p.clone())).collect();
        s.into_iter().collect()
    };
    let b_items = ctx.git.tree_items(&b_tree, &all)?;
    let rules = lww_rules(ctx.git, &b_tree)?;
    let mut result: BTreeMap<RepoPath, Slot> = BTreeMap::new();
    let mut conflicts: Vec<Value> = Vec::new();
    for (cp, changes) in &loaded {
        let paths: Vec<RepoPath> = changes.iter().map(|(p, _)| p.clone()).collect();
        let manifest = ctx.store.manifest(ctx.repo, &cp.base.tree)?.ok_or_else(|| {
            StrataError::new(Code::Corrupt, "the checkpoint's base manifest is missing")
                .with_detail(json!({ "tree": cp.base.tree.as_str(), "repo": ctx.repo.as_str() }))
        })?;
        let base_items = ctx.git.tree_items(&cp.base.tree, &paths)?;
        let base_dir = ctx.store.base_dir(&cp.base);
        for (p, ours) in changes {
            let base_v = manifest.entries.get(p).map(|m| Ver {
                mode: git_mode(m.mode),
                blake: Some(m.blob.as_str().to_string()),
                oid: base_items.get(p.as_str()).filter(|i| i.kind == "blob").map(|i| i.oid.clone()),
                size: Some(m.size),
                file: Some((base_dir.clone(), p.clone())),
                data: None,
            });
            let cur = result
                .get(p)
                .cloned()
                .unwrap_or_else(|| Slot { ver: b_items.get(p.as_str()).and_then(tree_ver), prov: None });
            let lww = lww_matches(&rules, p).then(|| match &cur.prov {
                Some(pv) if pv.session == cp.session => pv.checkpoint > cp.id,
                _ => false,
            });
            match merge_path(&src, base_v.as_ref(), ours.as_ref(), cur.ver.as_ref(), lww)? {
                Step::Keep => {}
                Step::Take(v) => {
                    result.insert(
                        p.clone(),
                        Slot { ver: v, prov: Some(Prov { session: cp.session.clone(), checkpoint: cp.id }) },
                    );
                }
                Step::Conflict(kind, hunks) => conflicts.push(json!({
                    "path": p.as_str(), "kind": kind, "session": cp.session.as_str(),
                    "checkpoint": cp.id.get(), "hunks": hunks,
                })),
            }
        }
    }
    // A file where B still has a directory with surviving entries.
    let deleted: BTreeSet<&str> =
        result.iter().filter(|(_, s)| s.ver.is_none()).map(|(p, _)| p.as_str()).collect();
    for (p, slot) in &result {
        if slot.ver.is_none() {
            continue;
        }
        let survivors: Vec<&String> = b_items
            .iter()
            .filter(|(q, i)| i.kind == "blob" && is_under(q, p.as_str()))
            .filter(|(q, _)| !deleted.contains(q.as_str()))
            .map(|(q, _)| q)
            .collect();
        if !survivors.is_empty() {
            conflicts.push(json!({
                "path": p.as_str(), "kind": "type", "session": slot.prov.as_ref().map(|v| v.session.as_str()),
                "hunks": [], "below": survivors,
            }));
        }
    }
    if !conflicts.is_empty() {
        let n = conflicts.len();
        return Err(StrataError::new(Code::Conflict, format!("{n} path(s) conflict; nothing was published"))
            .with_detail(json!({ "reason": "merge", "conflicts": conflicts })));
    }
    let mut changed = BTreeMap::new();
    for (p, slot) in result {
        let b = b_items.get(p.as_str()).and_then(tree_ver);
        if !src.same(slot.ver.as_ref(), b.as_ref())? {
            changed.insert(p, slot.ver);
        }
    }
    if changed.is_empty() {
        return Err(no_commit(base));
    }
    let layout_session = group.batches.iter().map(|b| b.session.clone()).min().expect("non-empty");
    Ok(Plan { b_tree, changed, layout_session })
}

pub(crate) fn no_commit(base: &CommitOid) -> StrataError {
    StrataError::new(Code::NoCommit, "the selected paths do not change B; nothing to commit")
        .with_detail(json!({ "base": base.as_str() }))
}

/// D4: `<root of the smallest session id>/.strata-publish-<repo>-<id>`, as a
/// `RepoPath` under `paths.root`.
pub(crate) fn layout_rel(ctx: &Ctx, session: &SessionId, id: u64) -> Result<(PathBuf, RepoPath)> {
    let root = &ctx.deps.paths.root;
    let rec = ctx.store.session(session)?.ok_or_else(|| {
        StrataError::new(Code::NoSuchSession, format!("no session {}", session.as_str()))
            .with_detail(json!({ "session": session.as_str() }))
    })?;
    let sroot = rec.manifest.root;
    let rel = sroot.strip_prefix(root).map_err(|_| {
        StrataError::new(Code::InvalidInput, "the session root is not under the strata root")
            .with_detail(json!({ "reason": "session_root", "path": sroot.to_string_lossy() }))
    })?;
    let rel = format!("{}/.strata-publish-{}-{id}", rel.to_string_lossy(), ctx.repo.as_str());
    let rp = RepoPath::try_from(rel)?;
    Ok((root.join(rp.as_str()), rp))
}

/// Refuse a layout root whose location or any parent (from the strata root
/// down) is a symlink; nothing is followed or created.
fn refuse_symlinks(root: &Path, rel: &RepoPath) -> Result<()> {
    let mut at = root.to_path_buf();
    let check = |p: &Path| -> Result<()> {
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => Err(StrataError::new(
                Code::NotDirectory,
                format!("{} is a symlink; staging never follows one", p.display()),
            )
            .with_detail(json!({ "path": p.to_string_lossy(), "reason": "symlink" }))),
            Ok(m) if !m.is_dir() => Err(StrataError::new(Code::NotDirectory, format!("{} is not a directory", p.display()))
                .with_detail(json!({ "path": p.to_string_lossy() }))),
            _ => Ok(()),
        }
    };
    check(&at)?;
    for c in rel.as_str().split('/') {
        at.push(c);
        check(&at)?;
    }
    Ok(())
}

/// Remove the layout root; `Unknown` when anything is left behind.
pub(crate) fn remove_layout(root: &Path, rel: &RepoPath) -> Result<()> {
    match strata_apfs::remove_tree_rooted(root, rel) {
        Ok(r) if r.stuck_total == 0 => Ok(()),
        Ok(r) => Err(StrataError::new(Code::Unknown, "the staging layout could not be removed").with_detail(json!({
            "path": root.join(rel.as_str()).to_string_lossy(), "stuck": r.stuck_total, "reason": "cleanup",
        }))),
        Err(e) if e.code == Code::NoSuchPath => Ok(()),
        Err(e) => Err(StrataError::new(Code::Unknown, "the staging layout could not be removed").with_detail(json!({
            "path": root.join(rel.as_str()).to_string_lossy(), "reason": "cleanup", "error": e.message,
        }))),
    }
}

fn write_ver(src: &Src, root: &Path, p: &RepoPath, v: &Ver) -> Result<()> {
    if let Some(parent) = Path::new(p.as_str()).parent()
        && !parent.as_os_str().is_empty()
    {
        let parent = RepoPath::try_from(parent.to_string_lossy().into_owned())?;
        match std::fs::symlink_metadata(root.join(parent.as_str())) {
            Ok(m) if m.is_dir() => {}
            _ => strata_apfs::mkdir_rooted(root, &parent, 0o755, true)?,
        }
    }
    if v.is_link() {
        let bytes = src.load(v)?.ok_or_else(|| StrataError::new(Code::TooLarge, "symlink target too large"))?;
        return strata_apfs::symlink_rooted(root, p, std::ffi::OsStr::from_bytes(&bytes));
    }
    let perm = if v.mode == MODE_EXEC { 0o755 } else { 0o644 };
    let file = match src.load(v)? {
        Some(bytes) => {
            let mut f = strata_apfs::create_file_rooted(root, p, 0o600)?;
            f.write_all(&bytes)
                .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Write, &root.join(p.as_str())))?;
            f
        }
        None => {
            let id = strata_types::BlobId::try_from(v.blake.clone().ok_or_else(|| {
                StrataError::new(Code::TooLarge, format!("{} is too large to rewrite", p.as_str()))
            })?)?;
            src.store.get_blob_rooted(&id, root, p)?;
            strata_apfs::open_rooted_file(root, p)?
        }
    };
    file.set_permissions(std::fs::Permissions::from_mode(perm))
        .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::AfterWrite, &root.join(p.as_str())))
}

fn clear_dir(staging: &Path, p: &RepoPath) -> Result<()> {
    let r = strata_apfs::remove_tree_rooted(staging, p)?;
    if r.stuck_total > 0 {
        return Err(StrataError::new(Code::Unknown, format!("{} could not be cleared in staging", p.as_str()))
            .with_detail(json!({ "path": p.as_str(), "stuck": r.stuck_total })));
    }
    Ok(())
}

/// Write the plan into a fresh export of B: deletions first, then files.
fn apply(src: &Src, staging: &Path, changed: &BTreeMap<RepoPath, Option<Ver>>) -> Result<()> {
    for (p, v) in changed.iter().rev() {
        let full = staging.join(p.as_str());
        match std::fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() => {
                if v.is_some() {
                    clear_dir(staging, p)?;
                }
            }
            Ok(_) => strata_apfs::remove_rooted(staging, p, RemoveKind::File)?,
            Err(_) => {}
        }
    }
    for (p, v) in changed {
        if let Some(v) = v {
            if let Ok(m) = std::fs::symlink_metadata(staging.join(p.as_str()))
                && m.is_dir()
            {
                clear_dir(staging, p)?;
            }
            write_ver(src, staging, p, v)?;
        }
    }
    Ok(())
}

/// Build the layout for `plan` (D4, D12): export B, attach the private git
/// dir, clone the siblings from the session's pinned bases, write the
/// merged paths and stage them through gitio. Any failure removes the
/// layout first; a removal that fails is `Unknown`.
pub(crate) fn build(ctx: &Ctx, group: &ReleaseGroup, base: &CommitOid, plan: &Plan) -> Result<Staged> {
    let root = ctx.deps.paths.root.clone();
    let (layout, rel) = layout_rel(ctx, &plan.layout_session, group.id)?;
    refuse_symlinks(&root, &rel)?;
    strata_apfs::mkdir_rooted(&root, &rel, 0o755, false)?;
    let r = (|| -> Result<Staged> {
        let worktree = layout.join(ctx.repo.as_str());
        let gitdir = layout.join(format!(".git-{}", ctx.repo.as_str()));
        ctx.git.export_tree(&plan.b_tree, &worktree)?;
        ctx.git.attach_staging(&worktree, &gitdir, base)?;
        if let Some(rec) = ctx.store.session(&plan.layout_session)? {
            for (rn, ws) in &rec.manifest.repos {
                if rn == ctx.repo {
                    continue;
                }
                let src = ctx.store.base_dir(&ws.base);
                if src.is_dir() {
                    strata_apfs::clone_tree_rooted(&src, &layout, &RepoPath::try_from(rn.as_str().to_string())?)?;
                }
            }
        }
        let src = Src { store: ctx.store, git: ctx.git };
        apply(&src, &worktree, &plan.changed)?;
        let paths: Vec<RepoPath> = plan.changed.keys().cloned().collect();
        let tree = ctx.git.stage(&worktree, &paths)?;
        if tree == plan.b_tree {
            return Err(no_commit(base));
        }
        Ok(Staged { worktree, gitdir, tree, paths })
    })();
    match r {
        Ok(s) => Ok(s),
        Err(e) => {
            remove_layout(&root, &rel).map_err(|mut u| {
                u.detail.get_or_insert_with(|| json!({}))["cause"] =
                    json!({ "code": e.code.as_str(), "message": e.message, "detail": e.detail });
                u
            })?;
            Err(e)
        }
    }
}

/// §3 `stage`: merge `group` onto `base` (= B) by path and build the
/// staging layout. Conflicts are decided before any directory exists.
pub fn stage(ctx: &Ctx, group: &ReleaseGroup, base: &CommitOid) -> Result<Staged> {
    let plan = plan(ctx, group, base)?;
    build(ctx, group, base, &plan)
}

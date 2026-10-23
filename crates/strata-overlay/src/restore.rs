//! §5.7 `restore`: put a workspace back to a checkpoint. Checkpoints the
//! current state first (`BeforeRestore`), stages every new content in the
//! private directory T under the session root, and removes a replaced or
//! deleted file only after proving it is a copy of what `BeforeRestore`
//! records. Every write is rooted at the session root R.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::{Attrs, FileKind, FileMeta, RemoveKind, RenameMode, attrs_of};
use strata_types::{
    BaseManifest, BlobId, CheckpointId, CheckpointReason, Code, RepoPath, StrataError,
};

use crate::checkpoint::{Guard, finish, give_back, invalid, stable, validate};
use crate::compute::{begin, frame, settle};
use crate::context::{Base, Fold, WorkspaceRef, under};
use crate::hash::{hash_bytes, hash_file};
use crate::overlay::{
    MODE_EXEC, MODE_LINK, Overlay, ancestors, entry_blob, entry_from, groups, rp,
};
use crate::tracker::Tracker;
use crate::{DEFAULT_DIRTY_CAPACITY, RESTORE_TEMP_PREFIX, Result};

/// restore 的请求。
pub struct RestoreRequest<'a> {
    /// 要恢复到的 checkpoint：本会话的一个号，它的 repo 必须是 ws.repo。
    pub target: CheckpointId,
    /// 目标的基线与工作区当前基线不同时必须给出：目标基线的清单与目录（§5.7 第 4 步）。
    pub target_base: Option<&'a Base>,
    /// None：两边 overlay 里的全部路径；Some：这些路径（按字节比较），可以是文件，也可以是目录前缀（§5.7 第 6 步）。
    pub paths: Option<&'a [RepoPath]>,
    /// restore --verify：写之前用 store::verify_blob 重算每个要写回的 blob，并核对要克隆回去的基线文件。
    pub verify: bool,
}

/// restore 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restored {
    /// restore 之前记下的 BeforeRestore checkpoint：恢复它就回到 restore 之前（不变量 1）。
    pub before: CheckpointId,
    pub target: CheckpointId,
    /// 写成目标内容的路径与删掉的路径，按路径排序。
    pub written: Vec<RepoPath>,
    pub removed: Vec<RepoPath>,
    /// 跨基线恢复时三方投影有冲突、写成了目标 checkpoint 版本的路径。
    pub conflicts: Vec<RepoPath>,
}

/// (blob, git mode) at a path; `None` = nothing there.
type Content = Option<(BlobId, u32)>;

/// content(X, q) of §5.7 against the current base manifest M.
struct View<'a> {
    x: &'a Overlay,
    hidden: BTreeSet<&'a RepoPath>,
    m: &'a BaseManifest,
}

impl<'a> View<'a> {
    fn new(x: &'a Overlay, m: &'a BaseManifest) -> View<'a> {
        let hidden = x.entries.iter().filter_map(entry_from).collect();
        View { x, hidden, m }
    }

    fn content(&self, q: &RepoPath) -> Content {
        if let Some(e) = self.x.get(q) {
            return entry_blob(e).map(|b| (b.clone(), e.mode));
        }
        if self.hidden.contains(q) {
            return None;
        }
        self.m.entries.get(q).map(|e| (e.blob.clone(), e.mode))
    }

    /// The content comes from M (no entry at `q`): a clone of the base file.
    fn is_base(&self, q: &RepoPath) -> bool {
        self.x.get(q).is_none()
    }

    /// Every path with content: M not hidden, plus the live entries.
    fn live(&self) -> BTreeSet<&'a RepoPath> {
        let mut out: BTreeSet<&RepoPath> = self
            .m
            .entries
            .keys()
            .filter(|p| self.x.get(p).is_none() && !self.hidden.contains(p))
            .collect();
        out.extend(
            self.x
                .entries
                .iter()
                .filter(|e| entry_blob(e).is_some())
                .map(|e| &e.path),
        );
        out
    }
}

/// One planned change at a workspace path.
#[derive(Clone, Debug)]
struct Op {
    path: RepoPath,
    /// Its name in T (`<n>` for a write, `rm-<n>` for a delete).
    temp: String,
    target: Content,
    current: Content,
    /// The target content is a clone of the base file (no entry in D).
    from_base: bool,
}

#[derive(Serialize)]
struct PlanOp<'a> {
    path: &'a str,
    temp: &'a str,
    target: Option<PlanContent<'a>>,
    current: Option<PlanContent<'a>>,
}

#[derive(Serialize)]
struct PlanContent<'a> {
    blob: &'a str,
    mode: u32,
}

fn plan_content(c: &Content) -> Option<PlanContent<'_>> {
    c.as_ref().map(|(b, m)| PlanContent {
        blob: b.as_str(),
        mode: *m,
    })
}

fn depth(p: &str) -> usize {
    p.matches('/').count()
}

fn busy(msg: String) -> StrataError {
    StrataError::new(Code::Busy, msg).with_detail(json!({ "reason": "workspace_changed" }))
}

/// 把工作区恢复到一个 checkpoint（§5.7）。
pub fn restore(
    ws: &WorkspaceRef,
    base: &Base,
    tracker: Option<&mut Tracker>,
    req: &RestoreRequest,
) -> Result<Restored> {
    let given_root = ws.root.to_path_buf();
    // 2. The §5.6 step 1 checks; R and the workspace's name under it come
    // from the path the Store records, proven the same directory as ws.root.
    // Everything below goes through that spelling ("root/." or another case
    // of the same directory is accepted but not walked).
    let recorded = validate(ws, base)?;
    let (r, repo_dir) = session_root(&recorded)?;
    let ws = &WorkspaceRef {
        root: &recorded,
        ..*ws
    };
    let mut own = None;
    let tracker: &mut Tracker = match tracker {
        Some(t) if t.root == recorded => t,
        Some(t) => {
            if t.root != given_root {
                return Err(StrataError::new(
                    Code::InvalidInput,
                    format!(
                        "tracker is for {}, workspace is {}",
                        t.root.display(),
                        given_root.display()
                    ),
                )
                .with_detail(json!({ "reason": "wrong_tracker" })));
            }
            // Another spelling of the root: scan with a fresh tracker and
            // make the caller's next scan a full one.
            t.inbox.require_full();
            own.insert(Tracker::new(&recorded, DEFAULT_DIRTY_CAPACITY))
        }
        None => own.insert(Tracker::new(&recorded, DEFAULT_DIRTY_CAPACITY)),
    };
    let tname = format!("{RESTORE_TEMP_PREFIX}{}", ws.repo.as_str());

    // 1. T and an earlier scene.
    check_temp(&r, &tname)?;

    // 3. The target, exactly as the store returns it.
    let target = ws.store.checkpoint(ws.session, req.target)?;
    if target.repo != *ws.repo {
        return Err(invalid(
            "wrong_repo",
            format!(
                "checkpoint {} belongs to repo {}, workspace is {}",
                target.id.get(),
                target.repo.as_str(),
                ws.repo.as_str()
            ),
        ));
    }
    let tov = Overlay::from_checkpoint(&target);
    tov.validate()?;
    let f0 = frame(ws, base, &tracker.st)?;
    for e in &tov.entries {
        for p in std::iter::once(&e.path).chain(entry_from(e)) {
            if f0.x.covers(p.as_str()) {
                return Err(StrataError::new(
                    Code::InvalidInput,
                    format!(
                        "checkpoint {} has an entry under the exclusion set: {}",
                        target.id.get(),
                        p.as_str()
                    ),
                )
                .with_detail(json!({ "reason": "excluded_path", "path": p.as_str() })));
            }
        }
    }

    // 4. The target state D against the current base.
    if target.base != base.r#gen {
        return Err(invalid(
            "cross_base",
            format!(
                "checkpoint {} is on another base generation",
                target.id.get()
            ),
        ));
    }
    tov.check_base(&base.manifest)?;
    let d = tov.clone();
    let conflicts: Vec<RepoPath> = Vec::new();

    // 5. Scan (full, whatever the tracker's events say).
    let mut pass = begin(ws, base, tracker, None)?;
    let c = settle(base, &mut pass);

    let planned = (|| -> Result<Vec<Op>> {
        // 6. Select the paths.
        let fold = Fold {
            case_sensitive: pass.f.volume_cs,
        };
        let selected = select(&c, &d, &tov, &base.manifest, req.paths, fold)?;
        // 7. Plan.
        let cv = View::new(&c, &base.manifest);
        let dv = View::new(&d, &base.manifest);
        let mut ops = Vec::new();
        for q in &selected {
            let (t, cur) = (dv.content(q), cv.content(q));
            if t != cur {
                let n = ops.len();
                let temp = if t.is_some() {
                    n.to_string()
                } else {
                    format!("rm-{n}")
                };
                ops.push(Op {
                    path: q.clone(),
                    temp,
                    from_base: t.is_some() && dv.is_base(q),
                    target: t,
                    current: cur,
                });
            }
        }
        Ok(ops)
    })();
    let ops = match planned {
        Ok(o) => o,
        Err(e) => {
            give_back(tracker, pass.snap, Some(&c));
            return Err(e);
        }
    };
    let cv = View::new(&c, &base.manifest);
    let dirs = emptied_dirs(&cv, &ops);

    // 8. Precheck.
    crate::fault::before_precheck(ws.root);
    if let Err(e) = precheck(ws, base, &ops, &dirs) {
        give_back(tracker, pass.snap, Some(&c));
        return Err(e);
    }
    // The scan's attrs of each path to replace or delete (the proof's basis).
    let scanned: BTreeMap<RepoPath, Attrs> = ops
        .iter()
        .filter_map(|o| {
            let (blob, _) = o.current.as_ref()?;
            let k = pass.st.cache.get(&o.path)?;
            (k.blob == *blob).then(|| (o.path.clone(), k.attrs.clone()))
        })
        .collect();

    // 9. BeforeRestore: entries are C; a planned path that changed since the
    // scan means no row and Busy.
    let guard = |o: &Overlay| -> Result<()> {
        let v = View::new(o, &base.manifest);
        for op in &ops {
            if v.content(&op.path) != op.current {
                return Err(busy(format!(
                    "{} changed after the scan",
                    ws.root.join(op.path.as_str()).display()
                )));
            }
        }
        Ok(())
    };
    let g: Guard = &guard;
    let before = match finish(
        ws,
        base,
        &mut pass,
        CheckpointReason::BeforeRestore,
        Some(g),
    ) {
        Ok(out) => {
            let id = out.checkpoint.id;
            tracker.st = pass.st;
            id
        }
        Err(failed) => {
            let (e, o) = *failed;
            give_back(tracker, pass.snap, o.as_ref());
            return Err(e);
        }
    };
    crate::fault::after_plan(ws.root);

    // 10-14. Apply.
    let mut a = Apply {
        ws_root: ws.root,
        r: &r,
        repo_dir: &repo_dir,
        t: tname,
        before,
        done: Vec::new(),
        steps: 0,
    };
    let res = a.run(ws, base, &target.id, &ops, &dirs, &scanned);
    let touched: Vec<RepoPath> = ops
        .iter()
        .map(|o| o.path.clone())
        .chain(dirs.iter().cloned())
        .collect();
    match res {
        Ok(()) => {
            // The next scan judges every path this restore wrote again.
            tracker.inbox.hint(&touched);
            let mut written: Vec<RepoPath> = ops
                .iter()
                .filter(|o| o.target.is_some())
                .map(|o| o.path.clone())
                .collect();
            let mut removed: Vec<RepoPath> = ops
                .iter()
                .filter(|o| o.target.is_none())
                .map(|o| o.path.clone())
                .collect();
            written.sort();
            removed.sort();
            Ok(Restored {
                before,
                target: target.id,
                written,
                removed,
                conflicts,
            })
        }
        Err(e) => {
            tracker.inbox.require_full();
            Err(a.unknown(e, &ops, &dirs))
        }
    }
}

/// R (the session root) and the workspace's directory name under it.
fn session_root(ws_path: &Path) -> Result<(PathBuf, String)> {
    let (Some(r), Some(name)) = (ws_path.parent(), ws_path.file_name()) else {
        return Err(invalid(
            "wrong_root",
            format!("{} has no session root", ws_path.display()),
        ));
    };
    let Some(name) = name.to_str() else {
        return Err(invalid(
            "wrong_root",
            format!("{} is not UTF-8", ws_path.display()),
        ));
    };
    Ok((r.to_path_buf(), name.to_owned()))
}

/// §5.7 step 1: T absent or an empty directory; anything else refuses.
fn check_temp(r: &Path, tname: &str) -> Result<()> {
    let t = rp(tname);
    let shown = r.join(tname);
    match attrs_of(r, &t) {
        Err(e) if e.code == Code::NoSuchPath => Ok(()),
        Err(e) => Err(e),
        Ok(a) if a.kind != FileKind::Dir => Err(StrataError::new(
            Code::Exists,
            format!(
                "{} is in the way of restore's temporary directory",
                shown.display()
            ),
        )
        .with_detail(json!({ "reason": "restore_temp_taken", "path": shown.to_string_lossy() }))),
        Ok(_) => {
            let items = list_names(r, &t)?;
            if items.is_empty() {
                return Ok(());
            }
            Err(StrataError::new(
                Code::Unknown,
                format!("{} holds the scene of an earlier restore", shown.display()),
            )
            .with_detail(json!({
                "reason": "restore_scene",
                "path": shown.to_string_lossy(),
                "unproven": items,
            })))
        }
    }
}

fn list_names(r: &Path, dir: &RepoPath) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for item in strata_apfs::list_dir(r, Some(dir)) {
        let (p, _) = item?;
        out.push(p.as_str().to_owned());
    }
    out.sort();
    Ok(out)
}

/// §5.7 step 6: the selected paths, closed over §5.3 groups.
fn select(
    c: &Overlay,
    d: &Overlay,
    tov: &Overlay,
    m: &BaseManifest,
    paths: Option<&[RepoPath]>,
    fold: Fold,
) -> Result<BTreeSet<RepoPath>> {
    let names = |o: &Overlay| -> Vec<RepoPath> {
        o.entries
            .iter()
            .flat_map(|e| std::iter::once(e.path.clone()).chain(entry_from(e).cloned()))
            .collect()
    };
    let Some(ps) = paths else {
        let mut all: BTreeSet<RepoPath> = names(c).into_iter().collect();
        all.extend(names(d));
        return Ok(all);
    };
    let mut cand: BTreeSet<RepoPath> = BTreeSet::new();
    cand.extend(names(c));
    cand.extend(names(d));
    cand.extend(names(tov));
    cand.extend(m.entries.keys().cloned());
    let mut picked: BTreeSet<RepoPath> = BTreeSet::new();
    for p in ps {
        let prefix = format!("{}/", p.as_str());
        let mut hit = false;
        if cand.contains(p) {
            picked.insert(p.clone());
            hit = true;
        }
        for k in cand.iter().filter(|k| k.as_str().starts_with(&prefix)) {
            picked.insert(k.clone());
            hit = true;
        }
        if !hit {
            let key = fold.key(p.as_str());
            let n = p.as_str().split('/').count();
            let on_disk = cand.iter().find_map(|k| {
                let kk = fold.key(k.as_str());
                (kk == key || under(&kk, &key))
                    .then(|| k.as_str().split('/').take(n).collect::<Vec<_>>().join("/"))
            });
            let mut detail = json!({ "path": p.as_str() });
            if let Some(s) = &on_disk {
                detail["on_disk"] = json!(s);
            }
            return Err(StrataError::new(
                Code::NoSuchPath,
                format!(
                    "{} is in neither the checkpoint, the workspace's overlay nor the base{}",
                    p.as_str(),
                    on_disk
                        .map(|s| format!(" (did you mean {s}?)"))
                        .unwrap_or_default()
                ),
            )
            .with_detail(detail));
        }
    }
    let g = groups(c.entries.iter().chain(d.entries.iter()));
    let mut out = picked.clone();
    for p in &picked {
        if let Some(members) = g.get(p) {
            out.extend(members.iter().cloned());
        }
    }
    Ok(out)
}

/// §5.7 step 7: directories this restore empties, with no target path under them.
fn emptied_dirs(cv: &View, ops: &[Op]) -> Vec<RepoPath> {
    let gone: BTreeSet<&RepoPath> = ops
        .iter()
        .filter(|o| o.target.is_none())
        .map(|o| &o.path)
        .collect();
    let mut after: BTreeSet<&RepoPath> = cv
        .live()
        .into_iter()
        .filter(|p| !gone.contains(p))
        .collect();
    after.extend(ops.iter().filter(|o| o.target.is_some()).map(|o| &o.path));
    let needed: BTreeSet<&str> = after.iter().flat_map(|p| ancestors(p.as_str())).collect();
    let mut dirs: Vec<&str> = gone
        .iter()
        .flat_map(|p| ancestors(p.as_str()))
        .filter(|d| !needed.contains(d))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    dirs.sort_by(|a, b| depth(b).cmp(&depth(a)).then(a.cmp(b)));
    dirs.into_iter().map(rp).collect()
}

const LOCK_FLAGS: u32 = libc::UF_IMMUTABLE | libc::SF_IMMUTABLE | libc::UF_APPEND | libc::SF_APPEND;

fn flags_of(root: &Path, p: &RepoPath) -> Option<u32> {
    use std::os::macos::fs::MetadataExt;
    std::fs::symlink_metadata(root.join(p.as_str()))
        .ok()
        .map(|m| m.st_flags())
}

fn read_only(root: &Path, p: &RepoPath) -> StrataError {
    let shown = root.join(p.as_str());
    StrataError::new(
        Code::ReadOnly,
        format!("{} is immutable or append-only", shown.display()),
    )
    .with_detail(json!({ "path": shown.to_string_lossy(), "reason": "flags" }))
}

/// §5.7 step 8 (the checks this module makes before anything is written).
fn precheck(ws: &WorkspaceRef, base: &Base, ops: &[Op], dirs: &[RepoPath]) -> Result<()> {
    // ReadOnly: files to replace or delete, directories to delete.
    for o in ops.iter().filter(|o| o.current.is_some()) {
        if flags_of(ws.root, &o.path).is_some_and(|f| f & LOCK_FLAGS != 0) {
            return Err(read_only(ws.root, &o.path));
        }
    }
    for d in dirs {
        if flags_of(ws.root, d).is_some_and(|f| f & LOCK_FLAGS != 0) {
            return Err(read_only(ws.root, d));
        }
    }
    // OpenFiles: a writable mapping hides the gen count.
    for o in ops.iter().filter(|o| o.current.is_some()) {
        let a = match attrs_of(ws.root, &o.path) {
            Ok(a) => a,
            Err(_) => {
                return Err(busy(format!(
                    "{} changed after the scan",
                    ws.root.join(o.path.as_str()).display()
                )));
            }
        };
        if a.kind == FileKind::File && !a.gen_count_known() {
            let shown = ws.root.join(o.path.as_str());
            return Err(StrataError::new(
                Code::OpenFiles,
                format!("{} is mapped writable by a process", shown.display()),
            )
            .with_detail(json!({ "path": shown.to_string_lossy() })));
        }
    }
    // Corrupt: base files to clone must be there with the manifest's size.
    for o in ops.iter().filter(|o| o.from_base) {
        let shown = base.dir.join(o.path.as_str());
        let corrupt = |why: &str| {
            StrataError::new(
                Code::Corrupt,
                format!("base file {}: {why}", shown.display()),
            )
            .with_detail(json!({ "path": shown.to_string_lossy(), "reason": why }))
        };
        let Some(me) = base.manifest.entries.get(&o.path) else {
            return Err(corrupt("not in the manifest"));
        };
        let a = match attrs_of(&base.dir, &o.path) {
            Ok(a) => a,
            Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory) => {
                return Err(corrupt("missing"));
            }
            Err(e) => return Err(e),
        };
        let kind_ok = match a.kind {
            FileKind::Symlink { .. } => me.mode == MODE_LINK,
            FileKind::File => me.mode != MODE_LINK,
            _ => false,
        };
        if !kind_ok {
            return Err(corrupt("not the file the manifest records"));
        }
        if a.size != me.size {
            return Err(corrupt("size differs from the manifest"));
        }
    }
    Ok(())
}

/// Phase C/D state, for the `Unknown` detail.
struct Apply<'a> {
    ws_root: &'a Path,
    r: &'a Path,
    repo_dir: &'a str,
    t: String,
    before: CheckpointId,
    done: Vec<RepoPath>,
    steps: u32,
}

impl Apply<'_> {
    fn wsp(&self, q: &str) -> RepoPath {
        rp(&format!("{}/{q}", self.repo_dir))
    }

    fn tp(&self, name: &str) -> RepoPath {
        rp(&format!("{}/{name}", self.t))
    }

    fn step(&mut self, q: &RepoPath) {
        self.done.push(q.clone());
        self.steps += 1;
        crate::fault::in_apply(self.ws_root, self.steps);
    }

    fn unknown(&self, e: StrataError, ops: &[Op], dirs: &[RepoPath]) -> StrataError {
        let done: BTreeSet<&RepoPath> = self.done.iter().collect();
        let mut pending: Vec<&str> = ops
            .iter()
            .map(|o| &o.path)
            .chain(dirs.iter())
            .filter(|p| !done.contains(p))
            .map(|p| p.as_str())
            .collect();
        pending.sort();
        let mut done: Vec<&str> = done.into_iter().map(|p| p.as_str()).collect();
        done.sort();
        let temp = self.r.join(&self.t);
        StrataError::new(
            Code::Unknown,
            format!(
                "restore stopped part way ({}); the scene is kept in {}",
                e.message,
                temp.display()
            ),
        )
        .with_detail(json!({
            "before": self.before.get(),
            "done": done,
            "pending": pending,
            "temp": temp.to_string_lossy(),
            "cause": { "code": e.code, "message": e.message, "detail": e.detail },
        }))
    }

    fn run(
        &mut self,
        ws: &WorkspaceRef,
        base: &Base,
        target: &CheckpointId,
        ops: &[Op],
        dirs: &[RepoPath],
        scanned: &BTreeMap<RepoPath, Attrs>,
    ) -> Result<()> {
        let r = self.r;
        let t = rp(&self.t);
        // 10. T, plan.json, then the staged contents.
        match strata_apfs::mkdir_rooted(r, &t, 0o700, false) {
            Ok(()) => {}
            Err(e) if e.code == Code::Exists => {
                let a = attrs_of(r, &t)?;
                if a.kind != FileKind::Dir || !list_names(r, &t)?.is_empty() {
                    return Err(busy(format!(
                        "{} appeared after the check",
                        r.join(&self.t).display()
                    )));
                }
            }
            Err(e) => return Err(e),
        }
        self.write_plan(target, ops)?;
        for o in ops.iter().filter(|o| o.target.is_some()) {
            self.stage(ws, base, o)?;
        }

        // 11. Deletes, deepest first.
        let mut dels: Vec<&Op> = ops.iter().filter(|o| o.target.is_none()).collect();
        dels.sort_by(|a, b| {
            depth(b.path.as_str())
                .cmp(&depth(a.path.as_str()))
                .then(a.path.cmp(&b.path))
        });
        for o in dels {
            let tn = self.tp(&o.temp);
            strata_apfs::rename_rooted(r, &self.wsp(o.path.as_str()), &tn, RenameMode::Excl)?;
            self.step(&o.path);
            self.prove_and_remove(&tn, o, scanned)?;
        }

        // 12. Directories this restore emptied, deepest first.
        for d in dirs {
            strata_apfs::remove_rooted(r, &self.wsp(d.as_str()), RemoveKind::EmptyDir)?;
            self.done.push(d.clone());
        }

        // 13. Writes, shallowest first.
        let mut writes: Vec<&Op> = ops.iter().filter(|o| o.target.is_some()).collect();
        writes.sort_by(|a, b| {
            depth(a.path.as_str())
                .cmp(&depth(b.path.as_str()))
                .then(a.path.cmp(&b.path))
        });
        for o in writes {
            if let Some((parent, _)) = o.path.as_str().rsplit_once('/') {
                strata_apfs::mkdir_rooted(r, &self.wsp(parent), 0o755, true)?;
            }
            let tn = self.tp(&o.temp);
            let dst = self.wsp(o.path.as_str());
            if o.current.is_some() {
                strata_apfs::rename_rooted(r, &tn, &dst, RenameMode::Swap)?;
                self.step(&o.path);
                self.prove_and_remove(&tn, o, scanned)?;
            } else {
                strata_apfs::rename_rooted(r, &tn, &dst, RenameMode::Excl)?;
                self.step(&o.path);
            }
        }

        // 14. Only plan.json is left: remove it, then T.
        let left = list_names(r, &t)?;
        let plan = format!("{}/plan.json", self.t);
        if left != [plan.clone()] {
            return Err(StrataError::new(
                Code::Unknown,
                format!("{} holds more than plan.json", r.join(&self.t).display()),
            )
            .with_detail(json!({ "left": left })));
        }
        strata_apfs::remove_rooted(r, &rp(&plan), RemoveKind::File)?;
        strata_apfs::remove_rooted(r, &t, RemoveKind::EmptyDir)?;
        Ok(())
    }

    /// plan.json in T, then fsync plan.json, T and R, and F_FULLFSYNC R.
    fn write_plan(&self, target: &CheckpointId, ops: &[Op]) -> Result<()> {
        let r = self.r;
        let plan_ops: Vec<PlanOp> = ops
            .iter()
            .map(|o| PlanOp {
                path: o.path.as_str(),
                temp: &o.temp,
                target: plan_content(&o.target),
                current: plan_content(&o.current),
            })
            .collect();
        let body = serde_json::to_vec_pretty(&json!({
            "repo": self.repo_dir,
            "before": self.before.get(),
            "target": target.get(),
            "ops": plan_ops,
        }))
        .expect("plan serializes");
        let pp = self.tp("plan.json");
        let mut f = strata_apfs::create_file_rooted(r, &pp, 0o600)?;
        let shown = r.join(pp.as_str());
        let io = |e: std::io::Error| {
            strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Write, &shown)
        };
        f.write_all(&body).map_err(io)?;
        fsync(&f, &shown)?;
        let tf = strata_apfs::open_rooted(r, &rp(&self.t))?;
        fsync(&tf, &r.join(&self.t))?;
        let rf = strata_apfs::open_root(r)?;
        fsync(&rf, r)?;
        strata_apfs::full_fsync(&rf)
    }

    /// Put the op's target content at `T/<n>`.
    fn stage(&self, ws: &WorkspaceRef, base: &Base, o: &Op) -> Result<()> {
        let r = self.r;
        let (blob, mode) = o.target.as_ref().expect("a write has a target");
        let tn = self.tp(&o.temp);
        if *mode == MODE_LINK {
            let target = if o.from_base {
                match attrs_of(&base.dir, &o.path)?.kind {
                    FileKind::Symlink { target } => target.into_os_string(),
                    _ => {
                        return Err(StrataError::new(
                            Code::Corrupt,
                            format!(
                                "base file {} is no longer a symlink",
                                base.dir.join(o.path.as_str()).display()
                            ),
                        ));
                    }
                }
            } else {
                std::ffi::OsString::from(std::ffi::OsStr::from_bytes(
                    &ws.store.read_blob(blob, 1024)?,
                ))
            };
            return strata_apfs::symlink_rooted(r, &tn, &target);
        }
        let perm = if *mode == MODE_EXEC { 0o755 } else { 0o644 };
        if o.from_base {
            let src = base.dir.join(o.path.as_str());
            strata_apfs::clone_file_rooted(&src, r, &tn, None)?;
            crate::fault::before_temp_meta(self.ws_root);
            let f = strata_apfs::open_rooted_file(r, &tn)?;
            strata_apfs::set_file_meta(
                &f,
                &FileMeta {
                    clear_flags: true,
                    mode: Some(perm),
                    mtime: None,
                },
            )?;
            strip_xattrs(&f, &r.join(tn.as_str()))?;
            strata_apfs::set_file_meta(
                &f,
                &FileMeta {
                    clear_flags: false,
                    mode: None,
                    mtime: Some(SystemTime::now()),
                },
            )?;
        } else {
            ws.store.get_blob_rooted(blob, r, &tn)?;
            crate::fault::before_temp_meta(self.ws_root);
            if perm == 0o755 {
                let f = strata_apfs::open_rooted_file(r, &tn)?;
                strata_apfs::set_file_meta(
                    &f,
                    &FileMeta {
                        clear_flags: false,
                        mode: Some(0o755),
                        mtime: None,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// The file moved to `tn` holds content(C, q): proven → removed, else Unknown.
    fn prove_and_remove(
        &self,
        tn: &RepoPath,
        o: &Op,
        scanned: &BTreeMap<RepoPath, Attrs>,
    ) -> Result<()> {
        let (blob, _) = o.current.as_ref().expect("a moved file has content");
        if !prove(self.r, tn, blob, scanned.get(&o.path))? {
            let shown = self.r.join(tn.as_str());
            return Err(StrataError::new(
                Code::Unknown,
                format!(
                    "{} (was {}) changed during the restore; kept",
                    shown.display(),
                    o.path.as_str()
                ),
            )
            .with_detail(json!({ "path": shown.to_string_lossy(), "was": o.path.as_str() })));
        }
        strata_apfs::remove_rooted(self.r, tn, RemoveKind::File)
    }
}

/// The file at `root/p` is a copy of `blob`: its attrs are unchanged since
/// the scan's (whose content was `blob`), or BLAKE3 recomputed between two
/// equal attrs says so. A symlink: its target hashes to `blob`.
fn prove(root: &Path, p: &RepoPath, blob: &BlobId, scan: Option<&Attrs>) -> Result<bool> {
    let a = attrs_of(root, p)?;
    match &a.kind {
        FileKind::Symlink { target } => Ok(hash_bytes(target.as_os_str().as_bytes()) == *blob),
        FileKind::File => {
            if scan.is_some_and(|s| stable(s, &a)) {
                return Ok(true);
            }
            if !a.gen_count_known() {
                return Ok(false);
            }
            let f = strata_apfs::open_rooted_file(root, p)?;
            let (id, _) = hash_file(&f, &root.join(p.as_str()))?;
            let a2 = attrs_of(root, p)?;
            Ok(id == *blob && a2.unchanged_since(&a))
        }
        _ => Ok(false),
    }
}

fn fsync(f: &std::fs::File, shown: &Path) -> Result<()> {
    if unsafe { libc::fsync(f.as_raw_fd()) } == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    Err(strata_apfs::classify_io_error(
        &e,
        strata_apfs::IoSide::AfterWrite,
        shown,
    ))
}

/// Remove every extended attribute through the handle (strata never
/// restores xattrs). Names the system keeps (`com.apple.*`) may refuse.
fn strip_xattrs(f: &std::fs::File, shown: &Path) -> Result<()> {
    let fd = f.as_raw_fd();
    let io =
        |e: std::io::Error| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Write, shown);
    let n = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    if n < 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    if n == 0 {
        return Ok(());
    }
    let mut buf = vec![0u8; n as usize];
    let n = unsafe { libc::flistxattr(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    buf.truncate(n as usize);
    for name in buf.split_inclusive(|&b| b == 0) {
        let Ok(c) = CStr::from_bytes_with_nul(name) else {
            continue;
        };
        if unsafe { libc::fremovexattr(fd, c.as_ptr(), 0) } != 0 {
            let e = std::io::Error::last_os_error();
            let system = c.to_bytes().starts_with(b"com.apple.");
            if e.raw_os_error() != Some(libc::ENOATTR) && !system {
                return Err(io(e));
            }
        }
    }
    Ok(())
}

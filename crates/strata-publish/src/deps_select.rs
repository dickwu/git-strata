//! deps drift (design r8): the publishing repo's own deps set for the
//! checks follows the staged install inputs. Inside the repo lock
//! `select_own_deps` keeps the session's pinned set P, substitutes a
//! sealed set for the staged inputs, or asks `publish_bound` for one cold
//! install outside every publish lock (`cold_install`). Never a refusal of
//! the publish for drift alone; a warning says what the checks used.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};
use strata_deps::{DepsConfigKey, InputRecord, ProposalState};
use strata_types::{Code, DepsSet, DepsSetId, RepoPath, SessionId, StrataError, WorkspaceState};

use crate::push::{PublishWarning, WarningKind, err_json};
use crate::seams::{self, FailPoint, Point};
use crate::stage::{Plan, at_or_under};
use crate::{Ctx, Result};

/// `reason` of the internal round-1 signal (§2.3).
pub(crate) const INSTALL_NEEDED: &str = "deps_install_needed";
const STAGE_SELECT: &str = "deps_select";

/// The set the call's cold install sealed (§2.1 `installed`).
#[derive(Clone, Debug)]
pub(crate) struct Installed {
    pub set: DepsSetId,
    pub inputs: String,
    pub first_row: u64,
    pub install_ms: u64,
    /// `DepsInputsLeft` when the snapshot could not be removed.
    pub left: Option<PublishWarning>,
}

/// Which selection `stage::build` does (§2.2).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Select<'a> {
    /// standalone `stage::stage`: the pinned set, nothing else.
    PinnedOnly,
    Publish { allow_install: bool, installed: Option<&'a Installed> },
}

/// What `clone_own_deps` put into the worktree.
pub(crate) struct OwnDeps {
    pub pinned: DepsSet,
    pub cloned: Vec<RepoPath>,
}

/// The layout session's workspace of this repo.
pub(crate) struct Layout<'a> {
    pub session: &'a SessionId,
    pub ws: &'a WorkspaceState,
    pub single: bool,
    pub row: u64,
}

impl Select<'_> {
    /// Step 0 (§2.2): whether there is anything to select.
    pub(crate) fn applies(&self, ctx: &Ctx, own: Option<&OwnDeps>) -> bool {
        matches!(self, Select::Publish { .. }) && !ctx.cfg.checks.is_empty() && own.is_some_and(|o| !o.cloned.is_empty())
    }
}

/// The canonical install-input records of `root`, `None` when they cannot
/// be read (the diagnosis then says `mixed`, §2.6).
pub(crate) fn records(ctx: &Ctx, root: &Path) -> Option<Vec<InputRecord>> {
    strata_deps::input_records(ctx.cfg, ctx.git, root).ok()
}

fn tag_stage(mut e: StrataError) -> StrataError {
    let d = e.detail.get_or_insert_with(|| json!({}));
    if let Some(m) = d.as_object_mut() {
        m.entry("stage").or_insert(json!(STAGE_SELECT));
    }
    e
}

/// §2.3: after the layout is gone, an `Unknown` from the selection becomes
/// `Drift`/`deps_select_failed` (the row closes `Done`); other codes pass.
pub(crate) fn rewrite_select_failure(ctx: &Ctx, e: StrataError) -> StrataError {
    let from_select = e.detail.as_ref().and_then(|d| d.get("stage")).and_then(Value::as_str) == Some(STAGE_SELECT);
    if e.code != Code::Unknown || !from_select {
        return e;
    }
    StrataError::new(
        Code::Drift,
        format!("{}: the deps for the staged install inputs could not be selected: {}", ctx.repo, e.message),
    )
    .with_detail(json!({
        "reason": "deps_select_failed",
        "stage": STAGE_SELECT,
        "repo": ctx.repo.as_str(),
        "cause": err_json(&e),
    }))
}

fn switch_fix(ctx: &Ctx, session: &SessionId) -> String {
    format!("git-strata deps propose --session {} --repo {} --switch", session.as_str(), ctx.repo)
}

/// §2.6: the cause of the difference, for the text and `fix` only.
#[derive(Debug)]
pub(crate) struct Diagnosis {
    pub cause: &'static str,
    pub paths: Vec<String>,
    pub fix: Option<String>,
    pub hint: String,
}

impl Diagnosis {
    pub(crate) fn json(&self) -> Value {
        let mut v = json!({ "cause": self.cause, "paths": self.paths, "hint": self.hint });
        if let Some(f) = &self.fix {
            v["fix"] = json!(f);
        }
        v
    }
}

fn by_path(r: &[InputRecord]) -> BTreeMap<&str, &InputRecord> {
    r.iter().map(|x| (x.path.as_str(), x)).collect()
}

/// Compare S (staged), B (export before apply), W (workspace) and A (the
/// pinned base) by canonical record (§2.6). `used` says which set the
/// checks used, for the `base_moved` hint.
pub(crate) fn diagnose(
    ctx: &Ctx,
    l: &Layout,
    s: &[InputRecord],
    b: Option<&[InputRecord]>,
    used: &str,
) -> Diagnosis {
    let h = strata_deps::hash_records(s);
    let w = records(ctx, &l.ws.path);
    let a = records(ctx, &ctx.store.base_dir(&l.ws.base));
    let (sm, bm, wm, am) = (by_path(s), b.map(by_path), w.as_deref().map(by_path), a.as_deref().map(by_path));
    let mut all: BTreeSet<&str> = sm.keys().copied().collect();
    for m in [&bm, &wm, &am].into_iter().flatten() {
        all.extend(m.keys().copied());
    }
    let neq = |x: &Option<BTreeMap<&str, &InputRecord>>, y: &Option<BTreeMap<&str, &InputRecord>>, p: &str| -> Option<bool> {
        Some(x.as_ref()?.get(p) != y.as_ref()?.get(p))
    };
    let smo = Some(sm.clone());
    let mut session_changed = Some(false);
    let mut main_changed = Some(false);
    let mut paths = Vec::new();
    for p in &all {
        let sc = neq(&wm, &am, p);
        let mc = neq(&bm, &am, p);
        let sw = neq(&smo, &wm, p);
        session_changed = session_changed.and_then(|v| sc.map(|c| v || c));
        main_changed = main_changed.and_then(|v| mc.map(|c| v || c));
        if sc == Some(true) || mc == Some(true) || sw != Some(false) {
            paths.push((*p).to_owned());
        }
    }
    let fix = switch_fix(ctx, l.session);
    if w.as_deref().is_some_and(|w| strata_deps::hash_records(w) == h) {
        return Diagnosis {
            cause: "unswitched",
            paths,
            hint: format!(
                "the checks used the deps set matching the published install inputs; run `{fix}` so the session's workspace uses it too, and check that data.switched.kind is \"moved\" (when it is skipped, e.g. OpenFiles, stop the processes you started that hold the deps and run it again)"
            ),
            fix: Some(fix),
        };
    }
    // A workspace whose inputs could not be read is `mixed` (§2.6).
    if w.is_some() && main_changed == Some(false) && l.single {
        return Diagnosis {
            cause: "selection",
            paths,
            fix: None,
            hint: "the checks used the deps set for the install inputs this publish selected, which differ from the session workspace's current install inputs; make sure the published versions are the ones you want".into(),
        };
    }
    let sb_eq = b.is_some_and(|b| strata_deps::hash_records(b) == h);
    if session_changed == Some(false) && sb_eq && l.single {
        let mut hint = format!(
            "main's install inputs are newer than the session's base; the checks used main's deps set ({used}); the session's rebase / proposal flow handles the workspace as usual"
        );
        match approvable(ctx, &h) {
            Some(q) => hint.push_str(&format!(
                "; a human can run `git-strata deps approve {q}` (needs a TTY) and must check that the session's result is moved"
            )),
            None => hint.push_str("; once the rebase / observe flow has made a proposal for the new main, a human handles it"),
        }
        return Diagnosis { cause: "base_moved", paths, fix: None, hint };
    }
    Diagnosis {
        cause: "mixed",
        paths,
        fix: None,
        hint: "the checks used the deps set for the published (merged) install inputs; a human decides the workspace's deps: get the wanted merged install inputs into one workspace, then `deps propose --switch` there; a group of several sessions can publish separately".into(),
    }
}

/// A Pending proposal of this repo to `h` whose head is main's tip (§2.6
/// rule 3; rollout's `check_tip`).
fn approvable(ctx: &Ctx, h: &str) -> Option<String> {
    let tip = strata_deps::main_tip(ctx.cfg, ctx.git).ok()??;
    strata_deps::open_proposals(ctx.store, Some(ctx.repo))
        .ok()?
        .into_iter()
        .find(|q| q.state == ProposalState::Pending && q.to_inputs == h && q.head.as_ref() == Some(&tip))
        .map(|q| q.id.to_string())
}

#[allow(clippy::too_many_arguments)]
fn warning(
    ctx: &Ctx,
    kind: WarningKind,
    l: &Layout,
    own: &OwnDeps,
    used: &DepsSetId,
    h: &str,
    diag: &Diagnosis,
    extra: Value,
    lead: String,
) -> PublishWarning {
    let tail = match &diag.fix {
        Some(f) => format!("Run `{f}` so the workspace uses it too."),
        None => diag.hint.clone(),
    };
    let mut d = json!({
        "repo": ctx.repo.as_str(),
        "session": l.session.as_str(),
        "pinned": own.pinned.id.as_str(),
        "used": used.as_str(),
        "inputs": h,
        "cause": diag.cause,
        "paths": diag.paths,
        "hint": diag.hint,
        "message": format!("{}: install inputs differ from the session's deps set; {lead}. {tail}", ctx.repo),
    });
    if let Some(f) = &diag.fix {
        d["fix"] = json!(f);
    }
    if let Value::Object(m) = extra {
        for (k, v) in m {
            d[k] = v;
        }
    }
    PublishWarning { kind, detail: d }
}

/// §2.2 steps 1–4, after `gitio::stage`. Errors carry `stage:
/// "deps_select"`.
pub(crate) fn select_own_deps(
    ctx: &Ctx,
    select: Select,
    plan: &Plan,
    worktree: &Path,
    own: &OwnDeps,
    b: Option<&[InputRecord]>,
    l: &Layout,
) -> Result<Option<PublishWarning>> {
    let Select::Publish { allow_install, installed } = select else { return Ok(None) };
    select_inner(ctx, allow_install, installed, plan, worktree, own, b, l).map_err(tag_stage)
}

#[allow(clippy::too_many_arguments)]
fn select_inner(
    ctx: &Ctx,
    allow_install: bool,
    installed: Option<&Installed>,
    plan: &Plan,
    worktree: &Path,
    own: &OwnDeps,
    b: Option<&[InputRecord]>,
    l: &Layout,
) -> Result<Option<PublishWarning>> {
    let s = strata_deps::input_records(ctx.cfg, ctx.git, worktree)?;
    let h = strata_deps::hash_records(&s);
    if h == own.pinned.inputs_hash {
        return Ok(None);
    }
    let overlap: Vec<&RepoPath> = own
        .cloned
        .iter()
        .filter(|d| plan.changed.iter().any(|(p, v)| v.is_some() && at_or_under(p.as_str(), d.as_str())))
        .collect();
    if !overlap.is_empty() {
        let dirs: Vec<&str> = overlap.iter().map(|d| d.as_str()).collect();
        let fix = switch_fix(ctx, l.session);
        return Err(StrataError::new(
            Code::Drift,
            format!(
                "{}: the session's deps set covers {}, which this publish writes; switch the session's deps (`deps propose --switch`) and publish again",
                ctx.repo,
                dirs.join(", ")
            ),
        )
        .with_detail(json!({
            "reason": "deps_select_failed",
            "sub": "tracked_overlap",
            "dirs": dirs,
            "stage": STAGE_SELECT,
            "repo": ctx.repo.as_str(),
            "session": l.session.as_str(),
            "fix": fix,
            "hint": format!("the workspace's install inputs may first need to match what you want to publish; then run `{fix}`, check that data.switched.kind is \"moved\", and publish again"),
        })));
    }
    let tc = strata_deps::toolchain(ctx.cfg, ctx.deps.env)?;
    let id = strata_deps::set_id(&h, &strata_deps::platform(), &tc, &DepsConfigKey::of(ctx.cfg)?);
    if let Some(mut x) = strata_deps::find_set(ctx.store, ctx.repo, &id)? {
        for d in &own.cloned {
            let r = strata_apfs::remove_tree_rooted(worktree, d)?;
            if r.stuck_total > 0 {
                return Err(StrataError::new(Code::Unknown, format!("{} could not be cleared in staging", d.as_str()))
                    .with_detail(json!({ "reason": "cleanup", "path": d.as_str(), "stuck": r.stuck_total })));
            }
        }
        x.dirs.retain(|d| std::fs::symlink_metadata(worktree.join(d.as_str())).is_err());
        if !x.dirs.is_empty() {
            strata_deps::clone_into(ctx.store, &x, worktree)?;
        }
        let w = match installed {
            Some(i) if i.set == id => {
                let diag = diagnose(ctx, l, &s, b, "installed by this publish");
                warning(
                    ctx,
                    WarningKind::DepsReinstalled,
                    l,
                    own,
                    &id,
                    &h,
                    &diag,
                    json!({ "first_row": i.first_row, "install_ms": i.install_ms }),
                    format!(
                        "installed and used set {} for checks ({:.1} s)",
                        id.as_str(),
                        i.install_ms as f64 / 1000.0
                    ),
                )
            }
            _ => {
                let diag = diagnose(ctx, l, &s, b, "already sealed");
                let extra = match installed {
                    Some(i) => json!({ "after_install": { "set": i.set.as_str(), "first_row": i.first_row } }),
                    None => json!({}),
                };
                warning(
                    ctx,
                    WarningKind::DepsSubstituted,
                    l,
                    own,
                    &id,
                    &h,
                    &diag,
                    extra,
                    format!("used sealed set {} for checks", id.as_str()),
                )
            }
        };
        return Ok(Some(w));
    }
    let diag = diagnose(ctx, l, &s, b, "installed by this publish");
    if allow_install {
        let snap = strata_deps::snapshot_inputs(ctx.deps, worktree, l.row, &h)?;
        return Err(StrataError::new(
            Code::Drift,
            format!("{}: the staged install inputs have no sealed deps set; installing it", ctx.repo),
        )
        .with_detail(json!({
            "reason": INSTALL_NEEDED,
            "repo": ctx.repo.as_str(),
            "session": l.session.as_str(),
            "pinned": own.pinned.id.as_str(),
            "inputs": h,
            "set": id.as_str(),
            "snapshot": snap.to_string_lossy(),
            "diagnosis": diag.json(),
        })));
    }
    let i = installed.expect("round 2 has an install");
    Err(StrataError::new(
        Code::Drift,
        format!("{}: install inputs changed while the deps were installed; publish again", ctx.repo),
    )
    .with_detail(json!({
        "reason": "deps_drift",
        "repo": ctx.repo.as_str(),
        "session": l.session.as_str(),
        "pinned": own.pinned.id.as_str(),
        "inputs": h,
        "set": id.as_str(),
        "install": { "set": i.set.as_str(), "inputs": i.inputs, "first_row": i.first_row },
        "diagnosis": diag.json(),
    })))
}

/// Whether `e` (as `close_failed` returned it) is the round-1 signal.
pub(crate) fn is_install_needed(e: &StrataError) -> bool {
    e.code == Code::Drift && e.detail.as_ref().and_then(|d| d.get("reason")).and_then(Value::as_str) == Some(INSTALL_NEEDED)
}

/// §2.4: build the set for R1's snapshot from the layout session's pinned
/// base, with no publish lock held, then remove the snapshot. A failure is
/// returned with its own code (D10-A).
pub(crate) fn cold_install(ctx: &Ctx, e: &StrataError, first_row: u64) -> Result<Installed> {
    let d = e.detail.clone().unwrap_or_else(|| json!({}));
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let r = (|| -> Result<(DepsSet, u64)> {
        let session = SessionId::try_from(s("session"))?;
        let rec = ctx.store.session(&session)?.ok_or_else(|| {
            StrataError::new(Code::NoSuchSession, format!("no session {}", session.as_str()))
                .with_detail(json!({ "session": session.as_str() }))
        })?;
        let ws = rec.manifest.repos.get(ctx.repo).ok_or_else(|| {
            StrataError::new(Code::InvalidInput, format!("session {} has no workspace of {}", session.as_str(), ctx.repo))
                .with_detail(json!({ "reason": "no_workspace", "session": session.as_str() }))
        })?;
        let tree = ctx.store.base_dir(&ws.base);
        seams::hit(ctx.git, Point::BeforeColdInstall);
        seams::fail(ctx.git, FailPoint::ColdInstall)?;
        seams::fail(ctx.git, FailPoint::ColdInstallPanic)?;
        let t0 = Instant::now();
        let set =
            strata_deps::build(ctx.deps, &tree, Some(&strata_deps::snapshot_tree(ctx.deps.paths, ctx.repo, first_row)))?;
        Ok((set, t0.elapsed().as_millis() as u64))
    })();
    let left = remove_snapshot_warning(ctx, first_row);
    match r {
        Ok((set, install_ms)) => Ok(Installed { set: set.id, inputs: set.inputs_hash, first_row, install_ms, left }),
        Err(be) => {
            let mut detail = be.detail.clone().filter(Value::is_object).unwrap_or_else(|| json!({}));
            detail["stage"] = json!("deps_install");
            detail["publish"] = json!(first_row);
            let mut dd = d.get("diagnosis").cloned().unwrap_or_else(|| json!({}));
            dd["inputs"] = json!(s("inputs"));
            dd["set"] = json!(s("set"));
            detail["deps_drift"] = dd;
            if let Some(w) = left {
                detail["deps_inputs_left"] = w.detail;
            }
            Err(StrataError::new(
                be.code,
                format!(
                    "{}: the publish needs deps for its install inputs and installing them failed: {}",
                    ctx.repo, be.message
                ),
            )
            .with_detail(detail))
        }
    }
}

/// Remove R1's snapshot; `DepsInputsLeft` when something stays.
pub(crate) fn remove_snapshot_warning(ctx: &Ctx, row: u64) -> Option<PublishWarning> {
    if strata_deps::remove_snapshot(ctx.deps.paths, ctx.repo, row) {
        return None;
    }
    let p: PathBuf = strata_deps::snapshot_dir(ctx.deps.paths, ctx.repo, row);
    Some(PublishWarning {
        kind: WarningKind::DepsInputsLeft,
        detail: json!({
            "repo": ctx.repo.as_str(),
            "publish": row,
            "path": p.to_string_lossy(),
            "message": format!("{}: the install-input snapshot {} could not be removed; remove it by hand", ctx.repo, p.display()),
        }),
    })
}

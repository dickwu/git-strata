//! daemon.md §5.5: one handler per method. Every handler calls the lower
//! layer's function; the daemon adds only the queue / lock, the confirm
//! order and the reply shape. A handler that panics answers `Failed` when
//! nothing visible may have been written, `Unknown` otherwise; nothing is
//! ever replayed (invariant 6).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error};
use strata_gitio::Git;
use strata_overlay::{Base, DEFAULT_DIRTY_CAPACITY, Overlay, RestoreRequest, Tracker, WorkspaceRef};
use strata_search::{GrepOpts, Hit, OverlaySource, Search, Which};
use strata_store::PublishPhase;
use strata_types::{Action, BaseGen, Code, RepoConfig, RepoName, SessionId, StrataError, WorkspaceState};

use crate::confirm_flow::{apply, needs_confirm, recheck, require_validated};
use crate::fetch_after::{alert_attach, report_guard_disabled};
use crate::locks::{self, RequestScope};
use crate::resolve;
use crate::rpc::{Params, Reply, Request};
use crate::startup::{Core, Daemon};
use crate::track::{self, drop_tracker, prepare_tool_call};
use crate::{DaemonWarning, GC_KEEP, Result, RpcReject};

thread_local! {
    /// The handler is about to call (or is inside) a lower-layer function
    /// that may write. A panic from then on is `Unknown`, not `Failed`.
    /// Kept apart from `locks::mark_visible` so that the lower layer's own
    /// lock takes stay bounded (`Busy`) until it really wrote.
    static WRITING: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn writing() {
    WRITING.with(|w| w.set(true));
}

fn json_of<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn repo_cfg<'c>(core: &'c Core, repo: &RepoName) -> Result<&'c RepoConfig> {
    core.config.repos.get(repo).ok_or_else(|| {
        StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")).with_detail(json!({ "repo": repo.as_str() }))
    })
}

fn git_of<'c>(core: &'c Core, repo: &RepoName) -> Result<&'c Git> {
    core.gits.get(repo).ok_or_else(|| {
        StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")).with_detail(json!({ "repo": repo.as_str() }))
    })
}

/// The repo's configuration with the pinned (effective) safety flags.
fn effective_cfg(core: &Core, repo: &RepoName) -> Result<RepoConfig> {
    let mut rc = repo_cfg(core, repo)?.clone();
    apply(&mut rc, core.pinned(repo)?);
    Ok(rc)
}

/// The session's recorded workspace of `repo`.
fn workspace(core: &Core, s: &SessionId, repo: &RepoName) -> Result<WorkspaceState> {
    let Some(rec) = core.store.session(s)? else {
        return Err(StrataError::new(Code::NoSuchSession, format!("no session {s}")).with_detail(json!({ "session": s.as_str() })));
    };
    rec.manifest.repos.get(repo).cloned().ok_or_else(|| {
        StrataError::new(Code::NoSuchRepo, format!("session {s} has no repo {repo}")).with_detail(json!({ "repo": repo.as_str() }))
    })
}

/// A base generation with its manifest and exported directory.
fn base_of(core: &Core, g: &BaseGen) -> Result<Base> {
    let m = core.store.manifest(&g.repo, &g.tree)?.ok_or_else(|| {
        StrataError::new(Code::Unknown, format!("the manifest of base {} of {} is missing", g.tree.as_str(), g.repo))
            .with_detail(json!({ "reason": "base_manifest_missing", "repo": g.repo.as_str() }))
    })?;
    Ok(Base { dir: core.store.base_dir(g), r#gen: g.clone(), manifest: Arc::new(m) })
}

// ---- trackers (§5.4, I8) ---------------------------------------------------

struct Tracked {
    root: PathBuf,
    watching: bool,
    tracker: Tracker,
}

/// One `Tracker` per (session, repo), taken out only by a request that
/// holds the session queue and put back when it is done. Also search's
/// `OverlaySource`: the tracker's last overlay when it is on the current
/// base, else a cold `reconcile_full` (read only).
pub struct Trackers<'a> {
    core: &'a Core,
    map: Mutex<BTreeMap<(SessionId, RepoName), Tracked>>,
}

impl<'a> Trackers<'a> {
    pub fn new(core: &'a Core) -> Trackers<'a> {
        Trackers { core, map: Mutex::new(BTreeMap::new()) }
    }

    /// The caller holds `s`'s queue.
    fn take(&self, s: &SessionId, repo: &RepoName, ws: &WorkspaceState) -> Tracked {
        let old = guard(&self.map).remove(&(s.clone(), repo.clone()));
        let mut t = match old {
            Some(t) if t.root == ws.path => t,
            Some(t) => {
                drop_tracker(t.tracker);
                Tracked { root: ws.path.clone(), watching: false, tracker: Tracker::new(&ws.path, DEFAULT_DIRTY_CAPACITY) }
            }
            None => Tracked { root: ws.path.clone(), watching: false, tracker: Tracker::new(&ws.path, DEFAULT_DIRTY_CAPACITY) },
        };
        if t.tracker.last().is_some_and(|o| o.base != ws.base) {
            t.tracker.reset();
            t.watching = false;
        }
        if !t.watching {
            t.watching = track::watch_or_full(&mut t.tracker);
        }
        t
    }

    fn put(&self, s: &SessionId, repo: &RepoName, t: Tracked) {
        if let Some(old) = guard(&self.map).insert((s.clone(), repo.clone()), t) {
            drop_tracker(old.tracker);
        }
    }

    /// After an accepted move (the caller holds `s`'s queue): reset the
    /// Tracker, then watch again (§5.5 `resolve_move`).
    fn restart(&self, s: &SessionId, repo: &RepoName) {
        let old = guard(&self.map).remove(&(s.clone(), repo.clone()));
        if let Some(mut t) = old {
            t.tracker.reset();
            t.watching = track::watch_or_full(&mut t.tracker);
            self.put(s, repo, t);
        }
    }

    /// Drop every tracker of `s` (rm), on the request thread.
    fn drop_session(&self, s: &SessionId) {
        let gone: Vec<Tracked> = {
            let mut m = guard(&self.map);
            let keys: Vec<_> = m.keys().filter(|(id, _)| id == s).cloned().collect();
            keys.into_iter().filter_map(|k| m.remove(&k)).collect()
        };
        for t in gone {
            drop_tracker(t.tracker);
        }
    }

    /// Drop every tracker (shutdown), on the calling thread.
    pub fn clear(&self) {
        let all = std::mem::take(&mut *guard(&self.map));
        for (_, t) in all {
            drop_tracker(t.tracker);
        }
    }

    fn cold(&self, s: &SessionId, repo: &RepoName) -> Result<Overlay> {
        let ws = workspace(self.core, s, repo)?;
        let base = base_of(self.core, &ws.base)?;
        let wref = WorkspaceRef {
            session: s,
            repo,
            root: &ws.path,
            config: repo_cfg(self.core, repo)?,
            git: git_of(self.core, repo)?,
            store: &self.core.store,
        };
        strata_overlay::reconcile_full(&wref, &base)
    }
}

impl OverlaySource for Trackers<'_> {
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay> {
        let cached = guard(&self.map).get(&(session.clone(), repo.clone())).and_then(|t| t.tracker.last().cloned());
        if let Some(o) = cached
            && workspace(self.core, session, repo).is_ok_and(|ws| ws.base == o.base)
        {
            return Some(o);
        }
        self.cold(session, repo).ok()
    }

    fn needs_full(&self, session: &SessionId, repo: &RepoName) -> bool {
        guard(&self.map).get(&(session.clone(), repo.clone())).is_none_or(|t| t.tracker.needs_full())
    }
}

// ---- the server ------------------------------------------------------------

/// The handlers over one running [`Daemon`].
pub struct Server<'d, 'a> {
    d: &'d Daemon<'a>,
    trackers: &'d Trackers<'a>,
    search: Search<'d>,
}

/// Serve `d` with every handler until it stops; trackers are dropped on
/// this thread afterwards.
pub fn serve(d: &Daemon<'_>, listener: &std::os::unix::net::UnixListener, external: &AtomicBool) -> Result<()> {
    let trackers = Trackers::new(d.core);
    let r = {
        let server = Server::new(d, &trackers);
        d.serve(listener, external, &|req| server.handle(req))
    };
    trackers.clear();
    r
}

fn session_of(p: &Params) -> Option<SessionId> {
    match p {
        Params::Checkpoint(p) => Some(p.session.clone()),
        Params::Restore(p) => Some(p.session.clone()),
        Params::Files(p) => Some(p.session.clone()),
        Params::DepsPropose(p) => Some(p.session.clone()),
        Params::Assign(p) => Some(p.session.clone()),
        Params::Rm(p) => Some(p.session.clone()),
        Params::ResolveMove(p) => Some(p.session.clone()),
        _ => None,
    }
}

impl<'d, 'a> Server<'d, 'a> {
    pub fn new(d: &'d Daemon<'a>, trackers: &'d Trackers<'a>) -> Server<'d, 'a> {
        let search = Search::open(&d.core.config, &d.sessions, trackers);
        Server { d, trackers, search }
    }

    fn core(&self) -> &'a Core {
        self.d.core
    }

    /// One request: scope, handler, panic → `Failed` / `Unknown`.
    pub fn handle(&self, req: Request) -> Reply {
        let name = req.method.name();
        if matches!(req.params, Params::DepsPropose(_) | Params::DepsApprove(_)) {
            return Reply::rejected(&req.id, RpcReject::Failed, format!("{name} is not served yet"));
        }
        let _scope = RequestScope::begin(name, session_of(&req.params));
        WRITING.with(|w| w.set(false));
        let r = catch_unwind(AssertUnwindSafe(|| self.dispatch(&req.params)));
        let wrote = WRITING.with(|w| w.replace(false)) || locks::is_visible();
        match r {
            Ok(Ok(v)) => Reply::ok(&req.id, v),
            Ok(Err(e)) => Reply::error(&req.id, &e),
            Err(_) if wrote => Reply::error(
                &req.id,
                &StrataError::new(Code::Unknown, format!("{name} panicked after it may have written; nothing is replayed"))
                    .with_detail(json!({ "reason": "handler_panicked", "method": name })),
            ),
            Err(_) => Reply::rejected(&req.id, RpcReject::Failed, format!("{name} panicked before writing anything")),
        }
    }

    fn dispatch(&self, p: &Params) -> Result<Value> {
        let core = self.core();
        match p {
            Params::New(p) => self.new_session(p.ticket.as_deref(), &p.repos),
            Params::Status(p) => self.status(p.cwd.as_deref(), p.session.as_deref(), p.all),
            Params::Checkpoint(p) => core.locks.with_sessions(std::slice::from_ref(&p.session), || {
                let ws = workspace(core, &p.session, &p.repo)?;
                let base = base_of(core, &ws.base)?;
                let wref = WorkspaceRef {
                    session: &p.session,
                    repo: &p.repo,
                    root: &ws.path,
                    config: repo_cfg(core, &p.repo)?,
                    git: git_of(core, &p.repo)?,
                    store: &core.store,
                };
                let mut t = self.trackers.take(&p.session, &p.repo, &ws);
                prepare_tool_call(&t.tracker, &p.paths);
                writing();
                let r = strata_overlay::checkpoint(&wref, &base, Some(&mut t.tracker), p.reason.clone());
                self.trackers.put(&p.session, &p.repo, t);
                let c = r?;
                Ok(json!({
                    "checkpoint": json_of(&c.checkpoint),
                    "recorded": c.recorded,
                    "changed": json_of(&c.changed),
                    "warnings": json_of(&c.warnings),
                }))
            }),
            Params::Restore(p) => core.locks.with_sessions(std::slice::from_ref(&p.session), || {
                let ws = workspace(core, &p.session, &p.repo)?;
                let base = base_of(core, &ws.base)?;
                let target = core.store.checkpoint(&p.session, p.checkpoint)?;
                let target_base = if target.base != ws.base { Some(base_of(core, &target.base)?) } else { None };
                let wref = WorkspaceRef {
                    session: &p.session,
                    repo: &p.repo,
                    root: &ws.path,
                    config: repo_cfg(core, &p.repo)?,
                    git: git_of(core, &p.repo)?,
                    store: &core.store,
                };
                let mut t = self.trackers.take(&p.session, &p.repo, &ws);
                let req = RestoreRequest { target: p.checkpoint, target_base: target_base.as_ref(), paths: p.paths.as_deref(), verify: false };
                writing();
                let r = strata_overlay::restore(&wref, &base, Some(&mut t.tracker), &req);
                self.trackers.put(&p.session, &p.repo, t);
                Ok(json_of(&r?))
            }),
            Params::Grep(p) => self.grep(&p.pattern, p.session.as_ref(), p.all, p.ignore_case),
            Params::Files(p) => {
                let view = self.search.attach(&p.session, &p.repo)?;
                let r = self.search.files(view.clone(), &p.glob);
                let _ = self.search.detach(view);
                Ok(json!({ "paths": json_of(&r?) }))
            }
            Params::Diff(p) => self.with_publish_ctx(&p.repo, |ctx| {
                let d = strata_publish::diff_between(ctx, p.a.clone(), p.b.clone(), &p.repo)?;
                let both: Vec<Value> = d
                    .both
                    .iter()
                    .map(|(path, r)| match r {
                        Ok(m) => json!({
                            "path": path.as_str(),
                            "merged": m.merged,
                            "hunks": m.hunks.iter().map(|h| json!({
                                "start": h.start, "ours": h.ours, "theirs": h.theirs, "truncated": h.truncated,
                            })).collect::<Vec<_>>(),
                        }),
                        Err(e) => json!({ "path": path.as_str(), "error": json_of(e) }),
                    })
                    .collect();
                Ok(json!({ "only_a": json_of(&d.only_a), "only_b": json_of(&d.only_b), "both": both }))
            }),
            Params::DepsList(p) => Ok(json!({ "proposals": json_of(&strata_deps::open_proposals(&core.store, p.repo.as_ref())?) })),
            Params::Publish(p) => self.publish(&p.group),
            Params::Push(p) => self.push(&p.repo),
            Params::Assign(p) => {
                let repo = core.store.publishes()?.into_iter().find(|r| r.group.id == p.group).and_then(|r| {
                    r.group.batches.iter().find(|b| b.session == p.session).or(r.group.batches.first()).map(|b| b.repo.clone())
                });
                let run = || {
                    writing();
                    strata_publish::assign(&core.store, p.group, &p.path, p.session.clone())
                };
                match repo {
                    Some(repo) => core.locks.with_repo(&repo, run)?,
                    // No such publish: assign names the refusal; nothing to lock.
                    None => run()?,
                }
                Ok(json!({}))
            }
            Params::Rm(p) => {
                let action = Action::RemoveDiscard { session: p.session.clone() };
                let token = if p.discard { Some(require_validated(&core.confirmer, action.clone())?) } else { None };
                core.locks.with_sessions(std::slice::from_ref(&p.session), || {
                    if let Some(t) = &token {
                        recheck(t, &action)?;
                    }
                    self.trackers.drop_session(&p.session);
                    writing();
                    Ok(json_of(&self.d.sessions.remove(&p.session, p.discard, token)?))
                })
            }
            Params::Gc(p) => {
                let repos: Vec<RepoName> = match &p.repo {
                    Some(r) => vec![r.clone()],
                    None => core.config.repos.keys().cloned().collect(),
                };
                let mut done = Vec::new();
                let mut skipped = Vec::new();
                for repo in repos {
                    repo_cfg(core, &repo)?;
                    match core.locks.with_repo(&repo, || {
                        writing();
                        core.store.gc_repo(&repo, GC_KEEP)
                    }) {
                        Ok(rep) => done.push(json!({ "repo": repo.as_str(), "report": json_of(&rep) })),
                        Err(e) if e.code == Code::Busy => skipped.push(json!({ "repo": repo.as_str(), "error": json_of(&e) })),
                        Err(e) => return Err(e),
                    }
                }
                Ok(json!({ "done": done, "skipped": skipped }))
            }
            Params::ConfigRelax(p) => {
                writing();
                core.config_relax(&p.repo)?;
                Ok(json!({}))
            }
            Params::DaemonStatus(p) => self.d.daemon_status(p.verify),
            Params::ResolveMove(p) => {
                let Some(choice) = resolve::move_choice(p.abandon, p.accept) else {
                    return Err(StrataError::new(Code::InvalidInput, "exactly one of abandon and accept")
                        .with_detail(json!({ "reason": "move_choice" })));
                };
                let r = resolve::resolve_move(
                    core,
                    &p.session,
                    &p.repo,
                    choice,
                    &|s, repo, c| self.d.sessions.resolve_failed_move(s, repo, c),
                    &mut || self.trackers.restart(&p.session, &p.repo),
                )?;
                Ok(json_of(&r))
            }
            Params::ChainResolve(p) => {
                let r = resolve::chain_resolve(core, &p.repo, p.choice, &mut |tip| resolve::observe_tip(core, &p.repo, tip));
                if r.as_ref().is_err_and(|e| e.code == Code::Unknown) {
                    self.report_unknown();
                }
                Ok(json_of(&r?))
            }
            Params::DepsPropose(_) | Params::DepsApprove(_) => {
                Err(StrataError::new(Code::InvalidInput, "not served yet"))
            }
        }
    }

    /// §5.7: `new`, then the anchor and the observe retry.
    fn new_session(&self, ticket: Option<&str>, repos: &[RepoName]) -> Result<Value> {
        let core = self.core();
        writing();
        let mut rep = match self.d.sessions.new(ticket, repos) {
            Ok(r) => r,
            Err(e) => {
                for r in repos {
                    if let Some(c) = core.config.repos.get(r)
                        && alert_attach(&core.warnings, &c.path, &e)
                    {
                        break;
                    }
                }
                return Err(e);
            }
        };
        locks::mark_visible();
        let after = core.after_new(&mut rep, &|r, repo| core.observe_new(r, repo));
        for (repo, _) in &after.anchor_unknown {
            core.warnings.add(DaemonWarning::AnchorNotAdvanced { repo: repo.clone() });
        }
        Ok(json!({
            "session": json_of(&rep),
            "anchor_unknown": after.anchor_unknown.iter().map(|(r, e)| json!({ "repo": r.as_str(), "error": json_of(e) })).collect::<Vec<_>>(),
        }))
    }

    /// Read only; no queue, no lock.
    fn status(&self, cwd: Option<&Path>, selector: Option<&str>, all: bool) -> Result<Value> {
        let core = self.core();
        let sessions = &self.d.sessions;
        let ids: Vec<SessionId> = if all {
            sessions.list()?
        } else if let Some(sel) = selector {
            vec![sessions.select(sel)?]
        } else if let Some(cwd) = cwd {
            match sessions.resolve(cwd)? {
                Some((id, _)) => vec![id],
                None => {
                    return Err(StrataError::new(Code::NoSuchSession, format!("{} is in no session's workspace", cwd.display()))
                        .with_detail(json!({ "path": cwd.to_string_lossy() })));
                }
            }
        } else {
            sessions.list()?
        };
        let mut out = Vec::new();
        for id in &ids {
            let st = sessions.status(id)?;
            report_guard_disabled(&core.warnings, &st, &core.config.paths.root);
            let cps: Vec<Value> = core
                .store
                .checkpoints(id, None)?
                .iter()
                .map(|c| {
                    json!({
                        "id": json_of(&c.id),
                        "repo": c.repo.as_str(),
                        "base": json_of(&c.base),
                        "at": c.at.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                        "reason": json_of(&c.reason),
                        "entries": c.entries,
                    })
                })
                .collect();
            out.push(json!({
                "status": json_of(&st),
                "checkpoints": cps,
                "in_flight": core.locks.in_flight(id),
                "queued": core.locks.queued(id),
            }));
        }
        let mut chains = serde_json::Map::new();
        for (repo, git) in &core.gits {
            let branch = &repo_cfg(core, repo)?.publish.branch;
            let v = match strata_deps::publish_chain(git, branch) {
                Ok(c) => json_of(&c),
                Err(e) => json!({ "error": json_of(&e) }),
            };
            chains.insert(repo.as_str().to_string(), v);
        }
        let daemon = self.d.daemon_status(false)?;
        Ok(json!({
            "sessions": out,
            "leftovers": json_of(&sessions.leftovers()?),
            "chains": chains,
            "warnings": daemon["warnings"].clone(),
            "holds": daemon["holds"].clone(),
        }))
    }

    fn grep(&self, pattern: &str, session: Option<&SessionId>, all: bool, ignore_case: bool) -> Result<Value> {
        let core = self.core();
        let mut targets: Vec<(SessionId, RepoName)> = Vec::new();
        let ids = match (session, all) {
            (_, true) => self.d.sessions.list()?,
            (Some(s), false) => vec![s.clone()],
            (None, false) => return Err(StrataError::new(Code::InvalidInput, "grep needs a session or all")),
        };
        for id in &ids {
            if let Some(rec) = core.store.session(id)? {
                for repo in rec.manifest.repos.keys() {
                    targets.push((id.clone(), repo.clone()));
                }
            } else if !all {
                return Err(StrataError::new(Code::NoSuchSession, format!("no session {id}")));
            }
        }
        let opts = GrepOpts { case_insensitive: ignore_case };
        let mut hits: Vec<Hit> = Vec::new();
        let mut skipped = Vec::new();
        for (s, repo) in targets {
            let view = match self.search.attach(&s, &repo) {
                Ok(v) => v,
                Err(e) if all => {
                    skipped.push(json!({ "session": s.as_str(), "repo": repo.as_str(), "error": json_of(&e) }));
                    continue;
                }
                Err(e) => return Err(e),
            };
            let r = self.search.grep(Which::One(view.clone()), pattern, &opts);
            let _ = self.search.detach(view);
            hits.extend(r?);
        }
        let hits: Vec<Value> = hits
            .iter()
            .map(|h| json!({ "session": h.session.as_str(), "repo": h.repo.as_str(), "path": h.path.as_str(), "line": h.line, "text": h.text }))
            .collect();
        Ok(json!({ "hits": hits, "skipped": skipped }))
    }

    fn with_publish_ctx<T>(&self, repo: &RepoName, f: impl FnOnce(&strata_publish::Ctx) -> Result<T>) -> Result<T> {
        let core = self.core();
        let rc = effective_cfg(core, repo)?;
        let git = git_of(core, repo)?;
        let deps = strata_deps::Ctx {
            store: &core.store,
            paths: &core.config.paths,
            repo,
            cfg: &rc,
            git,
            env: &core.env,
            locks: &core.locks,
        };
        let ctx = strata_publish::Ctx { store: &core.store, git, cfg: &rc, repo, deps: &deps, locks: &core.locks, sessions: &self.d.sessions };
        f(&ctx)
    }

    /// Every `Unknown` publish row becomes an `UnknownPublish` warning
    /// (reported, never replayed).
    fn report_unknown(&self) {
        let core = self.core();
        if let Ok(rows) = core.store.open_publishes() {
            for r in rows.into_iter().filter(|r| r.phase == PublishPhase::Unknown) {
                core.warnings.add(DaemonWarning::UnknownPublish { id: r.group.id });
            }
        }
    }

    /// `publish::publish` once per repo of the group, in name order. The
    /// confirmation (when the pinned flags ask for one and the publish
    /// pushes) is redeemed before; publish takes its own locks.
    fn publish(&self, group: &strata_types::ReleaseGroup) -> Result<Value> {
        let core = self.core();
        let repos: BTreeSet<RepoName> = group.batches.iter().map(|b| b.repo.clone()).collect();
        if repos.is_empty() {
            return Err(StrataError::new(Code::InvalidInput, "the release group has no batch"));
        }
        let mut done: Vec<Value> = Vec::new();
        for repo in repos {
            let pinned = core.pinned(&repo)?;
            let branch = repo_cfg(core, &repo)?.publish.branch.clone();
            let token = if pinned.push && needs_confirm(pinned) {
                Some(require_validated(&core.confirmer, Action::PushDeploy { repo: repo.clone(), branch })?)
            } else {
                None
            };
            writing();
            let r = self.with_publish_ctx(&repo, |ctx| strata_publish::publish(ctx, group, token));
            match r {
                Ok(p) => done.push(json!({
                    "repo": repo.as_str(),
                    "id": p.id,
                    "commit": p.commit.as_str(),
                    "checks_ran": p.checks_ran,
                    "push": json_of(&p.push),
                    "warnings": json_of(&p.warnings),
                    "rebases": p.rebases.iter().map(|(s, r)| json!({ "session": s.as_str(), "report": json_of(r) })).collect::<Vec<_>>(),
                })),
                Err(mut e) => {
                    if e.code == Code::Unknown {
                        self.report_unknown();
                    }
                    if !done.is_empty() {
                        let d = e.detail.get_or_insert_with(|| json!({}));
                        if d.is_object() {
                            d["published_before"] = Value::Array(done);
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(json!({ "published": done }))
    }

    /// §5.5 push: the chain tip is read before the prompt and checked
    /// again under the repo lock; a moved tip is `NotConfirmed`.
    fn push(&self, repo: &RepoName) -> Result<Value> {
        let core = self.core();
        let rc = effective_cfg(core, repo)?;
        let git = git_of(core, repo)?;
        let branch = rc.publish.branch.clone();
        let action = Action::PushDeploy { repo: repo.clone(), branch: branch.clone() };
        let before = strata_deps::publish_chain(git, &branch)?.and_then(|c| c.top);
        let token = if needs_confirm(core.pinned(repo)?) { Some(require_validated(&core.confirmer, action.clone())?) } else { None };
        core.locks.with_repo(repo, || {
            if let Some(t) = &token {
                recheck(t, &action)?;
            }
            let tip = strata_deps::publish_chain(git, &branch)?.and_then(|c| c.top);
            if token.is_some() && tip != before {
                return Err(StrataError::new(Code::NotConfirmed, "the publish chain moved after the confirmation").with_detail(json!({
                    "reason": "chain_moved",
                    "confirmed": before.as_ref().map(|c| c.as_str()),
                    "now": tip.as_ref().map(|c| c.as_str()),
                })));
            }
            let Some(tip) = tip else {
                return Err(StrataError::new(Code::NoCommit, format!("the publish chain of {repo} has no commit to push"))
                    .with_detail(json!({ "repo": repo.as_str() })));
            };
            let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = core.config.paths.root.join("daemon/push").join(format!("{}-{stamp}", std::process::id()));
            fs::create_dir_all(&dir).map_err(|e| classify_io_error(&e, IoSide::Write, &dir))?;
            let r = (|| {
                let wt = dir.join(repo.as_str());
                let gd = dir.join(format!(".git-{repo}"));
                git.export_tree(&git.tree_of(&tip)?, &wt)?;
                git.attach_staging(&wt, &gd, &tip)?;
                writing();
                strata_publish::push(git, &rc, repo, &wt, token)
            })();
            let _ = fs::remove_dir_all(&dir);
            if r.as_ref().is_err_and(|e| e.code == Code::Unknown) {
                self.report_unknown();
            }
            Ok(json!({ "push": json_of(&r?), "commit": tip.as_str() }))
        })
    }
}

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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error};
use strata_gitio::Git;
use strata_overlay::{Base, DEFAULT_DIRTY_CAPACITY, Overlay, RestoreRequest, Tracker, WorkspaceRef};
use strata_search::{GrepOpts, Hit, OverlaySource, Search, Which};
use strata_store::PublishPhase;
use strata_types::{Action, BaseGen, Code, CommitOid, RepoConfig, RepoName, RepoPath, SessionId, StrataError, WorkspaceState};

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

    /// The tracked workspaces of every Active session.
    fn tracked(&self) -> Vec<(SessionId, RepoName)> {
        let Ok(all) = self.core.store.sessions() else { return Vec::new() };
        all.into_iter()
            .filter(|r| r.lifecycle == strata_store::Lifecycle::Active)
            .flat_map(|r| {
                let id = r.manifest.id.clone();
                r.manifest.repos.into_iter().filter(|(_, w)| w.tracked).map(move |(repo, _)| (id.clone(), repo))
            })
            .collect()
    }

    /// §5.1 step 13: a Tracker that watches for every existing tracked
    /// workspace. Only for a session whose queue is free right now (a busy
    /// one gets its Tracker from the request that holds it).
    pub fn watch_tracked(&self) {
        for (s, repo) in self.tracked() {
            if self.core.locks.is_shut_down() {
                return;
            }
            let _ = self.core.locks.try_session(&s, || {
                let ws = workspace(self.core, &s, &repo)?;
                let t = self.take(&s, &repo, &ws);
                self.put(&s, &repo, t);
                Ok(())
            });
        }
    }

    /// §5.9, one pass: for every tracked workspace whose session queue is
    /// free, a full scan through its Tracker (`compute` with no dirty list
    /// walks the whole tree, so an event that never arrived is found) that
    /// refreshes the cache and writes no checkpoint (D16). Busy sessions
    /// are skipped and listed. Then gc per repo under its write lock.
    pub fn reconcile_once(&self) -> Value {
        let core = self.core;
        let (mut scanned, mut skipped, mut failed, mut gc) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (s, repo) in self.tracked() {
            if core.locks.is_shut_down() {
                break;
            }
            let r = core.locks.try_session(&s, || {
                let ws = workspace(core, &s, &repo)?;
                let base = base_of(core, &ws.base)?;
                let wref = WorkspaceRef { session: &s, repo: &repo, root: &ws.path, config: repo_cfg(core, &repo)?, git: git_of(core, &repo)?, store: &core.store };
                let mut t = self.take(&s, &repo, &ws);
                let r = strata_overlay::compute(&wref, &base, &mut t.tracker, None);
                self.put(&s, &repo, t);
                r
            });
            let at = json!({ "session": s.as_str(), "repo": repo.as_str() });
            match r {
                None => skipped.push(json!({ "session": s.as_str(), "repo": repo.as_str(), "holder": core.locks.in_flight(&s) })),
                Some(Ok(scan)) => scanned.push(json!({ "session": s.as_str(), "repo": repo.as_str(), "changed": json_of(&scan.changed) })),
                Some(Err(e)) => failed.push(json!({ "at": at, "error": json_of(&e) })),
            }
        }
        for repo in core.config.repos.keys() {
            if core.locks.is_shut_down() {
                break;
            }
            match core.locks.with_repo(repo, || core.store.gc_repo(repo, GC_KEEP).map(|_| ())) {
                Ok(()) => gc.push(json!({ "repo": repo.as_str(), "ok": true })),
                Err(e) => gc.push(json!({ "repo": repo.as_str(), "ok": false, "error": json_of(&e) })),
            }
        }
        json!({ "at": json_of(&SystemTime::now()), "scanned": scanned, "skipped": skipped, "failed": failed, "gc": gc })
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

/// Review M2: search must see the workspace as it is now, not the overlay
/// the Tracker cached at the last checkpoint (a file created, deleted or
/// renamed since, or an ignore rule changed, would be missing or stale).
/// So `last` never returns the cache as is. With the session queue free
/// right now it takes the Tracker and runs a full-walk `compute(.., None)`:
/// the cache keeps unchanged files from being read again, it writes
/// nothing to the Store or the workspace, and it does not depend on
/// FSEvents having delivered (flush is not a barrier). If a request holds
/// the queue (the Tracker is checked out), a cold `reconcile_full`, read
/// only. Never waits for the queue [实现时补的决定].
impl OverlaySource for Trackers<'_> {
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay> {
        let warm = self.core.locks.try_session(session, || {
            let core = self.core;
            let ws = workspace(core, session, repo)?;
            let base = base_of(core, &ws.base)?;
            let wref = WorkspaceRef { session, repo, root: &ws.path, config: repo_cfg(core, repo)?, git: git_of(core, repo)?, store: &core.store };
            let mut t = self.take(session, repo, &ws);
            let r = strata_overlay::compute(&wref, &base, &mut t.tracker, None);
            self.put(session, repo, t);
            r.map(|scan| scan.overlay)
        });
        match warm {
            Some(Ok(o)) => Some(o),
            _ => self.cold(session, repo).ok(),
        }
    }

    fn needs_full(&self, session: &SessionId, repo: &RepoName) -> bool {
        guard(&self.map).get(&(session.clone(), repo.clone())).is_none_or(|t| t.tracker.needs_full())
    }
}

/// `NoSuchProposal` for a proposal that is no longer Pending; `detail.state`
/// is its state, plus the replacement when `ensure_target` filed one.
fn not_pending(p: &strata_deps::Proposal, replacement: Option<&strata_deps::Proposal>) -> StrataError {
    let mut d = json!({ "proposal": p.id.get(), "state": json_of(&p.state) });
    if let Some(r) = replacement {
        d["replacement"] = json_of(r);
    }
    StrataError::new(Code::NoSuchProposal, format!("proposal {} is not pending", p.id.get())).with_detail(d)
}

// ---- the server ------------------------------------------------------------

/// The handlers over one running [`Daemon`].
pub struct Server<'d, 'a> {
    d: &'d Daemon<'a>,
    trackers: &'d Trackers<'a>,
    search: Search<'d>,
}

/// Serve `d` with every handler until it stops; trackers are dropped on
/// this thread afterwards. Alongside the accept loop runs the §5.9
/// reconcile loop (step 13's watchers first, then a pass every
/// `opts.reconcile_every`); it ends with the accept loop and is joined
/// before the trackers are dropped (review M1).
pub fn serve(d: &Daemon<'_>, listener: &std::os::unix::net::UnixListener, external: &AtomicBool) -> Result<()> {
    let trackers = Trackers::new(d.core);
    let done = AtomicBool::new(false);
    let r = std::thread::scope(|s| {
        let server = Server::new(d, &trackers);
        s.spawn(|| reconcile_loop(d, &trackers, &|| done.load(Ordering::SeqCst) || d.stopping() || external.load(Ordering::SeqCst)));
        let r = d.serve(listener, external, &|req| server.handle(req));
        done.store(true, Ordering::SeqCst);
        r
    });
    trackers.clear();
    r
}

/// §5.9 on its own thread: watch existing tracked workspaces, then one
/// [`Trackers::reconcile_once`] every `reconcile_every` until `stop`. The
/// wait is checked every 20 ms, so shutdown never waits for an interval;
/// a pass in progress finishes (its queue / lock takes fail `ReadOnly`
/// once the locks are shut down). A panicking pass is recorded, not fatal.
fn reconcile_loop(d: &Daemon<'_>, trackers: &Trackers<'_>, stop: &dyn Fn() -> bool) {
    let every = d.core.opts.reconcile_every;
    trackers.watch_tracked();
    loop {
        let next = std::time::Instant::now() + every;
        loop {
            if stop() {
                return;
            }
            let now = std::time::Instant::now();
            if now >= next {
                break;
            }
            std::thread::sleep((next - now).min(std::time::Duration::from_millis(20)));
        }
        let last = catch_unwind(AssertUnwindSafe(|| trackers.reconcile_once()))
            .unwrap_or_else(|_| json!({ "at": json_of(&SystemTime::now()), "panicked": true }));
        d.reconciled(last);
    }
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
            Params::Publish(p) => self.publish(&p.group, &p.shown),
            Params::Push(p) => self.push(&p.repo, p.shown.as_ref()),
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
                // Review M5: wait out `publishing` (holding nothing), then the
                // queue place, the flag checked again under it.
                crate::resolve::with_queue_not_publishing(core, &p.session, || {
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
                let r = resolve::chain_resolve(core, &p.repo, p.choice, p.shown.as_ref(), &mut |tip| resolve::observe_tip(core, &p.repo, tip));
                if r.as_ref().is_err_and(|e| e.code == Code::Unknown) {
                    self.report_unknown();
                }
                Ok(json_of(&r?))
            }
            Params::DepsPropose(p) => self.deps_propose(&p.session, &p.repo, p.switch),
            Params::DepsApprove(p) => self.deps_approve(p.proposal, p.sessions.as_deref()),
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

    /// deps.md §5.12 propose: drift → build outside the lock → `propose`
    /// under the repo lock; with `switch`, the proposer's own move
    /// (`plan_switch`, no confirmation). No drift: `proposal` is null.
    fn deps_propose(&self, s: &SessionId, repo: &RepoName, switch: bool) -> Result<Value> {
        let core = self.core();
        let ws = workspace(core, s, repo)?;
        let rc = effective_cfg(core, repo)?;
        let git = git_of(core, repo)?;
        let ctx = strata_deps::Ctx {
            store: &core.store,
            paths: &core.config.paths,
            repo,
            cfg: &rc,
            git,
            env: &core.env,
            locks: &core.locks,
        };
        let Some(pinned) = strata_deps::find_set(&core.store, repo, &ws.base.deps)? else {
            return Err(StrataError::new(Code::Corrupt, format!("the pinned deps set {} has no record", ws.base.deps.as_str()))
                .with_detail(json!({ "repo": repo.as_str(), "set": ws.base.deps.as_str() })));
        };
        // 1.
        let Some(drift) = strata_deps::drift(&rc, git, &ws.path, &pinned)? else {
            return Ok(json!({ "proposal": null, "drift": null, "switched": null }));
        };
        // 2. Outside the lock: the deps set of the workspace's inputs.
        writing();
        let set = strata_deps::build(&ctx, &core.store.base_dir(&ws.base), Some(&ws.path))?;
        // 3.
        let p = core.locks.with_repo(repo, || {
            strata_deps::propose(
                &ctx,
                strata_deps::ProposeArgs {
                    origin: strata_deps::ProposalOrigin::Session { session: s.clone() },
                    from: ws.base.deps.clone(),
                    from_inputs: pinned.inputs_hash.clone(),
                    to: set.id.clone(),
                    to_inputs: set.inputs_hash.clone(),
                    tree: None,
                    head: None,
                },
            )
        })?;
        // 4.
        let switched = if switch {
            let target_tree = match &p.state {
                strata_deps::ProposalState::Pending => p.tree.clone().unwrap_or_else(|| ws.base.tree.clone()),
                _ => ws.base.tree.clone(),
            };
            self.base_for(repo, &target_tree, &p.to)?;
            let repo_id = git.repo_id();
            let r = core.locks.with_sessions(std::slice::from_ref(s), || {
                core.locks.with_repo(repo, || {
                    let ws = workspace(core, s, repo)?;
                    let plan = strata_deps::plan_switch(&ctx, p.id, s, &ws)?;
                    let mut out = Vec::new();
                    for step in &plan.steps {
                        out.push(self.apply_step(step, &repo_id, repo, &mut Vec::new()));
                    }
                    Ok(out)
                })
            })?;
            Value::Array(r)
        } else {
            Value::Null
        };
        Ok(json!({ "proposal": json_of(&p), "drift": json_of(&drift), "switched": switched }))
    }

    /// deps.md §5.12 approve: refusal before the prompt, the prompt, the
    /// target outside the lock, then queues → repo lock → plan → moves →
    /// record. A replaced proposal moves nothing and is `NoSuchProposal`
    /// with the replacement in `detail` (cli.md §5: never auto-approved).
    fn deps_approve(&self, id: strata_types::ProposalId, only: Option<&[SessionId]>) -> Result<Value> {
        let core = self.core();
        // 1.
        let p = strata_deps::get_proposal(&core.store, id)?;
        if p.state != strata_deps::ProposalState::Pending {
            return Err(not_pending(&p, None));
        }
        let repo = p.repo.clone();
        // 2.
        let action = Action::DepsApprove { proposal: id };
        let token = require_validated(&core.confirmer, action.clone())?;
        let rc = effective_cfg(core, &repo)?;
        let git = git_of(core, &repo)?;
        let ctx = strata_deps::Ctx {
            store: &core.store,
            paths: &core.config.paths,
            repo: &repo,
            cfg: &rc,
            git,
            env: &core.env,
            locks: &core.locks,
        };
        let Some(tree) = p.tree.clone() else {
            return Err(StrataError::new(Code::InvalidInput, "a pending proposal without a target")
                .with_detail(json!({ "reason": "no_target", "proposal": id.get() })));
        };
        // 3. Outside the lock.
        if strata_deps::find_set(&core.store, &repo, &p.to)?.is_none() {
            let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = core.config.paths.root.join("daemon/approve").join(format!("{}-{stamp}", std::process::id()));
            fs::create_dir_all(&dir).map_err(|e| classify_io_error(&e, IoSide::Write, &dir))?;
            let wt = dir.join(repo.as_str());
            let r = git.export_tree(&tree, &wt).and_then(|_| {
                writing();
                strata_deps::ensure_target(&ctx, id, &wt)
            });
            let _ = fs::remove_dir_all(&dir);
            match r? {
                strata_deps::Target::Ready { .. } => {}
                strata_deps::Target::Replaced { superseded, replacement } => {
                    let now = strata_deps::get_proposal(&core.store, superseded)?;
                    return Err(not_pending(&now, Some(&replacement)));
                }
            }
        }
        self.base_for(&repo, &tree, &p.to)?;
        // 4.
        let mut ids: Vec<SessionId> = core
            .store
            .sessions()?
            .into_iter()
            .filter(|r| r.lifecycle != strata_store::Lifecycle::Removed && r.manifest.repos.contains_key(&repo))
            .map(|r| r.manifest.id)
            .collect();
        let complete = only.is_none();
        if let Some(only) = only {
            for s in only {
                if !ids.contains(s) {
                    return Err(StrataError::new(Code::NoSuchSession, format!("no session {s} on {repo}"))
                        .with_detail(json!({ "session": s.as_str(), "repo": repo.as_str() })));
                }
            }
            ids = only.to_vec();
        }
        ids.sort();
        ids.dedup();
        let repo_id = git.repo_id();
        core.locks.with_sessions(&ids, || {
            core.locks.with_repo(&repo, || {
                recheck(&token, &action)?;
                let cur = strata_deps::get_proposal(&core.store, id)?;
                if cur.state != strata_deps::ProposalState::Pending {
                    return Err(not_pending(&cur, None));
                }
                // 5.
                let mut list = Vec::new();
                for s in &ids {
                    list.push((s.clone(), workspace(core, s, &repo)?));
                }
                let plan = strata_deps::plan_rollout(&ctx, id, &list, complete)?;
                // 6.
                writing();
                let mut moved = Vec::new();
                let results: Vec<Value> = plan.steps.iter().map(|st| self.apply_step(st, &repo_id, &repo, &mut moved)).collect();
                // 7.
                let after = strata_deps::record_rollout(&core.store, &plan, &moved)?;
                Ok(json!({ "results": results, "proposal": json_of(&after) }))
            })
        })
    }

    /// One rollout step: a move (`moved`) or a per-entry skip; a failed
    /// move is a skip with its code, never the whole call's failure.
    fn apply_step(&self, step: &strata_deps::RolloutStep, repo_id: &strata_types::RepoId, repo: &RepoName, moved: &mut Vec<SessionId>) -> Value {
        match step {
            strata_deps::RolloutStep::Move { session, new_base, head } => {
                match self.d.sessions.move_workspace(session, repo_id, new_base.clone(), head.clone()) {
                    Ok(r) => {
                        moved.push(session.clone());
                        self.trackers.restart(session, repo);
                        json!({ "session": session.as_str(), "kind": "moved", "report": json_of(&r) })
                    }
                    Err(e) => json!({
                        "session": session.as_str(), "kind": "skipped", "code": e.code.as_str(),
                        "detail": e.detail.clone().unwrap_or_else(|| json!({ "message": e.message })),
                    }),
                }
            }
            strata_deps::RolloutStep::Skip { session, reason } => {
                let code = match reason {
                    strata_deps::SkipReason::OpenFiles { .. } => Some(Code::OpenFiles),
                    strata_deps::SkipReason::Drift { .. } => Some(Code::Drift),
                    strata_deps::SkipReason::Error { error } => Some(error.code),
                    _ => None,
                };
                let mut v = json!({ "session": session.as_str(), "kind": "skipped", "detail": json_of(reason) });
                if let Some(c) = code {
                    v["code"] = json!(c.as_str());
                }
                v
            }
        }
    }

    /// `ensure_base_for` outside the locks; a plain lock `Busy` (no
    /// reason) is asked again, as publish's `base_for` does.
    fn base_for(&self, repo: &RepoName, tree: &strata_types::TreeOid, deps: &strata_types::DepsSetId) -> Result<BaseGen> {
        loop {
            match self.d.sessions.ensure_base_for(repo, tree, deps) {
                Err(e) if e.code == Code::Busy && e.detail.as_ref().and_then(|d| d.get("reason")).is_none() => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                r => return r,
            }
        }
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
    /// pushes) is redeemed before; publish takes its own locks. Review H2:
    /// a confirmed repo's publish is bound to the chain top the cli showed
    /// (`shown`), checked under the repo lock before staging.
    ///
    /// Review H4 (kept by design; daemon.md D15, D22): an agent re-runs the
    /// same publish after fixing a `Conflict`. Selectors whose current
    /// content is exactly what this session already landed are dropped
    /// before anything is called (`rerun_paths`, review-2 N1: per selector,
    /// by content); a repo all of whose batches are dropped, or whose publish is
    /// `NoCommit` over batches all published before, is reported in
    /// `already_published` and gets no second commit or push. Every error
    /// carries `detail.repo` (the repo that stopped) and, when an earlier
    /// repo of this call landed, `detail.published_before`. Review M4: an
    /// error after this repo's commit was written (`detail.commit`) also
    /// carries `detail.landed`, a `published` entry for it whose `push` is
    /// what is known of the push (`Unknown`, `Refused`, or null: not sent).
    fn publish(&self, group: &strata_types::ReleaseGroup, shown: &BTreeMap<RepoName, Option<CommitOid>>) -> Result<Value> {
        let core = self.core();
        if group.batches.is_empty() {
            return Err(StrataError::new(Code::InvalidInput, "the release group has no batch"));
        }
        let rows = core.store.publishes()?;
        let mut kept = Vec::new();
        let mut landed: BTreeMap<RepoName, Value> = BTreeMap::new();
        for b in &group.batches {
            let (paths, proof) = rerun_paths(core, &rows, b)?;
            if paths.is_empty()
                && let Some(v) = proof
            {
                landed.insert(b.repo.clone(), v);
            } else {
                kept.push(strata_types::Batch { paths, ..b.clone() });
            }
        }
        let group = strata_types::ReleaseGroup { id: group.id, batches: kept };
        let repos: BTreeSet<RepoName> = group.batches.iter().map(|b| b.repo.clone()).collect();
        landed.retain(|r, _| !repos.contains(r));
        let mut already: Vec<Value> = landed.into_values().collect();
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
            let bound = token.as_ref().map(|_| shown.get(&repo).cloned().flatten());
            let r = self.with_publish_ctx(&repo, |ctx| strata_publish::publish_bound(ctx, &group, token, bound.clone()));
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
                Err(e)
                    if e.code == Code::NoCommit
                        && let Some(v) = group
                            .batches
                            .iter()
                            .filter(|b| b.repo == repo)
                            .map(|b| landed_before(&rows, b))
                            .collect::<Option<Vec<Value>>>()
                            .and_then(|v| v.into_iter().next_back()) =>
                {
                    already.push(v);
                }
                Err(mut e) => {
                    if e.code == Code::Unknown {
                        self.report_unknown();
                    }
                    let d = e.detail.get_or_insert_with(|| json!({}));
                    if d.is_object() {
                        if d.get("repo").is_none() {
                            d["repo"] = json!(repo.as_str());
                        }
                        if let Some(c) = d.get("commit").filter(|c| c.is_string()).cloned() {
                            d["landed"] = landed_after_commit(&repo, c, d);
                        }
                        if !done.is_empty() {
                            d["published_before"] = Value::Array(done);
                        }
                        if !already.is_empty() {
                            d["already_published"] = Value::Array(already);
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(json!({ "published": done, "already_published": already }))
    }

    /// §5.5 push: the chain tip is checked under the repo lock against the
    /// one the cli showed the human (review H2: `shown`, not a read of the
    /// daemon's own); a different tip is `NotConfirmed` (`chain_moved`).
    fn push(&self, repo: &RepoName, shown: Option<&CommitOid>) -> Result<Value> {
        let core = self.core();
        let rc = effective_cfg(core, repo)?;
        let git = git_of(core, repo)?;
        let branch = rc.publish.branch.clone();
        let action = Action::PushDeploy { repo: repo.clone(), branch: branch.clone() };
        let before = shown.cloned();
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
            // Review M6: the scene is kept (and reported) when the outcome
            // is unknown; see `resolve::with_staging`.
            let uncertain = |r: &Result<strata_types::PushOutcome>| r.as_ref().is_err_and(|e| e.code == Code::Unknown);
            let r = resolve::with_staging(core, git, repo, &tip, uncertain, |wt| {
                writing();
                strata_publish::push(git, &rc, repo, wt, token)
            });
            if r.as_ref().is_err_and(|e| e.code == Code::Unknown) {
                self.report_unknown();
            }
            if matches!(r, Ok(strata_types::PushOutcome::Pushed)) {
                resolve::settle_push_scenes(core, git, repo, &tip);
            }
            Ok(json!({ "push": json_of(&r?), "commit": tip.as_str() }))
        })
    }
}

/// Review M4: the `published` entry of a repo whose publish failed after
/// its commit: `push` from the error's `kind` (`Unknown` → `Unknown`,
/// `PushRefused` → `Refused(message)`); any other after-commit error says
/// nothing about a push, so null [实现时补的决定].
fn landed_after_commit(repo: &RepoName, commit: Value, d: &Value) -> Value {
    let push = match d.get("kind").and_then(Value::as_str) {
        Some("Unknown") => json!({ "kind": "Unknown" }),
        Some("PushRefused") => json!({ "kind": "Refused", "message": d.get("message").cloned().unwrap_or(json!("")) }),
        _ => Value::Null,
    };
    json!({ "repo": repo.as_str(), "commit": commit, "checks_ran": null, "push": push, "warnings": [], "rebases": [] })
}

/// Review-2 N1: which selectors of `b` still go to stage on a re-run, and
/// the `already_published` entry of the newest proof for those that do not.
/// Per selector, with stage's own selection rules (`stage::selects`): one
/// that picks any checkpoint entry (new or changed content, exact path,
/// rename source or anything under a directory) is kept. One that picks
/// nothing (the session's content there equals its base) is dropped only
/// when the newest `Done` row of this session and repo that committed
/// files at or under it (`outcome.paths`, within a selector of that
/// session's batch) has, for each such file, the same mode and blob in its
/// commit as in the checkpoint's base, i.e. the current content is what
/// landed. Anything else is kept, and stage answers `NoMatch` honestly
/// (e.g. the session's file was later changed upstream by someone else)
/// [实现时补的决定]. Rename/type pairs are unaffected: a dropped selector
/// picked no entry. A checkpoint that cannot be read keeps the batch as is
/// (stage reports it). Read-only; git errors are returned before any write.
fn rerun_paths(core: &Core, rows: &[strata_store::PublishRecord], b: &strata_types::Batch) -> Result<(Vec<RepoPath>, Option<Value>)> {
    let Ok(cp) = core.store.checkpoint(&b.session, b.checkpoint) else {
        return Ok((b.paths.clone(), None));
    };
    let mut keep = Vec::new();
    let mut proof: Option<Value> = None;
    for p in &b.paths {
        if strata_publish::stage::selects(&cp, p) {
            keep.push(p.clone());
            continue;
        }
        match landed_content(core, rows, b, &cp.base.tree, p)? {
            Some(v) => {
                if proof.is_none() {
                    proof = Some(v);
                }
            }
            None => keep.push(p.clone()),
        }
    }
    Ok((keep, proof))
}

/// The newest `Done` row in which `b`'s session committed files at or under
/// `p` in `b`'s repo, as `{repo, publish, commit, push}`, when every such
/// file has the same mode and object in that commit as in `base`; `None`
/// when there is no such row or the content differs.
fn landed_content(core: &Core, rows: &[strata_store::PublishRecord], b: &strata_types::Batch, base: &strata_types::TreeOid, p: &RepoPath) -> Result<Option<Value>> {
    use strata_publish::stage::at_or_under;
    for r in rows.iter().rev() {
        if r.phase != PublishPhase::Done {
            continue;
        }
        let Some(o) = r.outcome.as_ref() else { continue };
        let Some(commit) = o.get("commit").and_then(Value::as_str) else { continue };
        let mine: Vec<&RepoPath> =
            r.group.batches.iter().filter(|x| x.session == b.session && x.repo == b.repo).flat_map(|x| x.paths.iter()).collect();
        let files: Vec<RepoPath> = o
            .get("paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|f| at_or_under(f, p.as_str()) && mine.iter().any(|q| at_or_under(f, q.as_str())))
            .filter_map(|f| RepoPath::try_from(f.to_string()).ok())
            .collect();
        if files.is_empty() {
            continue;
        }
        let Ok(c) = CommitOid::try_from(commit.to_string()) else { return Ok(None) };
        let git = git_of(core, &b.repo)?;
        let (now, then) = (git.tree_items(base, &files)?, git.tree_items(&git.tree_of(&c)?, &files)?);
        let key = |m: &BTreeMap<String, strata_gitio::TreeItem>, f: &RepoPath| m.get(f.as_str()).map(|i| (i.mode, i.oid.clone()));
        if files.iter().all(|f| key(&now, f) == key(&then, f)) {
            return Ok(Some(json!({ "repo": b.repo.as_str(), "publish": r.group.id, "commit": commit, "push": o.get("push").cloned().unwrap_or(Value::Null) })));
        }
        return Ok(None);
    }
    Ok(None)
}

/// Review H4: the newest `Done` publish row with a commit in which `b`'s
/// session published every selector of `b` in `b`'s repo (equal, or one
/// at or under the other: review-2 N1), as an `already_published` entry
/// `{repo, publish, commit, push}`. Used only after stage said `NoCommit`
/// (nothing differs from the chain), so no content is lost.
fn landed_before(rows: &[strata_store::PublishRecord], b: &strata_types::Batch) -> Option<Value> {
    use strata_publish::stage::at_or_under;
    rows.iter().rev().find_map(|r| {
        if r.phase != PublishPhase::Done {
            return None;
        }
        let o = r.outcome.as_ref()?;
        let commit = o.get("commit").and_then(Value::as_str)?;
        let covers = r.group.batches.iter().any(|x| {
            x.session == b.session
                && x.repo == b.repo
                && b.paths.iter().all(|p| x.paths.iter().any(|q| at_or_under(p.as_str(), q.as_str()) || at_or_under(q.as_str(), p.as_str())))
        });
        covers.then(|| {
            json!({ "repo": b.repo.as_str(), "publish": r.group.id, "commit": commit, "push": o.get("push").cloned().unwrap_or(Value::Null) })
        })
    })
}

//! The repo write locks and session queues (daemon.md §5.2, I2–I5, I11, D8).
//!
//! Both are FIFO, non-reentrant and released on drop (normal return, `Err`
//! or panic). The first take of a request that has not written anything
//! visible yet waits at most the lock wait (`LOCK_WAIT`, configurable for
//! tests) and then fails `Busy`; every later take, and any take after the
//! request has written, waits without bound. Shutdown fails every waiter
//! with `ReadOnly` (`shutting_down`) and lets the holders finish.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::drain::{Phase, Write};
use crate::{DaemonWarning, LockHold, Result};

/// Status-bar §3.1, §12.1: how long a background hold, or a hold doing
/// exempt long work (a move, a deps rollout, a base build, a session
/// creation), lasts before `LongHold`.
pub const BACKGROUND_HOLD_MAX: Duration = Duration::from_secs(30 * 60);

/// Who a hold or an activity works for (status-bar §3.1, §11.1): a client's
/// request (the request scope or the session layer's request tag is set on
/// the taking thread) or the daemon's own background work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Request,
    Background,
}

thread_local! {
    static YIELD_TO: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static SUBJECT: RefCell<Option<SessionId>> = const { RefCell::new(None) };
}

/// Status-bar §3.1, §12.7: run `f` with `session` as the subject of every
/// lock this thread takes. `holds[].session`, `LongHold.session` and a
/// waiter's Busy `holder_session` name it instead of the request's own
/// session (a catch-up move names the session it moves); the request's
/// session and the `LongWait` and expired-wait keys stay as they are.
/// Restored on every exit, including unwinding.
pub fn with_subject<R>(session: Option<SessionId>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<SessionId>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SUBJECT.with(|s| *s.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SUBJECT.with(|s| s.replace(session)));
    f()
}

/// Only cooperative old-tree work installs this flag. Every lock it takes
/// carries the same flag, so either a repo or session waiter can yield it.
pub fn with_yield<R>(flag: Arc<AtomicBool>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            YIELD_TO.with(|y| *y.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(YIELD_TO.with(|y| y.borrow_mut().replace(flag)));
    f()
}

/// The per-thread request context (D8).
struct Ctx {
    method: String,
    session: Option<SessionId>,
    write: Option<Arc<Write>>,
    /// Something visible has been written (set after `session::new`
    /// returns, after a checkpoint commits, …).
    visible: bool,
    /// A lock or queue has already been taken by this request.
    took: bool,
    /// Repos whose lock this thread holds (non-reentrant check).
    repos: BTreeSet<RepoName>,
    /// Sessions whose queue place this thread holds.
    sessions: BTreeSet<SessionId>,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

/// The request context of the current thread; dropped at the end of the
/// request (also on panic).
pub struct RequestScope {
    _priv: (),
}

impl RequestScope {
    /// Start a request on this thread.
    pub fn begin(method: &str, session: Option<SessionId>) -> RequestScope {
        CTX.with(|c| {
            *c.borrow_mut() = Some(Ctx {
                method: method.to_string(),
                session,
                write: None,
                visible: false,
                took: false,
                repos: BTreeSet::new(),
                sessions: BTreeSet::new(),
            })
        });
        RequestScope { _priv: () }
    }

    pub(crate) fn track(&self, write: Write) {
        CTX.with(|c| c.borrow_mut().as_mut().expect("request scope").write = Some(Arc::new(write)));
    }
}

impl Drop for RequestScope {
    fn drop(&mut self) {
        CTX.with(|c| *c.borrow_mut() = None);
    }
}

fn with_write<T>(f: impl FnOnce(&Arc<Write>) -> T) -> Option<T> {
    CTX.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.write.as_ref().map(f))
    })
}

/// Keep the same inflight row alive for work delegated beyond the RPC.
pub(crate) fn retain_request_write() -> Option<Arc<Write>> {
    with_write(Arc::clone)
}

pub(crate) fn request_session(session: SessionId) {
    with_write(|write| write.set_session(session));
}

pub(crate) fn request_repo(repo: RepoName) {
    with_write(|write| write.set_repo(repo));
}

/// Show a temporary phase, restoring the enclosing phase on every exit.
pub(crate) fn request_phase(phase: Phase) -> impl Drop {
    struct Restore(Option<Phase>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(phase) = self.0 {
                with_write(|write| write.set_phase(phase));
            }
        }
    }
    Restore(with_write(|write| write.set_phase(phase)).flatten())
}

/// The current request has written something visible: later takes wait
/// without bound.
pub fn mark_visible() {
    CTX.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.visible = true;
        }
    });
}

/// Whether the current request has written anything visible.
pub fn is_visible() -> bool {
    CTX.with(|c| c.borrow().as_ref().is_some_and(|x| x.visible))
}

/// The bound for this take, and mark the request as having taken one.
/// Outside a request (startup, reconcile, gc) every take is bounded.
fn take_bound(wait: Duration) -> Option<Duration> {
    CTX.with(|c| match c.borrow_mut().as_mut() {
        None => Some(wait),
        Some(ctx) => {
            let first = !ctx.visible && !ctx.took;
            ctx.took = true;
            first.then_some(wait)
        }
    })
}

/// The request this thread works for: the request scope, else the session
/// layer's request tag (a `new`/`add` dispatch worker, status-bar §11.1),
/// else the daemon itself.
fn ctx_method() -> (String, Option<SessionId>) {
    CTX.with(|c| {
        c.borrow()
            .as_ref()
            .map(|x| (x.method.clone(), x.session.clone()))
    })
    .or_else(|| strata_session::observe::request_tag().map(|t| (t.method, t.session)))
    .unwrap_or_else(|| ("daemon".to_string(), None))
}

/// The method of the request this thread works for (the request scope, else
/// the request tag); `None` on background work.
pub fn request_method() -> Option<String> {
    CTX.with(|c| c.borrow().as_ref().map(|x| x.method.clone()))
        .or_else(|| strata_session::observe::request_tag().map(|t| t.method))
}

/// The origin of what this thread does now: a request when the request
/// scope or the request tag is set (status-bar §11.1).
pub fn origin() -> Origin {
    match request_method() {
        Some(_) => Origin::Request,
        None => Origin::Background,
    }
}

#[derive(Clone, Debug)]
struct Holder {
    method: String,
    session: Option<SessionId>,
    /// [`with_subject`]'s session when taken; shown instead of `session`.
    subject: Option<SessionId>,
    origin: Origin,
    /// The taking thread's request tag id: which request's work it is.
    request: Option<u64>,
    since: SystemTime,
    started: Instant,
    yield_to: Option<Arc<AtomicBool>>,
    generation: Option<u64>,
}

impl Holder {
    /// The session `holds[]`, `LongHold` and Busy name: the subject, else
    /// the request's session.
    fn shown_session(&self) -> Option<SessionId> {
        self.subject.clone().or_else(|| self.session.clone())
    }
}

#[derive(Default)]
struct State {
    holder: Option<Holder>,
    queue: VecDeque<(u64, Holder)>,
    next: u64,
}

/// One FIFO lock.
#[derive(Default)]
struct Fifo {
    fence: AtomicU64,
    st: Mutex<State>,
    cv: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // I11: a panic elsewhere never leaves the lock unusable.
    m.lock().unwrap_or_else(|p| {
        m.clear_poison();
        p.into_inner()
    })
}

/// How a repo lock is taken.
#[derive(Clone, Copy)]
enum Take<'a> {
    /// A request's take: its first is bounded by the lock wait, later ones
    /// and any after a visible write wait without bound.
    Request,
    /// Only if free right now; the holder shows as this label.
    Try(&'a str),
    /// Like `Try`, the label naming this request's session too.
    TryAs(&'a str),
    /// At most the lock wait, given up once the closure says so.
    Abortable(&'a str, &'a dyn Fn() -> bool),
}

/// What a take failed with. The holder is boxed to keep the error small.
enum TakeError {
    Timeout(Option<Box<Holder>>),
    ShuttingDown,
    Policy(StrataError),
}

impl Fifo {
    fn acquire(
        &self,
        bound: Option<Duration>,
        closed: &AtomicBool,
        holder: Holder,
        check: impl Fn() -> Result<()>,
    ) -> std::result::Result<(), TakeError> {
        let deadline = bound.map(|b| Instant::now() + b);
        let mut st = lock(&self.st);
        let ticket = st.next;
        st.next += 1;
        st.queue.push_back((ticket, holder.clone()));
        loop {
            if closed.load(Ordering::SeqCst) {
                st.queue.retain(|(t, _)| *t != ticket);
                self.cv.notify_all();
                return Err(TakeError::ShuttingDown);
            }
            let policy = if holder
                .generation
                .is_some_and(|g| g < self.fence.load(Ordering::SeqCst))
            {
                Err(fenced_error())
            } else {
                check()
            };
            if let Err(e) = policy {
                st.queue.retain(|(t, _)| *t != ticket);
                self.cv.notify_all();
                return Err(TakeError::Policy(e));
            }
            if st.holder.is_none() && st.queue.front().map(|(t, _)| t) == Some(&ticket) {
                st.queue.pop_front();
                st.holder = Some(Holder {
                    since: SystemTime::now(),
                    started: Instant::now(),
                    ..holder
                });
                return Ok(());
            }
            // Every actual waiter, including post-visible unbounded takes,
            // asks a cooperative background holder to release promptly.
            if bound != Some(Duration::ZERO)
                && let Some(flag) = st.holder.as_ref().and_then(|h| h.yield_to.as_ref())
            {
                flag.store(true, Ordering::SeqCst);
            }
            match deadline {
                None => st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        st.queue.retain(|(t, _)| *t != ticket);
                        let h = st.holder.clone().map(Box::new);
                        self.cv.notify_all();
                        return Err(TakeError::Timeout(h));
                    }
                    st = self
                        .cv
                        .wait_timeout(st, d - now)
                        .unwrap_or_else(|p| p.into_inner())
                        .0;
                }
            }
        }
    }

    fn release(&self) {
        let mut st = lock(&self.st);
        st.holder = None;
        self.cv.notify_all();
    }

    fn holder(&self) -> Option<Holder> {
        lock(&self.st).holder.clone()
    }

    fn waiting(&self) -> usize {
        lock(&self.st).queue.len()
    }

    fn wake(&self) {
        let _st = lock(&self.st);
        self.cv.notify_all();
    }
}

/// Releases on drop, including during unwinding.
struct Held<'a> {
    fifo: Arc<Fifo>,
    on_drop: Box<dyn FnMut() + 'a>,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        (self.on_drop)();
        self.fifo.release();
    }
}

type LongWaitKey = (RepoName, Option<SessionId>, String);

/// Reads a repo's [`strata_deps::RefSnapshot`] refs (origin, chain top,
/// anchor) for the release-point refresh: an owned `Git` and the refs'
/// names on the default branch.
#[derive(Clone)]
struct RefSource {
    git: strata_gitio::Git,
    names: [String; 3],
}

impl RefSource {
    fn names(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }
}

/// A repo's snapshot and the stamp of its refs' files when it was read
/// (`None` after a failed or skipped read).
struct Snap {
    snapshot: strata_deps::RefSnapshot,
    stamp: Option<strata_gitio::RefsStamp>,
}

/// How long the release-point refresh's one git subprocess may run.
const SNAPSHOT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The daemon's repo write locks and session queues.
pub struct Locks {
    wait: Duration,
    /// [`BACKGROUND_HOLD_MAX`] in milliseconds, unless a test shortened it.
    background_max: AtomicU64,
    catalog_id: AtomicU64,
    repos: RwLock<BTreeMap<RepoName, Arc<Fifo>>>,
    store: Mutex<Option<Arc<strata_store::Store>>>,
    queues: Mutex<BTreeMap<SessionId, Arc<Fifo>>>,
    expired_waits: Mutex<BTreeMap<LongWaitKey, DaemonWarning>>,
    closed: AtomicBool,
    close_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// new-lock §2.1: per repo, the reader of its snapshot refs and the
    /// snapshot as the last holder left it (a missing one is not valid).
    ref_sources: Mutex<BTreeMap<RepoName, RefSource>>,
    snapshots: Mutex<BTreeMap<RepoName, Snap>>,
    /// new-lock §3.3: per repo, how many reloads changed its configuration;
    /// part of every old-tree judgement's token.
    config_epochs: Mutex<BTreeMap<RepoName, u64>>,
}

impl Locks {
    /// One write lock per configured repo; `wait` is the bound of a
    /// request's first take (`LOCK_WAIT` in production).
    pub fn new(repos: impl IntoIterator<Item = RepoName>, wait: Duration) -> Locks {
        Locks {
            wait,
            background_max: AtomicU64::new(BACKGROUND_HOLD_MAX.as_millis() as u64),
            catalog_id: AtomicU64::new(0),
            repos: RwLock::new(
                repos
                    .into_iter()
                    .map(|r| (r, Arc::new(Fifo::default())))
                    .collect(),
            ),
            store: Mutex::new(None),
            queues: Mutex::new(BTreeMap::new()),
            expired_waits: Mutex::new(BTreeMap::new()),
            closed: AtomicBool::new(false),
            close_hook: Mutex::new(None),
            ref_sources: Mutex::new(BTreeMap::new()),
            snapshots: Mutex::new(BTreeMap::new()),
            config_epochs: Mutex::new(BTreeMap::new()),
        }
    }

    /// new-lock §3.3: a reload changed `repo`'s configuration, so every
    /// old-tree judgement made under the previous one is void.
    pub fn bump_config_epoch(&self, repo: &RepoName) {
        *lock(&self.config_epochs).entry(repo.clone()).or_default() += 1;
    }

    /// How many reloads changed `repo`'s configuration in this process.
    pub fn config_epoch(&self, repo: &RepoName) -> u64 {
        lock(&self.config_epochs).get(repo).copied().unwrap_or(0)
    }

    pub fn set_close_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.close_hook) = Some(hook);
    }

    /// new-lock §2.1: where `repo`'s release-point snapshot comes from (its
    /// default branch in `git`). Set at startup and when a reload adds the
    /// repo; [`Locks::refresh_snapshot`] then computes it.
    pub fn set_ref_source(&self, repo: &RepoName, git: strata_gitio::Git, branch: &str) {
        let names = strata_deps::snapshot_refs(branch);
        lock(&self.ref_sources).insert(repo.clone(), RefSource { git, names });
    }

    /// The snapshot of `repo`'s default-branch refs as the lock's last
    /// holder left them; `None` before the first refresh.
    pub fn ref_snapshot(&self, repo: &RepoName) -> Option<strata_deps::RefSnapshot> {
        lock(&self.snapshots).get(repo).map(|s| s.snapshot.clone())
    }

    /// Test seam: mark `repo`'s snapshot invalid, as a failed refresh does
    /// (the next release of the lock refreshes it).
    #[cfg(feature = "test-seams")]
    pub fn invalidate_snapshot(&self, repo: &RepoName) {
        if let Some(s) = lock(&self.snapshots).get_mut(repo) {
            s.snapshot.valid = false;
            s.stamp = None;
        }
    }

    /// Re-read `repo`'s snapshot refs. Called by the holder of the repo lock
    /// before it releases it (and once for a repo a reload adds, before its
    /// lock is published). While unwinding it only invalidates; a failed or
    /// panicking read invalidates too. A repo without a source keeps no
    /// snapshot. The git subprocess runs only when the refs' files changed
    /// since the last good read (their stamp, taken first, differs): the
    /// holder is the only writer of these refs, so equal files read equal.
    pub fn refresh_snapshot(&self, repo: &RepoName) {
        let Some(source) = lock(&self.ref_sources).get(repo).cloned() else {
            return;
        };
        // `None`: unchanged and kept; `Some(None)`: invalid.
        let read = if std::thread::panicking() {
            Some(None)
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let stamp = source.git.strata_refs_stamp(&source.names());
                if let Some(s) = &stamp {
                    let mut snapshots = lock(&self.snapshots);
                    if let Some(prev) = snapshots.get_mut(repo)
                        && prev.snapshot.valid
                        && prev.stamp.as_ref() == Some(s)
                    {
                        prev.snapshot.generation += 1;
                        return None;
                    }
                }
                let refs = source
                    .git
                    .read_strata_refs(&source.names(), SNAPSHOT_READ_TIMEOUT);
                Some(refs.ok().filter(|v| v.len() == 3).map(|v| (v, stamp)))
            }))
            .unwrap_or(Some(None))
        };
        let Some(read) = read else {
            return;
        };
        let mut snapshots = lock(&self.snapshots);
        let generation = snapshots.get(repo).map_or(0, |s| s.snapshot.generation) + 1;
        let next = match read {
            Some((mut v, stamp)) => Snap {
                snapshot: strata_deps::RefSnapshot {
                    anchor: v.pop().flatten(),
                    chain_top: v.pop().flatten(),
                    origin: v.pop().flatten(),
                    generation,
                    valid: true,
                },
                stamp,
            },
            None => Snap {
                snapshot: strata_deps::RefSnapshot {
                    generation,
                    valid: false,
                    ..Default::default()
                },
                stamp: None,
            },
        };
        snapshots.insert(repo.clone(), next);
    }

    pub fn set_catalog(&self, id: u64) {
        self.catalog_id.store(id, Ordering::Relaxed);
    }

    pub fn set_store(&self, store: Arc<strata_store::Store>) {
        *lock(&self.store) = Some(store);
    }

    fn repo_entries(&self) -> BTreeMap<RepoName, Arc<Fifo>> {
        self.repos.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Insert only: never replace a FIFO that may be held by an older catalog.
    pub fn insert(&self, repo: RepoName) {
        self.repos
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entry(repo)
            .or_default();
    }

    pub fn tighten(&self, repos: &[RepoName], generation: u64) {
        let entries = self.repo_entries();
        for repo in repos {
            if let Some(f) = entries.get(repo) {
                f.fence.fetch_max(generation, Ordering::SeqCst);
                f.wake();
            }
        }
        let queues: Vec<_> = lock(&self.queues).values().cloned().collect();
        for q in queues {
            q.wake();
        }
    }

    pub fn check_fence(&self, repos: &[RepoName]) -> Result<()> {
        let generation =
            strata_session::catalog::generation_for(self.catalog_id.load(Ordering::Relaxed));
        let entries = self.repo_entries();
        if repos.iter().any(|r| {
            entries
                .get(r)
                .is_some_and(|f| generation < f.fence.load(Ordering::SeqCst))
        }) {
            Err(fenced_error())
        } else {
            Ok(())
        }
    }

    fn check_session(&self, id: &SessionId) -> Result<()> {
        let store = lock(&self.store).clone();
        if let Some(store) = store
            && let Some(rec) = store.session(id)?
        {
            self.check_fence(&rec.manifest.repos.keys().cloned().collect::<Vec<_>>())?;
        }
        Ok(())
    }

    pub fn wait(&self) -> Duration {
        self.wait
    }

    /// [`BACKGROUND_HOLD_MAX`], or what a test set.
    pub fn background_hold_max(&self) -> Duration {
        Duration::from_millis(self.background_max.load(Ordering::Relaxed))
    }

    /// Test seam: shorten (or lengthen) [`BACKGROUND_HOLD_MAX`] for this
    /// daemon, at any time.
    #[cfg(feature = "test-seams")]
    pub fn set_background_hold_max(&self, max: Duration) {
        self.background_max
            .store(max.as_millis() as u64, Ordering::Relaxed);
    }

    fn holder(&self, method: String, session: Option<SessionId>) -> Holder {
        Holder {
            method,
            session,
            subject: SUBJECT.with(|s| s.borrow().clone()),
            origin: origin(),
            request: strata_session::observe::request_tag().map(|t| t.id),
            since: SystemTime::now(),
            started: Instant::now(),
            yield_to: YIELD_TO.with(|y| y.borrow().clone()),
            generation: Some(strata_session::catalog::generation_for(
                self.catalog_id.load(Ordering::Relaxed),
            )),
        }
    }

    fn holder_now(&self) -> Holder {
        let (method, session) = ctx_method();
        self.holder(method, session)
    }

    fn holder_as(&self, method: &str) -> Holder {
        self.holder(method.into(), None)
    }

    fn take_error(&self, e: TakeError, what: serde_json::Value, waited: Duration) -> StrataError {
        match e {
            TakeError::Policy(e) => e,
            TakeError::ShuttingDown => {
                StrataError::new(Code::ReadOnly, "the daemon is shutting down")
                    .with_detail(json!({ "reason": "shutting_down" }))
            }
            TakeError::Timeout(h) => {
                let mut detail = json!({
                    "reason": "lock_wait",
                    "lock": what,
                    "holder": h.as_ref().map(|h| h.method.clone()),
                    "holder_session": h.as_ref().and_then(|h| h.shown_session().map(|s| s.as_str().to_string())),
                });
                // new-lock §4.3: additive; absent when the lock was between
                // holders at the timeout (`holder_since` also when its time
                // cannot be written).
                if let Some(h) = &h {
                    detail["holder_origin"] = json!(h.origin);
                    if let Ok(since) = strata_types::serde_rfc3339::format(&h.since) {
                        detail["holder_since"] = json!(since);
                    }
                    detail["held_ms"] = json!(h.started.elapsed().as_millis() as u64);
                }
                StrataError::new(
                    Code::Busy,
                    format!(
                        "waited {} ms before anything was written",
                        waited.as_millis()
                    ),
                )
                .with_detail(detail)
            }
        }
    }

    /// Run `f` while holding `repo`'s write lock.
    pub fn with_repo<T>(&self, repo: &RepoName, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.repo_take(repo, Take::Request, false, f)
    }

    /// rm-followups §1.6 T2: [`Locks::with_repo`] only if the lock is free
    /// right now; else `Busy` (`reason: "lock_wait"`), nothing ran. The
    /// holder shows as `holder` (`catch_up`).
    pub fn try_repo<T>(
        &self,
        repo: &RepoName,
        holder: &str,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.repo_take(repo, Take::Try(holder), false, f)
    }

    /// new-lock §2.2: [`Locks::try_repo`] whose holder names this request's
    /// session as well (`new`'s and `add`'s head step).
    pub fn try_repo_as<T>(
        &self,
        repo: &RepoName,
        holder: &str,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.repo_take(repo, Take::TryAs(holder), false, f)
    }

    /// new-lock §3.1 phase 2 (§15.2): the repo lock for the old-tree
    /// worker, waited for at most the lock wait and given up as soon as
    /// `abort` says so (a foreground guard registered for the session;
    /// [`Locks::wake_repo`] makes the waiter look again). The holder shows
    /// as `holder`; no yield is installed. Expiry is `Busy` `lock_wait`, an
    /// abort `Busy` `foreground_guard`.
    pub fn repo_abortable<T>(
        &self,
        repo: &RepoName,
        holder: &str,
        abort: &dyn Fn() -> bool,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.repo_take(repo, Take::Abortable(holder, abort), false, f)
    }

    /// Wake the waiters of `repo`'s lock (they re-check their conditions).
    pub fn wake_repo(&self, repo: &RepoName) {
        if let Some(f) = self.repo_entries().get(repo) {
            f.wake();
        }
    }

    fn repo_take<T>(
        &self,
        repo: &RepoName,
        take: Take<'_>,
        cleanup: bool,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let Some(fifo) = self
            .repos
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(repo)
            .cloned()
        else {
            return Err(
                StrataError::new(Code::NoSuchRepo, format!("no repo {repo}"))
                    .with_detail(json!({ "repo": repo.as_str() })),
            );
        };
        let reentered = CTX.with(|c| c.borrow().as_ref().is_some_and(|x| x.repos.contains(repo)));
        if reentered {
            return Err(StrataError::new(
                Code::Busy,
                format!("the write lock of {repo} is already held by this request"),
            )
            .with_detail(json!({ "reason": "reentrant", "repo": repo.as_str() })));
        }
        let (bound, mut holder) = match take {
            Take::Request => (take_bound(self.wait), self.holder_now()),
            Take::Try(m) => (Some(Duration::ZERO), self.holder_as(m)),
            Take::TryAs(m) => (Some(Duration::ZERO), self.holder(m.into(), ctx_method().1)),
            Take::Abortable(m, _) => (Some(self.wait), self.holder_as(m)),
        };
        let t0 = Instant::now();
        if cleanup {
            holder.generation = None;
        }
        {
            let _queued = request_phase(Phase::Queued);
            let check = || match take {
                Take::Abortable(_, abort) if abort() => Err(StrataError::new(
                    Code::Busy,
                    format!("a foreground request for the session stopped the wait for {repo}"),
                )
                .with_detail(json!({ "reason": "foreground_guard", "repo": repo.as_str() }))),
                _ => Ok(()),
            };
            fifo.acquire(bound, &self.closed, holder, check)
                .map_err(|e| self.take_error(e, json!({ "repo": repo.as_str() }), t0.elapsed()))?;
        }
        let _locked = request_phase(Phase::Locked);
        CTX.with(|c| {
            if let Some(x) = c.borrow_mut().as_mut() {
                x.repos.insert(repo.clone());
            }
        });
        let _held = Held {
            fifo,
            on_drop: Box::new(move || {
                // new-lock §2.1: the holder publishes the refs it leaves
                // behind before anyone else can take the lock.
                self.refresh_snapshot(repo);
                CTX.with(|c| {
                    if let Ok(mut b) = c.try_borrow_mut()
                        && let Some(x) = b.as_mut()
                    {
                        x.repos.remove(repo);
                    }
                })
            }),
        };
        // An expired attempt is no longer waiting once this request has the
        // lock, even if its operation later returns an error. Other requests'
        // repo/session/method records remain visible.
        let (method, session) = ctx_method();
        lock(&self.expired_waits).remove(&(repo.clone(), session, method));
        f()
    }

    fn queue_of(&self, id: &SessionId) -> Arc<Fifo> {
        lock(&self.queues).entry(id.clone()).or_default().clone()
    }

    /// Run `f` holding the queue place of every session in `ids` (taken in
    /// ascending order, I4). Places this request already holds are kept.
    pub fn with_sessions<T>(&self, ids: &[SessionId], f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.sessions_bounded(ids, None, f)
    }

    /// rm-followups §1.6 T2: [`Locks::with_sessions`] where every place is
    /// taken only if free right now; else `Busy` (`reason: "lock_wait"`)
    /// and the places already taken are released.
    pub fn try_sessions<T>(
        &self,
        ids: &[SessionId],
        holder: &str,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.sessions_take(ids, None, Some(holder), f)
    }

    /// [`Locks::with_sessions`] where every take of this call, not only the
    /// request's first, waits at most until `deadline` (review-2 N5: one
    /// overall bound across a wait that may take the queue more than once).
    /// After a visible write the takes wait without bound, as everywhere.
    pub fn with_sessions_until<T>(
        &self,
        ids: &[SessionId],
        deadline: Instant,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.sessions_bounded(ids, Some(deadline), f)
    }

    fn sessions_bounded<T>(
        &self,
        ids: &[SessionId],
        deadline: Option<Instant>,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.sessions_take(ids, deadline, None, f)
    }

    fn sessions_take<T>(
        &self,
        ids: &[SessionId],
        deadline: Option<Instant>,
        try_as: Option<&str>,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let want: BTreeSet<SessionId> = ids.iter().cloned().collect();
        let fifos: Vec<(SessionId, Arc<Fifo>)> = want
            .into_iter()
            .filter(|id| {
                !CTX.with(|c| c.borrow().as_ref().is_some_and(|x| x.sessions.contains(id)))
            })
            .map(|id| {
                let q = self.queue_of(&id);
                (id, q)
            })
            .collect();
        let mut guards: Vec<Held<'_>> = Vec::new();
        for (id, fifo) in &fifos {
            let (bound, holder) = match try_as {
                Some(m) => (
                    Some(Duration::ZERO),
                    Holder {
                        session: Some(id.clone()),
                        ..self.holder_as(m)
                    },
                ),
                None => {
                    let first = take_bound(self.wait);
                    let bound = match deadline {
                        Some(d) if !is_visible() => {
                            Some(d.saturating_duration_since(Instant::now()))
                        }
                        _ => first,
                    };
                    (bound, self.holder_now())
                }
            };
            let t0 = Instant::now();
            {
                let _queued = request_phase(Phase::Queued);
                fifo.acquire(bound, &self.closed, holder, || self.check_session(id))
                    .map_err(|e| {
                        self.take_error(e, json!({ "session": id.as_str() }), t0.elapsed())
                    })?;
            }
            CTX.with(|c| {
                if let Some(x) = c.borrow_mut().as_mut() {
                    x.sessions.insert(id.clone());
                }
            });
            let id2 = id.clone();
            guards.push(Held {
                fifo: fifo.clone(),
                on_drop: Box::new(move || {
                    CTX.with(|c| {
                        if let Ok(mut b) = c.try_borrow_mut()
                            && let Some(x) = b.as_mut()
                        {
                            x.sessions.remove(&id2);
                        }
                    })
                }),
            });
        }
        let _locked = (!guards.is_empty()).then(|| request_phase(Phase::Locked));
        let r = f();
        // Release in reverse order of taking.
        while let Some(g) = guards.pop() {
            drop(g);
        }
        r
    }

    /// §5.9: run `f` holding `id`'s queue place only if it is free right
    /// now (no waiting, no queueing behind a request). `None` = busy, the
    /// caller skips. The holder shows as method `reconcile`.
    pub fn try_session<T>(
        &self,
        id: &SessionId,
        method: &str,
        f: impl FnOnce() -> Result<T>,
    ) -> Option<Result<T>> {
        let fifo = self.queue_of(id);
        let holder = self.holder(method.into(), Some(id.clone()));
        match fifo.acquire(Some(Duration::ZERO), &self.closed, holder, || {
            self.check_session(id)
        }) {
            Err(TakeError::Timeout(_)) => None,
            Err(e) => Some(Err(self.take_error(
                e,
                json!({ "session": id.as_str() }),
                Duration::ZERO,
            ))),
            Ok(()) => {
                let _held = Held {
                    fifo: fifo.clone(),
                    on_drop: Box::new(|| {}),
                };
                Some(f())
            }
        }
    }

    /// The held repo write locks.
    pub fn holds(&self) -> Vec<LockHold> {
        self.repo_entries()
            .iter()
            .filter_map(|(r, f)| {
                f.holder().map(|h| LockHold {
                    repo: r.clone(),
                    session: h.shown_session(),
                    method: h.method,
                    since: h.since,
                    origin: Some(h.origin),
                    elapsed_ms: Some(h.started.elapsed().as_millis() as u64),
                })
            })
            .collect()
    }

    /// A background queue place counts as well as a repo lock. Reads may
    /// use a session queue without being part of the drain.
    pub(crate) fn has_background_holder(&self) -> bool {
        let background = |fifo: &Arc<Fifo>| {
            fifo.holder()
                .is_some_and(|h| h.origin == Origin::Background)
        };
        self.repo_entries().values().any(background) || lock(&self.queues).values().any(background)
    }

    /// [`Locks::long_holds_with`] with no exempt work in flight.
    pub fn long_holds(&self) -> Vec<DaemonWarning> {
        self.long_holds_with(&|_, _| None)
    }

    /// `LongHold` for every repo lock held too long (status-bar §3.1,
    /// §12.1, Q16). `exempt(repo, request)` is the age of the current step
    /// of the exempt long work (a move, a deps rollout, a base build, its
    /// own request's session creation) the holder of `repo`, taken on a
    /// thread working for request `request`, does, if any. Such a hold is
    /// raised only once that step, counted from the take at the earliest,
    /// has run longer than [`Locks::background_hold_max`]; any other
    /// background hold once the hold has; any other request hold (publish
    /// and push included) once the hold has run longer than the lock wait.
    pub fn long_holds_with(
        &self,
        exempt: &dyn Fn(&RepoName, Option<u64>) -> Option<Duration>,
    ) -> Vec<DaemonWarning> {
        let max = self.background_hold_max();
        self.repo_entries()
            .iter()
            .filter_map(|(r, f)| {
                let h = f.holder()?;
                let held = h.started.elapsed();
                let raise = match exempt(r, h.request) {
                    // A step that began before the take (a session creation
                    // that waited for this lock) does not make a young hold long.
                    Some(step) => step.min(held) > max,
                    None if h.origin == Origin::Background => held > max,
                    None => held > self.wait,
                };
                raise.then(|| DaemonWarning::LongHold {
                    repo: r.clone(),
                    session: h.shown_session(),
                    method: h.method,
                    since: h.since,
                    origin: Some(h.origin),
                })
            })
            .collect()
    }

    /// Snapshot only the small FIFO state mutexes, never acquire a queue place.
    pub fn queue_snapshot(&self) -> Vec<serde_json::Value> {
        lock(&self.queues)
            .iter()
            .filter_map(|(session, fifo)| {
                let state = lock(&fifo.st);
                if state.holder.is_none() && state.queue.is_empty() {
                    return None;
                }
                Some(
                    json!({ "session": session, "holder": state.holder.as_ref().map(|h| &h.method),
                "since": state.holder.as_ref().map(|h| h.since),
                "queued": state.queue.iter().map(|(_, w)| &w.method).collect::<Vec<_>>() }),
                )
            })
            .collect()
    }

    /// Live FIFO waits and expired attempts retried by Sessions. Keep one
    /// warning per repo/session/method, describing its latest long attempt.
    pub fn long_waits(&self) -> Vec<DaemonWarning> {
        let mut out: Vec<_> = lock(&self.expired_waits).values().cloned().collect();
        let mut collect = |fifo: &Fifo, repo: Option<RepoName>, session: Option<SessionId>| {
            for (_, waiter) in &lock(&fifo.st).queue {
                if waiter.started.elapsed() >= self.wait {
                    let session = session.clone().or_else(|| waiter.session.clone());
                    if let Some(DaemonWarning::LongWait {
                        since,
                        attempt_since,
                        ..
                    }) = out.iter_mut().find(|w| {
                        matches!(w, DaemonWarning::LongWait { repo: r, session: s, method, .. }
                            if r == &repo && s == &session && method == &waiter.method)
                    }) {
                        *attempt_since = (*attempt_since).max(waiter.since);
                        *since = *attempt_since;
                        continue;
                    }
                    out.push(DaemonWarning::LongWait {
                        repo: repo.clone(),
                        session,
                        method: waiter.method.clone(),
                        since: waiter.since,
                        attempt_since: waiter.since,
                    });
                }
            }
        };
        for (repo, fifo) in &self.repo_entries() {
            collect(fifo, Some(repo.clone()), None);
        }
        for (session, fifo) in lock(&self.queues).iter() {
            collect(fifo, None, Some(session.clone()));
        }
        out
    }

    /// The method holding `id`'s queue place (in flight), if any.
    pub fn in_flight(&self, id: &SessionId) -> Option<String> {
        lock(&self.queues)
            .get(id)
            .and_then(|f| f.holder())
            .map(|h| h.method)
    }

    /// How many requests wait for `id`'s queue place.
    pub fn queued(&self, id: &SessionId) -> usize {
        lock(&self.queues).get(id).map(|f| f.waiting()).unwrap_or(0)
    }

    /// How many requests wait for `repo`'s write lock.
    pub fn repo_waiting(&self, repo: &RepoName) -> usize {
        self.repo_entries()
            .get(repo)
            .map(|f| f.waiting())
            .unwrap_or(0)
    }

    /// Fail every waiter (`ReadOnly`, `shutting_down`) and every later
    /// take; holders finish.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(wake) = lock(&self.close_hook).as_ref() {
            wake();
        }
        for f in self.repo_entries().values() {
            f.wake();
        }
        for f in lock(&self.queues).values() {
            f.wake();
        }
    }

    pub fn is_shut_down(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl strata_deps::RepoWriteLock for Locks {
    fn record_long_wait(&self, repo: &RepoName, attempt_since: SystemTime) {
        let (method, session) = ctx_method();
        let warning = DaemonWarning::LongWait {
            repo: Some(repo.clone()),
            session: session.clone(),
            method: method.clone(),
            since: attempt_since,
            attempt_since,
        };
        tracing::warn!(?warning, "retrying an expired post-visible lock attempt");
        lock(&self.expired_waits).insert((repo.clone(), session, method), warning);
    }

    fn check_fence(&self, repos: &[RepoName]) -> Result<()> {
        Locks::check_fence(self, repos)
    }
    fn with_cleanup(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Result<()>> {
        self.repo_take(repo, Take::Request, true, || Ok(f()))
    }

    fn try_write_as(
        &self,
        repo: &RepoName,
        holder: &str,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        let mut inner = None;
        self.try_repo_as(repo, holder, || {
            inner = Some(f());
            Ok(())
        })?;
        Ok(inner.unwrap_or(Ok(())))
    }

    fn is_shut_down(&self) -> bool {
        Locks::is_shut_down(self)
    }

    fn ref_snapshot(&self, repo: &RepoName) -> Option<strata_deps::RefSnapshot> {
        Locks::ref_snapshot(self, repo)
    }

    fn wake(&self, repo: &RepoName) {
        self.wake_repo(repo);
    }

    fn config_epoch(&self, repo: &RepoName) -> u64 {
        Locks::config_epoch(self, repo)
    }
    fn with_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        let mut inner = None;
        match self.with_repo(repo, || {
            inner = Some(f());
            Ok(())
        }) {
            Ok(()) => Ok(inner.unwrap_or(Ok(()))),
            Err(e) => Err(e),
        }
    }

    fn try_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        let mut inner = None;
        self.try_repo(repo, "catch_up", || {
            inner = Some(f());
            Ok(())
        })?;
        Ok(inner.unwrap_or(Ok(())))
    }
}

impl strata_publish::PublishLocks for Locks {
    fn with_sessions(
        &self,
        sessions: &[SessionId],
        f: &mut dyn FnMut() -> strata_publish::Result<()>,
    ) -> strata_publish::Result<strata_publish::Result<()>> {
        let mut inner = None;
        Locks::with_sessions(self, sessions, || {
            inner = Some(f());
            Ok(())
        })?;
        Ok(inner.unwrap_or(Ok(())))
    }

    fn with_repo(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_publish::Result<()>,
    ) -> strata_publish::Result<strata_publish::Result<()>> {
        let mut inner = None;
        Locks::with_repo(self, repo, || {
            inner = Some(f());
            Ok(())
        })?;
        Ok(inner.unwrap_or(Ok(())))
    }
}

fn fenced_error() -> StrataError {
    StrataError::new(Code::Busy, "the daemon applied a stricter config").with_detail(
        json!({ "reason": "pins_tightened", "fix": "rerun; the daemon applied a stricter config" }),
    )
}

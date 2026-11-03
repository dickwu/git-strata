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

use serde_json::json;
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::{DaemonWarning, LockHold, Result};

/// The per-thread request context (D8).
#[derive(Debug)]
struct Ctx {
    method: String,
    session: Option<SessionId>,
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
                visible: false,
                took: false,
                repos: BTreeSet::new(),
                sessions: BTreeSet::new(),
            })
        });
        RequestScope { _priv: () }
    }
}

impl Drop for RequestScope {
    fn drop(&mut self) {
        CTX.with(|c| *c.borrow_mut() = None);
    }
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

fn ctx_method() -> (String, Option<SessionId>) {
    CTX.with(|c| match c.borrow().as_ref() {
        Some(x) => (x.method.clone(), x.session.clone()),
        None => ("daemon".to_string(), None),
    })
}

#[derive(Clone, Debug)]
struct Holder {
    method: String,
    session: Option<SessionId>,
    since: SystemTime,
    started: Instant,
    generation: Option<u64>,
}

#[derive(Default)]
struct State {
    holder: Option<Holder>,
    queue: VecDeque<u64>,
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

/// What a take failed with.
enum TakeError {
    Timeout(Option<Holder>),
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
        st.queue.push_back(ticket);
        loop {
            if closed.load(Ordering::SeqCst) {
                st.queue.retain(|&t| t != ticket);
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
                st.queue.retain(|&t| t != ticket);
                self.cv.notify_all();
                return Err(TakeError::Policy(e));
            }
            if st.holder.is_none() && st.queue.front() == Some(&ticket) {
                st.queue.pop_front();
                st.holder = Some(holder);
                return Ok(());
            }
            match deadline {
                None => st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner()),
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        st.queue.retain(|&t| t != ticket);
                        let h = st.holder.clone();
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

/// The daemon's repo write locks and session queues.
pub struct Locks {
    wait: Duration,
    catalog_id: AtomicU64,
    repos: RwLock<BTreeMap<RepoName, Arc<Fifo>>>,
    store: Mutex<Option<Arc<strata_store::Store>>>,
    queues: Mutex<BTreeMap<SessionId, Arc<Fifo>>>,
    closed: AtomicBool,
    close_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Locks {
    /// One write lock per configured repo; `wait` is the bound of a
    /// request's first take (`LOCK_WAIT` in production).
    pub fn new(repos: impl IntoIterator<Item = RepoName>, wait: Duration) -> Locks {
        Locks {
            wait,
            catalog_id: AtomicU64::new(0),
            repos: RwLock::new(
                repos
                    .into_iter()
                    .map(|r| (r, Arc::new(Fifo::default())))
                    .collect(),
            ),
            store: Mutex::new(None),
            queues: Mutex::new(BTreeMap::new()),
            closed: AtomicBool::new(false),
            close_hook: Mutex::new(None),
        }
    }

    pub fn set_close_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.close_hook) = Some(hook);
    }

    pub fn set_catalog(&self, id: u64) { self.catalog_id.store(id, Ordering::Relaxed); }

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
        let generation = strata_session::catalog::generation_for(self.catalog_id.load(Ordering::Relaxed));
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

    fn holder_now(&self) -> Holder {
        let (method, session) = ctx_method();
        Holder {
            method,
            session,
            since: SystemTime::now(),
            started: Instant::now(),
            generation: Some(strata_session::catalog::generation_for(self.catalog_id.load(Ordering::Relaxed))),
        }
    }

    fn holder_as(&self, method: &str) -> Holder {
        Holder {
            method: method.into(),
            session: None,
            since: SystemTime::now(),
            started: Instant::now(),
            generation: Some(strata_session::catalog::generation_for(self.catalog_id.load(Ordering::Relaxed))),
        }
    }

    fn take_error(&self, e: TakeError, what: serde_json::Value, waited: Duration) -> StrataError {
        match e {
            TakeError::Policy(e) => e,
            TakeError::ShuttingDown => StrataError::new(Code::ReadOnly, "the daemon is shutting down")
                .with_detail(json!({ "reason": "shutting_down" })),
            TakeError::Timeout(h) => StrataError::new(
                Code::Busy,
                format!("waited {} ms before anything was written", waited.as_millis()),
            )
            .with_detail(json!({
                "reason": "lock_wait",
                "lock": what,
                "holder": h.as_ref().map(|h| h.method.clone()),
                "holder_session": h.as_ref().and_then(|h| h.session.as_ref().map(|s| s.as_str().to_string())),
            })),
        }
    }

    /// Run `f` while holding `repo`'s write lock.
    pub fn with_repo<T>(&self, repo: &RepoName, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.repo_take(repo, None, false, f)
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
        self.repo_take(repo, Some(holder), false, f)
    }

    fn repo_take<T>(
        &self,
        repo: &RepoName,
        try_as: Option<&str>,
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
        let (bound, mut holder) = match try_as {
            None => (take_bound(self.wait), self.holder_now()),
            Some(m) => (Some(Duration::ZERO), self.holder_as(m)),
        };
        let t0 = Instant::now();
        if cleanup {
            holder.generation = None;
        }
        fifo.acquire(bound, &self.closed, holder, || Ok(()))
            .map_err(|e| self.take_error(e, json!({ "repo": repo.as_str() }), t0.elapsed()))?;
        CTX.with(|c| {
            if let Some(x) = c.borrow_mut().as_mut() {
                x.repos.insert(repo.clone());
            }
        });
        let _held = Held {
            fifo,
            on_drop: Box::new(move || {
                CTX.with(|c| {
                    if let Ok(mut b) = c.try_borrow_mut()
                        && let Some(x) = b.as_mut()
                    {
                        x.repos.remove(repo);
                    }
                })
            }),
        };
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
    pub fn try_sessions<T>(&self, ids: &[SessionId], holder: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.sessions_take(ids, None, Some(holder), f)
    }

    /// [`Locks::with_sessions`] where every take of this call, not only the
    /// request's first, waits at most until `deadline` (review-2 N5: one
    /// overall bound across a wait that may take the queue more than once).
    /// After a visible write the takes wait without bound, as everywhere.
    pub fn with_sessions_until<T>(&self, ids: &[SessionId], deadline: Instant, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.sessions_bounded(ids, Some(deadline), f)
    }

    fn sessions_bounded<T>(&self, ids: &[SessionId], deadline: Option<Instant>, f: impl FnOnce() -> Result<T>) -> Result<T> {
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
            fifo.acquire(bound, &self.closed, holder, || self.check_session(id))
                .map_err(|e| self.take_error(e, json!({ "session": id.as_str() }), t0.elapsed()))?;
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
        f: impl FnOnce() -> Result<T>,
    ) -> Option<Result<T>> {
        let fifo = self.queue_of(id);
        let holder = Holder {
            method: "reconcile".into(),
            session: Some(id.clone()),
            since: SystemTime::now(),
            started: Instant::now(),
            generation: Some(strata_session::catalog::generation_for(self.catalog_id.load(Ordering::Relaxed))),
        };
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
                    method: h.method,
                    session: h.session,
                    since: h.since,
                })
            })
            .collect()
    }

    /// `LongHold` for every repo lock held longer than the lock wait.
    pub fn long_holds(&self) -> Vec<DaemonWarning> {
        self.repo_entries()
            .iter()
            .filter_map(|(r, f)| {
                let h = f.holder()?;
                (h.started.elapsed() > self.wait).then(|| DaemonWarning::LongHold {
                    repo: r.clone(),
                    method: h.method,
                    since: h.since,
                })
            })
            .collect()
    }

    /// The method holding `id`'s queue place (in flight), if any.
    pub fn in_flight(&self, id: &SessionId) -> Option<String> {
        lock(&self.queues).get(id).and_then(|f| f.holder()).map(|h| h.method)
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
    fn check_fence(&self, repos: &[RepoName]) -> Result<()> {
        Locks::check_fence(self, repos)
    }
    fn with_cleanup(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Result<()>> {
        self.repo_take(repo, None, true, || Ok(f()))
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

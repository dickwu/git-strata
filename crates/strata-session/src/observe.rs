//! What the daemon shows of work in flight (`daemon_status.activity[]`,
//! status-bar design §11.1, §11.2, §12.3): the request a thread works for
//! and an observer told when long session work begins, changes step and
//! ends. Both are per thread. The daemon installs them on its request and
//! background threads, [`crate::catalog::spawn_pinned`] copies them to
//! scoped helpers, and this crate only reads them: it never depends on the
//! daemon.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use strata_types::{RepoName, SessionId, TreeOid};

/// The request a thread works for: its RPC method, the session the request
/// names (`None` when it names none) and the daemon's number for it, which
/// ties the request's lock holds to the work the request began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestTag {
    pub method: String,
    pub session: Option<SessionId>,
    pub id: u64,
}

/// Long session work, as an [`Observer`] hears of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    /// A workspace move (§5.7), from its checks to its old tree.
    Move { repo: RepoName, session: SessionId },
    /// A base generation registered under the repo lock (§5.2 step 7).
    BaseBuild { repo: RepoName, tree: TreeOid },
    /// `new` or `add`: the repos it builds workspaces of.
    SessionCreate {
        repos: Vec<RepoName>,
        session: Option<SessionId>,
    },
}

/// Told of [`Work`] on the threads it is installed on. Every call comes from
/// the working thread, so an implementation must not block.
pub trait Observer: Send + Sync {
    /// `work` begins; the returned id names it in the calls below.
    fn begin(&self, work: Work) -> u64;
    /// Its current step: a move's phase, a `new` or `add` step.
    fn step(&self, id: u64, step: &str);
    /// It ended: returned, failed or unwound.
    fn end(&self, id: u64);
}

type OpenMove = Option<(RepoName, SessionId, u64)>;

thread_local! {
    static TAG: RefCell<Option<RequestTag>> = const { RefCell::new(None) };
    static OBSERVER: RefCell<Option<Arc<dyn Observer>>> = const { RefCell::new(None) };
    /// The move this thread has open: a caller's own move scope (sync's
    /// begins when it takes the repo lock) and `move_inner` are one entry.
    static OPEN_MOVE: RefCell<OpenMove> = const { RefCell::new(None) };
}

/// This thread's request tag; `None` on background work.
pub fn request_tag() -> Option<RequestTag> {
    TAG.with(|t| t.borrow().clone())
}

/// Puts back what [`enter`] replaced when dropped, also during unwinding.
/// It cannot leave its thread.
pub struct Scope {
    tag: Option<RequestTag>,
    observer: Option<Arc<dyn Observer>>,
    open: OpenMove,
    _thread: PhantomData<Rc<()>>,
}

/// Install `tag` (`None`: background work) and `observer` on this thread
/// until the returned scope is dropped. The daemon enters one per request
/// and one per background loop that does session work.
pub fn enter(tag: Option<RequestTag>, observer: Option<Arc<dyn Observer>>) -> Scope {
    Scope {
        tag: TAG.with(|t| t.replace(tag)),
        observer: OBSERVER.with(|o| o.replace(observer)),
        open: OPEN_MOVE.with(|m| m.replace(None)),
        _thread: PhantomData,
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        TAG.with(|t| *t.borrow_mut() = self.tag.take());
        OBSERVER.with(|o| *o.borrow_mut() = self.observer.take());
        OPEN_MOVE.with(|m| *m.borrow_mut() = self.open.take());
    }
}

/// What [`crate::catalog::spawn_pinned`] hands a scoped helper: the
/// parent's tag and observer, entered on the helper thread.
pub(crate) struct Inherited(Option<RequestTag>, Option<Arc<dyn Observer>>);

pub(crate) fn inherit() -> Inherited {
    Inherited(request_tag(), OBSERVER.with(|o| o.borrow().clone()))
}

impl Inherited {
    pub(crate) fn enter(self) -> Scope {
        enter(self.0, self.1)
    }
}

/// One piece of [`Work`] in flight: it ends when this is dropped, also
/// during unwinding. Without an observer on the thread it does nothing.
#[must_use = "the work ends when this is dropped"]
pub struct Observed {
    to: Option<(Arc<dyn Observer>, u64)>,
    /// False for a move joined to the one this thread already has open:
    /// its steps go to that entry, and dropping it ends nothing.
    owned: bool,
    /// The open move this one replaced (moves this one owns only).
    restore: Option<OpenMove>,
    _thread: PhantomData<Rc<()>>,
}

impl Observed {
    fn new(
        to: Option<(Arc<dyn Observer>, u64)>,
        owned: bool,
        restore: Option<OpenMove>,
    ) -> Observed {
        Observed {
            to,
            owned,
            restore,
            _thread: PhantomData,
        }
    }

    /// The work's current step.
    pub fn step(&self, step: &str) {
        if let Some((o, id)) = &self.to {
            o.step(*id, step);
        }
    }
}

impl Drop for Observed {
    fn drop(&mut self) {
        if let Some(open) = self.restore.take() {
            OPEN_MOVE.with(|m| *m.borrow_mut() = open);
        }
        if self.owned
            && let Some((o, id)) = self.to.take()
        {
            o.end(id);
        }
    }
}

/// Tell this thread's observer that `work` begins. A move of the repo and
/// session this thread already has open joins that entry.
pub fn begin(work: Work) -> Observed {
    let Some(o) = OBSERVER.with(|x| x.borrow().clone()) else {
        return Observed::new(None, false, None);
    };
    let Work::Move { repo, session } = &work else {
        let id = o.begin(work);
        return Observed::new(Some((o, id)), true, None);
    };
    let open = OPEN_MOVE.with(|m| m.borrow().clone());
    if let Some((_, _, id)) = open.filter(|(r, s, _)| r == repo && s == session) {
        return Observed::new(Some((o, id)), false, None);
    }
    let key = (repo.clone(), session.clone());
    let id = o.begin(work);
    let replaced = OPEN_MOVE.with(|m| m.replace(Some((key.0, key.1, id))));
    Observed::new(Some((o, id)), true, Some(replaced))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every call, in order.
    #[derive(Default)]
    struct Log {
        next: Mutex<u64>,
        calls: Mutex<Vec<String>>,
    }

    impl Observer for Log {
        fn begin(&self, work: Work) -> u64 {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            let tag = request_tag()
                .map(|t| t.method)
                .unwrap_or_else(|| "-".into());
            self.calls
                .lock()
                .unwrap()
                .push(format!("begin {n} {tag} {work:?}"));
            *n
        }
        fn step(&self, id: u64, step: &str) {
            self.calls.lock().unwrap().push(format!("step {id} {step}"));
        }
        fn end(&self, id: u64) {
            self.calls.lock().unwrap().push(format!("end {id}"));
        }
    }

    fn rn(s: &str) -> RepoName {
        RepoName::try_from(s.to_string()).unwrap()
    }

    fn sid(s: &str) -> SessionId {
        SessionId::try_from(format!("s-{s}")).unwrap()
    }

    fn tag(method: &str, session: Option<SessionId>) -> Option<RequestTag> {
        Some(RequestTag {
            method: method.into(),
            session,
            id: 1,
        })
    }

    #[test]
    fn nested_scopes_restore_tag_and_observer_also_on_unwind() {
        assert_eq!(request_tag(), None);
        let log = Arc::new(Log::default());
        {
            let _outer = enter(tag("sync", Some(sid("aaaaaaaa"))), Some(log.clone()));
            assert_eq!(request_tag(), tag("sync", Some(sid("aaaaaaaa"))));
            {
                let _inner = enter(None, None);
                assert_eq!(request_tag(), None);
                drop(begin(Work::BaseBuild {
                    repo: rn("tool"),
                    tree: tree(),
                }));
            }
            assert_eq!(request_tag(), tag("sync", Some(sid("aaaaaaaa"))));
            let r = std::panic::catch_unwind(|| {
                let _inner = enter(tag("new", None), None);
                panic!("unwind");
            });
            assert!(r.is_err());
            assert_eq!(request_tag(), tag("sync", Some(sid("aaaaaaaa"))));
            drop(begin(Work::BaseBuild {
                repo: rn("tool"),
                tree: tree(),
            }));
        }
        assert_eq!(request_tag(), None);
        // Only the begin under the outer scope reached the observer.
        let calls = log.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].starts_with("begin 1 sync BaseBuild"), "{calls:?}");
        assert_eq!(calls[1], "end 1");
    }

    #[test]
    fn spawn_pinned_helpers_inherit_tag_and_observer() {
        let log = Arc::new(Log::default());
        let _scope = enter(tag("new", None), Some(log.clone()));
        std::thread::scope(|s| {
            crate::catalog::spawn_pinned(s, || {
                assert_eq!(request_tag(), tag("new", None));
                drop(begin(Work::SessionCreate {
                    repos: vec![rn("tool")],
                    session: None,
                }));
            })
            .join()
            .unwrap();
            // A plain thread inherits nothing.
            s.spawn(|| {
                assert_eq!(request_tag(), None);
                drop(begin(Work::BaseBuild {
                    repo: rn("tool"),
                    tree: tree(),
                }));
            })
            .join()
            .unwrap();
        });
        let calls = log.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(
            calls[0].starts_with("begin 1 new SessionCreate"),
            "{calls:?}"
        );
        assert_eq!(calls[1], "end 1");
    }

    #[test]
    fn an_inner_move_of_the_open_move_joins_it() {
        let log = Arc::new(Log::default());
        let _scope = enter(tag("sync", None), Some(log.clone()));
        let mv = || Work::Move {
            repo: rn("tool"),
            session: sid("aaaaaaaa"),
        };
        {
            let outer = begin(mv());
            outer.step("scan");
            {
                let inner = begin(mv());
                inner.step("prepare");
                // Another session's move is its own entry.
                let other = begin(Work::Move {
                    repo: rn("tool"),
                    session: sid("bbbbbbbb"),
                });
                other.step("swap");
            }
            outer.step("done");
        }
        let calls = log.calls.lock().unwrap().clone();
        assert_eq!(
            calls
                .iter()
                .map(|c| c.split(" Move").next().unwrap())
                .collect::<Vec<_>>(),
            [
                "begin 1 sync",
                "step 1 scan",
                "step 1 prepare",
                "begin 2 sync",
                "step 2 swap",
                "end 2",
                "step 1 done",
                "end 1"
            ],
        );
        // The open move is restored: a later move of the same key is new.
        drop(begin(mv()));
        assert_eq!(log.calls.lock().unwrap().last().unwrap(), "end 3");
    }

    #[test]
    fn without_an_observer_nothing_is_called() {
        let o = begin(Work::Move {
            repo: rn("tool"),
            session: sid("aaaaaaaa"),
        });
        o.step("prepare");
        drop(o);
        assert_eq!(request_tag(), None);
    }

    fn tree() -> TreeOid {
        TreeOid::try_from("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()).unwrap()
    }
}

//! daemon.md Task 2: repo write lock, session queue, `Busy` only before the
//! first visible write. These drive `Locks` with stand-in handlers (each
//! thread is one request with its own `RequestScope`, like the server's
//! request threads); the RPC-level versions of the in-flight cases run in
//! tests/daemon.rs against the real handlers.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use strata_daemon::DaemonWarning;
use strata_daemon::locks::{Locks, RequestScope, mark_visible};
use strata_types::{Code, RepoName, SessionId, StrataError};

const WAIT: Duration = Duration::from_millis(300);

fn rn(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}

fn sid(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}

fn locks() -> Arc<Locks> {
    Arc::new(Locks::new([rn("front"), rn("tool")], WAIT))
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// A request thread: `RequestScope::begin(method, session)` then `f`.
fn request<T: Send + 'static>(
    method: &'static str,
    session: Option<SessionId>,
    f: impl FnOnce() -> T + Send + 'static,
) -> thread::JoinHandle<T> {
    thread::spawn(move || {
        let _scope = RequestScope::begin(method, session);
        f()
    })
}

#[test]
fn concurrent_checkpoints_same_session_serialize() {
    let l = locks();
    let s = sid("aaaaaaaa");
    let log = Arc::new(Mutex::new(Vec::new()));
    let (l1, s1, g1) = (l.clone(), s.clone(), log.clone());
    let first = request("checkpoint", Some(s.clone()), move || {
        l1.with_sessions(&[s1], || {
            g1.lock().unwrap().push("cp1 start");
            thread::sleep(Duration::from_millis(150));
            g1.lock().unwrap().push("cp1 end");
            Ok(())
        })
    });
    wait_until("cp1 in flight", || l.in_flight(&s).is_some());
    let (l2, s2, g2) = (l.clone(), s.clone(), log.clone());
    let second = request("checkpoint", Some(s.clone()), move || {
        l2.with_sessions(&[s2], || {
            g2.lock().unwrap().push("cp2");
            Ok(())
        })
    });
    // The second queues behind the first (150 ms < the 300 ms bound): it
    // is neither refused nor does it cancel the first.
    wait_until("cp2 queued", || l.queued(&s) == 1);
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert_eq!(*log.lock().unwrap(), ["cp1 start", "cp1 end", "cp2"]);
    // With the request marked visible (a checkpoint whose hook already
    // wrote), the second waits it out and runs in arrival order.
    let log = Arc::new(Mutex::new(Vec::new()));
    let (l1, s1, g1) = (l.clone(), s.clone(), log.clone());
    let first = request("checkpoint", Some(s.clone()), move || {
        l1.with_sessions(&[s1], || {
            g1.lock().unwrap().push(1);
            thread::sleep(Duration::from_millis(600));
            Ok(())
        })
    });
    wait_until("in flight", || l.in_flight(&s).is_some());
    let mut hs = Vec::new();
    for n in 2..=4 {
        let (l2, s2, g2) = (l.clone(), s.clone(), log.clone());
        hs.push(request("checkpoint", Some(s.clone()), move || {
            mark_visible();
            l2.with_sessions(&[s2], || {
                g2.lock().unwrap().push(n);
                Ok(())
            })
        }));
        wait_until("queued", || l.queued(&s) == n - 1);
    }
    first.join().unwrap().unwrap();
    for h in hs {
        h.join().unwrap().unwrap();
    }
    assert_eq!(*log.lock().unwrap(), [1, 2, 3, 4]);
    // types.md has no InFlight code.
    assert!(!format!("{:?}", Code::Busy).contains("InFlight"));
}

#[test]
fn rm_queued_behind_in_flight_checkpoint() {
    let l = locks();
    let s = sid("bbbbbbbb");
    let done = Arc::new(AtomicU32::new(0));
    let (l1, s1, d1) = (l.clone(), s.clone(), done.clone());
    let cp = request("checkpoint", Some(s.clone()), move || {
        l1.with_sessions(&[s1], || {
            thread::sleep(Duration::from_millis(200));
            d1.store(1, Ordering::SeqCst);
            Ok(())
        })
    });
    wait_until("cp in flight", || l.in_flight(&s).is_some());
    let (l2, s2, d2) = (l.clone(), s.clone(), done.clone());
    let rm = request("rm", Some(s.clone()), move || l2.with_sessions(&[s2], || Ok(d2.load(Ordering::SeqCst))));
    cp.join().unwrap().unwrap();
    // rm ran after the checkpoint completed; it was not Busy (200 ms < wait).
    assert_eq!(rm.join().unwrap().unwrap(), 1);
}

#[test]
fn status_does_not_wait_on_write_lock() {
    let l = locks();
    let (l1, b) = (l.clone(), Arc::new(Barrier::new(2)));
    let b1 = b.clone();
    let holder = request("publish", None, move || {
        l1.with_repo(&rn("tool"), || {
            b1.wait();
            thread::sleep(Duration::from_millis(1500));
            Ok(())
        })
    });
    b.wait();
    // Read-only paths never touch the locks: they only read the holds.
    let t = Instant::now();
    for m in ["status", "grep", "files", "diff", "deps_list", "daemon_status"] {
        let l2 = l.clone();
        let holds = request(m, None, move || l2.holds()).join().unwrap();
        assert_eq!(holds.len(), 1, "{m}");
        assert_eq!(holds[0].method, "publish");
    }
    assert!(t.elapsed() < Duration::from_secs(1));
    holder.join().unwrap().unwrap();
}

#[test]
fn busy_only_before_first_write() {
    let l = locks();
    let b = Arc::new(Barrier::new(2));
    let (l1, b1) = (l.clone(), b.clone());
    let holder = request("publish", None, move || {
        l1.with_repo(&rn("tool"), || {
            b1.wait();
            thread::sleep(Duration::from_millis(900));
            Ok(())
        })
    });
    b.wait();
    // Not yet written: Busy after the bound, and `f` never ran.
    let ran = Arc::new(AtomicU32::new(0));
    let (l2, r2) = (l.clone(), ran.clone());
    let t = Instant::now();
    let e = request("assign", None, move || {
        l2.with_repo(&rn("tool"), || {
            r2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    })
    .join()
    .unwrap()
    .unwrap_err();
    assert_eq!(e.code, Code::Busy);
    assert_eq!(e.detail.as_ref().unwrap()["reason"], "lock_wait");
    assert!(t.elapsed() >= WAIT);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    // Already written: the same hold is waited out.
    let (l3, r3) = (l.clone(), ran.clone());
    let r = request("new", None, move || {
        mark_visible();
        l3.with_repo(&rn("tool"), || {
            r3.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    })
    .join()
    .unwrap();
    r.unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    holder.join().unwrap().unwrap();
    // A second take of the same request is unbounded too (only the first
    // take before a write is bounded).
    let b = Arc::new(Barrier::new(2));
    let (l1, b1) = (l.clone(), b.clone());
    let holder = request("publish", None, move || {
        l1.with_repo(&rn("front"), || {
            b1.wait();
            thread::sleep(Duration::from_millis(700));
            Ok(())
        })
    });
    b.wait();
    let l4 = l.clone();
    request("gc", None, move || {
        l4.with_repo(&rn("tool"), || Ok(()))?;
        l4.with_repo(&rn("front"), || Ok(()))
    })
    .join()
    .unwrap()
    .unwrap();
    holder.join().unwrap().unwrap();
}

#[test]
fn locks_released_on_return_error_and_panic() {
    let l = locks();
    let s = sid("cccccccc");
    // Normal return.
    let _scope = RequestScope::begin("checkpoint", Some(s.clone()));
    l.with_sessions(std::slice::from_ref(&s), || l.with_repo(&rn("tool"), || Ok(()))).unwrap();
    // Err: passed through unchanged.
    let orig = StrataError::new(Code::Conflict, "boom");
    let e = l
        .with_sessions(std::slice::from_ref(&s), || l.with_repo(&rn("tool"), || Err::<(), _>(orig.clone())))
        .unwrap_err();
    assert_eq!(e, orig);
    drop(_scope);
    // Panic inside both.
    let (l1, s1) = (l.clone(), s.clone());
    let p = request("checkpoint", Some(s.clone()), move || {
        let _ = l1.with_sessions(&[s1], || {
            l1.with_repo(&rn("tool"), || -> strata_daemon::Result<()> { panic!("handler panic") })
        });
    })
    .join();
    assert!(p.is_err());
    assert!(l.holds().is_empty());
    assert!(l.in_flight(&s).is_none());
    // Next requests are not Busy (no poison) and get the locks at once.
    let (l2, s2) = (l.clone(), s.clone());
    let t = Instant::now();
    request("rm", Some(s.clone()), move || l2.with_sessions(&[s2], || l2.with_repo(&rn("tool"), || Ok(()))))
        .join()
        .unwrap()
        .unwrap();
    assert!(t.elapsed() < WAIT);
}

#[test]
fn repo_write_lock_is_first_come_first_served() {
    let l = locks();
    let order = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::new(Barrier::new(2));
    let (l1, b1) = (l.clone(), b.clone());
    let holder = request("publish", None, move || {
        l1.with_repo(&rn("tool"), || {
            b1.wait();
            thread::sleep(Duration::from_millis(400));
            Ok(())
        })
    });
    b.wait();
    let mut hs = Vec::new();
    for n in 0..5 {
        let (l2, o2) = (l.clone(), order.clone());
        hs.push(request("assign", None, move || {
            mark_visible();
            l2.with_repo(&rn("tool"), || {
                o2.lock().unwrap().push(n);
                Ok(())
            })
        }));
        wait_until("queued", || l.repo_waiting(&rn("tool")) == n + 1);
    }
    holder.join().unwrap().unwrap();
    for h in hs {
        h.join().unwrap().unwrap();
    }
    assert_eq!(*order.lock().unwrap(), [0, 1, 2, 3, 4]);
}

#[test]
fn shutdown_fails_queued_lets_holder_finish() {
    let l = locks();
    let s = sid("dddddddd");
    let b = Arc::new(Barrier::new(2));
    let (l1, s1, b1) = (l.clone(), s.clone(), b.clone());
    let holder = request("checkpoint", Some(s.clone()), move || {
        l1.with_sessions(&[s1], || {
            b1.wait();
            thread::sleep(Duration::from_millis(400));
            Ok("finished")
        })
    });
    b.wait();
    let (l2, s2) = (l.clone(), s.clone());
    let queued = request("rm", Some(s.clone()), move || {
        mark_visible();
        l2.with_sessions(&[s2], || Ok("ran"))
    });
    wait_until("queued", || l.queued(&s) == 1);
    l.shutdown();
    let e = queued.join().unwrap().unwrap_err();
    assert_eq!(e.code, Code::ReadOnly);
    assert_eq!(e.detail.as_ref().unwrap()["reason"], "shutting_down");
    assert_eq!(holder.join().unwrap().unwrap(), "finished");
    // After shutdown every new take is refused.
    let l3 = l.clone();
    let e = request("assign", None, move || l3.with_repo(&rn("tool"), || Ok(()))).join().unwrap().unwrap_err();
    assert_eq!(e.code, Code::ReadOnly);
}

#[test]
fn long_hold_named_in_status() {
    let l = locks();
    let b = Arc::new(Barrier::new(2));
    let (l1, b1) = (l.clone(), b.clone());
    let s = sid("eeeeeeee");
    let holder = request("deps_approve", Some(s.clone()), move || {
        l1.with_repo(&rn("tool"), || {
            b1.wait();
            thread::sleep(WAIT + Duration::from_millis(300));
            Ok(())
        })
    });
    b.wait();
    assert!(l.long_holds().is_empty());
    let before = std::time::SystemTime::now();
    thread::sleep(WAIT + Duration::from_millis(100));
    let w = l.long_holds();
    assert_eq!(w.len(), 1);
    match &w[0] {
        DaemonWarning::LongHold { repo, method, since } => {
            assert_eq!(repo, &rn("tool"));
            assert_eq!(method, "deps_approve");
            assert!(*since <= before);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(l.holds()[0].session.as_ref(), Some(&s));
    holder.join().unwrap().unwrap();
    assert!(l.long_holds().is_empty());
}

#[test]
fn concurrent_sessions_different_files_all_commit() {
    // Different sessions never wait on each other's queues.
    let l = locks();
    let b = Arc::new(Barrier::new(8));
    let hs: Vec<_> = (0..8)
        .map(|n| {
            let (l1, b1) = (l.clone(), b.clone());
            let s = sid(&format!("{}aaaaaaa", (b'a' + n as u8) as char));
            request("checkpoint", Some(s.clone()), move || {
                l1.with_sessions(&[s], || {
                    b1.wait(); // all eight are inside at once
                    Ok(n)
                })
            })
        })
        .collect();
    let mut got: Vec<i32> = hs.into_iter().map(|h| h.join().unwrap().unwrap()).collect();
    got.sort();
    assert_eq!(got, (0..8).collect::<Vec<_>>());
}

#[test]
fn concurrent_sessions_are_not_refused_for_a_sibling() {
    let l = locks();
    let a = sid("ffffffff");
    let b = sid("gggggggg");
    let bar = Arc::new(Barrier::new(2));
    let (l1, a1, bar1) = (l.clone(), a.clone(), bar.clone());
    let sibling = request("rm", Some(a.clone()), move || {
        l1.with_sessions(&[a1], || {
            bar1.wait();
            thread::sleep(Duration::from_millis(800)); // between two steps
            Ok(())
        })
    });
    bar.wait();
    let (l2, b2) = (l.clone(), b.clone());
    let t = Instant::now();
    request("checkpoint", Some(b.clone()), move || l2.with_sessions(&[b2], || Ok(()))).join().unwrap().unwrap();
    assert!(t.elapsed() < Duration::from_millis(300));
    sibling.join().unwrap().unwrap();
}

#[test]
fn status_sees_in_flight_without_cancelling() {
    let l = locks();
    let s = sid("hhhhhhhh");
    let b = Arc::new(Barrier::new(2));
    let (l1, s1, b1) = (l.clone(), s.clone(), b.clone());
    let cp = request("checkpoint", Some(s.clone()), move || {
        l1.with_sessions(&[s1], || {
            b1.wait();
            thread::sleep(Duration::from_millis(250));
            Ok("committed")
        })
    });
    b.wait();
    let l2 = l.clone();
    let s2 = s.clone();
    assert_eq!(request("status", None, move || l2.in_flight(&s2)).join().unwrap().as_deref(), Some("checkpoint"));
    let (l3, s3) = (l.clone(), s.clone());
    let rm = request("rm", Some(s.clone()), move || l3.with_sessions(&[s3], || Ok(())));
    assert_eq!(cp.join().unwrap().unwrap(), "committed");
    rm.join().unwrap().unwrap();
}

#[test]
fn checkpoint_during_publish_does_not_cancel() {
    // publish holds the session's queue place (PublishLocks::with_sessions)
    // and the repo lock while it fetches/checks/commits/pushes; a
    // same-session checkpoint and rm queue and run afterwards, and publish
    // returns its own result.
    use strata_publish::PublishLocks;
    let l = locks();
    let s = sid("iiiiiiii");
    let b = Arc::new(Barrier::new(2));
    let (l1, s1, b1) = (l.clone(), s.clone(), b.clone());
    let publish = request("publish", None, move || {
        let pl: &dyn PublishLocks = l1.as_ref();
        pl.with_sessions(&[s1], &mut || {
            pl.with_repo(&rn("tool"), &mut || {
                b1.wait();
                thread::sleep(Duration::from_millis(250));
                Ok(())
            })?
        })
    });
    b.wait();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut hs = Vec::new();
    for m in ["checkpoint", "rm"] {
        let (l2, s2, o2) = (l.clone(), s.clone(), order.clone());
        hs.push(request(m, Some(s.clone()), move || {
            l2.with_sessions(&[s2], || {
                o2.lock().unwrap().push(m);
                Ok(())
            })
        }));
        wait_until("queued", || l.queued(&s) >= 1);
    }
    publish.join().unwrap().unwrap().unwrap();
    for h in hs {
        h.join().unwrap().unwrap();
    }
    assert_eq!(*order.lock().unwrap(), ["checkpoint", "rm"]);
}

#[test]
fn toolcall_does_not_wait_out_approve_ensure() {
    // deps_approve's cold `ensure_target` runs under the repo write lock
    // and the build lock, not the session queues; a ToolCall checkpoint of
    // an affected session takes only its queue place and runs at once.
    let l = locks();
    let s = sid("jjjjjjjj");
    let b = Arc::new(Barrier::new(2));
    let (l1, b1) = (l.clone(), b.clone());
    let approve = request("deps_approve", None, move || {
        l1.with_repo(&rn("tool"), || {
            b1.wait();
            thread::sleep(Duration::from_millis(800)); // the cold install
            Ok(())
        })
    });
    b.wait();
    let (l2, s2) = (l.clone(), s.clone());
    let t = Instant::now();
    request("checkpoint", Some(s.clone()), move || l2.with_sessions(&[s2], || Ok(()))).join().unwrap().unwrap();
    assert!(t.elapsed() < Duration::from_millis(300));
    approve.join().unwrap().unwrap();
}

#[test]
fn reentrant_repo_take_is_refused_not_deadlocked() {
    let l = locks();
    let _scope = RequestScope::begin("publish", None);
    let e = l.with_repo(&rn("tool"), || l.with_repo(&rn("tool"), || Ok(()))).unwrap_err();
    assert_eq!(e.detail.unwrap()["reason"], "reentrant");
    let e = l.with_repo(&rn("nope"), || Ok(())).unwrap_err();
    assert_eq!(e.code, Code::NoSuchRepo);
}

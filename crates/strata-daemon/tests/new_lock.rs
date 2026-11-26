//! new-lock §2 and §4 (item 1, the daemon's half of item 3): `new` and
//! `add` no longer fail behind a long holder of a repo lock before they
//! write; the release-point ref snapshot; the lock-free shadow, pin and
//! unpin; the fence checkpoints; the certified pre-write refusals. Only
//! temporary repositories, config directories and stores.
mod support;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use strata_daemon::handlers;
use strata_daemon::locks::RequestScope;
use strata_daemon::startup::{self, Core, Daemon};
use strata_session::{Layout, SessionSeam};
use strata_store::Lifecycle;
use strata_types::SessionId;
use support::*;

fn serving<R>(d: &Daemon<'_>, f: impl FnOnce() -> R) -> R {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(d, &listener, &ext));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn call(socket: &Path, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let line = json!({ "id": 7, "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

fn ok(v: Value) -> Value {
    assert_eq!(v["ok"], json!(true), "{v:#}");
    v["data"].clone()
}

fn sid(s: &str) -> SessionId {
    SessionId::try_from(s.to_string()).unwrap()
}

fn new_id(reply: &Value) -> SessionId {
    serde_json::from_value(reply["session"]["manifest"]["id"].clone()).unwrap()
}

fn wait_until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn active(core: &Core) -> usize {
    core.store
        .sessions()
        .unwrap()
        .iter()
        .filter(|r| r.lifecycle == Lifecycle::Active)
        .count()
}

fn pin(core: &Core, repo: &str, id: &SessionId) -> Option<String> {
    let _catalog = core.admit();
    core.snapshot().unwrap().gits[&repo_name(repo)]
        .read_ref(&format!("refs/strata/pins/{id}"))
        .unwrap()
        .map(|o| o.as_str().to_string())
}

/// A long `sync` of `session` holding `repo`'s write lock until the
/// returned sender sends (or is dropped).
fn hold<'scope>(
    s: &'scope std::thread::Scope<'scope, '_>,
    core: &'scope Core,
    repo: &str,
    session: &SessionId,
) -> mpsc::Sender<()> {
    let (release, wait) = mpsc::channel::<()>();
    let (held, is_held) = mpsc::channel::<()>();
    let repo = repo_name(repo);
    let session = session.clone();
    s.spawn(move || {
        let _scope = RequestScope::begin("sync", Some(session));
        core.locks
            .with_repo(&repo, || {
                held.send(()).unwrap();
                let _ = wait.recv();
                Ok(())
            })
            .unwrap();
    });
    is_held.recv_timeout(Duration::from_secs(10)).unwrap();
    release
}

/// `holder` writes `refs` (name, oid) under `repo`'s lock and holds it
/// until the returned sender sends.
fn hold_writing<'scope>(
    s: &'scope std::thread::Scope<'scope, '_>,
    fx: &'scope Dfx,
    core: &'scope Core,
    repo: &str,
    refs: Vec<(String, String)>,
) -> mpsc::Sender<()> {
    let (release, wait) = mpsc::channel::<()>();
    let (held, is_held) = mpsc::channel::<()>();
    let name = repo_name(repo);
    let main = fx.main(repo);
    s.spawn(move || {
        let _scope = RequestScope::begin("sync", Some(sid("s-hhhhhhhh")));
        core.locks
            .with_repo(&name, || {
                for (r, oid) in &refs {
                    fx.sb.git(&main, &["update-ref", r.as_str(), oid.as_str()]);
                }
                held.send(()).unwrap();
                let _ = wait.recv();
                Ok(())
            })
            .unwrap();
    });
    is_held.recv_timeout(Duration::from_secs(10)).unwrap();
    release
}

const TRACKING: &str = "refs/strata/remotes/origin/main";

/// new-lock §8 `new_head_no_30s_failure`, `follow_ups_unchanged_wait_post_visible`,
/// `fetch_due_busy_is_fetch_skipped`, `fetch_skipped_warning_in_report` and
/// the `new` half of `shadow_pin_unpin_take_no_repo_lock`: with the repo
/// lock held far past the lock wait, `new` builds and pins the workspace
/// and activates the session (a due fetch is skipped and says so; a fresh
/// one reads the snapshot), then waits in its follow-ups until the holder
/// releases, as before.
#[test]
fn new_head_no_30s_failure() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let other = sid("s-hhhhhhhh");
    serving(&d, || {
        // A first session fetches origin, so the snapshot has a tip.
        ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        for fresh in [false, true] {
            d.sessions.set_fetch_window(if fresh {
                Duration::from_secs(600)
            } else {
                Duration::ZERO
            });
            let before = active(&core);
            std::thread::scope(|s| {
                let release = hold(s, &core, "tool", &other);
                let h =
                    s.spawn(|| call(&sock, "new", json!({ "ticket": null, "repos": ["tool"] })));
                wait_until("the session is Active while the lock is held", || {
                    active(&core) > before
                });
                let id = core
                    .store
                    .sessions()
                    .unwrap()
                    .into_iter()
                    .filter(|r| r.lifecycle == Lifecycle::Active)
                    .max_by_key(|r| r.created)
                    .unwrap()
                    .manifest
                    .id;
                assert!(pin(&core, "tool", &id).is_some(), "pinned without the lock");
                std::thread::sleep(fx.lock_wait * 2);
                assert!(
                    !h.is_finished(),
                    "the follow-ups wait for the holder, as before"
                );
                release.send(()).unwrap();
                let r = ok(h.join().unwrap());
                assert_eq!(new_id(&r), id);
                let skipped: Vec<&Value> = r["session"]["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|w| w["kind"] == "FetchSkipped")
                    .collect();
                if fresh {
                    assert!(skipped.is_empty(), "{r:#}");
                } else {
                    assert_eq!(skipped.len(), 1, "{r:#}");
                    assert_eq!(skipped[0]["repo"], "tool");
                    assert_eq!(skipped[0]["holder"], "sync");
                    assert_eq!(skipped[0]["holder_session"], other.as_str());
                    assert!(skipped[0]["last_fetch_at"].is_string(), "{r:#}");
                }
            });
        }
    });
    drop(d);
    core.close().unwrap();
}

/// new-lock §8, `new_head_no_30s_failure` for `add`: the added workspace
/// is recorded while its repo's lock is held; the due fetch is skipped and
/// the warning is returned (no longer dropped).
#[test]
fn add_head_no_30s_failure() {
    let fx = Dfx::with(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let other = sid("s-hhhhhhhh");
    let front = repo_name("front");
    serving(&d, || {
        // Every repo fetched once (the full layout), then a slim session.
        ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        let id = new_id(&ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"], "only": true }),
        )));
        std::thread::scope(|s| {
            let release = hold(s, &core, "front", &other);
            let h = s.spawn(|| call(&sock, "add", json!({ "session": id, "repos": ["front"] })));
            wait_until("front is recorded while its lock is held", || {
                core.store
                    .session(&id)
                    .unwrap()
                    .unwrap()
                    .manifest
                    .repos
                    .contains_key(&front)
            });
            assert!(pin(&core, "front", &id).is_some());
            std::thread::sleep(fx.lock_wait * 2);
            assert!(!h.is_finished(), "add waits after recording, as before");
            release.send(()).unwrap();
            let r = ok(h.join().unwrap());
            assert!(r["report"]["added"]["front"].is_object(), "{r:#}");
            let warnings = r["report"]["warnings"].as_array().unwrap();
            assert!(
                warnings.iter().any(|w| w["kind"] == "FetchSkipped"
                    && w["repo"] == "front"
                    && w["holder"] == "sync"
                    && w["holder_session"] == other.as_str()),
                "{r:#}"
            );
        });
    });
    drop(d);
    core.close().unwrap();
}

/// new-lock §8 `base_flag_busy_is_lock_wait` and `holder_detail_fields`:
/// an explicit `--base` waits its FIFO turn (the lock wait) and then is
/// the certified pre-write refusal naming the holder; nothing is created.
#[test]
fn base_flag_busy_is_lock_wait() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let other = sid("s-hhhhhhhh");
    serving(&d, || {
        ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        let before = core.store.sessions().unwrap().len();
        std::thread::scope(|s| {
            let release = hold(s, &core, "tool", &other);
            let t0 = Instant::now();
            let r = call(
                &sock,
                "new",
                json!({ "ticket": null, "repos": ["tool"], "only": true, "base": { "tool": "HEAD" } }),
            );
            let waited = t0.elapsed();
            release.send(()).unwrap();
            assert_eq!(r["error"]["code"], "Busy", "{r:#}");
            let detail = &r["error"]["detail"];
            assert_eq!(detail["reason"], "lock_wait", "{r:#}");
            assert_eq!(detail["retry_safe"], true, "{r:#}");
            assert_eq!(detail["lock"]["repo"], "tool");
            assert_eq!(detail["holder"], "sync");
            assert_eq!(detail["holder_session"], other.as_str());
            assert_eq!(detail["holder_origin"], "request");
            assert!(detail["holder_since"].is_string(), "{r:#}");
            assert!(
                detail["held_ms"].as_u64().unwrap() >= fx.lock_wait.as_millis() as u64,
                "{r:#}"
            );
            assert!(waited >= fx.lock_wait, "{waited:?}");
        });
        assert_eq!(
            core.store.sessions().unwrap().len(),
            before,
            "nothing was created"
        );
    });
    drop(d);
    core.close().unwrap();
}

/// new-lock §4.1 (i): `add`'s initial session-queue timeout is certified
/// (`Sessions::add` never started); the session is unchanged.
#[test]
fn add_queue_timeout_is_certified() {
    let fx = Dfx::with(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_id(&ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"], "only": true }),
        )));
        std::thread::scope(|s| {
            let (release, wait) = mpsc::channel::<()>();
            let (held, is_held) = mpsc::channel::<()>();
            let queue = id.clone();
            let core = &core;
            s.spawn(move || {
                let _scope = RequestScope::begin("checkpoint", Some(queue.clone()));
                core.locks
                    .with_sessions(std::slice::from_ref(&queue), || {
                        held.send(()).unwrap();
                        let _ = wait.recv();
                        Ok(())
                    })
                    .unwrap();
            });
            is_held.recv_timeout(Duration::from_secs(10)).unwrap();
            let r = call(&sock, "add", json!({ "session": id, "repos": ["front"] }));
            release.send(()).unwrap();
            assert_eq!(r["error"]["code"], "Busy", "{r:#}");
            assert_eq!(r["error"]["detail"]["reason"], "lock_wait", "{r:#}");
            assert_eq!(
                r["error"]["detail"]["lock"]["session"],
                id.as_str(),
                "{r:#}"
            );
            assert_eq!(r["error"]["detail"]["retry_safe"], true, "{r:#}");
            assert_eq!(r["error"]["detail"]["holder"], "checkpoint", "{r:#}");
        });
        let rec = core.store.session(&id).unwrap().unwrap();
        assert_eq!(rec.manifest.repos.len(), 1, "unchanged");
    });
    drop(d);
    core.close().unwrap();
}

/// new-lock §8 `snapshot_refreshed_at_release_only`: a holder's ref
/// writes are invisible to snapshot readers (a `new` on a fresh fetch)
/// until it releases the lock.
#[test]
fn snapshot_refreshed_at_release_only() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let tool = repo_name("tool");
    let main = fx.main("tool");
    serving(&d, || {
        ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        let pre = core.locks.ref_snapshot(&tool).expect("a snapshot");
        assert!(pre.valid);
        let o0 = fx
            .sb
            .git(&main, &["rev-parse", TRACKING])
            .trim()
            .to_string();
        assert_eq!(pre.origin.as_ref().map(|o| o.as_str()), Some(o0.as_str()));
        let c1 = fx.sb.commit(&main, &[("later.txt", b"1\n")], "later");
        d.sessions.set_fetch_window(Duration::from_secs(600));
        let before = active(&core);
        std::thread::scope(|s| {
            let release = hold_writing(s, &fx, &core, "tool", vec![(TRACKING.into(), c1.clone())]);
            let mid = core.locks.ref_snapshot(&tool).unwrap();
            assert_eq!(
                (&mid.origin, &mid.chain_top, &mid.anchor, mid.valid),
                (&pre.origin, &pre.chain_top, &pre.anchor, pre.valid),
                "mid-section state is not published"
            );
            let h = s.spawn(|| {
                call(
                    &sock,
                    "new",
                    json!({ "ticket": null, "repos": ["tool"], "only": true }),
                )
            });
            wait_until("a reader's session is Active", || active(&core) > before);
            release.send(()).unwrap();
            let r = ok(h.join().unwrap());
            assert_eq!(
                r["session"]["manifest"]["repos"]["tool"]["head"], o0,
                "{r:#}"
            );
        });
        let post = core.locks.ref_snapshot(&tool).unwrap();
        assert!(post.valid);
        assert_eq!(post.origin.as_ref().map(|o| o.as_str()), Some(c1.as_str()));
        assert!(post.generation > pre.generation);
    });
    drop(d);
    core.close().unwrap();
}

/// new-lock §8 `snapshot_invalid_falls_back_to_lock`: without a valid
/// snapshot the head step takes the lock with the bounded wait, as before
/// new-lock, fresh fetch or due: a held lock is then the certified refusal.
#[test]
fn snapshot_invalid_falls_back_to_lock() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let tool = repo_name("tool");
    let other = sid("s-hhhhhhhh");
    serving(&d, || {
        ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        let before = core.store.sessions().unwrap().len();
        for fresh in [false, true] {
            d.sessions.set_fetch_window(if fresh {
                Duration::from_secs(600)
            } else {
                Duration::ZERO
            });
            std::thread::scope(|s| {
                let release = hold(s, &core, "tool", &other);
                core.locks.invalidate_snapshot(&tool);
                let r = call(&sock, "new", json!({ "ticket": null, "repos": ["tool"] }));
                release.send(()).unwrap();
                assert_eq!(
                    r["error"]["detail"]["reason"], "lock_wait",
                    "fresh {fresh}: {r:#}"
                );
                assert_eq!(r["error"]["detail"]["retry_safe"], true, "{r:#}");
            });
            assert!(
                core.locks.ref_snapshot(&tool).unwrap().valid,
                "the release refreshed it"
            );
        }
        assert_eq!(core.store.sessions().unwrap().len(), before);
    });
    drop(d);
    core.close().unwrap();
}

/// new-lock §8 `explicit_base_uses_live_tip_not_snapshot`: `--base` is
/// judged against the tip the lock's holder leaves, not the snapshot from
/// before: a full SHA the holder's write left behind main is
/// `base_behind_main`.
#[test]
fn explicit_base_uses_live_tip_not_snapshot() {
    let mut fx = Dfx::new();
    // The `--base` take must outwait the holder below.
    fx.lock_wait = Duration::from_secs(10);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let tool = repo_name("tool");
    let main = fx.main("tool");
    serving(&d, || {
        ok(call(
            &sock,
            "new",
            json!({ "ticket": null, "repos": ["tool"] }),
        ));
        // No fetch inside the take: the live tip is what the holder left.
        d.sessions.set_fetch_window(Duration::from_secs(600));
        let o0 = fx
            .sb
            .git(&main, &["rev-parse", TRACKING])
            .trim()
            .to_string();
        let c1 = fx.sb.commit(&main, &[("later.txt", b"1\n")], "later");
        std::thread::scope(|s| {
            let release = hold_writing(s, &fx, &core, "tool", vec![(TRACKING.into(), c1.clone())]);
            let base = o0.clone();
            let h = s.spawn(move || {
                call(
                    &sock,
                    "new",
                    json!({ "ticket": null, "repos": ["tool"], "only": true, "base": { "tool": base } }),
                )
            });
            wait_until("new waits for the lock", || {
                core.locks.repo_waiting(&tool) > 0
            });
            assert_eq!(
                core.locks
                    .ref_snapshot(&tool)
                    .unwrap()
                    .origin
                    .unwrap()
                    .as_str(),
                o0
            );
            release.send(()).unwrap();
            let r = h.join().unwrap();
            assert_eq!(r["error"]["code"], "InvalidInput", "{r:#}");
            assert_eq!(r["error"]["detail"]["reason"], "base_behind_main", "{r:#}");
            assert_eq!(r["error"]["detail"]["main"], c1, "{r:#}");
        });
    });
    drop(d);
    core.close().unwrap();
}

struct ReleaseSession<'a, 'b>(&'a strata_session::Sessions<'b>);
impl Drop for ReleaseSession<'_, '_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn tighten(fx: &mut Dfx, repo: &str) {
    fx.config
        .repos
        .get_mut(&repo_name(repo))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
}

/// new-lock §8 `fence_checkpoints_new`: a pins tightening while `new` is
/// in its (lock-free) git step is refused at the pin checkpoint; the
/// session is rolled back (D56): nothing Active, no pin.
#[test]
fn fence_checkpoints_new() {
    let mut fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let _old = core.admit();
    let id = sid("s-fencenew");
    d.sessions.set_next_id(id.clone());
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "new.git@tool".into(),
    });
    std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            let _old = core.admit();
            d.sessions
                .new_with(None, &[repo_name("tool")], Layout::Only)
        });
        let _release = ReleaseSession(&d.sessions);
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(10)),
            "seam not reached"
        );
        tighten(&mut fx, "tool");
        let result = core.reload_for_test();
        d.sessions.release();
        assert_eq!(result.outcome, "applied");
        let e = h.join().unwrap().unwrap_err();
        assert!(strata_deps::pins_tightened(&e), "{e:?}");
    });
    assert_eq!(active(&core), 0);
    assert_eq!(pin(&core, "tool", &id), None);
}

/// new-lock §8 `fence_checkpoints_add`: a tightening before the lock-free
/// attach is refused there; nothing is recorded and no pin is left.
#[test]
fn fence_checkpoints_add() {
    let mut fx = Dfx::with(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let _old = core.admit();
    let id = d
        .sessions
        .new_with(None, &[repo_name("tool")], Layout::Only)
        .unwrap()
        .manifest
        .id;
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "add.build@front".into(),
    });
    std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            let _old = core.admit();
            d.sessions.add(&id, &[repo_name("front")])
        });
        let _release = ReleaseSession(&d.sessions);
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(10)),
            "seam not reached"
        );
        tighten(&mut fx, "front");
        let result = core.reload_for_test();
        d.sessions.release();
        assert_eq!(result.outcome, "applied");
        let e = h.join().unwrap().unwrap_err();
        assert!(strata_deps::pins_tightened(&e), "{e:?}");
        assert_eq!(e.detail.as_ref().unwrap()["op"], "add", "{e:?}");
    });
    let rec = core.store.session(&id).unwrap().unwrap();
    assert_eq!(
        rec.manifest.repos.keys().collect::<Vec<_>>(),
        [&repo_name("tool")]
    );
    assert_eq!(pin(&core, "front", &id), None);
}

/// new-lock §8 `rm_unpin_no_lock_fixed_expected_oid` and the `rm` half of
/// `shadow_pin_unpin_take_no_repo_lock`: `rm` deletes the pins without the
/// repo lock, so a long holder does not hold it up; a pin already gone
/// counts as deleted.
#[test]
fn rm_unpin_no_lock_fixed_expected_oid() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let other = sid("s-hhhhhhhh");
    serving(&d, || {
        let only = json!({ "ticket": null, "repos": ["tool"], "only": true });
        let a = new_id(&ok(call(&sock, "new", only.clone())));
        let b = new_id(&ok(call(&sock, "new", only)));
        assert!(pin(&core, "tool", &a).is_some());
        let gone = format!("refs/strata/pins/{b}");
        fx.sb
            .git(&fx.main("tool"), &["update-ref", "-d", gone.as_str()]);
        std::thread::scope(|s| {
            let release = hold(s, &core, "tool", &other);
            for id in [&a, &b] {
                let t0 = Instant::now();
                ok(call(
                    &sock,
                    "rm",
                    json!({ "session": id, "discard": false }),
                ));
                assert!(t0.elapsed() < fx.lock_wait * 10, "{:?}", t0.elapsed());
                assert_eq!(pin(&core, "tool", id), None);
            }
            release.send(()).unwrap();
        });
    });
    drop(d);
    core.close().unwrap();
}

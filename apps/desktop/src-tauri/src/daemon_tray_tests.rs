//! Controlled temporary sockets only. These exercise the actual blocking flights.
use super::*;
use crate::{
    state::AppState,
    tray_model::{Icon, model_at},
};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Instant, SystemTime},
};
struct Request {
    stream: UnixStream,
    value: Value,
}
impl Request {
    fn reply(mut self, data: Value) {
        writeln!(
            self.stream,
            "{}",
            json!({"id":self.value["id"],"ok":true,"data":data})
        )
        .unwrap();
    }
}
struct Fake {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    requests: mpsc::Receiver<Request>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fake {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("tray-flight")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else {
                    break;
                };
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                if tx
                    .send(Request {
                        stream,
                        value: serde_json::from_str(&line).unwrap(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            _dir: dir,
            socket,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn next(&self) -> Request {
        self.requests
            .recv_timeout(Duration::from_secs(2))
            .expect("request on fixture socket")
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn now_at(base: Stamp, t: u64) -> Stamp {
    Stamp {
        instant: base.instant + Duration::from_secs(t),
        wall: base.wall + Duration::from_secs(t),
    }
}
async fn attempted(state: &AppState, r: &Read, at: Stamp) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state
                .snapshot()
                .input(r)
                .unwrap()
                .last_attempt
                .is_some_and(|t| t.wall == at.wall)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pollers_join_but_window_waits_for_its_own_fresh_flight() {
    let fake = Fake::new();
    let d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_secs(1));
    let state = AppState::new(Some(d.clone()));
    let d1 = d.clone();
    let poll = tokio::spawn(async move { d1.poll_read(Read::DepsList).await });
    let first = fake.next();
    let d2 = d.clone();
    let joined = tokio::spawn(async move { d2.poll_read(Read::DepsList).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    let d3 = d.clone();
    let window = tokio::spawn(async move { d3.read(Read::DepsList).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!window.is_finished());
    assert!(fake.requests.try_recv().is_err());
    first.reply(json!({"proposals":[],"flight":"old"}));
    let second = fake.next();
    assert_eq!(
        state.snapshot().deps.value.as_ref().unwrap()["flight"],
        "old"
    );
    second.reply(json!({"proposals":[],"flight":"fresh"}));
    assert_eq!(poll.await.unwrap().unwrap()["flight"], "old");
    assert_eq!(joined.await.unwrap().unwrap()["flight"], "old");
    assert_eq!(window.await.unwrap().unwrap()["flight"], "fresh");
    assert_eq!(state.snapshot().deps.applied_gen, 2);
    assert_eq!(state.snapshot().deps.value.unwrap()["flight"], "fresh");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_open_connection_counts_joined_deadlines_for_ten_minutes() {
    for window_started in [false, true] {
        let fake = Fake::new();
        let base = Stamp {
            instant: Instant::now(),
            wall: SystemTime::now(),
        };
        let clock = Arc::new(Mutex::new(base));
        let mut d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_millis(40));
        let c = clock.clone();
        d.clock = Arc::new(move || *c.lock().unwrap());
        let state = AppState::new(Some(d.clone()));
        d.store.record(
            &Read::DaemonStatus,
            Some(1),
            base,
            base,
            &Ok(json!({"fds":{}})),
        );
        // Align the explicit fixture seed with the daemon's own generation counter.
        d.reads
            .lock()
            .unwrap()
            .generations
            .insert(Read::DaemonStatus.flight_key(), 1);
        *clock.lock().unwrap() = now_at(base, 10);
        let d1 = d.clone();
        let poll = tokio::spawn(async move {
            if window_started {
                d1.read(Read::DaemonStatus).await
            } else {
                d1.poll_read(Read::DaemonStatus).await
            }
        });
        let held = fake.next();
        *clock.lock().unwrap() = now_at(base, 15);
        assert_eq!(
            poll.await.unwrap().unwrap_err().reason(),
            Some("daemon_timeout")
        );
        assert_eq!(state.snapshot().status_timeouts, 1);
        for t in (20..=600).step_by(10) {
            *clock.lock().unwrap() = now_at(base, t);
            if window_started {
                assert_eq!(
                    d.read(Read::DaemonStatus).await.unwrap_err().reason(),
                    Some("request_in_flight")
                );
            }
            let d1 = d.clone();
            let poll = tokio::spawn(async move { d1.poll_read(Read::DaemonStatus).await });
            attempted(&state, &Read::DaemonStatus, now_at(base, t)).await;
            *clock.lock().unwrap() = now_at(base, t + 5);
            assert_eq!(
                poll.await.unwrap().unwrap_err().reason(),
                Some("daemon_timeout")
            );
            assert_eq!(
                model_at(&state.snapshot(), now_at(base, t + 5)).icon,
                Icon::Down
            );
            assert_eq!(
                d.reads.lock().unwrap().generations[&Read::DaemonStatus.flight_key()],
                2
            );
            assert!(fake.requests.try_recv().is_err());
        }
        held.reply(json!({"fds":{}}));
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.snapshot().status_timeouts > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_budget_expires_without_starting_or_joining_an_old_window_result() {
    let fake = Fake::new();
    let d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_millis(50));
    let state = AppState::new(Some(d.clone()));
    let d1 = d.clone();
    let poll = tokio::spawn(async move { d1.poll_read(Read::DepsList).await });
    let held = fake.next();
    let attempted = state.snapshot().deps.last_attempt.unwrap();
    assert_eq!(
        d.read(Read::DepsList).await.unwrap_err().reason(),
        Some("daemon_timeout")
    );
    assert_eq!(
        state.snapshot().deps.last_attempt.unwrap().wall,
        attempted.wall
    );
    assert_eq!(state.snapshot().deps.applied_gen, 0);
    poll.await.unwrap().unwrap_err();
    held.reply(json!({"late":true}));
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.snapshot().deps.applied_gen == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.snapshot().deps.value.unwrap()["late"], true);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_kind_has_its_own_flight_and_window_deadline_restarts_after_wait() {
    let fake = Fake::new();
    let d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_millis(250));
    let d1 = d.clone();
    let poll = tokio::spawn(async move { d1.poll_read(Read::Sessions).await });
    let first = fake.next();
    let d2 = d.clone();
    let window = tokio::spawn(async move { d2.read(Read::Sessions).await });
    let d3 = d.clone();
    let health = tokio::spawn(async move { d3.poll_read(Read::DaemonStatus).await });
    let health_req = fake.next();
    health_req.reply(json!({"fds":{}}));
    health.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    first.reply(json!({"sessions":[]}));
    poll.await.unwrap().unwrap();
    let second = fake.next();
    tokio::time::sleep(Duration::from_millis(150)).await;
    second.reply(json!({"fresh":true}));
    assert_eq!(window.await.unwrap().unwrap()["fresh"], true);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_watch_wakes_window_and_per_session_and_du_still_refuse() {
    let fake = Fake::new();
    let d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_secs(1));
    let (tx, rx) = watch::channel(None);
    let key = Read::DaemonStatus.flight_key();
    d.reads.lock().unwrap().slots.insert(
        key.clone(),
        ReadFlight {
            started_by: Caller::Poller,
            reply: rx,
        },
    );
    let d1 = d.clone();
    let window = tokio::spawn(async move { d1.read(Read::DaemonStatus).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    // Exactly the unwind ordering: slot is removed before the sender is dropped.
    d.reads.lock().unwrap().slots.remove(&key);
    drop(tx);
    fake.next().reply(json!({"ready":true}));
    assert_eq!(window.await.unwrap().unwrap()["ready"], true);
    for r in [Read::Du, Read::Session("s-one".into())] {
        let (d1, r1) = (d.clone(), r.clone());
        let first = tokio::spawn(async move { d1.read(r1).await });
        let held = fake.next();
        assert_eq!(
            d.poll_read(r.clone()).await.unwrap_err().reason(),
            Some("request_in_flight")
        );
        held.reply(json!({}));
        first.await.unwrap().unwrap();
        assert!(
            !d.reads
                .lock()
                .unwrap()
                .generations
                .contains_key(&r.flight_key())
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_wire_replies_keep_start_age_and_completion_liveness_separate() {
    for delay in [17, 26] {
        let fake = Fake::new();
        let base = Stamp {
            instant: Instant::now(),
            wall: SystemTime::now(),
        };
        let clock = Arc::new(Mutex::new(base));
        let mut d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_millis(40));
        let c = clock.clone();
        d.clock = Arc::new(move || *c.lock().unwrap());
        let state = AppState::new(Some(d.clone()));
        // Warm steady state as clarified in both r9 reviews.
        d.store.record(
            &Read::DaemonStatus,
            Some(1),
            base,
            now_at(base, 7),
            &Ok(json!({"fds":{}})),
        );
        d.reads
            .lock()
            .unwrap()
            .generations
            .insert(Read::DaemonStatus.flight_key(), 1);
        let step = if delay == 17 { 20 } else { 30 };
        for start in (10..610).step_by(step) {
            *clock.lock().unwrap() = now_at(base, start);
            let d1 = d.clone();
            let first = tokio::spawn(async move { d1.poll_read(Read::DaemonStatus).await });
            let held = fake.next();
            *clock.lock().unwrap() = now_at(base, start + 5);
            first.await.unwrap().unwrap_err();
            for offset in (10..delay).step_by(10) {
                *clock.lock().unwrap() = now_at(base, start + offset);
                let d1 = d.clone();
                let joined = tokio::spawn(async move { d1.poll_read(Read::DaemonStatus).await });
                attempted(&state, &Read::DaemonStatus, now_at(base, start + offset)).await;
                *clock.lock().unwrap() = now_at(base, start + offset + 5);
                joined.await.unwrap().unwrap_err();
            }
            let snapshot = state.snapshot();
            let observed = now_at(base, start + if delay == 17 { 15 } else { 25 });
            if delay == 17 {
                assert_eq!(snapshot.status_timeouts, 2);
                assert!(snapshot.last_status_ok.unwrap().age(observed) < Duration::from_secs(20));
                assert_ne!(model_at(&snapshot, observed).icon, Icon::Down);
            } else {
                assert_eq!(model_at(&snapshot, observed).icon, Icon::Down);
            }
            assert!(fake.requests.try_recv().is_err());
            *clock.lock().unwrap() = now_at(base, start + delay);
            held.reply(json!({"fds":{}}));
            tokio::time::timeout(Duration::from_secs(2), async {
                while state.snapshot().status_timeouts > 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let snapshot = state.snapshot();
            assert_eq!(
                snapshot.daemon_status.at.unwrap().wall,
                now_at(base, start).wall
            );
            assert_eq!(
                snapshot.last_status_ok.unwrap().wall,
                now_at(base, start + delay).wall
            );
            assert_ne!(
                model_at(&snapshot, now_at(base, start + delay)).icon,
                Icon::Down
            );
        }
    }
}

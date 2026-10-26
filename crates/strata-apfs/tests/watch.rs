//! Task 9: FSEvents watcher (§5.7).

mod common;
#[path = "watch_support/canary.rs"]
mod canary;

use std::collections::HashSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use common::{Tmp, rp, set_mode};
use strata_apfs::{WatchEvent, WatchOptions, Watcher, volume_caps, watch, watch_with};

// ───────── helpers ─────────

/// fseventsd is shared system-wide: the 20,000-file flood in one test delays
/// delivery to every other stream for seconds. Watch tests run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Upper bound for "this event arrives" waits; returns as soon as it does.
/// §4.2 F measured delivery at 15 ms to 3.3 s; after other test binaries'
/// churn it has exceeded 5 s here.
const ARRIVE: Duration = Duration::from_secs(15);

struct Rec {
    rx: Receiver<WatchEvent>,
    seen: Vec<WatchEvent>,
}

fn recorder() -> (Sender<WatchEvent>, Rec) {
    let (tx, rx) = channel();
    (tx, Rec { rx, seen: Vec::new() })
}

impl Rec {
    fn drain(&mut self) {
        while let Ok(e) = self.rx.try_recv() {
            self.seen.push(e);
        }
    }
    fn mark(&mut self) -> usize {
        self.drain();
        self.seen.len()
    }
    fn wait(&mut self, timeout: Duration, pred: impl Fn(&[WatchEvent]) -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            self.drain();
            if pred(&self.seen) {
                return true;
            }
            let now = Instant::now();
            if now >= end {
                return false;
            }
            if let Ok(e) = self.rx.recv_timeout((end - now).min(Duration::from_millis(50))) {
                self.seen.push(e);
            }
        }
    }
    fn changed_since(&self, m: usize) -> Vec<PathBuf> {
        self.seen[m..]
            .iter()
            .flat_map(|e| match e {
                WatchEvent::Changed(v) => v.clone(),
                _ => vec![],
            })
            .collect()
    }
    fn all_paths_since(&self, m: usize) -> Vec<PathBuf> {
        self.seen[m..]
            .iter()
            .flat_map(|e| match e {
                WatchEvent::Changed(v) => v.clone(),
                WatchEvent::MustRescan(p) => vec![p.clone()],
                WatchEvent::Dropped => vec![],
            })
            .collect()
    }
}

fn changed_has(seen: &[WatchEvent], p: &Path) -> bool {
    seen.iter().any(|e| matches!(e, WatchEvent::Changed(v) if v.iter().any(|q| q == p)))
}

/// Run `op`, then wait up to 5 s for a `Changed` naming every path.
fn step(rec: &mut Rec, paths: &[&Path], op: impl FnOnce()) {
    let m = rec.mark();
    let since = Instant::now();
    op();
    let ok = rec.wait(ARRIVE, |s| paths.iter().all(|p| changed_has(&s[m..], p)));
    if !ok {
        canary::starved_or_fail(since, format!("no Changed for {paths:?}; got {:?}", &rec.seen[m..]));
    }
}

fn send_to(tx: Sender<WatchEvent>) -> impl Fn(WatchEvent) + Send + 'static {
    move |e| {
        let _ = tx.send(e);
    }
}

fn strictly_under(p: &Path, dir: &Path) -> bool {
    p != dir && p.starts_with(dir)
}

struct DropCount(Arc<AtomicUsize>);
impl Drop for DropCount {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn wait_until(timeout: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// Write files into `dir` every 100 ms until `f()` or timeout.
fn poke_until(dir: &Path, timeout: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + timeout;
    let mut i = 0;
    while Instant::now() < end {
        if f() {
            return true;
        }
        let _ = fs::write(dir.join(format!("poke{i}")), b"x");
        i += 1;
        let _ = wait_until(Duration::from_millis(100), &f);
    }
    f()
}

fn mkroot(t: &Tmp, name: &str) -> PathBuf {
    let p = t.join(name);
    fs::create_dir_all(&p).unwrap();
    p
}

// ───────── tests ─────────

#[test]
fn watcher_reports_file_and_directory_changes() {
    canary::run("watcher_reports_file_and_directory_changes", watcher_reports_file_and_directory_changes_body);
}

fn watcher_reports_file_and_directory_changes_body() {
    let _serial = serial();
    let t = Tmp::new();
    let root = mkroot(&t, "w");
    let (tx, mut rec) = recorder();
    let _w = watch(&root, send_to(tx)).unwrap();

    let f = root.join("f");
    let g = root.join("g");
    step(&mut rec, &[&f], || fs::write(&f, b"1").unwrap());
    step(&mut rec, &[&f], || fs::write(&f, b"22").unwrap());
    step(&mut rec, &[&f], || set_mode(&f, 0o600));
    step(&mut rec, &[&f, &g], || fs::rename(&f, &g).unwrap());
    step(&mut rec, &[&g], || fs::remove_file(&g).unwrap());

    // Directory rename: both names, no per-child events.
    let d = root.join("d");
    let e = root.join("e");
    fs::create_dir_all(d.join("sub")).unwrap();
    let child = d.join("sub/child");
    step(&mut rec, &[&child], || fs::write(&child, b"c").unwrap());
    let m = rec.mark();
    fs::rename(&d, &e).unwrap();
    {
        let since = Instant::now();
        if !rec.wait(ARRIVE, |s| changed_has(&s[m..], &d) && changed_has(&s[m..], &e)) {
            canary::starved_or_fail(since, format!("{:?}", &rec.seen[m..]));
        }
    }
    thread::sleep(Duration::from_millis(500));
    rec.drain();
    let kids: Vec<_> = rec.all_paths_since(m).into_iter().filter(|p| strictly_under(p, &d) || strictly_under(p, &e)).collect();
    assert!(kids.is_empty(), "directory rename reported children: {kids:?}");

    // clone_tree into the root: the new top only.
    let src = mkroot(&t, "src");
    fs::create_dir_all(src.join("a/b")).unwrap();
    fs::write(src.join("a/b/f"), b"x").unwrap();
    let c = root.join("c");
    let m = rec.mark();
    strata_apfs::clone_tree(&src, &c).unwrap();
    {
        let since = Instant::now();
        if !rec.wait(ARRIVE, |s| changed_has(&s[m..], &c)) {
            canary::starved_or_fail(since, format!("{:?}", &rec.seen[m..]));
        }
    }
    thread::sleep(Duration::from_millis(500));
    rec.drain();
    let kids: Vec<_> = rec.all_paths_since(m).into_iter().filter(|p| strictly_under(p, &c)).collect();
    assert!(kids.is_empty(), "clone_tree reported children: {kids:?}");

    // Created and deleted within one latency window: still reported.
    let h = root.join("h");
    step(&mut rec, &[&h], || {
        fs::write(&h, b"h").unwrap();
        fs::remove_file(&h).unwrap();
    });
}

#[test]
fn watcher_paths_use_the_callers_root_and_disk_spelling() {
    canary::run("watcher_paths_use_the_callers_root_and_disk_spelling", watcher_paths_use_the_callers_root_and_disk_spelling_body);
}

fn watcher_paths_use_the_callers_root_and_disk_spelling_body() {
    let _serial = serial();
    let t = Tmp::new();
    let real = mkroot(&t, "real");
    let link = t.join("x");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let (tx, mut rec) = recorder();
    let _w = watch(&link, send_to(tx)).unwrap();

    let upper = link.join("Foo.txt");
    step(&mut rec, &[&upper], || fs::write(&upper, b"1").unwrap());
    let insensitive = !volume_caps(&real).unwrap().case_sensitive;
    if insensitive {
        let m = rec.mark();
        let since = Instant::now();
        fs::write(link.join("foo.txt"), b"2").unwrap();
        {
        if !rec.wait(ARRIVE, |s| changed_has(&s[m..], &upper)) {
            canary::starved_or_fail(since, format!("{:?}", &rec.seen[m..]));
        }
    }
        rec.drain();
        assert!(!rec.changed_since(m).contains(&link.join("foo.txt")), "event used the caller's spelling, not the disk's");
    }

    // NFD on disk stays NFD, even when written through the NFC spelling.
    let nfd = link.join("cafe\u{301}");
    step(&mut rec, &[&nfd], || fs::write(&nfd, b"n").unwrap());
    let m = rec.mark();
    let since = Instant::now();
    fs::write(link.join("caf\u{e9}"), b"m").unwrap();
    {
        if !rec.wait(ARRIVE, |s| changed_has(&s[m..], &nfd)) {
            canary::starved_or_fail(since, format!("{:?}", &rec.seen[m..]));
        }
    }

    rec.drain();
    for p in rec.all_paths_since(0) {
        assert!(p.starts_with(&link), "path not under the caller's root: {p:?}");
        assert!(!p.as_os_str().as_bytes().starts_with(real.as_os_str().as_bytes()), "{p:?}");
    }
}

#[test]
fn watcher_excludes_configured_dirs() {
    canary::run("watcher_excludes_configured_dirs", watcher_excludes_configured_dirs_body);
}

fn watcher_excludes_configured_dirs_body() {
    let _serial = serial();
    let t = Tmp::new();
    let root = mkroot(&t, "w");
    let (tx, mut rec) = recorder();
    let opts = WatchOptions { exclude: vec![rp("node_modules")] };
    let _w = watch_with(&root, &opts, send_to(tx)).unwrap();

    // The canary window starts before the writes it vouches for.
    let since = Instant::now();
    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::write(root.join("node_modules/pkg/x"), b"x").unwrap();
    fs::create_dir_all(root.join("node_modules2")).unwrap();
    let y = root.join("node_modules2/y");
    fs::write(&y, b"y").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    let sx = root.join("src/x");
    fs::write(&sx, b"x").unwrap();
    {
        if !rec.wait(ARRIVE, |s| changed_has(s, &y) && changed_has(s, &sx)) {
            canary::starved_or_fail(since, format!("{:?}", rec.seen));
        }
    }
    thread::sleep(Duration::from_secs(1));
    rec.drain();
    let nm = root.join("node_modules");
    let bad: Vec<_> = rec.all_paths_since(0).into_iter().filter(|p| p.starts_with(&nm)).collect();
    assert!(bad.is_empty(), "excluded paths reported: {bad:?}");
    assert!(rec.seen.iter().all(|e| !matches!(e, WatchEvent::Changed(v) if v.is_empty())), "empty Changed");

    // Case-insensitive volume: the exclude matches the folded spelling.
    if !volume_caps(&root).unwrap().case_sensitive {
        let root2 = mkroot(&t, "w2");
        let (tx2, mut rec2) = recorder();
        let _w2 = watch_with(&root2, &opts, send_to(tx2)).unwrap();
        let since = Instant::now();
        fs::create_dir_all(root2.join("Node_Modules")).unwrap();
        fs::write(root2.join("Node_Modules/z"), b"z").unwrap();
        let s2 = root2.join("sentinel");
        fs::write(&s2, b"s").unwrap();
        {
        if !rec2.wait(ARRIVE, |s| changed_has(s, &s2)) {
            canary::starved_or_fail(since, format!("{:?}", rec2.seen));
        }
    }
        thread::sleep(Duration::from_secs(1));
        rec2.drain();
        let nm2 = root2.join("Node_Modules");
        let bad: Vec<_> = rec2.all_paths_since(0).into_iter().filter(|p| p.starts_with(&nm2)).collect();
        assert!(bad.is_empty(), "case-folded exclude missed: {bad:?}");
    }
}

#[test]
fn dropping_the_watcher_stops_callbacks() {
    canary::run("dropping_the_watcher_stops_callbacks", dropping_the_watcher_stops_callbacks_body);
}

fn dropping_the_watcher_stops_callbacks_body() {
    let _serial = serial();
    let t = Tmp::new();

    // (1) After drop: closure dropped exactly once, no more calls.
    let root = mkroot(&t, "a");
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = channel::<WatchEvent>();
    let w = {
        let calls = calls.clone();
        let guard = DropCount(drops.clone());
        watch(&root, move |e| {
            let _ = &guard;
            calls.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(e);
        })
        .unwrap()
    };
    {
        let since = Instant::now();
        if !poke_until(&root, ARRIVE, || calls.load(Ordering::SeqCst) > 0) {
            canary::starved_or_fail(since, "poke_until(&root, ARRIVE, || calls.load(Ordering::SeqCst) > 0) never became true".to_string());
        }
    }
    drop(w);
    assert_eq!(drops.load(Ordering::SeqCst), 1, "closure not dropped by drop(Watcher)");
    while rx.try_recv().is_ok() {}
    assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Disconnected)));
    let n = calls.load(Ordering::SeqCst);
    for i in 0..20 {
        fs::write(root.join(format!("after{i}")), b"x").unwrap();
    }
    thread::sleep(Duration::from_secs(1));
    assert_eq!(calls.load(Ordering::SeqCst), n, "callback ran after drop");
    assert_eq!(drops.load(Ordering::SeqCst), 1);

    // (2) Drop while a callback runs: waits for it to return.
    let root = mkroot(&t, "b");
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let w = {
        let (started, finished) = (started.clone(), finished.clone());
        watch(&root, move |_| {
            if !started.swap(true, Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(300));
                finished.store(true, Ordering::SeqCst);
            }
        })
        .unwrap()
    };
    {
        let since = Instant::now();
        if !poke_until(&root, ARRIVE, || started.load(Ordering::SeqCst)) {
            canary::starved_or_fail(since, "poke_until(&root, ARRIVE, || started.load(Ordering::SeqCst)) never became true".to_string());
        }
    }
    drop(w);
    assert!(finished.load(Ordering::SeqCst), "drop returned while the callback was still running");

    // (3) A panicking callback: logged, Dropped reported, watcher still usable.
    let root = mkroot(&t, "c");
    let (tx, mut rec) = recorder();
    let w = watch(&root, move |e| {
        if matches!(&e, WatchEvent::Changed(v) if v.iter().any(|p| p.ends_with("boom"))) {
            panic!("boom");
        }
        let _ = tx.send(e);
    })
    .unwrap();
    fs::write(root.join("boom"), b"x").unwrap();
    {
        let since = Instant::now();
        if !rec.wait(ARRIVE, |s| s.contains(&WatchEvent::Dropped)) {
            canary::starved_or_fail(since, format!("{:?}", rec.seen));
        }
    }
    let ok = root.join("ok");
    step(&mut rec, &[&ok], || fs::write(&ok, b"x").unwrap());
    drop(w);
}

#[test]
fn watcher_reports_a_renamed_root_as_dropped() {
    canary::run("watcher_reports_a_renamed_root_as_dropped", watcher_reports_a_renamed_root_as_dropped_body);
}

fn watcher_reports_a_renamed_root_as_dropped_body() {
    let _serial = serial();
    let t = Tmp::new();
    let root = mkroot(&t, "r");
    let (tx, mut rec) = recorder();
    let w = watch(&root, send_to(tx)).unwrap();
    let f = root.join("f");
    step(&mut rec, &[&f], || fs::write(&f, b"x").unwrap());
    fs::rename(&root, t.join("r2")).unwrap();
    {
        let since = Instant::now();
        if !rec.wait(ARRIVE, |s| s.contains(&WatchEvent::Dropped)) {
            canary::starved_or_fail(since, format!("{:?}", rec.seen));
        }
    }

    // The old stream does not follow the new directory at the old path.
    fs::create_dir(&root).unwrap();
    let n = root.join("new");
    let m = rec.mark();
    fs::write(&n, b"x").unwrap();
    thread::sleep(Duration::from_secs(1));
    rec.drain();
    assert!(!rec.changed_since(m).contains(&n), "old watcher reported the replacement root");
    drop(w);

    // A new watcher on the new root works.
    let (tx, mut rec) = recorder();
    let _w = watch(&root, send_to(tx)).unwrap();
    let n2 = root.join("new2");
    step(&mut rec, &[&n2], || fs::write(&n2, b"x").unwrap());
}

#[derive(Default)]
struct Cov {
    changed: HashSet<PathBuf>,
    rescans: Vec<PathBuf>,
    dropped: usize,
}

#[test]
fn watcher_reports_must_rescan() {
    canary::run("watcher_reports_must_rescan", watcher_reports_must_rescan_body);
}

fn watcher_reports_must_rescan_body() {
    let _serial = serial();
    let mut triggered = false;
    for round in 0..3 {
        let t = Tmp::new();
        let root = mkroot(&t, "w");
        for d in 0..16 {
            fs::create_dir(root.join(format!("d{d}"))).unwrap();
        }
        let cov = Arc::new(Mutex::new(Cov::default()));
        let (gate_tx, gate_rx) = channel::<()>();
        let gate = Mutex::new(Some(gate_rx));
        let entered = Arc::new(AtomicBool::new(false));
        let w = {
            let (cov, entered) = (cov.clone(), entered.clone());
            watch(&root, move |e| {
                if let Some(rx) = gate.lock().unwrap().take() {
                    entered.store(true, Ordering::SeqCst);
                    let _ = rx.recv_timeout(Duration::from_secs(60));
                }
                let mut c = cov.lock().unwrap();
                match e {
                    WatchEvent::Changed(v) => c.changed.extend(v),
                    WatchEvent::MustRescan(p) => c.rescans.push(p),
                    WatchEvent::Dropped => c.dropped += 1,
                }
            })
            .unwrap()
        };
        // Get the callback stuck first.
        {
        let since = Instant::now();
        if !poke_until(&root, ARRIVE, || entered.load(Ordering::SeqCst)) {
            canary::starved_or_fail(since, "poke_until(&root, ARRIVE, || entered.load(Ordering::SeqCst)) never became true".to_string());
        }
    }
        let files: Vec<PathBuf> = (0..4)
            .map(|th| {
                let root = root.clone();
                thread::spawn(move || {
                    let mut v = Vec::new();
                    for i in 0..5000 {
                        let p = root.join(format!("d{}/f{th}_{i}", (th * 5000 + i) % 16));
                        fs::write(&p, b"").unwrap();
                        v.push(p);
                    }
                    v
                })
            })
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect();
        drop(gate_tx);
        let covered = |c: &Cov, p: &Path| {
            c.dropped > 0
                || c.changed.contains(p)
                || p.ancestors().skip(1).any(|a| a.starts_with(&root) && c.changed.contains(a))
                || c.rescans.iter().any(|r| p.starts_with(r))
        };
        let since = Instant::now();
        let all = wait_until(Duration::from_secs(30), || {
            let c = cov.lock().unwrap();
            files.iter().all(|p| covered(&c, p))
        });
        let (dropped, rescans, nchanged) = {
            let c = cov.lock().unwrap();
            (c.dropped, c.rescans.len(), c.changed.len())
        };
        eprintln!("round {round}: dropped={dropped} rescans={rescans} changed={nchanged} all_covered={all}");
        if !all {
            canary::starved_or_fail(since, format!("round {round}: some created files were never covered"));
        }
        if dropped > 0 || rescans > 0 {
            triggered = true;
            // Still usable after a drop/rescan.
            w.flush();
            // The backlog of the flood can still be draining, so "after" may
            // itself be reported by a later Dropped or MustRescan instead of a
            // Changed; any of the three proves the watcher is still alive.
            let (d0, r0) = {
                let c = cov.lock().unwrap();
                (c.dropped, c.rescans.len())
            };
            let after = root.join("after");
            fs::write(&after, b"x").unwrap();
            let since = Instant::now();
            let alive = wait_until(ARRIVE, || {
                let c = cov.lock().unwrap();
                c.changed.contains(&after)
                    || c.dropped > d0
                    || c.rescans[r0..].iter().any(|r| after.starts_with(r))
            });
            {
                let c = cov.lock().unwrap();
                eprintln!(
                    "after: changed={} dropped {d0}->{} rescans {r0}->{}",
                    c.changed.contains(&after),
                    c.dropped,
                    c.rescans.len()
                );
            }
            if !alive {
                let c = cov.lock().unwrap();
                canary::starved_or_fail(
                    since,
                    format!(
                        "watcher dead after Dropped/MustRescan (dropped {d0}->{}, rescans {r0}->{}, changed={})",
                        c.dropped,
                        c.rescans.len(),
                        c.changed.len()
                    ),
                );
            }
            break;
        }
    }
    assert!(triggered, "3 rounds of a 20,000-file flood behind a stuck callback produced neither MustRescan nor Dropped");
}

#[test]
fn flush_drains_the_callback_queue() {
    canary::run("flush_drains_the_callback_queue", flush_drains_the_callback_queue_body);
}

fn flush_drains_the_callback_queue_body() {
    let _serial = serial();
    let t = Tmp::new();
    // (a) flush waits for the callback in flight.
    let root = mkroot(&t, "a");
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let (gate_tx, gate_rx) = channel::<()>();
    let gate = Mutex::new(Some(gate_rx));
    let w = {
        let (started, finished) = (started.clone(), finished.clone());
        Arc::new(
            watch(&root, move |_| {
                started.fetch_add(1, Ordering::SeqCst);
                if let Some(rx) = gate.lock().unwrap().take() {
                    let _ = rx.recv_timeout(Duration::from_secs(30));
                }
                finished.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap(),
        )
    };
    {
        let since = Instant::now();
        if !poke_until(&root, ARRIVE, || started.load(Ordering::SeqCst) > 0) {
            canary::starved_or_fail(since, "poke_until(&root, ARRIVE, || started.load(Ordering::SeqCst) > 0) never became true".to_string());
        }
    }
    let n = started.load(Ordering::SeqCst);
    let flushed = Arc::new(AtomicBool::new(false));
    let h = {
        let (w, flushed) = (w.clone(), flushed.clone());
        thread::spawn(move || {
            w.flush();
            flushed.store(true, Ordering::SeqCst);
        })
    };
    thread::sleep(Duration::from_millis(200));
    assert!(!flushed.load(Ordering::SeqCst), "flush returned while a callback was still running");
    drop(gate_tx);
    h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
    assert!(finished.load(Ordering::SeqCst) >= n);

    // (a') quiet: after flush nothing is in flight.
    thread::sleep(Duration::from_millis(500));
    w.flush();
    assert_eq!(started.load(Ordering::SeqCst), finished.load(Ordering::SeqCst));

    // (b) idle flush returns promptly.
    let mut times = Vec::new();
    for _ in 0..100 {
        let t0 = Instant::now();
        w.flush();
        times.push(t0.elapsed());
    }
    for (i, d) in times.iter().enumerate() {
        if *d > Duration::from_millis(100) {
            eprintln!("slow idle flush #{i}: {d:?}");
        }
    }
    times.sort();
    eprintln!("idle flush: median {:?} max {:?}", times[50], times[99]);
    assert!(times[99] < Duration::from_secs(1), "{times:?}");

    // (c) concurrent flush from 4 threads.
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let w = w.clone();
            thread::spawn(move || {
                for _ in 0..25 {
                    w.flush();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
    }

    // (d) Send + Sync.
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Watcher>();
}

// ───────── re-entry (child process) ─────────

struct Holder(#[allow(dead_code)] Watcher);

fn reentry_drop_round() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let t = Tmp::new();
    let root = mkroot(&t, "w");
    let slot: Arc<Mutex<Option<Arc<Holder>>>> = Arc::new(Mutex::new(None));
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let w = {
        let (slot, calls, done) = (slot.clone(), calls.clone(), done.clone());
        let guard = DropCount(drops.clone());
        let payload = vec![7u8; 1024];
        watch(&root, move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let h = slot.lock().unwrap().take();
            drop(h); // last Arc<Holder>: drops the Watcher inside its own callback
            let s: usize = payload.iter().map(|&b| b as usize).sum();
            assert_eq!(s, 7 * 1024);
            let _ = &guard;
            done.store(true, Ordering::SeqCst);
        })
        .unwrap()
    };
    *slot.lock().unwrap() = Some(Arc::new(Holder(w)));
    drop(slot);
    {
        let since = Instant::now();
        if !poke_until(&root, Duration::from_secs(5), || done.load(Ordering::SeqCst)) {
            canary::starved_or_fail(since, "callback never ran".to_string());
        }
    }
    assert!(wait_until(Duration::from_secs(5), || drops.load(Ordering::SeqCst) == 1), "state never freed");
    eprintln!("{TICK}");
    (calls, drops)
}

fn reentry_flush_own() {
    let t = Tmp::new();
    let root = mkroot(&t, "w");
    let slot: Arc<Mutex<Weak<Watcher>>> = Arc::new(Mutex::new(Weak::new()));
    let (tx, mut rec) = recorder();
    let ran = Arc::new(AtomicBool::new(false));
    let w = {
        let (slot, ran) = (slot.clone(), ran.clone());
        Arc::new(
            watch(&root, move |e| {
                if !ran.swap(true, Ordering::SeqCst) {
                    let me = slot.lock().unwrap().upgrade();
                    if let Some(me) = me {
                        me.flush(); // debug: debug_assert panic → Dropped; release: returns
                    }
                }
                let _ = tx.send(e);
            })
            .unwrap(),
        )
    };
    *slot.lock().unwrap() = Arc::downgrade(&w);
    {
        let since = Instant::now();
        if !poke_until(&root, Duration::from_secs(5), || ran.load(Ordering::SeqCst)) {
            canary::starved_or_fail(since, "poke_until(&root, Duration::from_secs(5), || ran.load(Ordering::SeqCst)) never became true".to_string());
        }
    }
    thread::sleep(Duration::from_millis(500));
    rec.drain();
    let dropped = rec.seen.contains(&WatchEvent::Dropped);
    if cfg!(debug_assertions) {
        assert!(dropped, "debug flush-in-callback should panic and be reported as Dropped");
    }
    drop(w);
}

fn reentry_cross() {
    let t = Tmp::new();
    let ra = mkroot(&t, "a");
    let rb = mkroot(&t, "b");
    let b_started = Arc::new(AtomicUsize::new(0));
    let b_finished = Arc::new(AtomicUsize::new(0));
    let b_drops = Arc::new(AtomicUsize::new(0));
    let a_done = Arc::new(AtomicBool::new(false));
    let go = Arc::new(AtomicBool::new(false));
    let ok = Arc::new(AtomicBool::new(false));
    let b = {
        let (s, f, a_done) = (b_started.clone(), b_finished.clone(), a_done.clone());
        let guard = DropCount(b_drops.clone());
        watch(&rb, move |_| {
            let _ = &guard;
            if s.fetch_add(1, Ordering::SeqCst) == 0 {
                // Stay busy until A has flushed and dropped us (bounded).
                let _ = wait_until(Duration::from_secs(5), || a_done.load(Ordering::SeqCst));
                thread::sleep(Duration::from_millis(20));
            }
            f.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap()
    };
    let b_slot = Arc::new(Mutex::new(Some(b)));
    let a = {
        let (b_slot, go, a_done, ok) = (b_slot.clone(), go.clone(), a_done.clone(), ok.clone());
        let (s, f) = (b_started.clone(), b_finished.clone());
        watch(&ra, move |_| {
            if !go.load(Ordering::SeqCst) || a_done.load(Ordering::SeqCst) {
                return;
            }
            if let Some(b) = b_slot.lock().unwrap().take() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| b.flush()));
                let busy = f.load(Ordering::SeqCst) < s.load(Ordering::SeqCst);
                drop(b); // async path: must not wait for B's running callback
                ok.store(busy, Ordering::SeqCst);
                a_done.store(true, Ordering::SeqCst);
            }
        })
        .unwrap()
    };
    {
        let since = Instant::now();
        if !poke_until(&rb, Duration::from_secs(5), || b_started.load(Ordering::SeqCst) > 0) {
            canary::starved_or_fail(since, "B never called".to_string());
        }
    }
    go.store(true, Ordering::SeqCst);
    {
        let since = Instant::now();
        if !poke_until(&ra, Duration::from_secs(5), || a_done.load(Ordering::SeqCst)) {
            canary::starved_or_fail(since, "A never called".to_string());
        }
    }
    assert!(ok.load(Ordering::SeqCst), "B's callback was not running when A flushed/dropped it");
    assert!(wait_until(Duration::from_secs(5), || b_drops.load(Ordering::SeqCst) == 1), "B's state never freed");
    drop(a);
    eprintln!("{TICK}");
}

/// Printed on stderr by the child after every finished round.
const TICK: &str = "reentry-child: tick";

#[test]
#[ignore = "child process of watcher_reentry_from_the_callback_never_traps"]
fn watcher_reentry_child() {
    canary::run("watcher_reentry_child", watcher_reentry_child_body);
}

fn watcher_reentry_child_body() {
    // (a) 100 rounds of drop-self-in-callback, 50 at a time (each round
    // waits out one delivery latency, which after other test binaries'
    // churn has been ~2 s here).
    let t0 = Instant::now();
    let mut results = Vec::new();
    for _ in 0..2 {
        let hs: Vec<_> = (0..50).map(|_| thread::spawn(reentry_drop_round)).collect();
        results.extend(hs.into_iter().map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))));
    }
    thread::sleep(Duration::from_secs(1));
    for (calls, drops) in &results {
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a second callback after drop-in-callback");
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    eprintln!("reentry-child: (a) done at {:?}", t0.elapsed());
    // (b) flush of one's own watcher from its callback.
    reentry_flush_own();
    eprintln!("reentry-child: (b) done at {:?}", t0.elapsed());
    eprintln!("{TICK}");
    // (c) cross-watcher flush + drop from a callback, 20 rounds, 10 at a time.
    for _ in 0..2 {
        let hs: Vec<_> = (0..10).map(|_| thread::spawn(reentry_cross)).collect();
        for h in hs {
            h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        }
    }
    eprintln!("reentry-child: (c) done at {:?}", t0.elapsed());
    println!("reentry-child: {} rounds", results.len());
}

#[test]
fn watcher_reentry_from_the_callback_never_traps() {
    canary::run("watcher_reentry_from_the_callback_never_traps", watcher_reentry_from_the_callback_never_traps_body);
}

fn watcher_reentry_from_the_callback_never_traps_body() {
    let _serial = serial();
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["watcher_reentry_child", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Deadlock detector, not a performance bound. The child prints a tick
    // after every round; it is killed only when no round finishes for
    // STALL. A wall-clock cap misfired: under heavy machine load (load
    // average ~76 from parallel builds) each FSEvents stream's
    // create/first-delivery/teardown takes long enough that the healthy
    // child needs 94–122 s, daemon running or not; a trap or hang in a
    // callback still stops the ticks.
    const STALL: Duration = Duration::from_secs(60);
    const CAP: Duration = Duration::from_secs(900);
    let ticks = Arc::new(AtomicUsize::new(0));
    let collect = |r: Box<dyn std::io::Read + Send>, ticks: Option<Arc<AtomicUsize>>| {
        thread::spawn(move || {
            use std::io::BufRead;
            let mut all = String::new();
            for line in std::io::BufReader::new(r).lines().map_while(Result::ok) {
                if line == TICK
                    && let Some(t) = &ticks
                {
                    t.fetch_add(1, Ordering::SeqCst);
                }
                all.push_str(&line);
                all.push('\n');
            }
            all
        })
    };
    let out_h = collect(Box::new(child.stdout.take().unwrap()), None);
    let err_h = collect(Box::new(child.stderr.take().unwrap()), Some(ticks.clone()));
    let start = Instant::now();
    let (mut seen, mut last) = (0, Instant::now());
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        let n = ticks.load(Ordering::SeqCst);
        if n != seen {
            (seen, last) = (n, Instant::now());
        }
        if last.elapsed() > STALL || start.elapsed() > CAP {
            let _ = child.kill();
            let _ = child.wait();
            let (o, e) = (out_h.join().unwrap(), err_h.join().unwrap());
            panic!("child made no progress for {:?} after {seen} rounds, {:?} in (deadlock?)\n{o}\n{e}", last.elapsed(), start.elapsed());
        }
        thread::sleep(Duration::from_millis(50));
    };
    let (stdout, stderr) = (out_h.join().unwrap(), err_h.join().unwrap());
    use std::os::unix::process::ExitStatusExt;
    assert_ne!(status.code(), Some(133), "dispatch trap\n{stderr}");
    assert!(status.signal().is_none(), "child killed by signal {:?}\n{stderr}", status.signal());
    assert!(status.success(), "child failed: {status:?}\n{stdout}\n{stderr}");
    if stdout.contains("SKIPPED watcher_reentry_child") {
        let why = stderr.lines().find(|l| l.starts_with("SKIPPED")).unwrap_or("").to_string();
        std::panic::panic_any(canary::Starved(format!("in the child process: {why}")));
    }
    assert!(stdout.contains("reentry-child: 100 rounds"), "{stdout}\n{stderr}");
}

#[test]
#[ignore = "measurement only: prints how often flush already covered a just-written file"]
fn flush_is_not_a_fence() {
    canary::run("flush_is_not_a_fence", flush_is_not_a_fence_body);
}

fn flush_is_not_a_fence_body() {
    let _serial = serial();
    let t = Tmp::new();
    let root = mkroot(&t, "w");
    let (tx, mut rec) = recorder();
    let w = watch(&root, send_to(tx)).unwrap();
    for pre_sleep_ms in [0u64, 10, 100] {
        let mut hit = 0;
        let rounds = 50;
        for i in 0..rounds {
            let p = root.join(format!("s{pre_sleep_ms}_{i}"));
            let m = rec.mark();
            fs::write(&p, b"x").unwrap();
            thread::sleep(Duration::from_millis(pre_sleep_ms));
            w.flush();
            rec.drain();
            if changed_has(&rec.seen[m..], &p) {
                hit += 1;
            }
        }
        println!("flush_is_not_a_fence: sleep={pre_sleep_ms}ms covered={hit}/{rounds}");
    }
}

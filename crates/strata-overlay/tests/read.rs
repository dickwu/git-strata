//! Task 3: the race-safe read of one candidate (§5.2).

mod common;

use std::fs::OpenOptions;
use std::thread;

use common::*;
use strata_overlay::{OverlayFault, inject_fault, read_candidate, release_fault, wait_fault};

#[test]
fn write_race_retries_then_marks_unstable() {
    let w = World::new();
    let p = rp("src/lib.rs");
    let pause = || OverlayFault::PauseAfterHash { path: p.clone() };

    // No writes: neither retried nor unstable.
    let r = read_candidate(&w.ws, &p).unwrap();
    assert!(!r.retried && !r.unstable, "{r:?}");
    assert_eq!(r.blob.as_str(), blake3_hex(b"pub fn a() {}\n"));
    assert_eq!(r.mode, 0o100644);

    // One write during the pause: one retry, then stable on the new content.
    inject_fault(&w.ws, pause());
    thread::scope(|s| {
        let h = s.spawn(|| read_candidate(&w.ws, &p).unwrap());
        assert_eq!(wait_fault(&w.ws), Some(pause()));
        w.write("src/lib.rs", b"pub fn b() {}\n");
        release_fault(&w.ws);
        let r = h.join().unwrap();
        assert!(r.retried && !r.unstable, "{r:?}");
        assert_eq!(r.blob.as_str(), blake3_hex(b"pub fn b() {}\n"));
    });

    // A write before every reread: retried, then unstable with the last read.
    inject_fault(&w.ws, pause());
    inject_fault(&w.ws, pause());
    thread::scope(|s| {
        let h = s.spawn(|| read_candidate(&w.ws, &p).unwrap());
        assert!(wait_fault(&w.ws).is_some());
        w.write("src/lib.rs", b"pub fn c() {}\n");
        release_fault(&w.ws);
        assert!(wait_fault(&w.ws).is_some());
        w.write("src/lib.rs", b"pub fn dd() {}\n");
        release_fault(&w.ws);
        let r = h.join().unwrap();
        assert!(r.retried && r.unstable, "{r:?}");
        assert_eq!(r.blob.as_str(), blake3_hex(b"pub fn c() {}\n"));
    });

    // A writable mapping hides the gen count: read once, unstable.
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(w.ws.join("src/lib.rs"))
        .unwrap();
    let map = unsafe { memmap2::MmapMut::map_mut(&f) }.unwrap();
    let r = read_candidate(&w.ws, &p).unwrap();
    assert!(!r.retried && r.unstable, "{r:?}");
    assert_eq!(r.blob.as_str(), blake3_hex(b"pub fn dd() {}\n"));
    drop(map);
    drop(f);

    // Executable bit and symlinks.
    let r = read_candidate(&w.ws, &rp("bin/run.sh")).unwrap();
    assert_eq!(r.mode, 0o100755);
    let r = read_candidate(&w.ws, &rp("link")).unwrap();
    assert_eq!(
        (r.mode, r.blob.as_str().to_owned()),
        (0o120000, blake3_hex(b"README.md"))
    );
}

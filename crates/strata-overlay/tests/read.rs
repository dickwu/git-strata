//! Task 3: the race-safe read of one candidate (§5.2).

mod common;

use std::fs::OpenOptions;
use std::thread;

use common::*;
use strata_overlay::{
    DEFAULT_DIRTY_CAPACITY, OverlayFault, OverlayWarning, Tracker, compute, inject_fault,
    read_candidate, release_fault, wait_fault,
};
use strata_types::EntryKind;

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

#[test]
fn a_file_turned_directory_during_the_scan_is_unstable() {
    // The walk sees src/lib.rs as a file; before its read it becomes a
    // directory holding new.txt, which this scan never walks.
    let w = World::new();
    let p = rp("src/lib.rs");
    let mut t = Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY);
    inject_fault(&w.ws, OverlayFault::PauseBeforeRead { path: p.clone() });
    let (ws, pause) = (w.ws.clone(), OverlayFault::PauseBeforeRead { path: p.clone() });
    let s = thread::scope(|s| {
        let h = s.spawn(move || {
            assert_eq!(wait_fault(&ws), Some(pause));
            std::fs::remove_file(ws.join("src/lib.rs")).unwrap();
            put(&ws, "src/lib.rs/new.txt", b"new\n");
            release_fault(&ws);
        });
        let s = compute(&w.ws_ref(), &w.base, &mut t, None).expect("compute");
        h.join().unwrap();
        s
    });
    release_fault(&w.ws);
    let has = |s: &strata_overlay::Scan, path: &str| {
        s.overlay.entries.iter().any(|e| e.path.as_str() == path)
    };
    assert!(!has(&s, "src/lib.rs/new.txt"), "{:?}", s.overlay.entries);
    assert!(
        s.warnings
            .contains(&OverlayWarning::Unstable { path: p.clone() }),
        "{:?}",
        s.warnings
    );

    // The next scan looks again: the child is there and the warning is gone.
    let s = compute(&w.ws_ref(), &w.base, &mut t, Some(&[])).expect("compute");
    let e = s
        .overlay
        .entries
        .iter()
        .find(|e| e.path.as_str() == "src/lib.rs/new.txt")
        .expect("new.txt");
    assert!(matches!(e.kind, EntryKind::Untracked { .. }), "{e:?}");
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
}

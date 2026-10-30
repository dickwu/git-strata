//! rm-followups §1.4 G3: `rebase_preview` classifies like `rebase` and
//! writes nothing; it reads an uncheckpointed overlay's bytes from the
//! workspace and notices files changed since the scan.

mod common;

use common::*;
use strata_overlay::{
    CHANGED_SINCE_SCAN, PreviewBlobs, RebaseOptions, rebase, rebase_preview, reconcile_full,
};
use strata_types::{Code, RepoPath};

fn lines(n: usize, edit: &[(usize, &str)]) -> Vec<u8> {
    let mut out = String::new();
    for i in 1..=n {
        match edit.iter().find(|(k, _)| *k == i) {
            Some((_, s)) => out.push_str(s),
            None => out.push_str(&format!("line {i}")),
        }
        out.push('\n');
    }
    out.into_bytes()
}

fn paths(v: &[&str]) -> Vec<RepoPath> {
    v.iter().map(|s| rp(s)).collect()
}

fn world() -> World {
    World::with(|m| {
        put(m, "m.txt", &lines(9, &[]));
        put(m, "s.txt", &lines(9, &[]));
        put(m, "same.txt", b"a\n");
    })
}

#[test]
fn preview_matches_rebase_and_writes_nothing() {
    let w = world();
    w.write("m.txt", &lines(9, &[(1, "ours")]));
    w.write("s.txt", &lines(9, &[(5, "ours")]));
    w.write("same.txt", b"b\n");
    w.write("new.txt", b"brand new\n");
    let ov = reconcile_full(&w.ws_ref(), &w.base).unwrap();
    let (new, _) = w.next_base(|m| {
        put(m, "m.txt", &lines(9, &[(9, "theirs")]));
        put(m, "s.txt", &lines(9, &[(5, "theirs")]));
        put(m, "same.txt", b"b\n");
    });
    let opts = RebaseOptions::for_move(&new, false).unwrap();
    let before = w.store_files();
    let p = rebase_preview(&ov, &w.base, &new, &PreviewBlobs::new(&w.store, &w.ws, &ov), &opts).unwrap();
    assert_eq!(w.store_files(), before, "preview stored nothing");
    assert_eq!(p.conflicts, paths(&["s.txt"]));
    assert_eq!(p.merged, paths(&["m.txt"]));
    assert_eq!(p.dropped, paths(&["same.txt"]));
    assert!(p.lww.is_empty());

    // The real rebase (after a checkpoint stores the blobs) agrees.
    let c = strata_overlay::checkpoint(
        &w.ws_ref(),
        &w.base,
        None,
        strata_types::CheckpointReason::Manual { message: "m".into() },
    )
    .unwrap();
    let ov2 = strata_overlay::Overlay::from_checkpoint(&c.checkpoint);
    let r = rebase(&ov2, &w.base, &new, &w.store, &opts).unwrap();
    assert_eq!(r.conflicts, p.conflicts);
    assert_eq!(r.dropped, p.dropped);
}

#[test]
fn preview_notices_a_file_changed_since_the_scan() {
    let w = world();
    w.write("m.txt", &lines(9, &[(1, "ours")]));
    let ov = reconcile_full(&w.ws_ref(), &w.base).unwrap();
    let (new, _) = w.next_base(|m| put(m, "m.txt", &lines(9, &[(9, "theirs")])));
    w.write("m.txt", &lines(9, &[(1, "ours again")]));
    let opts = RebaseOptions::for_move(&new, false).unwrap();
    let e = rebase_preview(&ov, &w.base, &new, &PreviewBlobs::new(&w.store, &w.ws, &ov), &opts)
        .unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    assert_eq!(e.detail.unwrap()["reason"], CHANGED_SINCE_SCAN);
}

#[test]
fn preview_of_a_removed_file_is_changed_since_scan() {
    let w = world();
    w.write("m.txt", &lines(9, &[(1, "ours")]));
    let ov = reconcile_full(&w.ws_ref(), &w.base).unwrap();
    let (new, _) = w.next_base(|m| put(m, "m.txt", &lines(9, &[(9, "theirs")])));
    w.remove("m.txt");
    let opts = RebaseOptions::for_move(&new, false).unwrap();
    let e = rebase_preview(&ov, &w.base, &new, &PreviewBlobs::new(&w.store, &w.ws, &ov), &opts)
        .unwrap_err();
    assert_eq!(e.detail.unwrap()["reason"], CHANGED_SINCE_SCAN);
}

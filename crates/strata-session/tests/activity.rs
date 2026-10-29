//! rm-followups §1.4 G2: the whole-root ctime walk and the quiet window.

use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use strata_session::{busy_reason, walk_activity};

#[test]
fn walk_sees_the_newest_entry_without_following_symlinks() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("ws");
    fs::create_dir_all(root.join(".git/objects")).unwrap();
    fs::create_dir_all(root.join("node_modules/x")).unwrap();
    fs::write(root.join("a.txt"), b"a").unwrap();
    symlink("/", root.join("loop")).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(root.join("node_modules/x/new.js"), b"x").unwrap();
    let a = walk_activity(&root).unwrap();
    assert_eq!(a.newest, root.join("node_modules/x/new.js"));
    assert_eq!(a.entries, 8, "{a:?}");
    assert!(a.late.is_none());
    assert!(a.m0_ns <= a.t0_ns);
}

#[test]
fn quiet_window_defers_then_passes() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a.txt"), b"a").unwrap();
    let r = busy_reason(d.path(), Duration::from_secs(3600)).unwrap().expect("deferred");
    assert_eq!(r.reason, "recent_writes");
    assert_eq!(r.detail["quiet_secs"], 3600);
    assert!(busy_reason(d.path(), Duration::ZERO).unwrap().is_none());
}

#[test]
fn walk_error_counts_as_recent_writes() {
    let d = tempfile::tempdir().unwrap();
    let r = busy_reason(&d.path().join("missing"), Duration::ZERO);
    // open_files of a missing directory may itself fail; either way it is not "idle".
    match r {
        Ok(Some(i)) => assert_eq!(i.reason, "recent_writes"),
        Ok(None) => panic!("a missing root is never idle"),
        Err(_) => {}
    }
}

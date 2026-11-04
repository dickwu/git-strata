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

#[test]
fn scoped_deps_writes_are_excluded_but_nested_carried_cache_survives_restart() {
    use strata_session::{baseline_of, changed_since, walk_activity_scoped};
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    fs::create_dir_all(root.join("node_modules/.cache")).unwrap();
    fs::write(root.join("source.rs"), b"source").unwrap();
    fs::write(root.join("node_modules/generated"), b"a").unwrap();
    fs::write(root.join("node_modules/.cache/build"), b"a").unwrap();
    let scope = strata_store::ActivityScope { fold: false, skip: vec!["node_modules".into()], exceptions: vec!["node_modules/.cache".into()] };
    let a = walk_activity_scoped(root, &scope, &[]).unwrap();
    let baseline = baseline_of(&a);
    // Persisted scope, independent of subsequent configuration.
    let baseline = serde_json::from_str(&serde_json::to_string(&baseline).unwrap()).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    fs::write(root.join("node_modules/generated"), b"b").unwrap();
    assert!(changed_since(root, &baseline).unwrap().is_empty());
    let after = walk_activity_scoped(root, &scope, &[]).unwrap();
    assert_eq!(a.m0_counted, after.m0_counted);
    std::thread::sleep(Duration::from_millis(10));
    fs::write(root.join("node_modules/.cache/build"), b"b").unwrap();
    assert!(changed_since(root, &baseline).unwrap().contains(&"node_modules/.cache/build".to_string()));
    assert!(walk_activity_scoped(root, &scope, &[]).unwrap().m0_counted > a.m0_counted);
    fs::remove_dir_all(root.join("node_modules/.cache")).unwrap();
    assert!(changed_since(root, &baseline).unwrap().iter().any(|p| p.contains(".cache")));
}

#[test]
fn quiet_skip_only_changes_age_and_does_not_weaken_during_move_checks() {
    use strata_session::{baseline_of, changed_since, walk_activity_scoped};
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    fs::write(root.join(".eslintcache"), b"a").unwrap();
    fs::write(root.join("source.rs"), b"source").unwrap();
    let scope = strata_store::ActivityScope::default();
    let skip = vec![".eslintcache".into()];
    let a = walk_activity_scoped(root, &scope, &skip).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    fs::write(root.join(".eslintcache"), b"b").unwrap();
    let b = walk_activity_scoped(root, &scope, &skip).unwrap();
    assert_eq!(a.m0_counted, b.m0_counted);
    assert!(b.m0_ns > a.m0_ns);
    assert!(walk_activity_scoped(root, &scope, &[]).unwrap().m0_counted > a.m0_counted);
    assert_eq!(changed_since(root, &baseline_of(&a)).unwrap(), vec![".eslintcache"]);
}

#[test]
fn writes_during_scan_obey_deps_and_quiet_exclusions_but_source_counts() {
    let d=tempfile::tempdir().unwrap(); let root=d.path();
    fs::create_dir_all(root.join("node_modules")).unwrap();
    for p in ["node_modules/cache",".eslintcache","source"] { fs::write(root.join(p),b"a").unwrap(); }
    let scope=strata_store::ActivityScope {fold:false,skip:vec!["node_modules".into()],exceptions:vec![]};
    for (path,quiet,late) in [("node_modules/cache",vec![],false),(".eslintcache",vec![".eslintcache".into()],false),("source",vec![],true)] {
        let a=strata_session::walk_activity_during_scan(root,&scope,&quiet,||{
            std::thread::sleep(Duration::from_millis(20)); fs::write(root.join(path),b"b").unwrap();
        }).unwrap();
        assert_eq!(a.late.is_some(),late,"{path}: {a:?}");
    }
}

#[test]
fn deferral_notes_name_bounded_holders_and_write_age() {
    let holders:Vec<_>=(0..8).map(|pid|serde_json::json!({"pid":pid,"command":"node","hold":"cwd","path":"src"})).collect();
    let detail=serde_json::json!({"holders":holders,"count":8});
    let text=strata_session::rebase_cause("open_files",Some(&detail));
    assert!(text.contains("node[0] cwd src") && text.contains("node[2] cwd src") && text.contains("+5 more"));
    assert!(!text.contains("node[3]"));
    let legacy=serde_json::json!({"holders":[{"pid":9,"command":"zsh","path":"."}]});
    assert!(strata_session::rebase_cause("open_files",Some(&legacy)).contains("zsh[9] hold ."));
    let detail=serde_json::json!({"path":"src/a.rs","age_secs":68});
    assert!(strata_session::rebase_cause("recent_writes",Some(&detail)).contains("src/a.rs 68 s ago"));
    let detail=serde_json::json!({"path":"src/a.rs","during_scan":true});
    assert!(strata_session::rebase_cause("recent_writes",Some(&detail)).contains("written during the scan"));
    assert_eq!(strata_session::local_hhmmss(std::time::SystemTime::UNIX_EPOCH).matches(':').count(),2);
}

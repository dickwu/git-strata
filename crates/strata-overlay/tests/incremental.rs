//! Task 6: the dirty set and incremental scans that equal the cold scan (§5.4, §5.5).

mod common;

use std::ffi::OsStr;
use std::fs::{self, File, FileTimes};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use common::*;
use strata_apfs::WatchEvent;
use strata_overlay::{DEFAULT_DIRTY_CAPACITY, Overlay, Scan, Tracker, compute, reconcile_full};
use strata_types::{EntryKind, RepoPath};

fn tracker(w: &World) -> Tracker {
    Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY)
}

fn cold(w: &World) -> Overlay {
    let o = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    o.validate().expect("validate");
    o
}

/// A scan with the dirty set `dirty` (plus the inbox), checked against the cold scan.
fn scan_with(w: &World, t: &mut Tracker, dirty: Option<&[RepoPath]>) -> Scan {
    let s = compute(&w.ws_ref(), &w.base, t, dirty).expect("compute");
    s.overlay.validate().expect("validate");
    assert_eq!(s.overlay, cold(w), "compute equals the cold scan");
    s
}

/// An incremental call: only what the inbox holds.
fn inc(w: &World, t: &mut Tracker) -> Scan {
    scan_with(w, t, Some(&[]))
}

fn warm(w: &World) -> Tracker {
    let mut t = tracker(w);
    let s = scan_with(w, &mut t, None);
    assert!(s.stats.full);
    assert!(!t.needs_full());
    t
}

fn abs(w: &World, rel: &str) -> PathBuf {
    w.ws.join(rel)
}

fn changed(w: &World, t: &Tracker, rels: &[&str]) {
    t.inbox().note(&WatchEvent::Changed(
        rels.iter().map(|r| abs(w, r)).collect(),
    ));
}

fn kind(o: &Overlay, p: &str) -> Option<&'static str> {
    o.get(&rp(p)).map(|e| match &e.kind {
        EntryKind::Added { .. } => "Added",
        EntryKind::Modified { .. } => "Modified",
        EntryKind::Deleted => "Deleted",
        EntryKind::Renamed { .. } => "Renamed",
        EntryKind::Untracked { .. } => "Untracked",
    })
}

/// Rewrite `rel` with content of the same size and put its times back:
/// size and mtime equal, only the gen count tells.
fn rewrite_same_stat(w: &World, rel: &str, bytes: &[u8]) {
    let p = abs(w, rel);
    let md = fs::metadata(&p).unwrap();
    assert_eq!(md.len(), bytes.len() as u64, "same size");
    let (m, a) = (md.modified().unwrap(), md.accessed().unwrap());
    fs::write(&p, bytes).unwrap();
    File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_times(FileTimes::new().set_modified(m).set_accessed(a))
        .unwrap();
    assert_eq!(fs::metadata(&p).unwrap().modified().unwrap(), m);
}

fn mv(w: &World, a: &str, b: &str) {
    let to = abs(w, b);
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    fs::rename(abs(w, a), to).unwrap_or_else(|err| panic!("mv {a} {b}: {err}"));
}

fn case_insensitive(w: &World) -> bool {
    !strata_apfs::volume_caps(&w.ws).unwrap().case_sensitive
}

#[test]
fn point_query_and_walk_agree() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "build/keep.txt", b"keep\n");
        put(m, "sub/base.txt", b"base\n");
    });
    // Rules from every source: nested .gitignore with negation, dir-only
    // rules, info/exclude and the global excludes file.
    w.write(
        ".gitignore",
        b"*.log\n/node_modules\n/target\nbuild/\n*.tmp\n!keep.tmp\nlogs/\n",
    );
    w.write("sub/.gitignore", b"*.gen\n!important.gen\n");
    let info = w.tmp.join("shadows/app.git/info");
    fs::create_dir_all(&info).unwrap();
    fs::write(info.join("exclude"), b"excl.txt\n").unwrap();
    fs::create_dir_all(w.xdg.join("git")).unwrap();
    fs::write(w.xdg.join("git/ignore"), b"*.bak\n").unwrap();
    let mut t = warm(&w);
    let cases: &[(&str, &str)] = &[
        ("a.log", "a.log"),
        ("X.LOG", "X.LOG"),
        ("keep.tmp", "keep.tmp"),
        ("other.tmp", "other.tmp"),
        ("sub/x.gen", "sub/x.gen"),
        ("sub/important.gen", "sub/important.gen"),
        ("sub/plain.txt", "sub/plain.txt"),
        ("excl.txt", "excl.txt"),
        ("glob.bak", "glob.bak"),
        ("build/new.txt", "build/new.txt"),
        ("build/keep.txt", "build/keep.txt"),
        ("logs", "logs"),
        ("x/logs/a.txt", "x/logs/a.txt"),
        // The dirty path in another spelling than the disk's.
        ("sub/Y.GEN", "Sub/Y.GEN"),
        ("sub/base.txt", "SUB/BASE.TXT"),
    ];
    for (disk, dirty) in cases {
        if fs::symlink_metadata(abs(&w, disk)).is_ok() {
            let mut b = w.read(disk);
            b.extend_from_slice(b"more\n");
            w.write(disk, &b);
        } else {
            w.write(disk, format!("{disk}\n").as_bytes());
        }
        if !case_insensitive(&w) && disk != dirty {
            continue;
        }
        let s = scan_with(&w, &mut t, Some(&[rp(dirty)]));
        assert!(!s.stats.full, "{dirty}: incremental");
    }
    let o = cold(&w);
    assert_eq!(kind(&o, "a.log"), None);
    assert_eq!(kind(&o, "keep.tmp"), Some("Untracked"));
    assert_eq!(kind(&o, "sub/important.gen"), Some("Untracked"));
    assert_eq!(kind(&o, "excl.txt"), None);
    assert_eq!(kind(&o, "glob.bak"), None);
    assert_eq!(kind(&o, "build/new.txt"), None);
    assert_eq!(kind(&o, "build/keep.txt"), Some("Modified"));
    assert_eq!(kind(&o, "logs"), Some("Untracked"));
    assert_eq!(kind(&o, "x/logs/a.txt"), None);
}

#[test]
fn reconcile_after_kernel_dropped_equals_cold_scan() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "src/sub/a.txt", b"aaaa\n");
        put(m, "src/sub/inner/b.txt", b"bbbb\n");
    });
    w.write("bin/run.sh", b"#!/bin/sh\necho changed\n");
    w.write("added.txt", b"new\n");
    let mut t = warm(&w);
    // Round 1: changes nobody reported, then Dropped.
    rewrite_same_stat(&w, "README.md", b"HELLO\n");
    w.write("src/new.rs", b"pub fn n() {}\n");
    mv(&w, "src/new.rs", "src/lib.rs");
    mv(&w, "src/deep/x/y.txt", "src/deep/x/Y.txt");
    w.remove("empty.txt");
    w.write("empty.txt", b"");
    w.remove("link");
    w.write("link/a.txt", b"now a dir\n");
    let mut gi = w.read(".gitignore");
    gi.extend_from_slice(b"*.tmp\n");
    w.write(".gitignore", &gi);
    w.write("z.tmp", b"ignored\n");
    w.write("bin/run.sh", b"#!/bin/sh\necho run\n");
    w.git_ok(&w.ws, &["add", "added.txt"]);
    t.inbox().note(&WatchEvent::Dropped);
    assert!(t.needs_full());
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, "added.txt"), Some("Added"));
    assert_eq!(kind(&s.overlay, "README.md"), Some("Modified"));
    assert_eq!(kind(&s.overlay, "bin/run.sh"), None);
    // Round 2: the same with a rescan of the root.
    rewrite_same_stat(&w, "README.md", b"hello\n");
    w.remove("link");
    put_link(&w.ws, "link", "README.md");
    t.inbox().note(&WatchEvent::MustRescan(w.ws.clone()));
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, "README.md"), None);
    // Round 3: changes only under src/sub, a rescan of that subtree.
    rewrite_same_stat(&w, "src/sub/a.txt", b"AAAA\n");
    w.remove("src/sub/inner/b.txt");
    w.write("src/sub/inner/new.txt", b"new\n");
    t.inbox().note(&WatchEvent::MustRescan(abs(&w, "src/sub")));
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    // src/sub and src/sub/inner walked, plus the two listings (root, src)
    // that resolve the spelling of src/sub.
    assert_eq!(s.stats.dirs_listed, 2 + 2, "{:?}", s.stats);
    assert_eq!(kind(&s.overlay, "src/sub/a.txt"), Some("Modified"));
    assert_eq!(kind(&s.overlay, "src/sub/inner/b.txt"), Some("Deleted"));
    assert_eq!(kind(&s.overlay, "src/sub/inner/new.txt"), Some("Untracked"));
    // Control: the events arrive.
    rewrite_same_stat(&w, "src/lib.rs", b"pub fn z() {}\n");
    w.write("src/other.rs", b"x\n");
    changed(&w, &t, &["src/lib.rs", "src/other.rs"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "src/other.rs"), Some("Untracked"));
}

#[test]
fn dirty_set_dedups_by_canonical_path() {
    let w = World::new();
    let t = warm(&w);
    let inbox = t.inbox();
    for _ in 0..100 {
        changed(&w, &t, &["src/lib.rs"]);
    }
    assert_eq!(inbox.pending().paths, vec![rp("src/lib.rs")]);
    changed(&w, &t, &["./src/lib.rs"]);
    inbox.note(&WatchEvent::Changed(vec![
        w.ws.join(".").join("src/./lib.rs"),
    ]));
    if case_insensitive(&w) {
        changed(&w, &t, &["SRC/lib.rs", "src/LIB.RS"]);
    }
    assert_eq!(inbox.pending().paths.len(), 1, "{:?}", inbox.pending());
    changed(&w, &t, &["src/cafe\u{301}.rs"]);
    changed(&w, &t, &["src/caf\u{e9}.rs"]);
    assert_eq!(inbox.pending().paths.len(), 2, "{:?}", inbox.pending());
    changed(&w, &t, &["README.md", "src/deep"]);
    inbox.note(&WatchEvent::MustRescan(abs(&w, "src/deep")));
    inbox.note(&WatchEvent::MustRescan(abs(&w, "src/deep")));
    let p = inbox.pending();
    assert_eq!(p.paths.len(), 4, "{p:?}");
    assert_eq!(p.subtrees, vec![rp("src/deep")]);
    assert!(!p.full && !t.needs_full());
    // Outside the root: dropped.
    inbox.note(&WatchEvent::Changed(vec![w.tmp.join("main/README.md")]));
    assert_eq!(inbox.pending().paths.len(), 4);
    // A path that is not UTF-8: only a full scan can account for it.
    let bad = w.ws.join(OsStr::from_bytes(b"src/\xff.rs"));
    inbox.note(&WatchEvent::Changed(vec![bad]));
    let p = inbox.pending();
    assert!(
        p.full && p.paths.is_empty() && p.subtrees.is_empty(),
        "{p:?}"
    );
    assert!(t.needs_full());
}

#[test]
fn event_spellings_and_subtrees_reverify() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "dir/x", b"aaaa\n");
        put(m, "src/Foo.ts", b"foo\n");
        put(m, "src/\u{c9}cole.ts", b"ecole\n");
        put(
            m,
            ".gitignore",
            "*.log\n/node_modules\n/target\n\u{e9}coles/\n".as_bytes(),
        );
    });
    let mut t = warm(&w);
    // An event in another case than the disk spelling finds the rewrite.
    rewrite_same_stat(&w, "dir/x", b"AAAA\n");
    if case_insensitive(&w) {
        changed(&w, &t, &["DIR/x"]);
    } else {
        changed(&w, &t, &["dir/x"]);
    }
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "dir/x"), Some("Modified"));
    // An NFD event for the NFC name.
    rewrite_same_stat(&w, "src/\u{c9}cole.ts", b"ECOLE\n");
    changed(&w, &t, &["src/E\u{301}cole.ts"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "src/\u{c9}cole.ts"), Some("Modified"));
    // A directory event judges its whole subtree again.
    rewrite_same_stat(&w, "dir/x", b"aaaa\n");
    w.write("dir/y", b"y\n");
    w.write("dir/deeper/z", b"z\n");
    changed(&w, &t, &["dir"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "dir/x"), None);
    assert_eq!(kind(&s.overlay, "dir/deeper/z"), Some("Untracked"));
    w.remove("dir/deeper");
    w.write("dir/w", b"w\n");
    t.inbox().note(&WatchEvent::MustRescan(abs(&w, "dir")));
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "dir/deeper/z"), None);
    assert_eq!(kind(&s.overlay, "dir/w"), Some("Untracked"));
    // Case renames reported only in the old spelling.
    if case_insensitive(&w) {
        w.write("\u{e9}coles/a.txt", b"ignored\n");
        changed(&w, &t, &["\u{e9}coles/a.txt"]);
        inc(&w, &mut t);
        mv(&w, "src/Foo.ts", "src/foo.ts");
        mv(&w, "src/\u{c9}cole.ts", "src/\u{e9}cole.ts");
        mv(&w, "\u{e9}coles", "\u{c9}coles");
        changed(&w, &t, &["src/Foo.ts", "src/\u{c9}cole.ts", "\u{e9}coles"]);
        let s = inc(&w, &mut t);
        assert!(!s.stats.full);
        assert_eq!(kind(&s.overlay, "src/foo.ts"), Some("Renamed"));
        // git's ignorecase folds ASCII only: `écoles/` no longer matches
        // Écoles, which is whatever the cold scan says (Untracked).
        assert_eq!(kind(&s.overlay, "\u{c9}coles/a.txt"), Some("Untracked"));
    }
}

#[test]
#[ignore = "needs hdiutil (a case-sensitive disk image)"]
fn event_spellings_and_subtrees_reverify_case_sensitive() {
    let img = Image::case_sensitive();
    let w = World::with_in(Some(&img.mount), |m| {
        default_tree(m);
        put(m, "src/Foo.ts", b"foo\n");
    });
    let mut t = warm(&w);
    // On a case-sensitive volume the spellings are different paths.
    mv(&w, "src/Foo.ts", "src/foo.ts");
    w.write("src/FOO.TS", b"other\n");
    changed(&w, &t, &["src/Foo.ts", "src/foo.ts", "src/FOO.TS"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "src/FOO.TS"), Some("Untracked"));
}

#[test]
fn dropped_or_overflowing_events_force_full_scan() {
    let w = World::new();
    let mut t = warm(&w);
    t.inbox().note(&WatchEvent::Dropped);
    assert!(t.needs_full());
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert!(!t.needs_full());
    // Capacity 8: nine paths overflow it.
    let mut t = Tracker::new(&w.ws, 8);
    scan_with(&w, &mut t, None);
    let names: Vec<String> = (0..9).map(|i| format!("n{i}.txt")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    changed(&w, &t, &refs[..8]);
    assert!(!t.needs_full());
    changed(&w, &t, &refs[8..]);
    assert!(t.needs_full());
    assert!(t.inbox().pending().paths.is_empty());
    // The full scan finds a rewrite nobody reported, size and mtime restored.
    rewrite_same_stat(&w, "README.md", b"HELLO\n");
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, "README.md"), Some("Modified"));
    assert!(!t.needs_full());
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
}

#[test]
fn index_changes_reclassify_without_events() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, ".gitignore", b"*.log\n/node_modules\n/target\n.env\n");
    });
    let mut t = warm(&w);
    w.write("new.txt", b"new\n");
    w.write("a.log", b"log\n");
    changed(&w, &t, &["new.txt", "a.log"]);
    let s = inc(&w, &mut t);
    assert_eq!(kind(&s.overlay, "new.txt"), Some("Untracked"));
    assert_eq!(kind(&s.overlay, "a.log"), None);
    w.git_ok(&w.ws, &["add", "new.txt"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "new.txt"), Some("Added"));
    w.git_ok(&w.ws, &["add", "-f", "a.log"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "a.log"), Some("Added"));
    w.git_ok(&w.ws, &["rm", "-q", "--cached", "new.txt"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "new.txt"), Some("Untracked"));
    w.git_ok(&w.ws, &["rm", "-q", "--cached", "a.log"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "a.log"), None);
    // ignorecase changed: full.
    w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    // A seed file enters the index: X changed, full.
    w.write(".env", b"SECRET=1\n");
    w.git_ok(&w.ws, &["add", "-f", ".env"]);
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, ".env"), Some("Added"));
    w.git_ok(&w.ws, &["rm", "-q", "--cached", ".env"]);
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, ".env"), None);
    // ...and stays out of later incremental scans.
    w.write(".env", b"SECRET=2\n");
    changed(&w, &t, &[".env"]);
    assert!(t.inbox().pending().paths.is_empty());
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, ".env"), None);
    let s = scan_with(&w, &mut t, Some(&[rp(".env")]));
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, ".env"), None);
}

#[test]
fn excluded_dirs_never_enter_overlay() {
    let mut w = World::with(|m| {
        default_tree(m);
        put(m, "vendor/lib.go", b"package lib\n");
        put(m, ".gitignore", b"*.log\n/node_modules\n/target\n.env\n");
    });
    let mut t = warm(&w);
    let excluded = [
        "node_modules/a/b.js",
        "vendor/new.go",
        "src-tauri/target/x.o",
        ".next/cache/c",
        ".env",
    ];
    for round in 0..3 {
        for p in excluded {
            match round {
                0 => w.write(p, b"one\n"),
                1 => w.write(p, b"two\n"),
                _ => w.remove(p),
            }
        }
        changed(&w, &t, &excluded);
        for d in ["node_modules", "vendor", "src-tauri/target", ".next/cache"] {
            t.inbox().note(&WatchEvent::MustRescan(abs(&w, d)));
        }
        assert_eq!(t.inbox().pending(), Default::default(), "round {round}");
        let s = inc(&w, &mut t);
        assert!(s.overlay.is_empty(), "round {round}: {:?}", s.overlay);
    }
    // A tracked vendor/ replaced by a clone: no Deleted.
    w.remove("vendor");
    w.write("vendor/other.go", b"package other\n");
    changed(&w, &t, &["vendor", "vendor/lib.go"]);
    assert!(inc(&w, &mut t).overlay.is_empty());
    // .env no longer ignored: still excluded.
    w.write(".env", b"SECRET=1\n");
    w.write(".gitignore", b"*.log\n/node_modules\n/target\n");
    changed(&w, &t, &[".gitignore", ".env"]);
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, ".env"), None);
    assert_eq!(kind(&s.overlay, ".gitignore"), Some("Modified"));
    // Other spellings the volume folds together are filtered too.
    if case_insensitive(&w) {
        changed(&w, &t, &["NODE_MODULES/x", "Vendor/y"]);
        assert!(t.inbox().pending().paths.is_empty());
        w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
        inc(&w, &mut t);
        w.write(".ENV", b"SECRET=3\n");
        changed(&w, &t, &[".ENV"]);
        assert!(t.inbox().pending().paths.is_empty());
        let s = inc(&w, &mut t);
        assert_eq!(kind(&s.overlay, ".ENV"), None);
        assert_eq!(kind(&s.overlay, ".env"), None);
    }
    // A new deps dir: full scan, its files leave the overlay.
    w.write("gen/out.js", b"out\n");
    changed(&w, &t, &["gen/out.js"]);
    assert_eq!(
        kind(&inc(&w, &mut t).overlay, "gen/out.js"),
        Some("Untracked")
    );
    w.config.deps_dirs.push(rp("gen"));
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, "gen/out.js"), None);
    changed(&w, &t, &["gen/out.js"]);
    assert!(t.inbox().pending().paths.is_empty());
    // Removed again: full scan, they come back, later events are incremental.
    w.config.deps_dirs.retain(|d| d.as_str() != "gen");
    let s = inc(&w, &mut t);
    assert!(s.stats.full);
    assert_eq!(kind(&s.overlay, "gen/out.js"), Some("Untracked"));
    w.write("gen/out.js", b"out2\n");
    changed(&w, &t, &["gen/out.js"]);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "gen/out.js"), Some("Untracked"));
}

#[test]
fn excluded_dirs_never_enter_overlay_real_watch() {
    // Creating an entry directly in the root reports the root itself (a
    // whole-tree signal, so a full scan), and FSEvents also delivers writes
    // made just before the stream started: the excluded paths exist first,
    // and the events of the setup are drained by one scan.
    let w = World::new();
    w.write("node_modules/a/old.js", b"x\n");
    w.write(".env", b"x\n");
    let mut t = warm(&w);
    t.watch().expect("watch");
    std::thread::sleep(std::time::Duration::from_secs(2));
    inc(&w, &mut t);
    w.write("node_modules/a/b.js", b"x\n");
    w.write(".env", b"y\n");
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(t.inbox().pending(), Default::default());
    w.write("src/new.rs", b"x\n");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while t.inbox().pending().paths.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "event never arrived: {:?}",
            t.inbox().pending()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!t.inbox().pending().full);
    let s = inc(&w, &mut t);
    assert!(!s.stats.full);
    assert_eq!(kind(&s.overlay, "src/new.rs"), Some("Untracked"));
    assert_eq!(kind(&s.overlay, "node_modules/a/b.js"), None);
    assert_eq!(kind(&s.overlay, ".env"), None);
}

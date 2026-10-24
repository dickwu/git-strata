//! Task 7: checkpoints that store only what changed (§5.6, §5.2 steps 5–6).

mod common;

use std::collections::BTreeSet;
use std::fs::{self, File, FileTimes};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use common::*;
use strata_apfs::WatchEvent;
use strata_overlay::{
    Checkpointed, DEFAULT_DIRTY_CAPACITY, Overlay, OverlayFault, OverlayWarning, Tracker,
    checkpoint, compute, inject_fault, reconcile_full, release_fault, wait_fault,
};
use strata_store::Seam;
use strata_types::{BlobId, CheckpointReason, Code, EntryKind, OverlayEntry};

// ---------------------------------------------------------------- helpers

fn tracker(w: &World) -> Tracker {
    Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY)
}

fn cold(w: &World) -> Overlay {
    let o = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    o.validate().expect("validate");
    o
}

fn tool(id: &str) -> CheckpointReason {
    CheckpointReason::ToolCall { id: id.to_owned() }
}

fn manual() -> CheckpointReason {
    CheckpointReason::Manual {
        message: "m".to_owned(),
    }
}

fn try_cp(
    w: &World,
    t: Option<&mut Tracker>,
    r: CheckpointReason,
) -> Result<Checkpointed, strata_types::StrataError> {
    checkpoint(&w.ws_ref(), &w.base, t, r)
}

/// A checkpoint that must succeed; its entries must equal the cold scan.
fn cp(w: &World, t: Option<&mut Tracker>, r: CheckpointReason) -> Checkpointed {
    let c = try_cp(w, t, r).expect("checkpoint");
    let o = Overlay::from_checkpoint(&c.checkpoint);
    o.validate().expect("validate");
    assert_eq!(o, cold(w), "the checkpoint equals the cold scan");
    c
}

/// A warm tracker whose state is recorded as the latest checkpoint.
fn warm(w: &World) -> Tracker {
    let mut t = tracker(w);
    cp(w, Some(&mut t), manual());
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

fn entry<'a>(c: &'a Checkpointed, p: &str) -> Option<&'a OverlayEntry> {
    c.checkpoint.entries.iter().find(|e| e.path.as_str() == p)
}

fn kind(c: &Checkpointed, p: &str) -> Option<&'static str> {
    entry(c, p).map(|e| match &e.kind {
        EntryKind::Added { .. } => "Added",
        EntryKind::Modified { .. } => "Modified",
        EntryKind::Deleted => "Deleted",
        EntryKind::Renamed { .. } => "Renamed",
        EntryKind::Untracked { .. } => "Untracked",
    })
}

fn blob(c: &Checkpointed, p: &str) -> Option<BlobId> {
    entry(c, p).and_then(|e| match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Renamed { blob, .. }
        | EntryKind::Untracked { blob } => Some(blob.clone()),
        EntryKind::Deleted => None,
    })
}

fn bytes_of(w: &World, id: &BlobId) -> Vec<u8> {
    w.store.read_blob(id, 1 << 30).expect("read_blob")
}

fn rows(w: &World) -> usize {
    w.store.checkpoints(&w.sid, Some(&w.repo)).unwrap().len()
}

fn latest_id(w: &World) -> Option<u64> {
    w.store
        .latest_checkpoint(&w.sid, &w.repo)
        .unwrap()
        .map(|c| c.id.get())
}

fn blob_files(w: &World) -> BTreeSet<PathBuf> {
    w.store_files()
        .into_iter()
        .filter(|p| p.components().any(|c| c.as_os_str() == "blobs"))
        .collect()
}

fn tmp_files(w: &World) -> Vec<PathBuf> {
    w.store_files()
        .into_iter()
        .filter(|p| p.starts_with(w.root.join("store/tmp")))
        .collect()
}

fn rewrite_same_stat(w: &World, rel: &str, bytes: &[u8]) {
    let p = abs(w, rel);
    let md = fs::metadata(&p).unwrap();
    assert_eq!(md.len(), bytes.len() as u64, "same size");
    let (m, a) = (md.modified().unwrap(), md.accessed().unwrap());
    fs::write(&p, bytes).unwrap();
    set_times(&p, m, a);
}

fn set_times(p: &Path, m: SystemTime, a: SystemTime) {
    File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_times(FileTimes::new().set_modified(m).set_accessed(a))
        .unwrap();
}

/// Every file of the tree under `dir`: (path, bytes, mtime, inode, mode).
/// An unreadable file counts by its metadata alone.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, i64, u64, u32)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let md = fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                stack.push(p);
            } else if md.is_file() {
                let b = fs::read(&p).unwrap_or_default();
                out.push((
                    p,
                    b,
                    md.mtime_nsec() + md.mtime() * 1_000_000_000,
                    md.ino(),
                    md.mode(),
                ));
            } else {
                let b = fs::read_link(&p)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes();
                out.push((p, b, md.mtime(), md.ino(), md.mode()));
            }
        }
    }
    out.sort();
    out
}

/// A child process (this test binary, running `lock_child`) holding the
/// store's write lock (`BEGIN IMMEDIATE` on a raw connection) until `unlock`.
struct Locker(Child);

impl Locker {
    fn lock(w: &World) -> Locker {
        let mut c = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_child", "--nocapture", "--test-threads=1"])
            .env("STRATA_LOCK_DB", w.root.join("store/store.db"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn lock child");
        let mut out = BufReader::new(c.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(out.read_line(&mut line).unwrap() > 0, "lock child ended");
            if line.trim() == "LOCKED" {
                break;
            }
        }
        // Keep draining so the child never blocks on a full pipe.
        thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        Locker(c)
    }

    fn unlock(mut self) {
        let mut i = self.0.stdin.take().unwrap();
        i.write_all(b"go\n").unwrap();
        drop(i);
        assert!(self.0.wait().unwrap().success());
    }
}

/// Child side of [`Locker`]; a no-op in a normal run.
#[test]
fn lock_child() {
    let Some(db) = std::env::var_os("STRATA_LOCK_DB") else {
        return;
    };
    let c = strata_store::sql::open(Path::new(&db), strata_store::sql::Mode::Shared).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    println!("\nLOCKED");
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).unwrap();
    c.execute_batch("ROLLBACK").unwrap();
}

// ------------------------------------------------------------------ tests

#[test]
fn new_then_deleted_path_leaves_no_entry() {
    let w = World::with(default_tree);
    let mut t = warm(&w);
    let blobs = blob_files(&w);
    let top = latest_id(&w);

    // Files, a symlink, a directory tree: created, then gone.
    w.write("new.txt", b"new\n");
    put_link(&w.ws, "newlink", "README.md");
    w.write("tmpd/a/b.txt", b"b\n");
    w.remove("new.txt");
    w.remove("newlink");
    w.remove("tmpd");
    // An unlinked file still held open and written.
    w.write("held.txt", b"held\n");
    let mut f = File::options()
        .append(true)
        .open(abs(&w, "held.txt"))
        .unwrap();
    fs::remove_file(abs(&w, "held.txt")).unwrap();
    f.write_all(b"more\n").unwrap();
    changed(&w, &t, &["new.txt", "newlink", "tmpd", "held.txt", "never"]);
    let c = cp(&w, Some(&mut t), manual());
    assert!(c.checkpoint.entries.is_empty());
    assert!(!c.recorded, "nothing changed against the latest");
    assert_eq!(latest_id(&w), top);
    let c = cp(&w, Some(&mut t), tool("c1"));
    assert!(c.checkpoint.entries.is_empty());
    assert!(c.recorded && c.changed.is_empty(), "a new tool call id records the unchanged state (review M8)");
    assert_eq!(blob_files(&w), blobs, "nothing was stored");
    drop(f);

    // Among real changes only the real one is an entry.
    w.write("README.md", b"changed\n");
    w.write("gone.txt", b"x\n");
    w.remove("gone.txt");
    changed(&w, &t, &["README.md", "gone.txt"]);
    let c = cp(&w, Some(&mut t), tool("c2"));
    assert!(c.recorded);
    let paths: Vec<_> = c
        .checkpoint
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(paths, ["README.md"]);

    // Back to the base content: the overlay is empty again.
    w.write("README.md", b"hello\n");
    w.write("z.txt", b"z\n");
    w.remove("z.txt");
    changed(&w, &t, &["README.md", "z.txt"]);
    let c = cp(&w, Some(&mut t), tool("c3"));
    assert!(c.recorded);
    assert!(c.checkpoint.entries.is_empty());
}

#[test]
fn mtime_only_change_is_no_change() {
    let w = World::with(default_tree);
    let mut t = warm(&w);
    let top = latest_id(&w);
    let p = abs(&w, "README.md");
    let md = fs::metadata(&p).unwrap();
    let (m, a) = (md.modified().unwrap(), md.accessed().unwrap());
    for when in [SystemTime::now(), common::t(0), common::t(10_000_000), m] {
        set_times(&p, when, a);
        changed(&w, &t, &["README.md"]);
        let c = cp(&w, Some(&mut t), manual());
        assert!(!c.recorded);
        assert!(c.checkpoint.entries.is_empty());
        let s = compute(&w.ws_ref(), &w.base, &mut t, None).unwrap();
        assert!(s.overlay.is_empty());
    }
    let c = cp(&w, Some(&mut t), manual());
    assert!(!c.recorded);
    assert_eq!(latest_id(&w), top);

    // A real change is recorded; the record carries no mtime.
    w.write("README.md", b"other\n");
    changed(&w, &t, &["README.md"]);
    let c = cp(&w, Some(&mut t), tool("real"));
    assert!(c.recorded);
    let text = serde_json::to_string(&c.checkpoint.entries).unwrap();
    assert!(!text.contains("mtime"), "{text}");
}

#[test]
fn rename_records_renamed_entry() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "a.ts", b"export const a = 1;\n");
        put(m, "dup1.txt", b"same\n");
        put(m, "dup2.txt", b"same\n");
        put(m, "mod.txt", b"to be moved\n");
    });
    let mut t = warm(&w);
    let before = blob_files(&w);
    fs::rename(abs(&w, "a.ts"), abs(&w, "b.ts")).unwrap();
    changed(&w, &t, &["a.ts", "b.ts"]);
    let c = cp(&w, Some(&mut t), tool("mv"));
    assert_eq!(kind(&c, "b.ts"), Some("Renamed"));
    match &entry(&c, "b.ts").unwrap().kind {
        EntryKind::Renamed { from, blob } => {
            assert_eq!(from.as_str(), "a.ts");
            assert_eq!(bytes_of(&w, blob), b"export const a = 1;\n");
        }
        k => panic!("{k:?}"),
    }
    assert_eq!(kind(&c, "a.ts"), None, "no Deleted entry for the source");
    assert!(Overlay::from_checkpoint(&c.checkpoint).hides(&rp("a.ts")));
    assert_eq!(
        blob_files(&w).len(),
        before.len() + 1,
        "the moved content is stored once"
    );

    // The next checkpoint stores only the new change.
    let before = blob_files(&w);
    w.write("c.txt", b"c\n");
    changed(&w, &t, &["c.txt"]);
    let c = cp(&w, Some(&mut t), tool("c"));
    assert_eq!(kind(&c, "b.ts"), Some("Renamed"));
    assert_eq!(blob_files(&w).len(), before.len() + 1);

    // Two sources with the same content: a deterministic pairing.
    fs::rename(abs(&w, "dup1.txt"), abs(&w, "r1.txt")).unwrap();
    fs::rename(abs(&w, "dup2.txt"), abs(&w, "r2.txt")).unwrap();
    changed(&w, &t, &["dup1.txt", "dup2.txt", "r1.txt", "r2.txt"]);
    let c1 = cp(&w, Some(&mut t), tool("d"));
    let c2 = cp(&w, None, manual());
    assert_eq!(c1.checkpoint.entries, c2.checkpoint.entries);

    // Renamed, then changed: Deleted plus Untracked.
    fs::rename(abs(&w, "mod.txt"), abs(&w, "moved.txt")).unwrap();
    w.write("moved.txt", b"moved and changed\n");
    changed(&w, &t, &["mod.txt", "moved.txt"]);
    let c = cp(&w, Some(&mut t), tool("e"));
    assert_eq!(kind(&c, "mod.txt"), Some("Deleted"));
    assert_eq!(kind(&c, "moved.txt"), Some("Untracked"));
}

#[test]
fn every_change_kind_reaches_the_next_checkpoint() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "mv/a.txt", b"to move\n");
    });
    let mut t = warm(&w);
    type Change = Box<dyn Fn(&World)>;
    let cases: Vec<(&str, Vec<&str>, Change, &str, &str)> = vec![
        (
            "modify",
            vec!["README.md"],
            Box::new(|w| w.write("README.md", b"modified\n")),
            "README.md",
            "Modified",
        ),
        (
            "truncate",
            vec!["src/lib.rs"],
            Box::new(|w| w.write("src/lib.rs", b"pub")),
            "src/lib.rs",
            "Modified",
        ),
        (
            "extend",
            vec!["src/deep/x/y.txt"],
            Box::new(|w| {
                let mut f = File::options()
                    .append(true)
                    .open(abs(w, "src/deep/x/y.txt"))
                    .unwrap();
                f.write_all(b"more\n").unwrap();
            }),
            "src/deep/x/y.txt",
            "Modified",
        ),
        (
            "chmod",
            vec!["bin/run.sh"],
            Box::new(|w| {
                fs::set_permissions(abs(w, "bin/run.sh"), fs::Permissions::from_mode(0o644))
                    .unwrap();
            }),
            "bin/run.sh",
            "Modified",
        ),
        (
            "create",
            vec!["new.txt"],
            Box::new(|w| w.write("new.txt", b"new\n")),
            "new.txt",
            "Untracked",
        ),
        (
            "delete",
            vec!["empty.txt"],
            Box::new(|w| w.remove("empty.txt")),
            "empty.txt",
            "Deleted",
        ),
        (
            "rename",
            vec!["mv/a.txt", "mv/b.txt"],
            Box::new(|w| {
                fs::rename(abs(w, "mv/a.txt"), abs(w, "mv/b.txt")).unwrap();
            }),
            "mv/b.txt",
            "Renamed",
        ),
    ];
    for (name, paths, change, at, want) in cases {
        let top = latest_id(&w).unwrap();
        change(&w);
        changed(&w, &t, &paths);
        let c = cp(&w, Some(&mut t), tool(name));
        assert!(c.recorded, "{name}");
        assert!(c.checkpoint.id.get() > top, "{name}");
        assert_eq!(
            kind(&c, at),
            Some(want),
            "{name}: {:?}",
            c.checkpoint.entries
        );
        assert!(
            c.changed.iter().any(|p| p.as_str() == at),
            "{name}: {:?}",
            c.changed
        );
        assert!(!c.stats.full, "{name}: incremental");
    }
}

#[test]
fn periodic_full_scan_repairs_missed_changes() {
    let w = World::with(default_tree);
    w.write("x.bak", b"backup\n");
    w.write("src/deep/x/y.txt", b"not base\n");
    let mut t = warm(&w);
    // No events for any of these.
    rewrite_same_stat(&w, "README.md", b"HELLO\n");
    w.write("new.txt", b"new\n");
    w.remove("src/lib.rs");
    fs::create_dir_all(w.xdg.join("git")).unwrap();
    fs::write(w.xdg.join("git/ignore"), b"*.bak\n").unwrap();
    w.write("src/deep/x/y.txt", b"deep\n");

    let s = compute(&w.ws_ref(), &w.base, &mut t, None).unwrap();
    assert_eq!(s.overlay, cold(&w));
    let k = |p: &str| s.overlay.get(&rp(p)).map(|e| e.kind.clone());
    assert!(matches!(k("README.md"), Some(EntryKind::Modified { .. })));
    assert!(matches!(k("new.txt"), Some(EntryKind::Untracked { .. })));
    assert_eq!(k("src/lib.rs"), Some(EntryKind::Deleted));
    assert_eq!(k("x.bak"), None, "now ignored");
    assert_eq!(k("src/deep/x/y.txt"), None, "back to base");

    let c = cp(&w, Some(&mut t), manual());
    assert!(c.recorded);
    for p in ["README.md", "new.txt", "src/lib.rs"] {
        assert!(entry(&c, p).is_some(), "{p}");
    }
    let c = cp(&w, Some(&mut t), manual());
    assert!(!c.recorded);
    assert_eq!(c.stats.files_hashed, 0);
}

#[test]
fn quiet_workspace_stays_clean() {
    let w = World::with(default_tree);
    w.write("README.md", b"edited\n");
    let mut t = warm(&w);
    t.watch().expect("watch");
    // Let the setup's events arrive and be consumed.
    thread::sleep(Duration::from_secs(2));
    t.flush();
    let _ = compute(&w.ws_ref(), &w.base, &mut t, Some(&[])).unwrap();
    for i in 0..3 {
        let _ = fs::read(abs(&w, "README.md")).unwrap();
        let _ = fs::read(abs(&w, "src/lib.rs")).unwrap();
        thread::sleep(Duration::from_millis(700));
        t.flush();
        let v = t.inbox().pending();
        assert!(
            v.paths.is_empty() && v.subtrees.is_empty() && !v.full,
            "{:?}",
            v.paths
        );
        let c = cp(&w, Some(&mut t), tool("q"));
        // Review M8: the first `q` records the unchanged state; repeats are
        // the store's dedup.
        assert_eq!(c.recorded, i == 0);
        assert_eq!(c.stats.files_hashed, 0);
        assert!(!c.stats.full);
    }
}

#[test]
fn failed_checkpoint_keeps_dirty_set() {
    let w = World::with(default_tree);
    let mut t = warm(&w);
    let top = latest_id(&w);
    let rows0 = rows(&w);
    w.write("README.md", b"one\n");
    w.write("src/lib.rs", b"two\n");
    w.write("new.txt", b"three\n");
    changed(&w, &t, &["README.md", "src/lib.rs", "new.txt"]);
    w.store.arm(Seam::AfterCheckpointRow);
    let err = try_cp(&w, Some(&mut t), tool("f")).expect_err("the row write fails");
    assert_ne!(err.code, Code::InvalidInput);
    assert_eq!(latest_id(&w), top);
    assert_eq!(rows(&w), rows0);
    let v = t.inbox().pending();
    for p in ["README.md", "src/lib.rs", "new.txt"] {
        assert!(
            v.paths.iter().any(|q| q.as_str() == p) || v.full,
            "{p} kept"
        );
    }
    let c = cp(&w, Some(&mut t), tool("f"));
    assert!(c.recorded);
    for p in ["README.md", "src/lib.rs", "new.txt"] {
        assert!(entry(&c, p).is_some(), "{p}");
    }

    // A file that cannot be read fails the scan; nothing is recorded.
    let top = latest_id(&w);
    w.write("README.md", b"four\n");
    w.write("src/lib.rs", b"five\n");
    fs::set_permissions(abs(&w, "src/lib.rs"), fs::Permissions::from_mode(0o000)).unwrap();
    changed(&w, &t, &["README.md", "src/lib.rs"]);
    let r = try_cp(&w, Some(&mut t), tool("g"));
    fs::set_permissions(abs(&w, "src/lib.rs"), fs::Permissions::from_mode(0o644)).unwrap();
    r.expect_err("unreadable file");
    assert_eq!(latest_id(&w), top);
    let v = t.inbox().pending();
    for p in ["README.md", "src/lib.rs"] {
        assert!(
            v.paths.iter().any(|q| q.as_str() == p) || v.full,
            "{p} kept"
        );
    }
    let c = cp(&w, Some(&mut t), tool("g"));
    assert!(c.recorded);
    assert_eq!(bytes_of(&w, &blob(&c, "README.md").unwrap()), b"four\n");
    assert_eq!(bytes_of(&w, &blob(&c, "src/lib.rs").unwrap()), b"five\n");
}

#[test]
fn writes_during_scan_reach_the_next_scan() {
    let w = World::with(default_tree);
    let mut t = warm(&w);
    let ws = w.ws.clone();
    let names: Vec<String> = (0..50).map(|i| format!("many/f{i:02}.txt")).collect();

    // 50 files written while a checkpoint is paused after its snapshot.
    inject_fault(&ws, OverlayFault::PauseAfterSnapshot);
    let inbox = t.inbox();
    let r = thread::scope(|s| {
        let h = s.spawn(|| {
            assert_eq!(wait_fault(&ws), Some(OverlayFault::PauseAfterSnapshot));
            for n in &names {
                put(&ws, n, n.as_bytes());
            }
            inbox.note(&WatchEvent::Changed(
                names.iter().map(|n| ws.join(n)).collect(),
            ));
            release_fault(&ws);
        });
        let r = try_cp(&w, Some(&mut t), tool("during"));
        h.join().unwrap();
        r
    });
    r.expect("checkpoint");
    let c = cp(&w, Some(&mut t), tool("after"));
    for n in &names {
        assert!(entry(&c, n).is_some(), "{n}");
    }

    // An unchanged checkpoint racing a write: the write reaches the next one.
    inject_fault(&ws, OverlayFault::PauseAfterSnapshot);
    let inbox = t.inbox();
    let r = thread::scope(|s| {
        let h = s.spawn(|| {
            wait_fault(&ws).unwrap();
            put(&ws, "late.txt", b"late\n");
            inbox.note(&WatchEvent::Changed(vec![ws.join("late.txt")]));
            release_fault(&ws);
        });
        let r = try_cp(&w, Some(&mut t), tool("quiet"));
        h.join().unwrap();
        r
    });
    r.expect("checkpoint");
    let c = cp(&w, Some(&mut t), tool("next"));
    assert_eq!(kind(&c, "late.txt"), Some("Untracked"));

    // A file rewritten after every hash: Unstable now, settled next time.
    let path = rp("README.md");
    w.write("README.md", b"first edit\n");
    inject_fault(&ws, OverlayFault::PauseAfterHash { path: path.clone() });
    changed(&w, &t, &["README.md"]);
    let s = thread::scope(|s| {
        let h = s.spawn(|| {
            for i in 0..2 {
                wait_fault(&ws).unwrap();
                if i == 0 {
                    inject_fault(&ws, OverlayFault::PauseAfterHash { path: path.clone() });
                }
                put(&ws, "README.md", format!("rewrite {i}\n").as_bytes());
                release_fault(&ws);
            }
        });
        let s = compute(&w.ws_ref(), &w.base, &mut t, Some(&[])).expect("compute");
        h.join().unwrap();
        s
    });
    assert!(
        s.warnings.contains(&OverlayWarning::Unstable {
            path: rp("README.md")
        }),
        "{:?}",
        s.warnings
    );
    release_fault(&ws);
    let c = cp(&w, Some(&mut t), tool("settled"));
    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    assert_eq!(
        bytes_of(&w, &blob(&c, "README.md").unwrap()),
        b"rewrite 1\n"
    );
}

/// A world whose base holds `bulk` files in `bulk/dNNN/` plus 40 in `hot/`.
fn sized(bulk: usize) -> World {
    World::with(|m| {
        default_tree(m);
        for i in 0..bulk {
            put(
                m,
                &format!("bulk/d{:03}/f{i:05}.txt", i / 100),
                format!("{i}\n").as_bytes(),
            );
        }
        for i in 0..40 {
            put(
                m,
                &format!("hot/h{i:02}.txt"),
                format!("hot {i}\n").as_bytes(),
            );
        }
    })
}

#[test]
fn work_grows_with_the_change_not_the_base() {
    // Linear in the change: 40, 160, 640 and 1600 files.
    let w = sized(2000);
    let mut t = warm(&w);
    let mut round = 0;
    for n in [40usize, 160, 640, 1600] {
        round += 1;
        let paths: Vec<String> = (0..n)
            .map(|i| format!("bulk/d{:03}/f{i:05}.txt", i / 100))
            .collect();
        for p in &paths {
            w.write(p, format!("round {round}\n").as_bytes());
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        changed(&w, &t, &refs);
        let c = cp(&w, Some(&mut t), tool(&format!("n{n}")));
        assert!(!c.stats.full);
        assert_eq!(c.stats.files_hashed, n as u64, "n = {n}");
        let dirs = n.div_ceil(100) as u64;
        assert!(c.stats.dirs_listed <= dirs + 3, "n = {n}: {:?}", c.stats);
        assert!(
            c.stats.entries_seen <= 101 * dirs + 64,
            "n = {n}: {:?}",
            c.stats
        );
    }

    // The same 40 changes cost the same on a base 10x bigger.
    let stats_for = |bulk: usize| {
        let w = sized(bulk);
        let mut t = warm(&w);
        let paths: Vec<String> = (0..40).map(|i| format!("hot/h{i:02}.txt")).collect();
        for p in &paths {
            w.write(p, b"changed\n");
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        changed(&w, &t, &refs);
        let c = cp(&w, Some(&mut t), tool("hot"));
        let s = c.stats;
        (s.files_hashed, s.dirs_listed, s.entries_seen, s.full)
    };
    let small = stats_for(1000);
    let big = stats_for(10_000);
    assert_eq!(small, big);
    assert_eq!(small.0, 40);

    // A full scan lists every directory except ignored and excluded ones.
    let w = sized(300);
    w.write("node_modules/pkg/a/b.js", b"x\n");
    w.write("target/debug/x.o", b"x\n");
    let mut dirs = 0u64;
    let mut stack = vec![w.ws.clone()];
    while let Some(d) = stack.pop() {
        dirs += 1;
        for e in fs::read_dir(&d).unwrap().flatten() {
            let name = e.file_name();
            if e.file_type().unwrap().is_dir()
                && name != ".git"
                && !(d == w.ws && (name == "node_modules" || name == "target"))
            {
                stack.push(e.path());
            }
        }
    }
    let c = cp(&w, None, manual());
    assert!(c.stats.full);
    assert_eq!(c.stats.dirs_listed, dirs, "{:?}", c.stats);
}

#[test]
fn mmap_writes_count_once_written_back() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "data.bin", &[0u8; 4096]);
    });
    let mut t = warm(&w);
    let f = File::options()
        .read(true)
        .write(true)
        .open(abs(&w, "data.bin"))
        .unwrap();
    // SAFETY: the test owns the file; nothing else resizes it.
    let mut map = unsafe { memmap2::MmapMut::map_mut(&f).unwrap() };
    map[..5].copy_from_slice(b"hello");
    changed(&w, &t, &["data.bin"]);
    let c = cp(&w, Some(&mut t), tool("mapped"));
    // With a writable mapping the gen count is unknown: never vouched.
    assert!(
        c.warnings.contains(&OverlayWarning::Unstable {
            path: rp("data.bin")
        }),
        "{:?}",
        c.warnings
    );
    map[5..10].copy_from_slice(b"world");
    map.flush().unwrap();
    changed(&w, &t, &["data.bin"]);
    let c = cp(&w, Some(&mut t), tool("synced"));
    assert_eq!(
        &bytes_of(&w, &blob(&c, "data.bin").unwrap())[..10],
        b"helloworld"
    );
    drop(map);
    drop(f);
    // Unmapped: the next scan reads it once, then the cache vouches.
    let _ = compute(&w.ws_ref(), &w.base, &mut t, None).unwrap();
    let s = compute(&w.ws_ref(), &w.base, &mut t, None).unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert_eq!(s.stats.files_hashed, 0, "{:?}", s.stats);
}

#[test]
fn unchanged_checkpoint_is_up_to_date() {
    let w = World::with(default_tree);
    let mut t = tracker(&w);
    let first = cp(&w, Some(&mut t), manual());
    assert!(first.recorded, "no latest yet");
    let rows0 = rows(&w);
    let files = w.store_files().len();
    let c = cp(&w, Some(&mut t), manual());
    assert!(!c.recorded);
    assert_eq!(c.checkpoint.id, first.checkpoint.id);
    assert_eq!(c.stats.files_hashed, 0);
    assert!(c.changed.is_empty());
    assert_eq!(rows(&w), rows0);
    assert_eq!(w.store_files().len(), files);
    // Review M8: a new ToolCall id records a row even when nothing changed
    // (no new blob); a repeated id returns that row.
    let t1 = cp(&w, Some(&mut t), tool("t1"));
    assert!(t1.recorded && t1.changed.is_empty());
    assert_eq!(t1.checkpoint.entries, first.checkpoint.entries);
    assert_eq!(rows(&w), rows0 + 1);
    assert_eq!(w.store_files().len(), files);
    let rows0 = rows0 + 1;

    w.write("a.txt", b"a\n");
    changed(&w, &t, &["a.txt"]);
    let c2 = cp(&w, Some(&mut t), tool("t2"));
    assert!(c2.recorded);
    assert_eq!(rows(&w), rows0 + 1);
    assert_eq!(c2.changed, vec![rp("a.txt")]);
    let c = cp(&w, Some(&mut t), tool("t3"));
    assert!(c.recorded && c.changed.is_empty());
    assert_eq!(rows(&w), rows0 + 2);
    // The M8 sequence: replaying the no-op t1 after t2's change still
    // returns t1's own row.
    let again = try_cp(&w, Some(&mut t), tool("t1")).expect("checkpoint");
    assert!(!again.recorded);
    assert_eq!(again.checkpoint.id, t1.checkpoint.id);
    assert_eq!(rows(&w), rows0 + 2);

    // Lifecycle reasons always record.
    for r in [
        CheckpointReason::BeforePublish,
        CheckpointReason::BeforeRestore,
        CheckpointReason::BeforeRollout,
        CheckpointReason::BeforeRemove,
    ] {
        let n = rows(&w);
        let c = cp(&w, Some(&mut t), r.clone());
        assert!(c.recorded, "{r:?}");
        assert_eq!(rows(&w), n + 1);
    }

    // A tool call id seen before returns its checkpoint.
    w.write("b.txt", b"b\n");
    changed(&w, &t, &["b.txt"]);
    let n = rows(&w);
    let again = try_cp(&w, Some(&mut t), tool("t2")).expect("checkpoint");
    assert!(!again.recorded);
    assert_eq!(again.checkpoint.id, c2.checkpoint.id);
    assert_eq!(rows(&w), n);
}

#[test]
fn checkpoint_never_touches_base_or_leaves_residue() {
    let w = World::with(default_tree);
    let mut t = warm(&w);
    let base_dir = w.base.dir.clone();
    let base0 = snapshot(&base_dir);
    let ws0 = |w: &World| {
        snapshot(&w.ws)
            .into_iter()
            .filter(|(p, ..)| !p.components().any(|c| c.as_os_str() == ".git"))
            .map(|(p, b, ..)| (p, b))
            .collect::<Vec<_>>()
    };

    let blobs = blob_files(&w);
    w.write("README.md", b"changed\n");
    changed(&w, &t, &["README.md"]);
    let c = cp(&w, Some(&mut t), tool("a"));
    assert!(c.recorded);
    assert_eq!(blob_files(&w).len(), blobs.len() + 1);
    let blobs = blob_files(&w);
    let c = cp(&w, Some(&mut t), CheckpointReason::BeforePublish);
    assert!(c.recorded);
    assert_eq!(blob_files(&w), blobs, "no new blob for unchanged content");
    assert_eq!(snapshot(&base_dir), base0, "the base is untouched");
    assert!(tmp_files(&w).is_empty());

    // Failures: unreadable file, row write failure.
    let top = latest_id(&w);
    w.write("src/lib.rs", b"new\n");
    let before = ws0(&w);
    fs::set_permissions(abs(&w, "src/lib.rs"), fs::Permissions::from_mode(0o000)).unwrap();
    changed(&w, &t, &["src/lib.rs"]);
    let r = try_cp(&w, Some(&mut t), tool("b"));
    fs::set_permissions(abs(&w, "src/lib.rs"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(r.is_err());
    assert_eq!(ws0(&w), before);
    assert_eq!(latest_id(&w), top);
    w.store.arm(Seam::AfterCheckpointRow);
    assert!(try_cp(&w, Some(&mut t), tool("b")).is_err());
    assert_eq!(latest_id(&w), top);
    assert_eq!(ws0(&w), before);
    assert!(tmp_files(&w).is_empty());
    assert_eq!(snapshot(&base_dir), base0);
}

#[test]
fn checkpoint_store_failures_are_settled() {
    let w = World::with(default_tree);
    let mut t = warm(&w);

    // The store is busy: no row, the workspace is untouched, a retry works.
    let top = latest_id(&w);
    w.write("a.txt", b"a\n");
    changed(&w, &t, &["a.txt"]);
    let lock = Locker::lock(&w);
    let started = std::time::Instant::now();
    let err = try_cp(&w, Some(&mut t), tool("busy")).expect_err("busy");
    assert_eq!(err.code, Code::Busy, "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    lock.unlock();
    assert_eq!(latest_id(&w), top);
    let c = cp(&w, Some(&mut t), tool("busy"));
    assert!(c.recorded);

    // A 100 MB file put before the lock is seen: still no row.
    let top = latest_id(&w);
    let big = vec![7u8; 100 << 20];
    w.write("big.bin", &big);
    changed(&w, &t, &["big.bin"]);
    let lock = Locker::lock(&w);
    let err = try_cp(&w, Some(&mut t), tool("big")).expect_err("busy");
    assert_eq!(err.code, Code::Busy);
    lock.unlock();
    assert_eq!(latest_id(&w), top);
    let c = cp(&w, Some(&mut t), tool("big"));
    assert_eq!(bytes_of(&w, &blob(&c, "big.bin").unwrap()).len(), big.len());
    w.remove("big.bin");
    changed(&w, &t, &["big.bin"]);
    cp(&w, Some(&mut t), tool("nobig"));

    // A blob that cannot be stored.
    let top = latest_id(&w);
    w.write("b.txt", b"b\n");
    changed(&w, &t, &["b.txt"]);
    let blobs = blob_files(&w);
    w.store.arm(Seam::BlobRenameFails { errno: libc::EIO });
    assert!(try_cp(&w, Some(&mut t), tool("eio")).is_err());
    assert_eq!(latest_id(&w), top);
    // The store keeps the failed temp file for its own sweep (store.md).
    assert_eq!(blob_files(&w), blobs);
    let c = cp(&w, Some(&mut t), tool("eio"));
    assert_eq!(kind(&c, "b.txt"), Some("Untracked"));
}

#[test]
fn writers_are_never_blocked() {
    let w = World::with(default_tree);
    let other = World::with(default_tree);
    let mut t = warm(&w);
    let ws = w.ws.clone();
    let ows = other.ws.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicU64::new(0));
    let lines = 20;
    let (cps, still) = thread::scope(|s| {
        // 24 writers (half in another session's workspace) and 20 appenders.
        for i in 0..24 {
            let (stop, writes) = (stop.clone(), writes.clone());
            let dir = if i % 2 == 0 { ws.clone() } else { ows.clone() };
            s.spawn(move || {
                let mut k = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    put(&dir, &format!("w/{i}.txt"), format!("{k}\n").as_bytes());
                    writes.fetch_add(1, Ordering::Relaxed);
                    k += 1;
                }
            });
        }
        for i in 0..lines {
            let stop = stop.clone();
            let p = ws.join(format!("ap/{i}.log.txt"));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            s.spawn(move || {
                let mut f = File::options().create(true).append(true).open(&p).unwrap();
                let mut k = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    f.write_all(format!("{k}\n").as_bytes()).unwrap();
                    k += 1;
                    if k.is_multiple_of(64) {
                        thread::yield_now();
                    }
                }
            });
        }
        thread::sleep(Duration::from_millis(100));
        // While a checkpoint is paused before its puts, writers go on.
        inject_fault(&ws, OverlayFault::PauseBeforePut);
        let probe = s.spawn({
            let (ws, writes) = (ws.clone(), writes.clone());
            move || {
                wait_fault(&ws).unwrap();
                let n0 = writes.load(Ordering::Relaxed);
                thread::sleep(Duration::from_millis(300));
                let n1 = writes.load(Ordering::Relaxed);
                release_fault(&ws);
                n1 > n0
            }
        });
        let mut cps = vec![try_cp(&w, None, manual()).expect("checkpoint")];
        let still = probe.join().unwrap();
        for _ in 0..2 {
            cps.push(try_cp(&w, None, manual()).expect("checkpoint"));
        }
        stop.store(true, Ordering::Relaxed);
        (cps, still)
    });
    assert!(still, "writes went on during the checkpoint");
    // Every recorded append log is a prefix of what was written.
    for c in &cps {
        for i in 0..lines {
            let p = format!("ap/{i}.log.txt");
            let Some(id) = blob(c, &p) else { continue };
            let got = bytes_of(&w, &id);
            let all = fs::read(abs(&w, &p)).unwrap();
            assert!(all.starts_with(&got), "{p}: a prefix");
        }
        // Nothing of the other session's files.
        for e in &c.checkpoint.entries {
            if let Some(n) = e.path.as_str().strip_prefix("w/") {
                let i: usize = n.trim_end_matches(".txt").parse().unwrap();
                assert_eq!(i % 2, 0, "{n}");
            }
        }
    }
    let all: Vec<String> = (0..24)
        .step_by(2)
        .map(|i| format!("w/{i}.txt"))
        .chain((0..lines).map(|i| format!("ap/{i}.log.txt")))
        .collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    changed(&w, &t, &refs);
    cp(&w, Some(&mut t), tool("final"));
}

#[test]
fn mixed_text_and_binary_in_one_checkpoint() {
    let w = World::with(default_tree);
    let mut t = warm(&w);
    let bin: Vec<u8> = (0..=255u8).chain([0, 0, 0xff, 0xfe]).collect();
    w.write("README.md", b"text change\n");
    w.write("img.bin", &bin);
    w.write("blank.txt", b"");
    changed(&w, &t, &["README.md", "img.bin", "blank.txt"]);
    let c = cp(&w, Some(&mut t), tool("mixed"));
    assert_eq!(kind(&c, "README.md"), Some("Modified"));
    assert_eq!(kind(&c, "img.bin"), Some("Untracked"));
    assert_eq!(kind(&c, "blank.txt"), Some("Untracked"));
    assert_eq!(
        bytes_of(&w, &blob(&c, "README.md").unwrap()),
        b"text change\n"
    );
    assert_eq!(bytes_of(&w, &blob(&c, "img.bin").unwrap()), bin);
    assert_eq!(bytes_of(&w, &blob(&c, "blank.txt").unwrap()), b"");
    assert_eq!(blob(&c, "img.bin").unwrap().as_str(), blake3_hex(&bin));
}

#[test]
fn tool_call_checkpoints_take_hints_and_late_events() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "AGENTS.md", b"agents\n");
        put_link(m, "CLAUDE.md", "AGENTS.md");
        put(m, "quiet/keep.txt", b"keep\n");
        put(m, "same1.txt", b"aaaa\n");
        put(m, "same2.txt", b"bbbb\n");
    });
    let mut t = warm(&w);
    let hint = |t: &Tracker, p: &str| t.inbox().hint(&[rp(p)]);

    // A hinted path is in, without a full scan.
    w.write("a.ts", b"a\n");
    hint(&t, "a.ts");
    let c = cp(&w, Some(&mut t), tool("1"));
    assert_eq!(kind(&c, "a.ts"), Some("Untracked"));
    assert!(!c.stats.full);

    // A hint through a symlink in the workspace reaches the target.
    fs::write(abs(&w, "CLAUDE.md"), b"via link\n").unwrap();
    hint(&t, "CLAUDE.md");
    let c = cp(&w, Some(&mut t), tool("2"));
    assert_eq!(kind(&c, "AGENTS.md"), Some("Modified"));
    assert!(!c.stats.full);

    // Links that leave the workspace or climb: a full scan.
    for (i, (link, target)) in [("out.md", "../x.md"), ("weird.md", "x/../AGENTS.md")]
        .into_iter()
        .enumerate()
    {
        put_link(&w.ws, link, target);
        hint(&t, link);
        let c = cp(&w, Some(&mut t), tool(&format!("3{i}")));
        assert!(c.stats.full, "{link}");
        assert_eq!(kind(&c, link), Some("Untracked"));
    }

    // A write with no hint and no event yet; the late event brings it in.
    w.write("b.ts", b"b\n");
    let c = try_cp(&w, Some(&mut t), tool("4")).expect("checkpoint");
    assert_eq!(kind(&c, "b.ts"), None, "not seen yet");
    assert!(!c.stats.full);
    changed(&w, &t, &["b.ts"]);
    let c = cp(&w, Some(&mut t), tool("5"));
    assert_eq!(kind(&c, "b.ts"), Some("Untracked"));
    assert!(!c.stats.full);

    // A required full scan on a warm cache.
    w.write("quiet/c.ts", b"c\n");
    t.inbox().require_full();
    let c = cp(&w, Some(&mut t), tool("6"));
    assert_eq!(kind(&c, "quiet/c.ts"), Some("Untracked"));
    assert!(c.stats.full && !c.stats.cold);

    // Manual and lifecycle checkpoints scan fully, the cache vouching for
    // what did not change; a same-size rewrite with its mtime put back is seen.
    let all = compute(&w.ws_ref(), &w.base, &mut tracker(&w), None)
        .unwrap()
        .stats
        .files_hashed;
    for (i, (r, file)) in [
        (manual(), "same1.txt"),
        (CheckpointReason::BeforeRestore, "same2.txt"),
    ]
    .into_iter()
    .enumerate()
    {
        w.write(&format!("m{i}.txt"), b"m\n");
        rewrite_same_stat(&w, file, b"zzzz\n");
        let c = cp(&w, Some(&mut t), r);
        assert!(c.stats.full && !c.stats.cold);
        assert_eq!(kind(&c, &format!("m{i}.txt")), Some("Untracked"));
        assert_eq!(kind(&c, file), Some("Modified"));
        assert_eq!(c.stats.files_hashed, 2, "{:?}", c.stats);
        assert_eq!(
            c.stats.vouched + c.stats.files_hashed,
            all + 1 + i as u64,
            "{:?}",
            c.stats
        );
    }
}

#[test]
fn checkpoint_never_captures_through_a_swapped_directory() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "d/y.ts", b"const y = 1;\n");
        put(m, "d/z.ts", b"const z = 1;\n");
        put(m, "a/b/y.ts", b"const ab = 1;\n");
        put(m, "a/b/w.ts", b"const w = 1;\n");
    });
    let out1 = w.tmp.join("outside1");
    let out2 = w.tmp.join("outside2");
    let secret1 = b"const y = 2;\nOUTSIDE-ONE\n".to_vec();
    let secret2 = b"const ab = 2;\nOUTSIDE-TWO\n".to_vec();
    put(&out1, "y.ts", &secret1);
    put(&out1, "z.ts", b"const z = 1;\n");
    put(&out2, "b/y.ts", &secret2);
    put(&out2, "b/w.ts", b"const w = 1;\n");
    let mut t = warm(&w);
    let ws = w.ws.clone();

    for (dir, file, new, out) in [
        ("d", "d/y.ts", &b"const y = 3;\n"[..], &out1),
        ("a", "a/b/y.ts", &b"const ab = 3;\n"[..], &out2),
    ] {
        w.write(file, new);
        changed(&w, &t, &[file]);
        inject_fault(&ws, OverlayFault::PauseBeforePut);
        let r = thread::scope(|s| {
            let h = s.spawn(|| {
                assert_eq!(wait_fault(&ws), Some(OverlayFault::PauseBeforePut));
                fs::remove_dir_all(ws.join(dir)).unwrap();
                std::os::unix::fs::symlink(out, ws.join(dir)).unwrap();
                release_fault(&ws);
            });
            let r = try_cp(&w, Some(&mut t), tool(&format!("swap-{dir}")));
            h.join().unwrap();
            r
        });
        if let Ok(c) = &r {
            assert!(
                c.checkpoint.entries.iter().all(|e| !e
                    .path
                    .as_str()
                    .starts_with(&format!("{dir}/"))
                    || e.kind == EntryKind::Deleted),
                "{:?}",
                c.checkpoint.entries
            );
            assert_eq!(kind(c, dir), Some("Untracked"), "the link itself");
        }
        // Nothing from outside is in the store.
        for f in w.store_files() {
            let b = fs::read(&f).unwrap();
            assert_ne!(b, secret1);
            assert_ne!(b, secret2);
        }
        changed(&w, &t, &[dir]);
        let c = cp(&w, Some(&mut t), tool(&format!("after-{dir}")));
        assert_eq!(kind(&c, dir), Some("Untracked"));
        assert_eq!(kind(&c, file), Some("Deleted"));
    }
}

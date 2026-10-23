//! Task 9: a restore that checkpoints first (§5.7, same-base path).

mod common;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use common::*;
use strata_overlay::{
    Checkpointed, DEFAULT_DIRTY_CAPACITY, Overlay, OverlayWarning, RESTORE_TEMP_PREFIX,
    RestoreRequest, Restored, Tracker, WorkspaceRef, checkpoint, reconcile_full, restore,
};
use strata_store::MoveState;
use strata_types::{
    BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, EntryKind, OverlayEntry, StrataError,
};

// ---------------------------------------------------------------- helpers

fn tracker(w: &World) -> Tracker {
    Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY)
}

fn cold(w: &World) -> Overlay {
    let o = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    o.validate().expect("validate");
    o
}

fn manual() -> CheckpointReason {
    CheckpointReason::Manual {
        message: "m".to_owned(),
    }
}

fn cp(w: &World, t: Option<&mut Tracker>) -> Checkpointed {
    let c = checkpoint(&w.ws_ref(), &w.base, t, manual()).expect("checkpoint");
    assert_eq!(
        Overlay::from_checkpoint(&c.checkpoint),
        cold(w),
        "the checkpoint equals the cold scan"
    );
    c
}

fn id(c: &Checkpointed) -> CheckpointId {
    c.checkpoint.id
}

fn try_rs(
    w: &World,
    t: Option<&mut Tracker>,
    target: CheckpointId,
    paths: Option<&[strata_types::RepoPath]>,
) -> Result<Restored, StrataError> {
    restore(
        &w.ws_ref(),
        &w.base,
        t,
        &RestoreRequest {
            target,
            target_base: None,
            paths,
            verify: false,
        },
    )
}

/// A restore that must succeed; afterwards the workspace's overlay is the
/// target's (for a whole restore) and T is gone.
fn rs(w: &World, target: CheckpointId) -> Restored {
    let r = try_rs(w, None, target, None).expect("restore");
    let want = Overlay::from_checkpoint(&w.store.checkpoint(&w.sid, target).unwrap());
    assert_eq!(cold(w).entries, want.entries, "the workspace is the target");
    assert!(!temp(w).exists(), "T is removed after success");
    r
}

fn temp(w: &World) -> PathBuf {
    w.ws.parent()
        .unwrap()
        .join(format!("{RESTORE_TEMP_PREFIX}{}", w.repo.as_str()))
}

fn latest(w: &World) -> Option<CheckpointId> {
    w.store
        .latest_checkpoint(&w.sid, &w.repo)
        .unwrap()
        .map(|c| c.id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    File { bytes: Vec<u8>, exec: bool },
    Link(Vec<u8>),
    Dir,
}

/// Every path under `dir` except the top-level `.git`.
fn tree(dir: &Path) -> BTreeMap<String, Node> {
    let mut out = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for e in fs::read_dir(dir.join(&rel)).unwrap() {
            let e = e.unwrap();
            let r = rel.join(e.file_name());
            if r.as_os_str() == ".git" {
                continue;
            }
            let m = fs::symlink_metadata(e.path()).unwrap();
            let key = r.to_string_lossy().into_owned();
            if m.file_type().is_symlink() {
                let t = fs::read_link(e.path()).unwrap();
                out.insert(key, Node::Link(t.as_os_str().as_bytes().to_vec()));
            } else if m.is_dir() {
                out.insert(key, Node::Dir);
                stack.push(r);
            } else {
                out.insert(
                    key,
                    Node::File {
                        bytes: fs::read(e.path()).unwrap(),
                        exec: m.mode() & 0o111 != 0,
                    },
                );
            }
        }
    }
    out
}

fn mv(w: &World, from: &str, to: &str) {
    fs::rename(w.ws.join(from), w.ws.join(to)).unwrap();
}

fn entry<'a>(o: &'a Overlay, p: &str) -> Option<&'a OverlayEntry> {
    o.entries.iter().find(|e| e.path.as_str() == p)
}

fn kind(o: &Overlay, p: &str) -> Option<&'static str> {
    entry(o, p).map(|e| match &e.kind {
        EntryKind::Added { .. } => "Added",
        EntryKind::Modified { .. } => "Modified",
        EntryKind::Untracked { .. } => "Untracked",
        EntryKind::Renamed { .. } => "Renamed",
        EntryKind::Deleted => "Deleted",
    })
}

fn blob_at(o: &Overlay, p: &str) -> Option<String> {
    entry(o, p).and_then(|e| match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Untracked { blob }
        | EntryKind::Renamed { blob, .. } => Some(blob.as_str().to_owned()),
        EntryKind::Deleted => None,
    })
}

fn ino(w: &World, rel: &str) -> u64 {
    fs::symlink_metadata(w.ws.join(rel)).unwrap().ino()
}

fn chmod(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

fn user_xattrs(p: &Path) -> Vec<String> {
    let out = Command::new("xattr").arg(p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with("com.apple."))
        .map(str::to_owned)
        .collect()
}

fn reason(e: &StrataError) -> Option<&str> {
    e.detail.as_ref()?.get("reason")?.as_str()
}

fn with_root<'a>(w: &'a World, root: &'a Path) -> WorkspaceRef<'a> {
    WorkspaceRef { root, ..w.ws_ref() }
}

// ---------------------------------------------------------------- tests

#[test]
fn restore_is_itself_checkpointed() {
    let w = World::new();
    let pristine = tree(&w.ws);
    let mut t = tracker(&w);
    let c1 = cp(&w, Some(&mut t));
    w.write("README.md", b"changed\n");
    w.write("src/lib.rs", b"pub fn b() {}\n");
    w.write("new.txt", b"new\n");
    w.remove("empty.txt");
    let edited = tree(&w.ws);

    let r = try_rs(&w, Some(&mut t), id(&c1), None).expect("restore");
    assert!(r.before > id(&c1), "the BeforeRestore row is newer");
    assert_eq!(r.target, id(&c1));
    let before = w.store.checkpoint(&w.sid, r.before).unwrap();
    assert_eq!(before.reason, CheckpointReason::BeforeRestore);
    assert_eq!(
        kind(&Overlay::from_checkpoint(&before), "new.txt"),
        Some("Untracked")
    );
    assert_eq!(
        r.written,
        vec![rp("README.md"), rp("empty.txt"), rp("src/lib.rs")]
    );
    assert_eq!(r.removed, vec![rp("new.txt")]);
    assert!(r.conflicts.is_empty());
    assert_eq!(tree(&w.ws), pristine, "back to checkpoint 1");
    assert!(!temp(&w).exists());
    // The tracker keeps working after the restore.
    let c = checkpoint(&w.ws_ref(), &w.base, Some(&mut t), manual()).unwrap();
    assert!(c.checkpoint.entries.is_empty());

    // Restoring the BeforeRestore undoes the restore.
    let r2 = rs(&w, r.before);
    assert_eq!(tree(&w.ws), edited, "back to before the restore");
    assert!(w.ws.join("new.txt").exists());
    assert!(!w.ws.join("empty.txt").exists());
    // ... and that restore can be undone too.
    rs(&w, r2.before);
    assert_eq!(tree(&w.ws), pristine);
    // Checkpoint 1 is unchanged and can be restored again (append only).
    let again = w.store.checkpoint(&w.sid, id(&c1)).unwrap();
    assert_eq!(again, c1.checkpoint);
    rs(&w, r.before);
    rs(&w, id(&c1));
    assert_eq!(tree(&w.ws), pristine);
}

fn rename_tree(m: &Path) {
    default_tree(m);
    put(m, "a.ts", b"a\n");
    put(m, "src/Foo.ts", b"foo\n");
}

#[test]
fn restore_before_a_rename_brings_back_the_old_name() {
    let w = World::with(rename_tree);
    let c1 = cp(&w, None);
    let readme = ino(&w, "README.md");
    mv(&w, "a.ts", "b.ts");
    rs(&w, id(&c1));
    assert_eq!(w.read("a.ts"), b"a\n");
    assert!(!w.ws.join("b.ts").exists());
    assert!(cold(&w).is_empty());
    assert_eq!(
        ino(&w, "README.md"),
        readme,
        "untouched paths keep their inode"
    );

    // Renamed and changed: the BeforeRestore keeps the changed content.
    mv(&w, "a.ts", "b.ts");
    w.write("b.ts", b"a 2\n");
    let r = rs(&w, id(&c1));
    let before = Overlay::from_checkpoint(&w.store.checkpoint(&w.sid, r.before).unwrap());
    assert_eq!(blob_at(&before, "b.ts"), Some(blake3_hex(b"a 2\n")));
    assert_eq!(w.read("a.ts"), b"a\n");
    assert!(!w.ws.join("b.ts").exists());

    // A case-only rename on this case-insensitive volume (ignorecase = true).
    mv(&w, "src/Foo.ts", "src/foo.ts");
    let c2 = cp(&w, None);
    assert_eq!(
        kind(&c2.checkpoint_overlay(), "src/foo.ts"),
        Some("Renamed")
    );
    let names = |w: &World| -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(w.ws.join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.eq_ignore_ascii_case("foo.ts"))
            .collect();
        v.sort();
        v
    };
    rs(&w, id(&c1));
    assert_eq!(names(&w), vec!["Foo.ts"]);
    assert_eq!(w.read("src/Foo.ts"), b"foo\n");
    // From before the rename, restore(2): the spelling becomes foo.ts.
    rs(&w, id(&c2));
    assert_eq!(names(&w), vec!["foo.ts"]);
    assert_eq!(ino(&w, "README.md"), readme);
}

trait CpOverlay {
    fn checkpoint_overlay(&self) -> Overlay;
}

impl CpOverlay for Checkpointed {
    fn checkpoint_overlay(&self) -> Overlay {
        Overlay::from_checkpoint(&self.checkpoint)
    }
}

#[test]
fn deleted_base_file_is_tombstone() {
    let w = World::new();
    let c0 = cp(&w, None);
    let mut t = tracker(&w);
    w.remove("README.md");
    w.remove("link");
    let o = cp(&w, Some(&mut t)).checkpoint_overlay();
    for p in ["README.md", "link"] {
        let e = entry(&o, p).unwrap();
        assert_eq!(e.kind, EntryKind::Deleted);
        assert_eq!(e.mode, 0);
    }
    assert_eq!(o.entries.len(), 2, "nothing else is touched");
    // A whole base directory: one Deleted per file, no directory entry.
    w.remove("src/deep");
    let o = cp(&w, Some(&mut t)).checkpoint_overlay();
    assert_eq!(kind(&o, "src/deep/x/y.txt"), Some("Deleted"));
    assert!(entry(&o, "src/deep").is_none() && entry(&o, "src/deep/x").is_none());
    // Tombstones stay in every later checkpoint.
    w.write("other.txt", b"o\n");
    let o = cp(&w, Some(&mut t)).checkpoint_overlay();
    assert_eq!(kind(&o, "README.md"), Some("Deleted"));
    assert_eq!(kind(&o, "src/deep/x/y.txt"), Some("Deleted"));
    // Recreated with other content, then deleted again: still a tombstone.
    w.write("README.md", b"other\n");
    assert_eq!(
        kind(&cp(&w, Some(&mut t)).checkpoint_overlay(), "README.md"),
        Some("Modified")
    );
    w.remove("README.md");
    assert_eq!(
        kind(&cp(&w, Some(&mut t)).checkpoint_overlay(), "README.md"),
        Some("Deleted")
    );
    // skip-worktree in the index does not hide the deletion.
    w.git_ok(&w.ws, &["update-index", "--skip-worktree", "empty.txt"]);
    w.remove("empty.txt");
    assert_eq!(
        kind(&cp(&w, Some(&mut t)).checkpoint_overlay(), "empty.txt"),
        Some("Deleted")
    );
    // A deleted base file still open and written through its descriptor.
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(w.ws.join("src/lib.rs"))
        .unwrap();
    w.remove("src/lib.rs");
    f.write_all(b"more\n").unwrap();
    let o = cp(&w, Some(&mut t)).checkpoint_overlay();
    assert_eq!(kind(&o, "src/lib.rs"), Some("Deleted"));
    assert_eq!(entry(&o, "src/lib.rs").unwrap().mode, 0);
    drop(f);
    // restore brings every one of them back.
    w.git_ok(&w.ws, &["update-index", "--no-skip-worktree", "empty.txt"]);
    rs(&w, id(&c0));
    assert_eq!(w.read("README.md"), b"hello\n");
    assert_eq!(
        fs::read_link(w.ws.join("link")).unwrap(),
        Path::new("README.md")
    );
    assert_eq!(w.read("src/deep/x/y.txt"), b"deep\n");
    assert_eq!(w.read("src/lib.rs"), b"pub fn a() {}\n");
    assert!(cold(&w).is_empty());
}

#[test]
fn symlink_entry_is_exact_target_bytes() {
    let w = World::new();
    let c0 = cp(&w, None);
    let raw: &[u8] = b"\xff\xfe-not-utf8";
    let long = vec![b'a'; 1023];
    symlink(OsStr::from_bytes(raw), w.ws.join("raw")).unwrap();
    symlink(OsStr::from_bytes(&long), w.ws.join("long")).unwrap();
    symlink("src", w.ws.join("todir")).unwrap();
    symlink("nowhere/at/all", w.ws.join("dangling")).unwrap();
    put_link(&w.ws, "link", "src/lib.rs");
    w.remove("bin/run.sh");
    symlink("../README.md", w.ws.join("bin/run.sh")).unwrap();
    let a = cp(&w, None);
    let o = a.checkpoint_overlay();
    for (p, target) in [
        ("raw", raw),
        ("long", &long[..]),
        ("todir", b"src".as_slice()),
        ("dangling", b"nowhere/at/all".as_slice()),
        ("link", b"src/lib.rs".as_slice()),
        ("bin/run.sh", b"../README.md".as_slice()),
    ] {
        let e = entry(&o, p).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(e.mode, 0o120000, "{p}");
        assert_eq!(blob_at(&o, p), Some(blake3_hex(target)), "{p}");
    }
    assert_eq!(kind(&o, "link"), Some("Modified"));
    assert!(
        !o.entries
            .iter()
            .any(|e| e.path.as_str().starts_with("todir/")),
        "links are never followed"
    );
    // Unchanged links keep their entry in a later checkpoint.
    w.write("other.txt", b"o\n");
    let o2 = cp(&w, None).checkpoint_overlay();
    assert_eq!(entry(&o2, "raw"), entry(&o, "raw"));
    assert_eq!(entry(&o2, "long"), entry(&o, "long"));
    // Restore away and back: readlink is byte-for-byte the same.
    rs(&w, id(&c0));
    assert!(fs::symlink_metadata(w.ws.join("raw")).is_err());
    assert_eq!(
        fs::read_link(w.ws.join("link")).unwrap(),
        Path::new("README.md")
    );
    rs(&w, id(&a));
    for (p, target) in [
        ("raw", raw),
        ("long", &long[..]),
        ("todir", b"src".as_slice()),
        ("dangling", b"nowhere/at/all".as_slice()),
        ("link", b"src/lib.rs".as_slice()),
        ("bin/run.sh", b"../README.md".as_slice()),
    ] {
        let got = fs::read_link(w.ws.join(p)).unwrap();
        assert_eq!(got.as_os_str().as_bytes(), target, "{p}");
    }
    // A deleted base link is a Deleted entry.
    w.remove("link");
    assert_eq!(
        kind(&cp(&w, None).checkpoint_overlay(), "link"),
        Some("Deleted")
    );
}

#[test]
fn special_mode_bits_are_normalized_and_warned() {
    let w = World::new();
    let c0 = cp(&w, None);
    w.write("s1", b"1\n");
    w.write("s2", b"2\n");
    w.write("s3", b"3\n");
    w.write("plain.txt", b"plain\n");
    chmod(&w.ws.join("s1"), 0o4755);
    chmod(&w.ws.join("s2"), 0o2755);
    chmod(&w.ws.join("s3"), 0o1644);
    chmod(&w.ws.join("README.md"), 0o4755);
    let got = |p: &str| fs::metadata(w.ws.join(p)).unwrap().mode() & 0o7777;
    assert_eq!(
        (got("s1"), got("s2"), got("s3")),
        (0o4755, 0o2755, 0o1644),
        "the volume keeps the special bits"
    );
    let a = cp(&w, None);
    let o = a.checkpoint_overlay();
    let mode = |p: &str| entry(&o, p).unwrap().mode;
    assert_eq!(
        (mode("s1"), mode("s2"), mode("s3"), mode("README.md")),
        (0o100755, 0o100755, 0o100644, 0o100755)
    );
    assert_eq!(kind(&o, "plain.txt"), Some("Untracked"), "the scan goes on");
    for (p, m) in [
        ("s1", "4755"),
        ("s2", "2755"),
        ("s3", "1644"),
        ("README.md", "4755"),
    ] {
        assert!(
            a.warnings.contains(&OverlayWarning::SpecialModeBits {
                path: rp(p),
                mode: m.to_owned()
            }),
            "{p}: {:?}",
            a.warnings
        );
    }
    // Restore writes the files back without those bits.
    rs(&w, id(&c0));
    rs(&w, id(&a));
    assert_eq!(
        (got("s1"), got("s2"), got("s3"), got("README.md")),
        (0o755, 0o755, 0o644, 0o755)
    );
}

#[test]
fn git_modes_roundtrip() {
    let w = World::new();
    let c0 = cp(&w, None);
    w.write("README.md", b"edited\n");
    chmod(&w.ws.join("src/lib.rs"), 0o755);
    chmod(&w.ws.join("bin/run.sh"), 0o644);
    put_link(&w.ws, "newlink", "src");
    chmod(&w.ws.join("src/deep"), 0o700);
    let a = cp(&w, None);
    let o = a.checkpoint_overlay();
    assert_eq!(entry(&o, "README.md").unwrap().mode, 0o100644);
    assert_eq!(entry(&o, "src/lib.rs").unwrap().mode, 0o100755);
    assert_eq!(entry(&o, "bin/run.sh").unwrap().mode, 0o100644);
    assert_eq!(entry(&o, "newlink").unwrap().mode, 0o120000);
    assert!(
        entry(&o, "src/deep").is_none(),
        "directory modes are not recorded"
    );
    let exec = |p: &str| fs::metadata(w.ws.join(p)).unwrap().mode() & 0o111 != 0;
    rs(&w, id(&c0));
    assert!(!exec("src/lib.rs") && exec("bin/run.sh"));
    assert!(fs::symlink_metadata(w.ws.join("newlink")).is_err());
    let start = SystemTime::now() - Duration::from_secs(2);
    rs(&w, id(&a));
    assert!(exec("src/lib.rs") && !exec("bin/run.sh"));
    assert!(
        fs::symlink_metadata(w.ws.join("newlink"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(w.read("README.md"), b"edited\n");
    for p in ["README.md", "src/lib.rs", "bin/run.sh"] {
        let m = fs::metadata(w.ws.join(p)).unwrap().modified().unwrap();
        assert!(m >= start, "{p}: restored files carry the current time");
    }
}

#[test]
fn sparse_file_is_hashed_whole() {
    const N: u64 = 256 << 20;
    let w = World::new();
    let mut t = tracker(&w);
    let c0 = cp(&w, Some(&mut t));
    {
        let mut f = fs::File::create(w.ws.join("sparse.bin")).unwrap();
        f.write_all(b"A").unwrap();
        f.set_len(N).unwrap();
        f.seek(SeekFrom::Start(N - 1)).unwrap();
        f.write_all(b"Z").unwrap();
    }
    let mut whole = vec![0u8; N as usize];
    whole[0] = b'A';
    whole[N as usize - 1] = b'Z';
    let want = blake3_hex(&whole);
    drop(whole);
    let a = cp(&w, Some(&mut t));
    assert_eq!(
        blob_at(&a.checkpoint_overlay(), "sparse.bin"),
        Some(want.clone())
    );
    assert_eq!(a.stats.bytes_hashed, N);
    rs(&w, id(&c0));
    assert!(!w.ws.join("sparse.bin").exists());
    rs(&w, id(&a));
    let mut h = blake3::Hasher::new();
    let mut f = fs::File::open(w.ws.join("sparse.bin")).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    assert_eq!(h.finalize().to_hex().to_string(), want);
    assert_eq!(fs::metadata(w.ws.join("sparse.bin")).unwrap().len(), N);
    // The big blob leaves the workspace before the test ends.
    rs(&w, id(&c0));
}

#[test]
fn xattrs_are_never_recorded_or_restored() {
    let w = World::new();
    let c0 = cp(&w, None);
    let xw = |p: &Path| {
        let ok = Command::new("xattr")
            .args(["-w", "org.strata.test", "v"])
            .arg(p)
            .status()
            .unwrap()
            .success();
        assert!(ok, "xattr -w {}", p.display());
    };
    // Only an xattr and a BSD flag on a base file: no entry.
    xw(&w.ws.join("README.md"));
    let ok = Command::new("chflags")
        .args(["nodump"])
        .arg(w.ws.join("README.md"))
        .status()
        .unwrap()
        .success();
    assert!(ok);
    assert!(cp(&w, None).checkpoint.entries.is_empty());
    // A modified file with an xattr: content and mode come back, the xattr does not.
    w.write("src/lib.rs", b"modified\n");
    chmod(&w.ws.join("src/lib.rs"), 0o755);
    xw(&w.ws.join("src/lib.rs"));
    let a = cp(&w, None);
    rs(&w, id(&c0));
    rs(&w, id(&a));
    assert_eq!(w.read("src/lib.rs"), b"modified\n");
    assert_eq!(
        fs::metadata(w.ws.join("src/lib.rs")).unwrap().mode() & 0o777,
        0o755
    );
    assert!(user_xattrs(&w.ws.join("src/lib.rs")).is_empty());
    // A base file with an xattr: the clone restore puts back has none.
    let bf = w.base.dir.join("src/lib.rs");
    let mode = fs::metadata(&bf).unwrap().mode() & 0o7777;
    chmod(&bf, mode | 0o200);
    xw(&bf);
    chmod(&bf, mode);
    assert!(!user_xattrs(&bf).is_empty());
    rs(&w, id(&c0));
    assert_eq!(w.read("src/lib.rs"), b"pub fn a() {}\n");
    assert!(user_xattrs(&w.ws.join("src/lib.rs")).is_empty());
    assert_eq!(
        fs::metadata(w.ws.join("src/lib.rs")).unwrap().mode() & 0o777,
        0o644
    );
}

#[test]
fn empty_directories_are_not_carried() {
    let w = World::new();
    let c0 = cp(&w, None);
    fs::create_dir_all(w.ws.join("newdir/inner")).unwrap();
    assert!(
        cp(&w, None).checkpoint.entries.is_empty(),
        "no entry for an empty dir"
    );
    w.remove("src/deep");
    let c1 = cp(&w, None);
    // Missing parents are created when files are written back.
    rs(&w, id(&c0));
    assert_eq!(w.read("src/deep/x/y.txt"), b"deep\n");
    // Restoring the checkpoint whose directory lost all its files does not
    // leave (or rebuild) the empty directory.
    rs(&w, id(&c1));
    assert!(!w.ws.join("src/deep").exists());
    assert!(w.ws.join("src/lib.rs").exists());
    assert!(w.ws.join("newdir/inner").is_dir(), "untouched dirs stay");
}

#[test]
fn external_writers_are_captured_exactly() {
    let w = World::new();
    let c0 = cp(&w, None);
    let pristine = tree(&w.ws);
    let sh = |script: &str| {
        Command::new("sh")
            .args(["-c", script])
            .current_dir(&w.ws)
            .status()
            .unwrap()
    };
    // Exit 0: every kind of change.
    let st = sh(
        "printf 'by sh\\n' > README.md && ln -s src/lib.rs ln2 && chmod +x empty.txt \
                && rm .gitignore && mv src/lib.rs src/lib2.rs && rm link && ln -s bin link",
    );
    assert!(st.success());
    // Exit 1 after writing.
    let st = sh("mkdir -p out && printf 'x' > out/f.txt && exit 1");
    assert_eq!(st.code(), Some(1));
    // Still alive: an open descriptor, cwd in the workspace.
    let mut live = Command::new("sh")
        .args([
            "-c",
            "exec 3>live.txt; printf one >&3; echo ready; read line; printf two >&3",
        ])
        .current_dir(w.ws.join("src"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(live.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ready");
    let a = cp(&w, None);
    let at_a = tree(&w.ws);
    // A workspace back at the base (as if freshly cloned) restored to it.
    rs(&w, id(&c0));
    assert_eq!(tree(&w.ws), pristine);
    rs(&w, id(&a));
    assert_eq!(
        tree(&w.ws),
        at_a,
        "kind, content, link target and mode per path"
    );
    // The live process writes more and exits: the next checkpoint has it.
    // (Its descriptor points at the file restore replaced; write through
    // the path again so the workspace holds what it wrote.)
    live.stdin.take().unwrap().write_all(b"\n").unwrap();
    assert!(live.wait().unwrap().success());
    w.write("src/live.txt", b"onetwo");
    let b = cp(&w, None);
    assert_eq!(
        blob_at(&b.checkpoint_overlay(), "src/live.txt"),
        Some(blake3_hex(b"onetwo"))
    );
}

#[test]
fn checkpoint_is_immutable_and_restorable() {
    let w = World::new();
    let mut t = tracker(&w);
    w.write("README.md", b"one\n");
    let c1 = cp(&w, Some(&mut t));
    let at1 = tree(&w.ws);
    w.write("README.md", b"two\n");
    w.write("new.txt", b"n\n");
    let c2 = cp(&w, Some(&mut t));
    // Rebase the later checkpoint's overlay: the old rows do not change.
    let (nb, _) = w.next_base(|m| put(m, "upstream.txt", b"u\n"));
    let opts = strata_overlay::RebaseOptions::for_restore(true, false);
    strata_overlay::rebase(&c2.checkpoint_overlay(), &w.base, &nb, &w.store, &opts)
        .expect("rebase");
    assert_eq!(w.store.checkpoint(&w.sid, id(&c1)).unwrap(), c1.checkpoint);
    assert_eq!(w.store.checkpoint(&w.sid, id(&c2)).unwrap(), c2.checkpoint);
    rs(&w, id(&c1));
    assert_eq!(tree(&w.ws), at1);

    // Five incremental checkpoints, a writer between each two steps.
    let mut shots: Vec<(Checkpointed, BTreeMap<String, Node>)> = Vec::new();
    let mut t = tracker(&w);
    for i in 0..5 {
        w.write(&format!("step{i}.txt"), format!("{i}\n").as_bytes());
        w.write("README.md", format!("r{i}\n").as_bytes());
        let c = cp(&w, Some(&mut t));
        shots.push((c, tree(&w.ws)));
        // The writer between steps (captured by the next checkpoint).
        let st = Command::new("sh")
            .args(["-c", &format!("printf 'w{i}' >> writer.txt")])
            .current_dir(&w.ws)
            .status()
            .unwrap();
        assert!(st.success());
    }
    for pair in shots.windows(2) {
        assert_eq!(
            id(&pair[1].0).get(),
            id(&pair[0].0).get() + 1,
            "ids increase by one"
        );
    }
    for (c, at) in shots.iter().rev() {
        rs(&w, id(c));
        assert_eq!(&tree(&w.ws), at, "checkpoint {}", id(c).get());
    }
    // A checkpoint that does not exist reads NoSuchCheckpoint.
    let e = w
        .store
        .checkpoint(&w.sid, CheckpointId::try_from(9_999_u64).unwrap())
        .unwrap_err();
    assert_eq!(e.code, Code::NoSuchCheckpoint);
}

#[test]
fn restore_paths_select_whole_groups() {
    let w = World::with(rename_tree);
    let c0 = cp(&w, None);
    let pick = |ps: &[&str]| -> Vec<strata_types::RepoPath> { ps.iter().map(|p| rp(p)).collect() };
    // Either half of a rename brings back both.
    for half in ["b.ts", "a.ts"] {
        mv(&w, "a.ts", "b.ts");
        try_rs(&w, None, id(&c0), Some(&pick(&[half]))).expect("restore");
        assert_eq!(w.read("a.ts"), b"a\n", "{half}");
        assert!(!w.ws.join("b.ts").exists(), "{half}");
    }
    // A type swap group: empty.txt became a directory.
    w.remove("empty.txt");
    w.write("empty.txt/inner.txt", b"in\n");
    w.write("README.md", b"other change\n");
    try_rs(&w, None, id(&c0), Some(&pick(&["empty.txt/inner.txt"]))).expect("restore");
    assert!(fs::metadata(w.ws.join("empty.txt")).unwrap().is_file());
    assert_eq!(w.read("README.md"), b"other change\n", "not selected");
    // A directory prefix: everything under it.
    w.write("src/lib.rs", b"x\n");
    w.write("src/deep/x/y.txt", b"y\n");
    w.write("src/new.rs", b"n\n");
    try_rs(&w, None, id(&c0), Some(&pick(&["src"]))).expect("restore");
    assert_eq!(w.read("src/lib.rs"), b"pub fn a() {}\n");
    assert_eq!(w.read("src/deep/x/y.txt"), b"deep\n");
    assert!(
        !w.ws.join("src/new.rs").exists(),
        "created after the checkpoint: removed"
    );
    assert_eq!(w.read("README.md"), b"other change\n");

    // An unknown path: NoSuchPath, nothing written, no BeforeRestore.
    let before = (tree(&w.ws), latest(&w));
    let e = try_rs(&w, None, id(&c0), Some(&pick(&["nope/where.txt"]))).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath);
    assert_eq!((tree(&w.ws), latest(&w)), before);
    // Only the case differs: NoSuchPath with the spelling on disk.
    let e = try_rs(&w, None, id(&c0), Some(&pick(&["src/foo.ts"]))).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath);
    assert_eq!(e.detail.as_ref().unwrap()["on_disk"], "src/Foo.ts");
    assert_eq!((tree(&w.ws), latest(&w)), before);

    // Naming tools/new.ts while tools is now an Untracked symlink.
    w.write("tools/new.ts", b"tool\n");
    let c1 = cp(&w, None);
    w.remove("tools");
    put_link(&w.ws, "tools", "README.md");
    let r = try_rs(&w, None, id(&c1), Some(&pick(&["tools/new.ts"]))).expect("restore");
    let before = Overlay::from_checkpoint(&w.store.checkpoint(&w.sid, r.before).unwrap());
    assert_eq!(kind(&before, "tools"), Some("Untracked"));
    assert!(fs::symlink_metadata(w.ws.join("tools")).unwrap().is_dir());
    assert_eq!(w.read("tools/new.ts"), b"tool\n");
    assert_eq!(r.removed, vec![rp("tools")]);
    assert_eq!(r.written, vec![rp("tools/new.ts")]);
    assert!(!temp(&w).exists());
}

#[test]
fn restore_missing_checkpoint_is_store_error() {
    let w = World::new();
    w.write("README.md", b"edited\n");
    let mut t = tracker(&w);
    let files = w.store_files();
    let at = tree(&w.ws);
    let missing = CheckpointId::try_from(4242_u64).unwrap();
    let want = w.store.checkpoint(&w.sid, missing).unwrap_err();
    let e = try_rs(&w, Some(&mut t), missing, None).unwrap_err();
    assert_eq!(e, want, "the store's error, unwrapped");
    assert_eq!(e.code, Code::NoSuchCheckpoint);
    assert!(t.last().is_none(), "no scan");
    assert_eq!(latest(&w), None, "no BeforeRestore");
    assert_eq!(w.store_files(), files);
    assert_eq!(tree(&w.ws), at);
}

#[test]
fn restore_refuses_malformed_checkpoint() {
    let mut w = World::new();
    let c0 = cp(&w, None);
    let base_cp = |entries: Vec<OverlayEntry>| Checkpoint {
        entries,
        ..c0.checkpoint.clone()
    };
    let blob = BlobId::try_from(blake3_hex(b"x")).unwrap();
    let added = |p: &str| OverlayEntry {
        path: rp(p),
        kind: EntryKind::Untracked { blob: blob.clone() },
        mode: 0o100644,
    };
    for bad in [
        base_cp(vec![added("dup.txt"), added("dup.txt")]),
        base_cp(vec![OverlayEntry {
            path: rp("README.md"),
            kind: EntryKind::Deleted,
            mode: 0o100644,
        }]),
        base_cp(vec![OverlayEntry {
            path: rp("README.md"),
            kind: EntryKind::Renamed {
                from: rp("README.md"),
                blob: blob.clone(),
            },
            mode: 0o100644,
        }]),
    ] {
        let e = Overlay::from_checkpoint(&bad).validate().unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{bad:?}");
    }
    // A stored checkpoint with entries under a dir later made a deps dir.
    w.write("gen/out.js", b"out\n");
    w.remove("README.md");
    let c1 = cp(&w, None);
    w.write("gen/out.js", b"later\n");
    w.config.deps_dirs.push(rp("gen"));
    let at = tree(&w.ws);
    let rows = latest(&w);
    let e = try_rs(&w, None, id(&c1), None).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("excluded_path"));
    assert_eq!((tree(&w.ws), latest(&w)), (at, rows));
    // No tombstone is silently dropped: a good restore is still exact.
    w.config.deps_dirs.clear();
    rs(&w, id(&c1));
    assert!(!w.ws.join("README.md").exists());
    assert_eq!(w.read("gen/out.js"), b"out\n");
    rs(&w, id(&c0));
    assert_eq!(w.read("README.md"), b"hello\n");
}

#[test]
fn stale_base_and_wrong_root_are_refused() {
    let w = World::new();
    let other = World::new();
    w.write("README.md", b"edited\n");
    other.write("README.md", b"other\n");
    let c1 = cp(&w, None);
    w.write("README.md", b"edited again\n");
    let (mine, theirs) = (tree(&w.ws), tree(&other.ws));
    let rows = latest(&w);
    let req = RestoreRequest {
        target: id(&c1),
        target_base: None,
        paths: None,
        verify: false,
    };
    // Another session's workspace as the root.
    let e = restore(&with_root(&w, &other.ws), &w.base, None, &req).unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::InvalidInput, Some("wrong_root"))
    );
    assert_eq!(
        (tree(&w.ws), tree(&other.ws), latest(&w)),
        (mine.clone(), theirs, rows)
    );
    // The same directory spelled root/. or in another case: accepted.
    let dotted = w.ws.join(".");
    restore(&with_root(&w, &dotted), &w.base, None, &req).expect("root/.");
    assert_eq!(w.read("README.md"), b"edited\n");
    w.write("README.md", b"edited again\n");
    let name = w.ws.file_name().unwrap().to_string_lossy().to_uppercase();
    let upper = w.ws.parent().unwrap().join(name);
    restore(&with_root(&w, &upper), &w.base, None, &req).expect("other case");
    assert_eq!(w.read("README.md"), b"edited\n");
    assert!(!temp(&w).exists());

    // The store moves the workspace to another base generation.
    let (nb, head) = w.next_base(|m| put(m, "up.txt", b"u\n"));
    let staging = w.ws.parent().unwrap().join(".staging-app");
    let n = w
        .store
        .begin_move(&w.sid, &w.repo, &nb.r#gen, &head, &staging, t(50))
        .unwrap();
    w.store
        .set_move_state(
            &w.sid,
            &w.repo,
            n,
            MoveState::Preparing,
            MoveState::Swapping,
            Some(&[]),
            t(51),
        )
        .unwrap();
    w.store.finish_move(&w.sid, &w.repo, n, &[], t(52)).unwrap();
    w.write("README.md", b"after move\n");
    let at = tree(&w.ws);
    let rows = latest(&w);
    let e = try_rs(&w, None, id(&c1), None).unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::InvalidInput, Some("stale_base"))
    );
    assert_eq!(
        (tree(&w.ws), latest(&w)),
        (at, rows),
        "no row, nothing written"
    );
}

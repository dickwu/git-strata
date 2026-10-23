//! Task 4: membership, classification and the cold scan (§5.1, §5.2, §5.5).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::*;
use strata_gitio::Git;
use strata_overlay::{
    DEFAULT_DIRTY_CAPACITY, Overlay, OverlayWarning, Scan, Tracker, WorkspaceRef, compute,
    reconcile_full,
};
use strata_types::{Code, EntryKind};

fn tracker(w: &World) -> Tracker {
    Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY)
}

/// A scan with `t`, checked against the cold scan and §4.3.
fn scan(w: &World, t: &mut Tracker) -> Scan {
    let s = compute(&w.ws_ref(), &w.base, t, None).expect("compute");
    s.overlay.validate().expect("validate");
    let c = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    assert_eq!(s.overlay, c, "compute equals the cold scan");
    s
}

fn cold(w: &World) -> Overlay {
    let o = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    o.validate().expect("validate");
    o
}

fn kind(k: &EntryKind) -> &'static str {
    match k {
        EntryKind::Added { .. } => "Added",
        EntryKind::Modified { .. } => "Modified",
        EntryKind::Deleted => "Deleted",
        EntryKind::Renamed { .. } => "Renamed",
        EntryKind::Untracked { .. } => "Untracked",
    }
}

/// (path, kind, mode) of every entry.
fn kinds(o: &Overlay) -> Vec<(String, &'static str, u32)> {
    o.entries
        .iter()
        .map(|e| (e.path.as_str().to_owned(), kind(&e.kind), e.mode))
        .collect()
}

fn e(p: &str, k: &'static str, mode: u32) -> (String, &'static str, u32) {
    (p.to_owned(), k, mode)
}

fn blob(o: &Overlay, p: &str) -> String {
    let e = o.get(&rp(p)).unwrap_or_else(|| panic!("no entry for {p}"));
    match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Renamed { blob, .. }
        | EntryKind::Untracked { blob } => blob.as_str().to_owned(),
        EntryKind::Deleted => panic!("{p} is deleted"),
    }
}

fn base_blob(w: &World, p: &str) -> String {
    w.base.manifest.entries[&rp(p)].blob.as_str().to_owned()
}

fn chmod(dir: &Path, rel: &str, mode: u32) {
    fs::set_permissions(dir.join(rel), fs::Permissions::from_mode(mode)).unwrap();
}

const F: u32 = 0o100644;
const X: u32 = 0o100755;
const L: u32 = 0o120000;

#[test]
fn untouched_workspace_has_empty_overlay() {
    let w = World::new();
    let before = w.store_files();
    let mut t = tracker(&w);
    assert!(t.needs_full());
    let a = scan(&w, &mut t);
    assert!(a.overlay.is_empty(), "{:?}", kinds(&a.overlay));
    assert!(a.stats.full && a.stats.cold);
    assert!(a.stats.files_hashed > 0);
    assert!(a.changed.is_empty());
    assert!(!t.needs_full());
    assert_eq!(t.base(), Some(&w.base.r#gen));
    let b = scan(&w, &mut t);
    assert_eq!(a.overlay, b.overlay);
    assert_eq!(b.stats.files_hashed, 0, "{:?}", b.stats);
    assert!(b.stats.vouched > 0 && !b.stats.cold);
    assert!(cold(&w).is_empty());
    assert_eq!(t.last(), Some(&b.overlay));
    assert_eq!(w.store_files(), before, "scans never write the store");
}

#[test]
fn restored_to_base_removes_override() {
    let w = World::new();
    let other = World::new();
    other.write("src/lib.rs", b"other session\n");
    let other_before = cold(&other);
    let before = w.store_files();
    let mut t = tracker(&w);
    let modified = |t: &mut Tracker| {
        w.write("README.md", b"changed\n");
        let s = scan(&w, t);
        assert_eq!(kinds(&s.overlay), vec![e("README.md", "Modified", F)]);
        assert_eq!(s.changed, vec![rp("README.md")]);
    };
    let clean = |t: &mut Tracker, how: &str| {
        let s = scan(&w, t);
        assert!(s.overlay.is_empty(), "{how}: {:?}", kinds(&s.overlay));
    };
    // 1. In place.
    modified(&mut t);
    w.write("README.md", b"hello\n");
    clean(&mut t, "in place");
    // 2. A new file renamed over (new inode).
    modified(&mut t);
    w.write("README.md.tmp", b"hello\n");
    fs::rename(w.ws.join("README.md.tmp"), w.ws.join("README.md")).unwrap();
    clean(&mut t, "rename over");
    // 3. Another process.
    modified(&mut t);
    let st = Command::new("/bin/sh")
        .args(["-c", "printf 'hello\\n' > README.md"])
        .current_dir(&w.ws)
        .status()
        .unwrap();
    assert!(st.success());
    clean(&mut t, "other process");
    // 4. cp of the base copy.
    modified(&mut t);
    let st = Command::new("/bin/cp")
        .arg(w.base.dir.join("README.md"))
        .arg(w.ws.join("README.md"))
        .status()
        .unwrap();
    assert!(st.success());
    clean(&mut t, "cp");
    // 5. Removed, then recreated with the same bytes and mode.
    modified(&mut t);
    w.remove("README.md");
    w.write("README.md", b"hello\n");
    chmod(&w.ws, "README.md", 0o644);
    clean(&mut t, "recreate");
    // Renamed away and back.
    fs::rename(w.ws.join("src/lib.rs"), w.ws.join("src/lib2.rs")).unwrap();
    assert!(!scan(&w, &mut t).overlay.is_empty());
    fs::rename(w.ws.join("src/lib2.rs"), w.ws.join("src/lib.rs")).unwrap();
    clean(&mut t, "rename back");
    // Exec bit on and off.
    chmod(&w.ws, "README.md", 0o755);
    assert!(!scan(&w, &mut t).overlay.is_empty());
    chmod(&w.ws, "README.md", 0o644);
    clean(&mut t, "exec bit back");
    // Same-byte overwrite.
    w.write("src/lib.rs", b"pub fn a() {}\n");
    clean(&mut t, "same bytes");
    // Several files written back at once.
    w.write("README.md", b"x\n");
    w.write("src/lib.rs", b"y\n");
    w.write("src/deep/x/y.txt", b"z\n");
    assert_eq!(scan(&w, &mut t).overlay.entries.len(), 3);
    w.write("README.md", b"hello\n");
    w.write("src/lib.rs", b"pub fn a() {}\n");
    w.write("src/deep/x/y.txt", b"deep\n");
    clean(&mut t, "several");
    // Flipping the last byte is a change.
    w.write("README.md", b"hello\r");
    let s = scan(&w, &mut t);
    assert_eq!(kinds(&s.overlay), vec![e("README.md", "Modified", F)]);
    assert_eq!(blob(&s.overlay, "README.md"), blake3_hex(b"hello\r"));
    assert_eq!(w.store_files(), before);
    assert_eq!(cold(&other), other_before, "another session is untouched");
}

#[test]
fn content_change_and_new_files_classified() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "crlf.txt", b"a\r\nb\r\n");
        put(m, "grow.bin", b"head");
    });
    w.write("src/lib.rs", b"pub fn b() {}\n");
    w.write("added.txt", b"added\n");
    w.git_ok(&w.ws, &["add", "added.txt"]);
    w.write("new.txt", b"new\n");
    // Grown with a hole of zeros in the middle, truncated, binary, line endings.
    {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(w.ws.join("grow.bin"))
            .unwrap();
        f.set_len(1 << 20).unwrap();
        use std::os::unix::fs::FileExt;
        f.write_at(b"tail", (1 << 20) - 4).unwrap();
    }
    w.write("README.md", b"he");
    w.write("empty.txt", &[0xff, 0xfe, 0x00, 0x01]);
    w.write("crlf.txt", b"a\nb\n");
    w.write("img.png", b"\x89PNG\r\n\x1a\n");
    w.write("mod.wasm", b"\0asm\x01\0\0\0");
    w.write("new.log", b"ignored\n");
    // Staging a base file does not change its class.
    w.git_ok(&w.ws, &["add", "src/lib.rs"]);
    let mut t = tracker(&w);
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![
            e("README.md", "Modified", F),
            e("added.txt", "Added", F),
            e("crlf.txt", "Modified", F),
            e("empty.txt", "Modified", F),
            e("grow.bin", "Modified", F),
            e("img.png", "Untracked", F),
            e("mod.wasm", "Untracked", F),
            e("new.txt", "Untracked", F),
            e("src/lib.rs", "Modified", F),
        ]
    );
    for p in [
        "README.md",
        "added.txt",
        "crlf.txt",
        "empty.txt",
        "grow.bin",
        "img.png",
        "mod.wasm",
        "new.txt",
        "src/lib.rs",
    ] {
        assert_eq!(
            blob(&o, p),
            blake3_hex(&w.read(p)),
            "{p}: blob is the disk bytes"
        );
    }
}

#[test]
fn exec_bit_change_keeps_blob() {
    let w = World::new();
    let mut t = tracker(&w);
    chmod(&w.ws, "README.md", 0o755);
    let o = scan(&w, &mut t).overlay;
    assert_eq!(kinds(&o), vec![e("README.md", "Modified", X)]);
    assert_eq!(blob(&o, "README.md"), base_blob(&w, "README.md"));
    chmod(&w.ws, "README.md", 0o644);
    assert!(scan(&w, &mut t).overlay.is_empty());
    chmod(&w.ws, "README.md", 0o600);
    assert!(
        scan(&w, &mut t).overlay.is_empty(),
        "git records only the exec bit"
    );
    chmod(&w.ws, "README.md", 0o644);
    // One real change beside three no-ops.
    w.write("src/lib.rs", b"pub fn z() {}\n");
    let st = Command::new("/usr/bin/touch")
        .arg(w.ws.join("README.md"))
        .status()
        .unwrap();
    assert!(st.success());
    chmod(&w.ws, "empty.txt", 0o600);
    w.write("src/deep/x/y.txt", b"deep\n");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(kinds(&o), vec![e("src/lib.rs", "Modified", F)]);
}

#[test]
fn directory_mode_bits_are_never_recorded_or_warned() {
    let w = World::new();
    let mut t = tracker(&w);
    // setgid may be refused for a group we are not in; the scan must not care.
    let _ = fs::set_permissions(&w.ws, fs::Permissions::from_mode(0o2755));
    chmod(&w.ws, "src", 0o1755);
    chmod(&w.ws, "bin", 0o700);
    let s = scan(&w, &mut t);
    assert!(s.overlay.is_empty(), "{:?}", kinds(&s.overlay));
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    // The directory holding the base symlink (the root).
    chmod(&w.ws, ".", 0o700);
    let s = scan(&w, &mut t);
    assert!(s.overlay.get(&rp("link")).is_none());
    assert!(s.overlay.is_empty() && s.warnings.is_empty());
    chmod(&w.ws, ".", 0o755);
}

#[test]
fn file_symlink_swap_is_modified() {
    let w = World::new();
    let mut t = tracker(&w);
    put_link(&w.ws, "README.md", "src/lib.rs");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(kinds(&o), vec![e("README.md", "Modified", L)]);
    assert_eq!(blob(&o, "README.md"), blake3_hex(b"src/lib.rs"));
    w.remove("README.md");
    w.write("README.md", b"other\n");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(kinds(&o), vec![e("README.md", "Modified", F)]);
    w.write("README.md", b"hello\n");
    assert!(scan(&w, &mut t).overlay.is_empty());
    // The base symlink as a regular file holding its target bytes: only the mode moves.
    w.remove("link");
    w.write("link", b"README.md");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(kinds(&o), vec![e("link", "Modified", F)]);
    assert_eq!(blob(&o, "link"), base_blob(&w, "link"));
    put_link(&w.ws, "link", "README.md");
    // Exec bit only: the mode, not the blob.
    chmod(&w.ws, "bin/run.sh", 0o644);
    let o = scan(&w, &mut t).overlay;
    assert_eq!(kinds(&o), vec![e("bin/run.sh", "Modified", F)]);
    assert_eq!(blob(&o, "bin/run.sh"), base_blob(&w, "bin/run.sh"));
}

fn sorted(mut v: Vec<strata_types::RepoPath>) -> Vec<String> {
    v.sort();
    v.into_iter().map(|p| p.as_str().to_owned()).collect()
}

#[test]
fn directory_file_transitions_are_paired() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "d/x", b"x\n");
        put_link(m, "d/l", "x");
        put(m, "f", b"f\n");
        put_link(m, "s", "f");
    });
    let mut t = tracker(&w);
    let restore = |w: &World| {
        for p in ["d", "f", "s"] {
            if fs::symlink_metadata(w.ws.join(p)).is_ok() {
                w.remove(p);
            }
        }
        w.write("d/x", b"x\n");
        put_link(&w.ws, "d/l", "x");
        w.write("f", b"f\n");
        put_link(&w.ws, "s", "f");
    };
    // A directory becomes a file.
    w.remove("d");
    w.write("d", b"now a file\n");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![
            e("d", "Untracked", F),
            e("d/l", "Deleted", 0),
            e("d/x", "Deleted", 0)
        ]
    );
    assert_eq!(sorted(o.paired(&rp("d"))), vec!["d", "d/l", "d/x"]);
    assert_eq!(sorted(o.paired(&rp("d/x"))), vec!["d", "d/l", "d/x"]);
    restore(&w);
    assert!(scan(&w, &mut t).overlay.is_empty());
    // A file and a symlink become directories with children.
    w.remove("f");
    w.write("f/c", b"c\n");
    w.git_ok(&w.ws, &["add", "f/c"]);
    w.remove("s");
    w.write("s/c", b"c\n");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![
            e("f", "Deleted", 0),
            e("f/c", "Added", F),
            e("s", "Deleted", 0),
            e("s/c", "Untracked", F)
        ]
    );
    assert_eq!(sorted(o.paired(&rp("f"))), vec!["f", "f/c"]);
    assert_eq!(sorted(o.paired(&rp("s/c"))), vec!["s", "s/c"]);
    w.git_ok(&w.ws, &["rm", "-q", "--cached", "f/c"]);
    restore(&w);
    assert!(scan(&w, &mut t).overlay.is_empty());
    // A directory becomes a symlink.
    w.remove("d");
    put_link(&w.ws, "d", "src");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![
            e("d", "Untracked", L),
            e("d/l", "Deleted", 0),
            e("d/x", "Deleted", 0)
        ]
    );
    assert_eq!(sorted(o.paired(&rp("d/l"))), vec!["d", "d/l", "d/x"]);
}

#[test]
fn recreated_directory_compares_by_path() {
    let w = World::new();
    let mut t = tracker(&w);
    let expect = vec![
        e("src/deep/x/y.txt", "Modified", F),
        e("src/new.rs", "Untracked", F),
    ];
    // Removed and made again.
    w.remove("src");
    w.write("src/lib.rs", b"pub fn a() {}\n");
    w.write("src/deep/x/y.txt", b"changed\n");
    w.write("src/new.rs", b"new\n");
    assert_eq!(kinds(&scan(&w, &mut t).overlay), expect);
    // The original directory inode moved away, a new one in its place.
    fs::rename(w.ws.join("src"), w.tmp.join("old-src")).unwrap();
    w.write("src/lib.rs", b"pub fn a() {}\n");
    w.write("src/deep/x/y.txt", b"changed\n");
    w.write("src/new.rs", b"new\n");
    assert_eq!(kinds(&scan(&w, &mut t).overlay), expect);
}

#[test]
fn ignored_files_skipped_tracked_never_ignored() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "build/keep.txt", b"keep\n");
    });
    let mut t = tracker(&w);
    w.write("new.log", b"ignored\n");
    assert!(scan(&w, &mut t).overlay.is_empty());
    // A rule now ignores build/ and the tracked src/lib.rs.
    w.write(
        ".gitignore",
        b"*.log\n/node_modules\n/target\nbuild/\nsrc/lib.rs\n",
    );
    w.write("build/keep.txt", b"changed\n");
    w.write("build/other.txt", b"new\n");
    w.write("build/sub/deeper.txt", b"new\n");
    w.write("src/lib.rs", b"pub fn q() {}\n");
    // .ignore is not a git file: its rules do nothing.
    w.write(".ignore", b"secret.txt\n");
    w.write("secret.txt", b"s\n");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![
            e(".gitignore", "Modified", F),
            e(".ignore", "Untracked", F),
            e("build/keep.txt", "Modified", F),
            e("secret.txt", "Untracked", F),
            e("src/lib.rs", "Modified", F),
        ]
    );
}

#[test]
#[ignore = "needs about 130 MB of disk"]
fn size_threshold_never_affects_membership() {
    const N: u64 = 64 * 1024 * 1024 + 1;
    let big = |dir: &Path, rel: &str| {
        let f = fs::File::create(dir.join(rel)).unwrap();
        f.set_len(N).unwrap();
    };
    let w = World::with(|m| {
        default_tree(m);
        big(m, "big.bin");
        big(m, "big-same.bin");
    });
    let mut t = tracker(&w);
    assert!(
        scan(&w, &mut t).overlay.is_empty(),
        "big files are never tombstones"
    );
    big(&w.ws, "new.bin");
    {
        use std::os::unix::fs::FileExt;
        let f = fs::OpenOptions::new()
            .write(true)
            .open(w.ws.join("big.bin"))
            .unwrap();
        f.write_at(b"x", N - 1).unwrap();
    }
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![e("big.bin", "Modified", F), e("new.bin", "Untracked", F)]
    );
}

/// A Unix socket at `rel` in `dir`, made by a child process (socket paths
/// are limited to 104 bytes; a relative path from the cwd stays short).
fn make_socket(dir: &Path, rel: &str) {
    let st = Command::new("/usr/bin/perl")
        .args([
            "-MIO::Socket::UNIX",
            "-e",
            "IO::Socket::UNIX->new(Type => SOCK_STREAM(), Local => $ARGV[0], Listen => 1) or die $!",
            rel,
        ])
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "socket {rel}");
}

fn mkfifo(dir: &Path, rel: &str) {
    let st = Command::new("/usr/bin/mkfifo")
        .arg(dir.join(rel))
        .status()
        .unwrap();
    assert!(st.success());
}

/// compute on another thread, so a scan that opened a FIFO would hang there.
fn scan_bounded(w: &World) -> Scan {
    let ws = w.ws_ref();
    let base = &w.base;
    let (tx, rx) = mpsc::channel();
    thread::scope(|s| {
        s.spawn(move || {
            let mut t = Tracker::new(ws.root, DEFAULT_DIRTY_CAPACITY);
            let _ = tx.send(compute(&ws, base, &mut t, None));
        });
        let r = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the scan never blocks on a FIFO");
        r.expect("compute")
    })
}

#[test]
fn special_files_are_never_opened() {
    let w = World::new();
    mkfifo(&w.ws, "pipe");
    let s = scan_bounded(&w);
    assert!(s.overlay.is_empty(), "{:?}", kinds(&s.overlay));
    assert_eq!(
        s.warnings,
        vec![OverlayWarning::SpecialFile { path: rp("pipe") }]
    );
    // A base file replaced by a FIFO is absent, and warned about.
    w.remove("README.md");
    mkfifo(&w.ws, "README.md");
    let s = scan_bounded(&w);
    assert_eq!(kinds(&s.overlay), vec![e("README.md", "Deleted", 0)]);
    assert!(s.warnings.contains(&OverlayWarning::SpecialFile {
        path: rp("README.md")
    }));
    // Sockets the same.
    make_socket(&w.ws, "sock");
    w.remove("empty.txt");
    make_socket(&w.ws, "empty.txt");
    let s = scan_bounded(&w);
    assert_eq!(
        kinds(&s.overlay),
        vec![e("README.md", "Deleted", 0), e("empty.txt", "Deleted", 0)]
    );
    for p in ["sock", "empty.txt", "pipe"] {
        assert!(
            s.warnings
                .contains(&OverlayWarning::SpecialFile { path: rp(p) }),
            "{p}: {:?}",
            s.warnings
        );
    }
}

#[test]
fn embedded_repo_is_not_entered() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "lib/sub/base.txt", b"base\n");
        put(m, "lib/sub/keep.txt", b"keep\n");
    });
    let mut t = tracker(&w);
    w.git_ok(&w.ws.join("lib/sub"), &["init", "-q"]);
    w.write("lib/sub/new.txt", b"new\n");
    w.write("lib/sub/dir/new2.txt", b"new\n");
    w.write("lib/sub/base.txt", b"changed\n");
    let s = scan(&w, &mut t);
    assert_eq!(
        kinds(&s.overlay),
        vec![e("lib/sub/base.txt", "Modified", F)]
    );
    assert_eq!(
        s.warnings,
        vec![OverlayWarning::EmbeddedRepo {
            path: rp("lib/sub")
        }]
    );
}

#[test]
fn names_backslash_nfd_and_non_utf8() {
    let nfc = "caf\u{e9}.txt";
    let nfd = "cafe\u{301}.txt";
    let w = World::with(|m| {
        default_tree(m);
        put(m, nfc, b"coffee\n");
    });
    assert!(w.base.manifest.entries.contains_key(&rp(nfc)));
    let mut t = tracker(&w);
    w.write("a\\b.txt", b"backslash\n");
    w.write("a/b.txt", b"slash\n");
    // The same name spelled NFD on disk is the NFC path.
    w.remove(nfc);
    w.write(nfd, b"coffee\n");
    w.write("re\u{301}sume\u{301}.txt", b"cv\n");
    let o = scan(&w, &mut t).overlay;
    assert_eq!(
        kinds(&o),
        vec![
            e("a/b.txt", "Untracked", F),
            e("a\\b.txt", "Untracked", F),
            e("r\u{e9}sum\u{e9}.txt", "Untracked", F),
        ]
    );
    w.write(nfd, b"tea\n");
    let o = scan(&w, &mut t).overlay;
    assert!(o.get(&rp(nfc)).is_some());
    // Names that are not UTF-8: APFS refuses to create them, so the
    // conversion is tested directly (context::tests::non_utf8_names_are_not_text).
}

#[test]
fn workspace_git_reads_through_the_main_checkout() {
    let w = World::new();
    let Err(err) = Git::open(&w.ws) else {
        panic!("a workspace is not openable")
    };
    assert_eq!(err.code, Code::InvalidInput);
    let info = w.git.read_index(&w.ws).unwrap();
    let listed: Vec<String> = w
        .git_ok(&w.ws, &["ls-files", "-z"])
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let tracked: Vec<String> = info.tracked.iter().map(|p| p.as_str().to_owned()).collect();
    assert_eq!(tracked, listed);
    let mut t = tracker(&w);
    w.write("README.md", b"x\n");
    let s = scan(&w, &mut t);
    assert_eq!(kinds(&s.overlay), vec![e("README.md", "Modified", F)]);
    // Another repository's Git: gitio's error, passed through; tracker unchanged.
    let other = w.tmp.join("other");
    fs::create_dir_all(&other).unwrap();
    w.git_ok(&other, &["init", "-q"]);
    put(&other, "o.txt", b"o\n");
    w.git_ok(&other, &["add", "-A"]);
    w.git_ok(&other, &["commit", "-q", "-m", "o"]);
    let og = Git::open_with_env(&other, &w.env()).unwrap();
    let direct = og.read_index(&w.ws).unwrap_err();
    let ws = WorkspaceRef {
        git: &og,
        ..w.ws_ref()
    };
    w.write("README.md", b"y\n");
    let err = compute(&ws, &w.base, &mut t, None).unwrap_err();
    assert_eq!(err.code, Code::InvalidInput);
    assert_eq!(err, direct, "passed through unwrapped");
    assert_eq!(t.last(), Some(&s.overlay));
    assert!(!t.needs_full());
}

#[test]
fn excluded_paths_follow_disk_identity() {
    // The seed file .env is untracked and not ignored: X3 keeps it out, in
    // any spelling the volume folds together.
    let w = World::new();
    let check = |w: &World| {
        w.write(".env", b"SECRET=1\n");
        assert!(cold(w).is_empty());
        w.remove(".env");
        w.write(".ENV", b"SECRET=1\n");
        assert!(cold(w).is_empty(), "{:?}", kinds(&cold(w)));
        w.remove(".ENV");
    };
    check(&w);
    // core.ignorecase = false on a case-insensitive volume: the same.
    w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
    assert!(!w.git.read_index(&w.ws).unwrap().ignorecase);
    check(&w);
    // The base tracks .ENV and the seed list says .env: .env is not in X.
    let w = World::with(|m| {
        default_tree(m);
        put(m, ".ENV", b"A=1\n");
    });
    w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
    w.write(".ENV", b"A=2\n");
    assert_eq!(kinds(&cold(&w)), vec![e(".ENV", "Modified", F)]);
}

#[test]
#[ignore = "needs hdiutil (a case-sensitive disk image)"]
fn excluded_paths_follow_disk_identity_case_sensitive() {
    let img = Image::case_sensitive();
    let w = World::with_in(Some(&img.mount), default_tree);
    w.write(".env", b"SECRET=1\n");
    w.write(".ENV", b"OTHER=1\n");
    let o = cold(&w);
    assert_eq!(kinds(&o), vec![e(".ENV", "Untracked", F)]);
}

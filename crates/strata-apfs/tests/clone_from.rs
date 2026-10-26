//! `clone_from_rooted` (§8 Task 5, §7 rows clone_from_rooted_*).
mod common;

use std::cell::Cell;
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use strata_apfs::seams::set_before_open;
use strata_apfs::*;
use strata_types::Code;

struct ClearHook;
impl Drop for ClearHook {
    fn drop(&mut self) {
        set_before_open(None);
    }
}

fn cs(p: &Path) -> CString {
    CString::new(p.as_os_str().as_encoded_bytes()).unwrap()
}

fn set_xattr(p: &Path, k: &str, v: &[u8]) {
    let k = CString::new(k).unwrap();
    let r = unsafe { libc::setxattr(cs(p).as_ptr(), k.as_ptr(), v.as_ptr().cast(), v.len(), 0, libc::XATTR_NOFOLLOW) };
    assert_eq!(r, 0);
}

fn get_xattr(p: &Path, k: &str) -> Option<Vec<u8>> {
    let k = CString::new(k).unwrap();
    let mut buf = vec![0u8; 256];
    let n = unsafe {
        libc::getxattr(cs(p).as_ptr(), k.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, libc::XATTR_NOFOLLOW)
    };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

fn gone(p: &Path) -> bool {
    fs::symlink_metadata(p).is_err()
}

fn is_child() -> bool {
    std::env::var_os("STRATA_APFS_CHILD").is_some()
}

fn run_child(name: &str, secs: u64) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env("STRATA_APFS_CHILD", "1")
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            assert!(st.success(), "child {name} failed");
            return;
        }
        if start.elapsed() > Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child {name} hung past {secs}s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn clone_from_rooted_clones_files_and_directories() {
    let ws = Tmp::new();
    let store = Tmp::new();
    let root = ws.path.clone();
    // A file.
    let f = root.join("f");
    fs::write(&f, "content").unwrap();
    set_mode(&f, 0o600);
    set_xattr(&f, "com.example.k", b"v");
    set_mtime(&f, 1_577_836_800, 123_000_000);
    let start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i128 - 1_000_000_000;
    let dst = store.join("f");
    clone_from_rooted(&root, &rp("f"), &dst, CloneKind::File).unwrap();
    assert_eq!(fs::read(&dst).unwrap(), b"content");
    assert_eq!(mode_of(&dst), 0o600);
    assert_eq!(mtime_ns(&dst), mtime_ns(&f));
    assert_eq!(get_xattr(&dst, "com.example.k").as_deref(), Some(&b"v"[..]));
    assert!(ctime_ns(&dst) >= start);
    assert_ne!(st(&dst).st_ino, st(&f).st_ino);
    fs::write(&dst, "changed").unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"content");
    // A tree.
    let tree = root.join("tree");
    fs::create_dir_all(tree.join("empty")).unwrap();
    fs::write(tree.join("a"), "a").unwrap();
    symlink("a", tree.join("rel")).unwrap();
    symlink("nope", tree.join("dangling")).unwrap();
    make_fifo(&tree.join("fifo"));
    let mut deep = tree.join("deep");
    for i in 0..20 {
        deep.push(format!("n{i}"));
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("leaf"), "leaf").unwrap();
    let before = snapshot(&tree);
    clone_from_rooted(&root, &rp("tree"), &store.join("t1"), CloneKind::Dir).unwrap();
    clone_tree(&tree, &store.join("t2")).unwrap();
    assert_eq!(snapshot(&store.join("t1")), snapshot(&store.join("t2")));
    assert!(store.join("t1/rel").is_symlink());
    assert!(store.join("t1/dangling").is_symlink());
    assert!(fs::symlink_metadata(store.join("t1/fifo")).unwrap().file_type().is_fifo());
    assert_eq!(snapshot(&tree), before);
    // Refusals.
    symlink("f", root.join("lnk")).unwrap();
    make_fifo(&root.join("fifo"));
    make_socket(&root.join("sock"));
    assert_reason(clone_from_rooted(&root, &rp("lnk"), &store.join("x"), CloneKind::File), Code::NotFile, "symlink");
    assert_reason(clone_from_rooted(&root, &rp("fifo"), &store.join("x"), CloneKind::File), Code::NotFile, "fifo");
    assert_reason(clone_from_rooted(&root, &rp("sock"), &store.join("x"), CloneKind::File), Code::NotFile, "socket");
    assert_reason(clone_from_rooted(Path::new("/dev"), &rp("null"), &store.join("x"), CloneKind::File), Code::NotFile, "device");
    assert_code(clone_from_rooted(&root, &rp("missing"), &store.join("x"), CloneKind::File), Code::NoSuchPath);
    assert!(gone(&store.join("x")));
}

#[test]
fn clone_from_rooted_checks_the_source_kind() {
    let ws = Tmp::new();
    let store = Tmp::new();
    let root = ws.path.clone();
    fs::write(root.join("file"), "f").unwrap();
    fs::create_dir(root.join("dir")).unwrap();
    fs::write(root.join("dir/inner"), "i").unwrap();
    make_fifo(&root.join("fifo"));
    make_socket(&root.join("sock"));
    symlink("file", root.join("lfile")).unwrap();
    symlink("dir", root.join("ldir")).unwrap();
    symlink("nope", root.join("ldangle")).unwrap();
    use CloneKind::*;
    type Row<'a> = (&'a str, CloneKind, Option<(Code, Option<&'a str>)>);
    let rows: &[Row] = &[
        ("file", File, None),
        ("dir", File, Some((Code::IsDirectory, None))),
        ("fifo", File, Some((Code::NotFile, Some("fifo")))),
        ("sock", File, Some((Code::NotFile, Some("socket")))),
        ("lfile", File, Some((Code::NotFile, Some("symlink")))),
        ("dir", Dir, None),
        ("file", Dir, Some((Code::NotDirectory, Some("not_a_directory")))),
        ("fifo", Dir, Some((Code::NotDirectory, Some("not_a_directory")))),
        ("ldir", Dir, Some((Code::NotDirectory, Some("symlink")))),
        ("ldangle", Dir, Some((Code::NotDirectory, Some("symlink")))),
    ];
    let before = snapshot(&root);
    for (i, (src, kind, want)) in rows.iter().enumerate() {
        let dst = store.path.join(format!("d{i}"));
        let r = clone_from_rooted(&root, &rp(src), &dst, *kind);
        match want {
            None => {
                r.unwrap_or_else(|e| panic!("{src} {kind:?}: {e:?}"));
                assert!(!gone(&dst));
            }
            Some((code, reason)) => {
                let e = assert_code(r, *code);
                if let Some(reason) = reason {
                    assert_eq!(detail_str(&e, "reason").as_deref(), Some(*reason), "{src} {kind:?}");
                }
                assert!(gone(&dst), "{src} {kind:?} left a dst");
            }
        }
        assert_eq!(snapshot(&root), before, "{src} {kind:?}");
    }
    // Swaps between the caller's look and the open.
    let _c = ClearHook;
    let swap = |name: &'static str, to: Box<dyn Fn(&Path)>| {
        let p = root.join(name);
        set_before_open(Some(Box::new(move |_, n| {
            if n == name && p.is_file() && !p.is_symlink() {
                fs::remove_file(&p).unwrap();
                to(&p);
            }
        })));
    };
    fs::write(root.join("s1"), "f").unwrap();
    swap("s1", Box::new(|p| {
        fs::create_dir(p).unwrap();
        fs::write(p.join("in"), "x").unwrap();
    }));
    assert_code(clone_from_rooted(&root, &rp("s1"), &store.join("s1"), File), Code::IsDirectory);
    set_before_open(None);
    assert!(gone(&store.join("s1")));
    fs::create_dir(root.join("s3")).unwrap();
    let p3 = root.join("s3");
    set_before_open(Some(Box::new(move |_, n| {
        if n == "s3" && p3.is_dir() {
            fs::remove_dir(&p3).unwrap();
            fs::write(&p3, "now a file").unwrap();
        }
    })));
    assert_code(clone_from_rooted(&root, &rp("s3"), &store.join("s3"), Dir), Code::NotDirectory);
    set_before_open(None);
    assert!(gone(&store.join("s3")));
    // Kind before volume; dst before kind.
    let img = Image::create(Fs::Apfs, 32);
    let ir = fs::canonicalize(img.path()).unwrap();
    fs::create_dir(ir.join("d")).unwrap();
    assert_code(clone_from_rooted(&ir, &rp("d"), &store.join("xv"), File), Code::IsDirectory);
    fs::write(store.join("exists"), "e").unwrap();
    assert_code(clone_from_rooted(&root, &rp("dir"), &store.join("exists"), File), Code::Exists);
    assert_eq!(fs::read(store.join("exists")).unwrap(), b"e");
    run_child("clone_from_fifo_swap_child", 20);
}

#[test]
fn clone_from_fifo_swap_child() {
    if !is_child() {
        return;
    }
    let _c = ClearHook;
    let ws = Tmp::new();
    let store = Tmp::new();
    let p = ws.join("s2");
    fs::write(&p, "f").unwrap();
    let q = p.clone();
    set_before_open(Some(Box::new(move |_, n| {
        if n == "s2" && q.is_file() {
            fs::remove_file(&q).unwrap();
            make_fifo(&q);
        }
    })));
    assert_reason(clone_from_rooted(&ws.path, &rp("s2"), &store.join("s2"), CloneKind::File), Code::NotFile, "fifo");
    assert!(gone(&store.join("s2")));
}

#[test]
fn clone_from_rooted_refuses_a_symlinked_parent() {
    let t = Tmp::new();
    let store = Tmp::new();
    let root = t.join("root");
    let outside = t.join("outside");
    fs::create_dir_all(outside.join("a")).unwrap();
    fs::write(outside.join(".env"), "SECRET").unwrap();
    fs::write(outside.join("x"), "SECRET").unwrap();
    fs::write(outside.join("a/b"), "SECRET").unwrap();
    fs::create_dir_all(root.join("real")).unwrap();
    fs::write(root.join("real/x"), "inner").unwrap();
    symlink(&outside, root.join("link")).unwrap();
    symlink(root.join("real"), root.join("inner")).unwrap();
    for (src, kind) in [("link/x", CloneKind::File), ("inner/x", CloneKind::File), ("link/a/b", CloneKind::File), ("link/a", CloneKind::Dir)] {
        let dst = store.join("dst");
        assert_reason(clone_from_rooted(&root, &rp(src), &dst, kind), Code::NotDirectory, "symlink");
        assert!(gone(&dst), "{src}");
    }
    assert_eq!(fs::read_dir(&store.path).unwrap().count(), 0);
    symlink(&root, t.join("rootlink")).unwrap();
    assert_reason(clone_from_rooted(&t.join("rootlink"), &rp("real/x"), &store.join("dst"), CloneKind::File), Code::NotDirectory, "symlink");
    assert!(gone(&store.join("dst")));
}

fn ws_fixture(t: &Tmp) -> (PathBuf, PathBuf) {
    let root = t.join("root");
    let outside = t.join("outside");
    fs::create_dir_all(root.join("ws/a")).unwrap();
    fs::create_dir_all(root.join("ws/d")).unwrap();
    fs::write(root.join("ws/a/f"), "INSIDE").unwrap();
    fs::write(root.join("ws/d/g"), "INSIDE-G").unwrap();
    fs::create_dir_all(outside.join("dir")).unwrap();
    fs::write(outside.join("secret"), "OUTSIDE").unwrap();
    fs::write(outside.join("dir/g"), "OUTSIDE-G").unwrap();
    (root, outside)
}

#[test]
fn clone_from_rooted_clones_the_opened_object_after_a_swap() {
    let _c = ClearHook;
    // Round 1: the file becomes a symlink after it was opened.
    let t = Tmp::new();
    let store = Tmp::new();
    let (root, outside) = ws_fixture(&t);
    {
        let (f, secret) = (root.join("ws/a/f"), outside.join("secret"));
        set_before_open(Some(Box::new(move |_, n| {
            if n == "blob" && !f.is_symlink() {
                fs::remove_file(&f).unwrap();
                symlink(&secret, &f).unwrap();
            }
        })));
    }
    clone_from_rooted(&root, &rp("ws/a/f"), &store.join("blob"), CloneKind::File).unwrap();
    set_before_open(None);
    assert!(root.join("ws/a/f").is_symlink());
    assert_eq!(fs::read(store.join("blob")).unwrap(), b"INSIDE");

    // Round 2: the directory becomes a symlink after it was opened.
    let t = Tmp::new();
    let store = Tmp::new();
    let (root, outside) = ws_fixture(&t);
    {
        let (d, old, od) = (root.join("ws/d"), root.join("ws/d_old"), outside.join("dir"));
        set_before_open(Some(Box::new(move |_, n| {
            if n == "carry" && !d.is_symlink() {
                fs::rename(&d, &old).unwrap();
                symlink(&od, &d).unwrap();
            }
        })));
    }
    clone_from_rooted(&root, &rp("ws/d"), &store.join("carry"), CloneKind::Dir).unwrap();
    set_before_open(None);
    assert_eq!(fs::read(store.join("carry/g")).unwrap(), b"INSIDE-G");
    // Control: the path-based clone follows the swapped parent.
    clone_file(&root.join("ws/d/g"), &store.join("control")).unwrap();
    assert_eq!(fs::read(store.join("control")).unwrap(), b"OUTSIDE-G");

    // Round 3: a resolved ancestor becomes a symlink mid-resolution.
    let t = Tmp::new();
    let store = Tmp::new();
    let (root, outside) = ws_fixture(&t);
    fs::create_dir_all(outside.join("a")).unwrap();
    fs::write(outside.join("a/f"), "OUTSIDE").unwrap();
    let fired = Rc::new(Cell::new(false));
    {
        let (ws, old, out, fired) = (root.join("ws"), root.join("ws_old"), outside.clone(), fired.clone());
        set_before_open(Some(Box::new(move |_, n| {
            if n == "a" && !fired.get() {
                fired.set(true);
                fs::rename(&ws, &old).unwrap();
                symlink(&out, &ws).unwrap();
            }
        })));
    }
    clone_from_rooted(&root, &rp("ws/a/f"), &store.join("blob"), CloneKind::File).unwrap();
    set_before_open(None);
    assert!(fired.get());
    assert_eq!(fs::read(store.join("blob")).unwrap(), b"INSIDE");
    assert_reason(clone_from_rooted(&root, &rp("ws/a/f"), &store.join("blob2"), CloneKind::File), Code::NotDirectory, "symlink");
    assert!(gone(&store.join("blob2")));
}

#[test]
fn clone_from_rooted_error_codes() {
    let ws = Tmp::new();
    let store = Tmp::new();
    let root = ws.path.clone();
    fs::write(root.join("f"), "f").unwrap();
    fs::write(store.join("efile"), "e").unwrap();
    fs::create_dir(store.join("edir")).unwrap();
    symlink("nope", store.join("elink")).unwrap();
    fs::write(store.join("plainfile"), "p").unwrap();
    for e in ["efile", "edir", "elink"] {
        let before = snapshot(&store.path);
        let err = assert_code(clone_from_rooted(&root, &rp("f"), &store.join(e), CloneKind::File), Code::Exists);
        assert_eq!(detail_bool(&err, "dst_exists"), Some(true), "{e}");
        assert_eq!(snapshot(&store.path), before, "{e}");
    }
    assert_code(clone_from_rooted(&root, &rp("f"), &store.join("missing/x"), CloneKind::File), Code::NoSuchPath);
    assert_code(clone_from_rooted(&root, &rp("f"), &store.join("plainfile/x"), CloneKind::File), Code::NotDirectory);
    let slash = PathBuf::from(format!("{}/", store.join("new").display()));
    assert_reason(clone_from_rooted(&root, &rp("f"), &slash, CloneKind::File), Code::InvalidInput, "trailing_slash");
    let long = store.path.join("n".repeat(256));
    assert_reason(clone_from_rooted(&root, &rp("f"), &long, CloneKind::File), Code::InvalidInput, "name_too_long");
    assert!(gone(&store.join("new")) && gone(&store.join("missing")));
    // Two APFS volumes.
    let a = Image::create(Fs::Apfs, 32);
    let b = Image::create(Fs::Apfs, 32);
    let ar = fs::canonicalize(a.path()).unwrap();
    let br = fs::canonicalize(b.path()).unwrap();
    fs::write(ar.join("big"), vec![7u8; 1 << 20]).unwrap();
    let free = b.free_bytes();
    let e = assert_code(clone_from_rooted(&ar, &rp("big"), &br.join("x"), CloneKind::File), Code::CrossVolume);
    assert_ne!(detail_i64(&e, "src_dev"), detail_i64(&e, "dst_dev"));
    assert!(gone(&br.join("x")));
    assert!(b.free_bytes() >= free);
    // HFS+.
    let h = Image::create(Fs::Hfs, 32);
    let hr = fs::canonicalize(h.path()).unwrap();
    fs::write(hr.join("f"), "f").unwrap();
    assert_code(clone_from_rooted(&hr, &rp("f"), &hr.join("x"), CloneKind::File), Code::Unsupported);
    assert!(gone(&hr.join("x")));
    // A flagged source clones, flags and all.
    fs::write(root.join("u"), "u").unwrap();
    chflags(&root.join("u"), "uchg");
    clone_from_rooted(&root, &rp("u"), &store.join("u"), CloneKind::File).unwrap();
    assert!(flags_of(&store.join("u")) & UF_IMMUTABLE != 0);
    chflags(&store.join("u"), "nouchg");
    chflags(&root.join("u"), "nouchg");
}

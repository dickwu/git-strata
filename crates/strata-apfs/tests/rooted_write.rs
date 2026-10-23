//! §7 rows for the rooted destination variants (Task 4).
mod common;

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::*;
use strata_apfs::seams::{MetaChange, set_before_meta, set_before_open};
use strata_apfs::*;
use strata_types::{Code, RepoPath, StrataError};

struct ClearHooks;
impl Drop for ClearHooks {
    fn drop(&mut self) {
        set_before_open(None);
        set_before_meta(None);
    }
}

type Variant = (&'static str, Box<dyn Fn(&Path, &RepoPath) -> std::result::Result<(), StrataError>>);

/// Every destination variant, with sources living in `srcs`.
fn variants(srcs: &Path) -> Vec<Variant> {
    let sf = srcs.join("srcfile");
    let sd = srcs.join("srcdir");
    if !sf.exists() {
        fs::write(&sf, "src").unwrap();
        fs::create_dir_all(sd.join("in")).unwrap();
        fs::write(sd.join("in/f"), "f").unwrap();
    }
    let sf2 = sf.clone();
    vec![
        ("clone_file_rooted", Box::new(move |r, p| clone_file_rooted(&sf, r, p, None))),
        ("clone_file_rooted(mode)", Box::new(move |r, p| clone_file_rooted(&sf2, r, p, Some(0o644)))),
        ("clone_tree_rooted", Box::new(move |r, p| clone_tree_rooted(&sd, r, p))),
        ("create_file_rooted", Box::new(|r, p| create_file_rooted(r, p, 0o644).map(drop))),
        ("symlink_rooted", Box::new(|r, p| symlink_rooted(r, p, OsStr::new("target")))),
        ("mkdir_rooted(false)", Box::new(|r, p| mkdir_rooted(r, p, 0o755, false))),
        ("mkdir_rooted(true)", Box::new(|r, p| mkdir_rooted(r, p, 0o755, true))),
    ]
}

#[test]
fn rooted_destination_refuses_a_symlinked_parent() {
    let t = Tmp::new();
    let outside = t.join("outside");
    let root = t.join("root");
    fs::create_dir_all(outside.join("a")).unwrap();
    fs::write(outside.join("keep"), "k").unwrap();
    fs::create_dir_all(root.join("realdir/a")).unwrap();
    symlink(&outside, root.join("link")).unwrap();
    symlink("realdir", root.join("inner")).unwrap();
    let srcs = t.join("srcs");
    fs::create_dir(&srcs).unwrap();
    let (so, sr) = (snapshot(&outside), snapshot(&root));
    for (name, v) in variants(&srcs) {
        for p in ["link/x", "inner/x", "link/a/b"] {
            assert_reason(v(&root, &rp(p)), Code::NotDirectory, "symlink");
            assert_eq!(snapshot(&outside), so, "{name} {p}");
            assert_eq!(snapshot(&root), sr, "{name} {p}");
        }
    }
}

#[test]
fn rooted_destination_never_overwrites() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir/inside"), "i").unwrap();
    fs::write(root.join("file"), "file").unwrap();
    fs::write(t.join("pointee"), "pointee").unwrap();
    symlink(t.join("pointee"), root.join("lnk")).unwrap();
    symlink(t.join("nowhere"), root.join("dangling")).unwrap();
    let srcs = t.join("srcs");
    fs::create_dir(&srcs).unwrap();
    let (sr, sp) = (snapshot(&root), snapshot(&t.join("pointee")));
    for (name, v) in variants(&srcs) {
        for dst in ["file", "dir", "lnk", "dangling"] {
            if name == "mkdir_rooted(true)" && dst == "dir" {
                v(&root, &rp(dst)).unwrap();
            } else {
                let e = v(&root, &rp(dst)).expect_err(name);
                assert_eq!(e.code, Code::Exists, "{name} {dst}: {e:?}");
            }
            assert_eq!(snapshot(&root), sr, "{name} {dst}");
            assert_eq!(snapshot(&t.join("pointee")), sp);
            assert!(!t.join("nowhere").exists(), "{name} followed the dangling link");
        }
    }
}

#[test]
fn rooted_destination_swap_after_resolution_stays_inside() {
    let _c = ClearHooks;
    let srcs_t = Tmp::new();
    let srcs = srcs_t.join("srcs");
    fs::create_dir(&srcs).unwrap();
    for (name, v) in variants(&srcs) {
        let t = Tmp::new();
        let root = t.join("root");
        let outside = t.join("outside");
        fs::create_dir_all(root.join("par")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let (r2, o2) = (root.clone(), outside.clone());
        let fired = Rc::new(Cell::new(false));
        let f2 = fired.clone();
        set_before_open(Some(Box::new(move |_, n| {
            if n == OsStr::new("x") && !f2.get() {
                f2.set(true);
                fs::rename(r2.join("par"), r2.join("moved")).unwrap();
                symlink(&o2, r2.join("par")).unwrap();
            }
        })));
        let res = v(&root, &rp("par/x"));
        set_before_open(None);
        assert!(fired.get(), "{name}: seam did not fire");
        res.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(fs::symlink_metadata(root.join("moved/x")).is_ok(), "{name}: not in the pinned parent");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "{name}: escaped the root");
    }
}

#[test]
fn mkdir_rooted_parents_semantics() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir(&root).unwrap();
    mkdir_rooted(&root, &rp("a/b/c"), 0o755, true).unwrap();
    assert!(root.join("a/b/c").is_dir());
    mkdir_rooted(&root, &rp("a/b/c"), 0o755, true).unwrap();
    fs::write(root.join("a/file"), "f").unwrap();
    let before = snapshot(&root);
    assert_code(mkdir_rooted(&root, &rp("a/file/x/y"), 0o755, true), Code::NotDirectory);
    assert_eq!(snapshot(&root), before);
    let e = assert_code(mkdir_rooted(&root, &rp("p/q"), 0o555, true), Code::Unknown);
    assert_eq!(e.detail.as_ref().and_then(|d| d.get("created")).cloned(), Some(serde_json::json!(["p"])));
    assert!(root.join("p").is_dir());
    assert!(!root.join("p/q").exists());
    set_mode(&root.join("p"), 0o755);
}

#[test]
fn clone_file_rooted_sets_the_requested_mode() {
    let _c = ClearHooks;
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir(&root).unwrap();
    let src = t.join("src");
    fs::write(&src, "content").unwrap();
    set_mode(&src, 0o600);
    for (dst, m, want) in [("a", Some(0o644), 0o644), ("b", Some(0o755), 0o755), ("c", None, 0o600), ("d", Some(0o4755), 0o755)] {
        clone_file_rooted(&src, &root, &rp(dst), m).unwrap();
        assert_eq!(mode_of(&root.join(dst)), want, "{dst}");
        assert_eq!(mode_of(&src), 0o600);
    }
    symlink("whatever", t.join("srclink")).unwrap();
    clone_file_rooted(&t.join("srclink"), &root, &rp("l"), Some(0o644)).unwrap();
    assert_eq!(fs::read_link(root.join("l")).unwrap(), Path::new("whatever"));

    // Seam between the clone and the re-open: dst becomes a hard link to an
    // outside 0600 file.
    let secret = t.join("secret");
    fs::write(&secret, "secret").unwrap();
    set_mode(&secret, 0o600);
    let ctime_after_link = Rc::new(Cell::new(0i128));
    let (r2, s2, c2) = (root.clone(), secret.clone(), ctime_after_link.clone());
    set_before_open(Some(Box::new(move |step, n| {
        if step == 1 && n == OsStr::new("hl") {
            fs::remove_file(r2.join("hl")).unwrap();
            fs::hard_link(&s2, r2.join("hl")).unwrap();
            c2.set(ctime_ns(&s2));
        }
    })));
    let e = assert_code(clone_file_rooted(&src, &root, &rp("hl"), Some(0o644)), Code::Unknown);
    assert_eq!(detail_bool(&e, "dst_exists"), Some(true));
    assert_eq!(mode_of(&secret), 0o600);
    assert_eq!(ctime_ns(&secret), ctime_after_link.get());
    // ... or a symlink to it.
    let (r2, s2) = (root.clone(), secret.clone());
    set_before_open(Some(Box::new(move |step, n| {
        if step == 1 && n == OsStr::new("sl") {
            fs::remove_file(r2.join("sl")).unwrap();
            symlink(&s2, r2.join("sl")).unwrap();
        }
    })));
    let before = (mode_of(&secret), ctime_ns(&secret));
    let e = assert_code(clone_file_rooted(&src, &root, &rp("sl"), Some(0o644)), Code::Unknown);
    set_before_open(None);
    assert_eq!(detail_bool(&e, "dst_exists"), Some(true));
    assert_eq!((mode_of(&secret), ctime_ns(&secret)), before);

    // src carries uchg.
    let usrc = t.join("usrc");
    fs::write(&usrc, "u").unwrap();
    set_mode(&usrc, 0o600);
    chflags(&usrc, "uchg");
    let e = assert_code(clone_file_rooted(&usrc, &root, &rp("u1"), Some(0o644)), Code::Unknown);
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EPERM as i64), "{e:?}");
    assert_eq!(detail_bool(&e, "dst_exists"), Some(true));
    assert!(root.join("u1").exists());
    clone_file_rooted(&usrc, &root, &rp("u2"), None).unwrap();
    let h = open_rooted_file(&root, &rp("u2")).unwrap();
    set_file_meta(&h, &FileMeta { clear_flags: true, mode: Some(0o644), ..Default::default() }).unwrap();
    assert_eq!((flags_of(&root.join("u2")), mode_of(&root.join("u2"))), (0, 0o644));
    chflags(&usrc, "nouchg");
    chflags(&root.join("u1"), "nouchg");

    // The two seams never get confused (n4).
    let src644 = t.join("src644");
    fs::write(&src644, "x").unwrap();
    set_mode(&src644, 0o644);
    for dst in ["mode", "flags", "mtime"] {
        for (s, metas) in [(&src, 1usize), (&src644, 0usize)] {
            let opens: Rc<RefCell<Vec<(usize, OsString)>>> = Rc::default();
            let mets: Rc<RefCell<Vec<(usize, MetaChange)>>> = Rc::default();
            let (o2, m2) = (opens.clone(), mets.clone());
            set_before_open(Some(Box::new(move |i, n| o2.borrow_mut().push((i, n.to_owned())))));
            set_before_meta(Some(Box::new(move |i, c| m2.borrow_mut().push((i, c)))));
            let name = format!("{dst}-{metas}");
            let _ = fs::remove_file(root.join(&name));
            clone_file_rooted(s, &root, &rp(&name), Some(0o644)).unwrap();
            set_before_open(None);
            set_before_meta(None);
            let n = OsString::from(&name);
            assert_eq!(*opens.borrow(), vec![(0, n.clone()), (1, n)], "{name}");
            let want: Vec<_> = (0..metas).map(|_| (0, MetaChange::Mode)).collect();
            assert_eq!(*mets.borrow(), want, "{name}");
        }
    }
    // And literally named `mode`.
    let opens: Rc<RefCell<Vec<(usize, OsString)>>> = Rc::default();
    let o2 = opens.clone();
    set_before_open(Some(Box::new(move |i, n| o2.borrow_mut().push((i, n.to_owned())))));
    clone_file_rooted(&src, &root, &rp("mode"), Some(0o644)).unwrap();
    set_before_open(None);
    assert_eq!(*opens.borrow(), vec![(0, "mode".into()), (1, "mode".into())]);
    assert_eq!(mode_of(&root.join("mode")), 0o644);
}

#[test]
fn clone_tree_rooted_is_atomic_and_refuses_cross_volume() {
    let a = Image::create(Fs::Apfs, 32);
    let b = Image::create(Fs::Apfs, 32);
    let tree = a.path().join("tree");
    fs::create_dir_all(tree.join("x/y")).unwrap();
    fs::write(tree.join("x/y/f"), "f").unwrap();
    symlink("x", tree.join("l")).unwrap();
    let broot = fs::canonicalize(b.path()).unwrap();
    assert_code(clone_tree_rooted(&tree, &broot, &rp("dst")), Code::CrossVolume);
    assert!(!broot.join("dst").exists());

    let aroot = fs::canonicalize(a.path()).unwrap();
    let tree = aroot.join("tree");
    assert_code(clone_tree_rooted(&tree, &aroot, &rp("missing/dst")), Code::NoSuchPath);
    assert_reason(clone_tree_rooted(&tree, &aroot, &rp("tree/x/inner")), Code::InvalidInput, "dst_inside_src");
    assert!(!tree.join("x/inner").exists());
    clone_tree_rooted(&tree, &aroot, &rp("rooted")).unwrap();
    clone_tree(&tree, &aroot.join("plain")).unwrap();
    let strip = |base: &Path| -> Vec<(PathBuf, String)> {
        snapshot(base).into_iter().map(|(p, h)| (p.strip_prefix(base).unwrap_or(&p).to_path_buf(), h)).collect()
    };
    assert_eq!(strip(&aroot.join("rooted")), strip(&aroot.join("plain")));
    assert_eq!(strip(&aroot.join("rooted")), strip(&tree));
}

#[test]
fn create_file_rooted_and_symlink_rooted_basics() {
    use std::io::Write;
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir(&root).unwrap();
    let mut f = create_file_rooted(&root, &rp("new"), 0o666).unwrap();
    assert_eq!(fs::metadata(root.join("new")).unwrap().len(), 0);
    let umask = unsafe {
        let m = libc::umask(0o022);
        libc::umask(m);
        m
    } as u32;
    assert_eq!(mode_of(&root.join("new")), 0o666 & !umask);
    f.write_all(b"hello").unwrap();
    set_file_meta(&f, &FileMeta { mode: Some(0o666), ..Default::default() }).unwrap();
    assert_eq!(mode_of(&root.join("new")), 0o666);
    assert_eq!(fs::read(root.join("new")).unwrap(), b"hello");
    assert_code(create_file_rooted(&root, &rp("new"), 0o644), Code::Exists);

    for (name, target) in [("l1", b"../x".to_vec()), ("l2", b"\xff\xfe/odd \x01name".to_vec()), ("l3", b"plain".to_vec())] {
        let target = OsString::from_vec(target);
        symlink_rooted(&root, &rp(name), &target).unwrap();
        assert_eq!(fs::read_link(root.join(name)).unwrap().as_os_str().as_bytes(), target.as_bytes());
    }
    assert_reason(symlink_rooted(&root, &rp("l4"), OsStr::new("")), Code::InvalidInput, "empty_target");
    assert!(fs::symlink_metadata(root.join("l4")).is_err());
}

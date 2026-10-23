//! §7 rows for `rename_rooted` and `full_fsync` (Task 4).
mod common;

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::rc::Rc;

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

fn ino(p: &Path) -> u64 {
    st(p).st_ino
}

fn names(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn rename_rooted_excl_never_overwrites() {
    let t = Tmp::new();
    let root = t.join("root");
    for d in ["a", "b", "d"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("a/x"), "x").unwrap();
    fs::write(root.join("b/y"), "y").unwrap();
    symlink(t.join("nowhere"), root.join("dang")).unwrap();
    let before = snapshot(&root);
    for to in ["b/y", "d", "dang"] {
        let e = assert_code(rename_rooted(&root, &rp("a/x"), &rp(to), RenameMode::Excl), Code::Exists);
        assert_eq!(detail_str(&e, "path").as_deref(), Some(root.join(to).to_str().unwrap()), "{to}");
        assert_eq!(snapshot(&root), before);
        assert!(!t.join("nowhere").exists());
    }
    let x = root.join("a/x");
    let (i, m, c) = (ino(&x), mtime_ns(&x), ctime_ns(&x));
    let (ma, mb) = (mtime_ns(&root.join("a")), mtime_ns(&root.join("b")));
    tick();
    rename_rooted(&root, &rp("a/x"), &rp("b/z"), RenameMode::Excl).unwrap();
    let z = root.join("b/z");
    assert_eq!(fs::read(&z).unwrap(), b"x");
    assert_eq!((ino(&z), mtime_ns(&z)), (i, m));
    assert!(ctime_ns(&z) > c);
    assert!(mtime_ns(&root.join("a")) > ma && mtime_ns(&root.join("b")) > mb);
    assert_reason(rename_rooted(&root, &rp("a/x"), &rp("b/w"), RenameMode::Excl), Code::NoSuchPath, "from_missing");
    assert_reason(rename_rooted(&root, &rp("b/z"), &rp("b/z"), RenameMode::Excl), Code::InvalidInput, "same_path");
    assert_eq!(fs::read(&z).unwrap(), b"x");
}

#[test]
fn rename_rooted_excl_renames_links_and_case_only_spellings() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("dir/sub")).unwrap();
    fs::write(root.join("dir/sub/f"), "f").unwrap();
    symlink("dir", root.join("ldir")).unwrap();
    symlink("nowhere", root.join("ldang")).unwrap();
    let sd = snapshot(&root.join("dir"));
    rename_rooted(&root, &rp("ldir"), &rp("ldir2"), RenameMode::Excl).unwrap();
    rename_rooted(&root, &rp("ldang"), &rp("ldang2"), RenameMode::Excl).unwrap();
    assert_eq!(fs::read_link(root.join("ldir2")).unwrap(), Path::new("dir"));
    assert_eq!(fs::read_link(root.join("ldang2")).unwrap(), Path::new("nowhere"));
    assert_eq!(snapshot(&root.join("dir")), sd);

    let ci = Image::create(Fs::Apfs, 32);
    let r = fs::canonicalize(ci.path()).unwrap();
    fs::write(r.join("Foo.txt"), "foo").unwrap();
    let i = ino(&r.join("Foo.txt"));
    rename_rooted(&r, &rp("Foo.txt"), &rp("foo.txt"), RenameMode::Excl).unwrap();
    assert!(names(&r).contains(&"foo.txt".to_string()));
    assert!(!names(&r).contains(&"Foo.txt".to_string()));
    assert_eq!((fs::read(r.join("foo.txt")).unwrap(), ino(&r.join("foo.txt"))), (b"foo".to_vec(), i));
    // NFD on disk, NFC RepoPath.
    fs::write(r.join("e\u{301}t\u{e9}"), "nfd").unwrap();
    rename_rooted(&r, &rp("\u{e9}t\u{e9}"), &rp("renamed"), RenameMode::Excl).unwrap();
    assert_eq!(fs::read(r.join("renamed")).unwrap(), b"nfd");

    let cs = Image::create(Fs::ApfsCaseSensitive, 32);
    let r = fs::canonicalize(cs.path()).unwrap();
    fs::write(r.join("a"), "a").unwrap();
    rename_rooted(&r, &rp("a"), &rp("A"), RenameMode::Excl).unwrap();
    assert_eq!(names(&r).iter().filter(|n| !n.starts_with('.')).cloned().collect::<Vec<_>>(), vec!["A".to_string()]);
    fs::write(r.join("a"), "a2").unwrap();
    assert_code(rename_rooted(&r, &rp("a"), &rp("A"), RenameMode::Excl), Code::Exists);
}

#[test]
fn rename_rooted_swap_exchanges_two_existing_entries() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("p1")).unwrap();
    fs::create_dir_all(root.join("p2")).unwrap();
    let mk = |p: &str, kind: &str| {
        let full = root.join(p);
        match kind {
            "file" => fs::write(&full, p).unwrap(),
            "dir" => {
                fs::create_dir(&full).unwrap();
                fs::write(full.join("in"), p).unwrap();
            }
            _ => symlink(p, &full).unwrap(),
        }
    };
    let content = |p: &Path| -> String {
        let m = fs::symlink_metadata(p).unwrap();
        if m.is_dir() {
            fs::read_to_string(p.join("in")).unwrap()
        } else if m.file_type().is_symlink() {
            fs::read_link(p).unwrap().to_string_lossy().into_owned()
        } else {
            fs::read_to_string(p).unwrap()
        }
    };
    let cases = [
        ("p1/f1", "file", "p1/f2", "file"),
        ("p1/f3", "file", "p1/d1", "dir"),
        ("p1/d2", "dir", "p1/d3", "dir"),
        ("p1/d4", "dir", "p2/d5", "dir"),
        ("p1/l1", "link", "p2/f4", "file"),
    ];
    for (a, ka, b, kb) in cases {
        mk(a, ka);
        mk(b, kb);
        let (pa, pb) = (root.join(a), root.join(b));
        let (ia, ib, ca, cb) = (ino(&pa), ino(&pb), content(&pa), content(&pb));
        rename_rooted(&root, &rp(a), &rp(b), RenameMode::Swap).unwrap_or_else(|e| panic!("{a}<->{b}: {e:?}"));
        assert_eq!((ino(&pa), ino(&pb)), (ib, ia), "{a}<->{b}");
        assert_eq!((content(&pa), content(&pb)), (cb, ca), "{a}<->{b}");
    }
    let before = snapshot(&root);
    assert_reason(rename_rooted(&root, &rp("p1/none"), &rp("p1/f1"), RenameMode::Swap), Code::NoSuchPath, "from_missing");
    assert_reason(rename_rooted(&root, &rp("p1/f1"), &rp("p1/none"), RenameMode::Swap), Code::NoSuchPath, "to_missing");
    assert_eq!(snapshot(&root), before);
    // overlay restore: staged new file swapped into the workspace.
    fs::create_dir_all(root.join("ws")).unwrap();
    fs::create_dir_all(root.join(".staging")).unwrap();
    fs::write(root.join("ws/file"), "old").unwrap();
    fs::write(root.join(".staging/file"), "new").unwrap();
    rename_rooted(&root, &rp(".staging/file"), &rp("ws/file"), RenameMode::Swap).unwrap();
    assert_eq!(fs::read(root.join("ws/file")).unwrap(), b"new");
    assert_eq!(fs::read(root.join(".staging/file")).unwrap(), b"old");
    // a directory and its own descendant.
    fs::create_dir_all(root.join("anc/desc")).unwrap();
    assert_code(rename_rooted(&root, &rp("anc"), &rp("anc/desc"), RenameMode::Swap), Code::InvalidInput);
    assert_code(rename_rooted(&root, &rp("anc/desc"), &rp("anc"), RenameMode::Swap), Code::InvalidInput);
    // between two volumes.
    let mut img = Image::create(Fs::Apfs, 32);
    fs::create_dir(root.join("mnt")).unwrap();
    img.attach_at(&root.join("mnt"));
    fs::write(root.join("mnt/g"), "g").unwrap();
    assert_code(rename_rooted(&root, &rp("p1/f1"), &rp("mnt/g"), RenameMode::Swap), Code::CrossVolume);
    img.detach();
    // HFS+.
    let hfs = Image::create(Fs::Hfs, 32);
    let h = fs::canonicalize(hfs.path()).unwrap();
    fs::write(h.join("a"), "a").unwrap();
    fs::write(h.join("b"), "b").unwrap();
    let e = assert_code(rename_rooted(&h, &rp("a"), &rp("b"), RenameMode::Swap), Code::Unsupported);
    assert_eq!(detail_str(&e, "fs_type").as_deref(), Some("hfs"));
    assert_eq!(fs::read(h.join("a")).unwrap(), b"a");
}

#[test]
fn rename_rooted_error_mapping() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("f"), "f").unwrap();
    fs::write(root.join("g"), "g").unwrap();
    fs::create_dir_all(root.join("ro")).unwrap();
    fs::write(root.join("ro/inside"), "i").unwrap();
    fs::write(root.join("u"), "u").unwrap();
    fs::create_dir(root.join("mnt")).unwrap();
    let mut img = Image::create(Fs::Apfs, 32);
    img.attach_at(&root.join("mnt"));
    fs::write(root.join("mnt/g"), "g").unwrap();
    set_mode(&root.join("ro"), 0o555);
    chflags(&root.join("u"), "uchg");
    let long = "n".repeat(256);
    let cases: Vec<(&str, String, Code, Option<&str>)> = vec![
        ("f", "missing/x".into(), Code::NoSuchPath, None),
        ("f", "g/x".into(), Code::NotDirectory, None),
        ("a", "a/b/c".into(), Code::InvalidInput, Some("into_itself")),
        ("f", long.clone(), Code::InvalidInput, Some("name_too_long")),
        ("f", "mnt/f".into(), Code::CrossVolume, None),
        ("mnt/g", "g2".into(), Code::CrossVolume, None),
        ("ro/inside", "moved".into(), Code::ReadOnly, Some("no_permission")),
        ("u", "u2".into(), Code::ReadOnly, Some("immutable")),
    ];
    let before = snapshot(&root);
    for (from, to, code, reason) in cases {
        let e = assert_code(rename_rooted(&root, &rp(from), &rp(&to), RenameMode::Excl), code);
        if let Some(r) = reason {
            assert_eq!(detail_str(&e, "reason").as_deref(), Some(r), "{from} -> {to}: {e:?}");
        }
        if code == Code::CrossVolume {
            assert_ne!(detail_i64(&e, "src_dev"), detail_i64(&e, "dst_dev"), "{e:?}");
            assert!(detail_i64(&e, "src_dev").is_some());
        }
        assert_eq!(snapshot(&root), before, "{from} -> {to}");
    }
    // Between two APFS images (root on A, B mounted inside it).
    let mut b = Image::create(Fs::Apfs, 32);
    let a = Image::create(Fs::Apfs, 32);
    let ar = fs::canonicalize(a.path()).unwrap();
    fs::create_dir(ar.join("bm")).unwrap();
    b.attach_at(&ar.join("bm"));
    fs::write(ar.join("x"), "x").unwrap();
    let e = assert_code(rename_rooted(&ar, &rp("x"), &rp("bm/x"), RenameMode::Excl), Code::CrossVolume);
    assert_ne!(detail_i64(&e, "src_dev"), detail_i64(&e, "dst_dev"));
    b.detach();
    img.detach();
    chflags(&root.join("u"), "nouchg");
    set_mode(&root.join("ro"), 0o755);
}

#[test]
fn rename_rooted_refuses_symlinked_parents_on_both_sides() {
    let _c = ClearHook;
    let t = Tmp::new();
    let root = t.join("root");
    let outside = t.join("outside");
    fs::create_dir_all(root.join("real")).unwrap();
    fs::create_dir_all(root.join("plain")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    for d in [&outside, &root.join("real")] {
        fs::write(d.join("x"), "x").unwrap();
        fs::write(d.join("y"), "y").unwrap();
    }
    fs::write(root.join("plain/x"), "px").unwrap();
    fs::write(root.join("plain/y"), "py").unwrap();
    symlink(&outside, root.join("out")).unwrap();
    symlink("real", root.join("inn")).unwrap();
    let (so, sr) = (snapshot(&outside), snapshot(&root));
    let froms = ["out/x", "inn/x", "plain/x"];
    let tos = ["out/y", "inn/y", "plain/y"];
    for mode in [RenameMode::Excl, RenameMode::Swap] {
        for f in froms {
            for to in tos {
                if f.starts_with("plain") && to.starts_with("plain") {
                    continue;
                }
                assert_reason(rename_rooted(&root, &rp(f), &rp(to), mode), Code::NotDirectory, "symlink");
                assert_eq!(snapshot(&outside), so);
                assert_eq!(snapshot(&root), sr);
            }
        }
    }
    // Seam before `to`'s parent resolves: swap the pinned `from` parent.
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/m"), "m").unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    let (r2, o2) = (root.clone(), outside.clone());
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    set_before_open(Some(Box::new(move |_, n| {
        if n == OsStr::new("dst") && !f2.get() {
            f2.set(true);
            fs::rename(r2.join("src"), r2.join("src-moved")).unwrap();
            symlink(&o2, r2.join("src")).unwrap();
        }
    })));
    rename_rooted(&root, &rp("src/m"), &rp("dst/m"), RenameMode::Excl).unwrap();
    set_before_open(None);
    assert!(fired.get());
    assert_eq!(fs::read(root.join("dst/m")).unwrap(), b"m");
    assert!(!root.join("src-moved/m").exists());
    assert_eq!(snapshot(&outside), so);
    assert_reason(rename_rooted(&root, &rp("src/x"), &rp("dst/x"), RenameMode::Excl), Code::NotDirectory, "symlink");
    assert_eq!(snapshot(&outside), so);
}

#[test]
fn rename_rooted_directory_reparenting_needs_write_permission() {
    let t = Tmp::new();
    let root = t.join("root");
    for d in ["staging/pkg/sub", "sealed", "a", "b"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    rename_rooted(&root, &rp("staging/pkg"), &rp("sealed/pkg"), RenameMode::Excl).unwrap();
    let h = open_rooted(&root, &rp("sealed/pkg")).unwrap();
    set_file_meta(&h, &FileMeta { mode: Some(0o555), ..Default::default() }).unwrap();
    assert_eq!(mode_of(&root.join("sealed/pkg")), 0o555);

    fs::create_dir(root.join("a/ro")).unwrap();
    fs::write(root.join("a/ro/f"), "f").unwrap();
    set_mode(&root.join("a/ro"), 0o555);
    let before = snapshot(&root);
    let e = assert_reason(rename_rooted(&root, &rp("a/ro"), &rp("b/ro"), RenameMode::Excl), Code::ReadOnly, "reparent_needs_write");
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EACCES as i64));
    assert_eq!(snapshot(&root), before);
    rename_rooted(&root, &rp("a/ro"), &rp("a/ro2"), RenameMode::Excl).unwrap();
    fs::create_dir(root.join("b/other")).unwrap();
    assert_reason(rename_rooted(&root, &rp("a/ro2"), &rp("b/other"), RenameMode::Swap), Code::ReadOnly, "reparent_needs_write");
    assert_reason(rename_rooted(&root, &rp("b/other"), &rp("a/ro2"), RenameMode::Swap), Code::ReadOnly, "reparent_needs_write");
    make_removable(&open_rooted(&root, &rp("a/ro2")).unwrap()).unwrap();
    rename_rooted(&root, &rp("a/ro2"), &rp("b/other"), RenameMode::Swap).unwrap();
    rename_rooted(&root, &rp("a/ro2"), &rp("sealed/other"), RenameMode::Excl).unwrap();

    fs::create_dir(root.join("rop")).unwrap();
    fs::write(root.join("rop/f"), "f").unwrap();
    set_mode(&root.join("rop"), 0o555);
    assert_reason(rename_rooted(&root, &rp("rop/f"), &rp("f"), RenameMode::Excl), Code::ReadOnly, "no_permission");
    set_mode(&root.join("rop"), 0o755);
    set_mode(&root.join("sealed/pkg"), 0o755);
}

#[test]
fn full_fsync_accepts_file_and_directory_handles() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("d")).unwrap();
    fs::write(root.join("f"), "f").unwrap();
    full_fsync(&open_rooted(&root, &rp("f")).unwrap()).unwrap();
    full_fsync(&open_rooted(&root, &rp("d")).unwrap()).unwrap();
    let mut w = create_file_rooted(&root, &rp("new"), 0o644).unwrap();
    w.write_all(b"data").unwrap();
    full_fsync(&w).unwrap();
    full_fsync(&open_root(&root).unwrap()).unwrap();
    for fsk in [Fs::Hfs, Fs::MsDos] {
        let img = Image::create(fsk, 32);
        let r = fs::canonicalize(img.path()).unwrap();
        fs::write(r.join("F"), "f").unwrap();
        full_fsync(&open_rooted(&r, &rp("F")).unwrap()).unwrap_or_else(|e| panic!("{e:?}"));
        full_fsync(&open_root(&r).unwrap()).unwrap();
    }
    let (rd, _wr) = std::io::pipe().unwrap();
    let pf = fs::File::from(OwnedFd::from(rd));
    let e = assert_code(full_fsync(&pf), Code::Unknown);
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EBADF as i64));
    // power-safe rename recipe.
    fs::create_dir(root.join("e")).unwrap();
    rename_rooted(&root, &rp("new"), &rp("e/new"), RenameMode::Excl).unwrap();
    full_fsync(&open_root(&root).unwrap()).unwrap();
    full_fsync(&open_rooted(&root, &rp("e")).unwrap()).unwrap();
}

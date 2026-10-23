//! §7 rows for `remove_rooted` (Task 4).
mod common;

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::symlink;
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

fn count(d: &std::path::Path) -> usize {
    fs::read_dir(d).unwrap().count()
}

#[test]
fn remove_rooted_refuses_symlinked_parents() {
    let _c = ClearHook;
    let t = Tmp::new();
    let root = t.join("root");
    let outside = t.join("outside");
    for d in [&outside, &root.join("real")] {
        fs::create_dir_all(d.join("d")).unwrap();
        fs::create_dir_all(d.join("a")).unwrap();
        fs::write(d.join("x"), "x").unwrap();
        fs::write(d.join("a/b"), "b").unwrap();
    }
    symlink(&outside, root.join("link")).unwrap();
    symlink("real", root.join("inner")).unwrap();
    let (so, sr) = (snapshot(&outside), snapshot(&root));
    for (p, k) in [
        ("link/x", RemoveKind::File),
        ("inner/x", RemoveKind::File),
        ("link/d", RemoveKind::EmptyDir),
        ("inner/d", RemoveKind::EmptyDir),
        ("link/a/b", RemoveKind::File),
    ] {
        assert_reason(remove_rooted(&root, &rp(p), k), Code::NotDirectory, "symlink");
        assert_eq!(snapshot(&outside), so);
        assert_eq!(snapshot(&root), sr);
    }
    // Seam before the landing unlinkat swaps the pinned parent.
    for (name, k) in [("x", RemoveKind::File), ("d", RemoveKind::EmptyDir)] {
        let _ = fs::remove_dir_all(root.join("par"));
        let _ = fs::remove_dir_all(root.join("moved"));
        let _ = fs::remove_file(root.join("par"));
        fs::create_dir_all(root.join("par/d")).unwrap();
        fs::write(root.join("par/x"), "x").unwrap();
        let (r2, o2) = (root.clone(), outside.clone());
        let fired = Rc::new(Cell::new(false));
        let f2 = fired.clone();
        let target = name.to_string();
        set_before_open(Some(Box::new(move |_, n| {
            if n == OsStr::new(&target) && !f2.get() {
                f2.set(true);
                fs::rename(r2.join("par"), r2.join("moved")).unwrap();
                symlink(&o2, r2.join("par")).unwrap();
            }
        })));
        remove_rooted(&root, &rp(&format!("par/{name}")), k).unwrap();
        set_before_open(None);
        assert!(fired.get());
        assert!(fs::symlink_metadata(root.join("moved").join(name)).is_err(), "{name}: pinned entry not removed");
        assert_eq!(snapshot(&outside), so, "{name}: outside touched");
        assert_reason(remove_rooted(&root, &rp(&format!("par/{name}")), k), Code::NotDirectory, "symlink");
        assert_eq!(snapshot(&outside), so);
    }
}

#[test]
fn remove_rooted_file_on_a_directory_returns_is_directory() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("full/sub")).unwrap();
    fs::write(root.join("full/f"), "f").unwrap();
    fs::create_dir_all(root.join("ro")).unwrap();
    set_mode(&root.join("ro"), 0o555);
    fs::create_dir_all(root.join("uchg")).unwrap();
    chflags(&root.join("uchg"), "uchg");
    let before = snapshot(&root);
    for d in ["empty", "full", "ro", "uchg"] {
        let e = assert_code(remove_rooted(&root, &rp(d), RemoveKind::File), Code::IsDirectory);
        assert_eq!(detail_i64(&e, "errno"), Some(libc::EPERM as i64), "{d}");
        assert_eq!(snapshot(&root), before);
    }
    fs::write(root.join("uf"), "u").unwrap();
    chflags(&root.join("uf"), "uchg");
    let e = assert_reason(remove_rooted(&root, &rp("uf"), RemoveKind::File), Code::ReadOnly, "immutable");
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EPERM as i64));
    assert!(root.join("uf").exists());
    assert_code(remove_rooted(&root, &rp("nope"), RemoveKind::File), Code::NoSuchPath);
    chflags(&root.join("uf"), "nouchg");
    chflags(&root.join("uchg"), "nouchg");
}

#[test]
fn remove_rooted_file_removes_one_entry_without_following() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("dir/in")).unwrap();
    fs::write(root.join("dir/in/f"), "f").unwrap();
    fs::write(t.join("pointee"), "p").unwrap();
    fs::write(root.join("file"), "f").unwrap();
    symlink(t.join("pointee"), root.join("lfile")).unwrap();
    symlink("dir", root.join("ldir")).unwrap();
    symlink("nowhere", root.join("ldang")).unwrap();
    make_fifo(&root.join("fifo"));
    make_socket(&root.join("sock"));
    let dir_snap = snapshot(&root.join("dir"));
    for name in ["file", "lfile", "ldir", "ldang", "fifo", "sock"] {
        let n = count(&root);
        remove_rooted(&root, &rp(name), RemoveKind::File).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(count(&root), n - 1, "{name}");
        assert!(fs::symlink_metadata(root.join(name)).is_err());
    }
    assert_eq!(fs::read(t.join("pointee")).unwrap(), b"p");
    assert_eq!(snapshot(&root.join("dir")), dir_snap);
    fs::create_dir(root.join("emptyd")).unwrap();
    for d in ["dir", "emptyd"] {
        assert_code(remove_rooted(&root, &rp(d), RemoveKind::File), Code::IsDirectory);
        assert!(root.join(d).is_dir());
    }
    assert_code(remove_rooted(&root, &rp("nope"), RemoveKind::File), Code::NoSuchPath);
    fs::write(root.join("u"), "u").unwrap();
    chflags(&root.join("u"), "uchg");
    assert_reason(remove_rooted(&root, &rp("u"), RemoveKind::File), Code::ReadOnly, "immutable");
    chflags(&root.join("u"), "nouchg");
    fs::create_dir(root.join("rop")).unwrap();
    fs::write(root.join("rop/f"), "f").unwrap();
    set_mode(&root.join("rop"), 0o555);
    assert_reason(remove_rooted(&root, &rp("rop/f"), RemoveKind::File), Code::ReadOnly, "no_permission");
    set_mode(&root.join("rop"), 0o755);
    symlink("dir", root.join("ldir2")).unwrap();
    fs::write(root.join("dir/target"), "t").unwrap();
    assert_code(remove_rooted(&root, &rp("ldir2/target"), RemoveKind::File), Code::NotDirectory);
    assert!(root.join("dir/target").exists());
    fs::write(root.join("plain"), "p").unwrap();
    assert_code(remove_rooted(&root, &rp("plain"), RemoveKind::EmptyDir), Code::NotDirectory);
    assert!(root.join("plain").exists());
}

#[test]
fn remove_rooted_empty_dir_removes_only_empty_directories() {
    let _c = ClearHook;
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("e1")).unwrap();
    fs::create_dir_all(root.join("e555")).unwrap();
    set_mode(&root.join("e555"), 0o555);
    remove_rooted(&root, &rp("e1"), RemoveKind::EmptyDir).unwrap();
    remove_rooted(&root, &rp("e555"), RemoveKind::EmptyDir).unwrap();
    assert!(!root.join("e1").exists() && !root.join("e555").exists());
    fs::create_dir_all(root.join("full/sub")).unwrap();
    fs::write(root.join("full/f"), "f").unwrap();
    let s = snapshot(&root.join("full"));
    assert_code(remove_rooted(&root, &rp("full"), RemoveKind::EmptyDir), Code::NotEmpty);
    assert_eq!(snapshot(&root.join("full")), s);
    fs::write(root.join("file"), "f").unwrap();
    assert_code(remove_rooted(&root, &rp("file"), RemoveKind::EmptyDir), Code::NotDirectory);
    fs::create_dir(root.join("target")).unwrap();
    symlink("target", root.join("ltarget")).unwrap();
    assert_reason(remove_rooted(&root, &rp("ltarget"), RemoveKind::EmptyDir), Code::NotDirectory, "symlink");
    assert!(root.join("target").is_dir() && fs::symlink_metadata(root.join("ltarget")).is_ok());
    assert_code(remove_rooted(&root, &rp("nope"), RemoveKind::EmptyDir), Code::NoSuchPath);
    fs::create_dir(root.join("ud")).unwrap();
    chflags(&root.join("ud"), "uchg");
    assert_reason(remove_rooted(&root, &rp("ud"), RemoveKind::EmptyDir), Code::ReadOnly, "immutable");
    chflags(&root.join("ud"), "nouchg");
    assert_code(remove_rooted(&root, &rp("ltarget/x"), RemoveKind::EmptyDir), Code::NotDirectory);

    // A mount point under the root.
    fs::create_dir(root.join("mnt")).unwrap();
    let mut img = Image::create(Fs::Apfs, 32);
    img.attach_at(&root.join("mnt"));
    fs::write(root.join("mnt/kept"), "k").unwrap();
    let e = assert_reason(remove_rooted(&root, &rp("mnt"), RemoveKind::EmptyDir), Code::CrossVolume, "mount_point");
    assert!(detail_i64(&e, "src_dev").is_some());
    assert_ne!(detail_i64(&e, "src_dev"), detail_i64(&e, "dst_dev"));
    assert_eq!(fs::read(root.join("mnt/kept")).unwrap(), b"k");
    img.detach();
    assert!(root.join("mnt").is_dir());

    // Becomes a mount point after the fstatat (late EBUSY).
    fs::create_dir(root.join("late")).unwrap();
    let img = Rc::new(RefCell::new(img));
    let (i2, r2) = (img.clone(), root.clone());
    set_before_open(Some(Box::new(move |_, n| {
        if n == OsStr::new("late") {
            i2.borrow_mut().attach_at(&r2.join("late"));
        }
    })));
    let e = assert_reason(remove_rooted(&root, &rp("late"), RemoveKind::EmptyDir), Code::CrossVolume, "mount_point");
    set_before_open(None);
    let _ = e;
    assert_eq!(fs::read(root.join("late/kept")).unwrap(), b"k");
    img.borrow_mut().detach();
    assert!(root.join("late").is_dir());
}

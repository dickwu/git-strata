mod common;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use strata_apfs::*;
use strata_types::Code;

fn random_bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_ne_bytes());
    }
    v.truncate(n);
    v
}

fn make_tree(p: &Path) {
    fs::create_dir_all(p.join("sub/deeper")).unwrap();
    fs::write(p.join("a.txt"), "alpha").unwrap();
    fs::write(p.join("sub/b.txt"), "beta").unwrap();
    fs::write(p.join("sub/deeper/c.txt"), "gamma").unwrap();
    symlink("a.txt", p.join("link")).unwrap();
}

fn sh(cmd: &str) -> String {
    let o = Command::new("sh").arg("-c").arg(cmd).output().unwrap();
    assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn clone_dir_refuses_cross_volume() {
    let a = Image::create(Fs::Apfs, 256);
    let b = Image::create(Fs::Apfs, 256);
    let tree = a.path().join("tree");
    make_tree(&tree);
    fs::write(tree.join("big"), random_bytes(64 << 20, 7)).unwrap();
    let before = snapshot(&tree);
    let free_before = b.free_bytes();
    let e = assert_code(clone_tree(&tree, &b.path().join("clone")), Code::CrossVolume);
    assert_ne!(detail_i64(&e, "src_dev"), detail_i64(&e, "dst_dev"));
    assert_eq!(detail_bool(&e, "dst_exists"), Some(false));
    assert!(fs::symlink_metadata(b.path().join("clone")).is_err());
    let free_after = b.free_bytes();
    assert!(free_before.abs_diff(free_after) < (1 << 20), "{free_before} vs {free_after}");
    assert_eq!(snapshot(&tree), before);
    clone_tree(&tree, &a.path().join("clone")).unwrap();
    assert_eq!(snapshot(&a.path().join("clone")), before);
}

#[test]
fn clone_file_refuses_cross_volume() {
    let a = Image::create(Fs::Apfs, 32);
    let b = Image::create(Fs::Apfs, 32);
    let hfs = Image::create(Fs::Hfs, 32);
    fs::write(a.path().join("file"), "x").unwrap();
    let src = a.path().join("file");
    assert_code(clone_file(&src, &b.path().join("copy")), Code::CrossVolume);
    assert_code(clone_file(&src, &hfs.path().join("copy")), Code::CrossVolume);
    assert_code(clone_file(&src, &b.path().join("nope/copy")), Code::NoSuchPath);
    fs::write(b.path().join("there"), "y").unwrap();
    assert_code(clone_file(&src, &b.path().join("there")), Code::Exists);
    assert_eq!(fs::read(b.path().join("there")).unwrap(), b"y");
    assert!(fs::symlink_metadata(b.path().join("copy")).is_err());
}

#[test]
fn clone_on_unsupported_volume_is_unsupported() {
    let hfs = Image::create(Fs::Hfs, 32);
    fs::write(hfs.path().join("f"), "x").unwrap();
    fs::create_dir(hfs.path().join("d")).unwrap();
    let e = assert_code(clone_file(&hfs.path().join("f"), &hfs.path().join("g")), Code::Unsupported);
    assert_eq!(detail_str(&e, "fs_type").as_deref(), Some("hfs"));
    let e = assert_code(clone_tree(&hfs.path().join("d"), &hfs.path().join("e")), Code::Unsupported);
    assert_eq!(detail_str(&e, "fs_type").as_deref(), Some("hfs"));
    assert!(fs::symlink_metadata(hfs.path().join("g")).is_err());
    assert!(fs::symlink_metadata(hfs.path().join("e")).is_err());

    let dos = Image::create(Fs::MsDos, 64);
    fs::write(dos.path().join("f"), "x").unwrap();
    fs::create_dir(dos.path().join("d")).unwrap();
    let e = assert_code(clone_file(&dos.path().join("f"), &dos.path().join("g")), Code::Unsupported);
    assert_eq!(detail_str(&e, "fs_type").as_deref(), Some("msdos"));
    assert!(detail_i64(&e, "errno").is_none(), "refused before clonefile: {e:?}");
    assert_code(clone_tree(&dos.path().join("d"), &dos.path().join("e")), Code::Unsupported);
    assert!(fs::symlink_metadata(dos.path().join("g")).is_err());
    assert!(fs::symlink_metadata(dos.path().join("e")).is_err());
}

fn listing(p: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(p).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn clone_refusals_leave_no_residue() {
    let a = Image::create(Fs::Apfs, 32);
    let b = Image::create(Fs::Apfs, 32);
    let dos = Image::create(Fs::MsDos, 64);
    let tree = a.path().join("tree");
    make_tree(&tree);
    fs::create_dir(dos.path().join("d")).unwrap();
    fs::write(a.path().join("exists"), "e").unwrap();
    let before = snapshot(&tree);
    let la = listing(a.path());
    let lb = listing(b.path());
    let ld = listing(dos.path());
    assert!(clone_tree(&tree, &b.path().join("x")).is_err());
    assert!(clone_tree(&dos.path().join("d"), &dos.path().join("x")).is_err());
    assert!(clone_tree(&tree, &a.path().join("exists")).is_err());
    assert!(clone_tree(&tree, &a.path().join("missing/x")).is_err());
    assert!(clone_file(&tree.join("a.txt"), &b.path().join("x")).is_err());
    assert!(clone_file(&tree.join("a.txt"), &a.path().join("exists")).is_err());
    assert!(clone_file(&tree.join("a.txt"), &a.path().join("missing/x")).is_err());
    // clone_tree_rooted's half (Task 4).
    let (ar, br, dr) = (
        fs::canonicalize(a.path()).unwrap(),
        fs::canonicalize(b.path()).unwrap(),
        fs::canonicalize(dos.path()).unwrap(),
    );
    let rtree = ar.join("tree");
    assert_code(clone_tree_rooted(&rtree, &br, &rp("x")), Code::CrossVolume);
    assert!(clone_tree_rooted(&dr.join("d"), &dr, &rp("x")).is_err());
    assert_code(clone_tree_rooted(&rtree, &ar, &rp("exists")), Code::Exists);
    assert_code(clone_tree_rooted(&rtree, &ar, &rp("missing/x")), Code::NoSuchPath);
    // clone_from_rooted's half (Task 5).
    assert_code(clone_from_rooted(&ar, &rp("tree"), &br.join("x"), CloneKind::Dir), Code::CrossVolume);
    assert_code(clone_from_rooted(&ar, &rp("tree/a.txt"), &br.join("x"), CloneKind::File), Code::CrossVolume);
    assert_code(clone_from_rooted(&ar, &rp("tree"), &ar.join("exists"), CloneKind::Dir), Code::Exists);
    assert_code(clone_from_rooted(&ar, &rp("tree/a.txt"), &ar.join("missing/x"), CloneKind::File), Code::NoSuchPath);
    assert_eq!(listing(a.path()), la);
    assert_eq!(listing(b.path()), lb);
    assert_eq!(listing(dos.path()), ld);
    assert_eq!(snapshot(&tree), before);
    assert_eq!(fs::read(a.path().join("exists")).unwrap(), b"e");
}

#[test]
fn clone_tree_error_precedence() {
    let t = Tmp::new();
    let other = Image::create(Fs::Apfs, 32);
    let src = t.join("src");
    make_tree(&src);
    fs::write(t.join("file"), "f").unwrap();
    symlink(&src, t.join("dirlink")).unwrap();
    fs::create_dir(t.join("dstdir")).unwrap();
    symlink("nowhere", t.join("dangling")).unwrap();
    fs::write(other.path().join("there"), "x").unwrap();
    let before = snapshot(&t.path);

    assert_code(clone_tree(&t.join("missing"), &t.join("d1")), Code::NoSuchPath);
    assert_code(clone_tree(&t.join("file"), &t.join("d1")), Code::NotDirectory);
    assert_reason(clone_tree(&t.join("dirlink"), &t.join("d1")), Code::NotDirectory, "symlink");
    for dst in ["dstdir", "file", "dangling"] {
        assert_code(clone_tree(&src, &t.join(dst)), Code::Exists);
    }
    assert_code(clone_tree(&src, &src), Code::Exists);
    assert_code(clone_tree(&src, &t.join("missing/d")), Code::NoSuchPath);
    assert_code(clone_tree(&src, &t.join("file/d")), Code::NotDirectory);
    let mut slash = t.join("d1").into_os_string();
    slash.push("/");
    assert_reason(clone_tree(&src, Path::new(&slash)), Code::InvalidInput, "trailing_slash");
    assert_reason(clone_tree(&src, &src.join("sub/inner")), Code::InvalidInput, "dst_inside_src");
    // Kernel precedence across volumes.
    assert_code(clone_tree(&src, &other.path().join("there")), Code::Exists);
    assert_code(clone_tree(&src, &other.path().join("missing/x")), Code::NoSuchPath);
    assert_code(clone_tree(&src, &other.path().join("x")), Code::CrossVolume);
    assert_eq!(snapshot(&t.path), before);
}

#[test]
fn clone_tree_preserves_content_modes_links_and_holes() {
    let t = Tmp::new();
    let src = t.join("src");
    fs::create_dir(&src).unwrap();
    for (n, m) in [("x755", 0o755), ("x600", 0o600), ("x444", 0o444)] {
        fs::write(src.join(n), n).unwrap();
        set_mode(&src.join(n), m);
    }
    let f = fs::File::create(src.join("sparse")).unwrap();
    // On APFS a write that lands before a hole fills the hole, so the
    // fixture is a leading hole followed by a single tail write.
    f.set_len((16 << 20) + 4).unwrap();
    f.write_all_at(b"tail", 16 << 20).unwrap();
    drop(f);
    symlink("x755", src.join("rel")).unwrap();
    symlink("/etc/hosts", src.join("abs")).unwrap();
    symlink("nowhere", src.join("dangling")).unwrap();
    symlink("loop2", src.join("loop1")).unwrap();
    symlink("loop1", src.join("loop2")).unwrap();
    fs::create_dir(src.join("empty")).unwrap();
    let mut deep = src.join("deep");
    for i in 0..20 {
        deep = deep.join(format!("l{i}"));
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("leaf"), "leaf").unwrap();
    let before = snapshot(&src);
    clone_tree(&src, &t.join("dst")).unwrap();
    assert_eq!(snapshot(&t.join("dst")), before);
    assert_eq!(snapshot(&src), before);
    let ms = fs::metadata(src.join("sparse")).unwrap();
    let md = fs::metadata(t.join("dst/sparse")).unwrap();
    assert!(ms.blocks() * 512 < ms.len() / 2, "source is sparse: {} blocks", ms.blocks());
    assert!(md.blocks() * 512 < md.len() / 2, "hole kept: {} blocks", md.blocks());
    assert_eq!(ms.len(), md.len());
    assert_eq!(fs::metadata(t.join("dst/x600")).unwrap().uid(), unsafe { libc::getuid() });
}

#[test]
fn clone_tree_keeps_xattrs_and_flags_but_drops_acls_and_setuid() {
    let t = Tmp::new();
    let src = t.join("src");
    fs::create_dir_all(src.join("full")).unwrap();
    fs::create_dir(src.join("emptydir")).unwrap();
    fs::write(src.join("full/child"), "c").unwrap();
    fs::write(src.join("x"), "x").unwrap();
    sh(&format!("xattr -w user.test hello '{}'", src.join("x").display()));
    fs::write(src.join("frozen"), "f").unwrap();
    chflags(&src.join("frozen"), "uchg");
    fs::write(src.join("acl"), "a").unwrap();
    sh(&format!("chmod +a 'everyone deny delete' '{}'", src.join("acl").display()));
    fs::write(src.join("suid"), "s").unwrap();
    set_mode(&src.join("suid"), 0o4755);
    for p in [src.join("x"), src.join("emptydir")] {
        sh(&format!("touch -h -t 202001020304 '{}'", p.display()));
    }
    let dst = t.join("dst");
    clone_tree(&src, &dst).unwrap();
    assert!(sh(&format!("xattr -p user.test '{}'", dst.join("x").display())).contains("hello"));
    let e = fs::remove_file(dst.join("frozen")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EPERM));
    chflags(&dst.join("frozen"), "nouchg");
    assert!(!sh(&format!("ls -led '{}'", dst.join("acl").display())).contains("deny"));
    assert!(sh(&format!("ls -led '{}'", src.join("acl").display())).contains("deny"));
    assert_eq!(fs::metadata(dst.join("suid")).unwrap().permissions().mode() & 0o7777, 0o755);
    let y2020 = |p: &Path| fs::metadata(p).unwrap().mtime() < 1_600_000_000;
    assert!(y2020(&dst.join("x")));
    assert!(y2020(&dst.join("emptydir")));
    assert!(!y2020(&dst.join("full")));
    chflags(&src.join("frozen"), "nouchg");
}

#[test]
fn clone_tree_makes_independent_hard_links() {
    let t = Tmp::new();
    let src = t.join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("one"), "same").unwrap();
    fs::hard_link(src.join("one"), src.join("two")).unwrap();
    clone_tree(&src, &t.join("dst")).unwrap();
    let a = fs::metadata(t.join("dst/one")).unwrap();
    let b = fs::metadata(t.join("dst/two")).unwrap();
    assert_ne!(a.ino(), b.ino());
    assert_eq!((a.nlink(), b.nlink()), (1, 1));
    fs::write(t.join("dst/one"), "changed").unwrap();
    assert_eq!(fs::read(t.join("dst/two")).unwrap(), b"same");
    clone_file(&src.join("one"), &t.join("single")).unwrap();
    assert_eq!(fs::metadata(t.join("single")).unwrap().nlink(), 1);
    fs::write(t.join("single"), "x").unwrap();
    assert_eq!(fs::read(src.join("two")).unwrap(), b"same");
}

#[test]
fn clone_tree_clones_fifos_and_sockets_as_objects() {
    use std::os::unix::fs::FileTypeExt;
    let t = Tmp::new();
    let src = t.join("src");
    fs::create_dir(&src).unwrap();
    make_fifo(&src.join("fifo"));
    make_socket(&src.join("sock"));
    clone_tree(&src, &t.join("dst")).unwrap();
    assert!(fs::symlink_metadata(t.join("dst/fifo")).unwrap().file_type().is_fifo());
    assert!(fs::symlink_metadata(t.join("dst/sock")).unwrap().file_type().is_socket());
    assert_code(clone_tree(&src.join("fifo"), &t.join("x")), Code::NotDirectory);
}

#[test]
fn clone_tree_leaves_a_nested_mount_as_an_empty_stub() {
    let t = Tmp::new();
    let tree = t.join("tree");
    fs::create_dir_all(tree.join("mnt")).unwrap();
    fs::write(tree.join("top"), "t").unwrap();
    let mut inner = Image::create(Fs::Apfs, 32);
    fs::write(inner.path().join("inside"), "i").unwrap();
    inner.attach_at(&tree.join("mnt"));
    assert!(tree.join("mnt/inside").exists());
    clone_tree(&tree, &t.join("clone")).unwrap();
    assert!(t.join("clone/top").exists());
    assert!(listing(&t.join("clone/mnt")).is_empty());
    drop(inner);
}

#[test]
fn clone_tree_fails_atomically_on_unreadable_entry() {
    let t = Tmp::new();
    for (round, unreadable) in ["file", "dir"].iter().enumerate() {
        let src = t.join(&format!("src{round}"));
        make_tree(&src);
        let p = src.join("sub/locked");
        if *unreadable == "file" {
            fs::write(&p, "secret").unwrap();
        } else {
            fs::create_dir(&p).unwrap();
            fs::write(p.join("x"), "x").unwrap();
        }
        set_mode(&p, 0o000);
        let dst = t.join(&format!("dst{round}"));
        let e = assert_code(clone_tree(&src, &dst), Code::ReadOnly);
        assert_eq!(detail_i64(&e, "errno"), Some(libc::EACCES as i64));
        assert_eq!(detail_bool(&e, "dst_exists"), Some(false));
        assert!(e.message.contains("unreadable") && e.message.contains("writable"), "{}", e.message);
        assert!(fs::symlink_metadata(&dst).is_err());
        set_mode(&p, if *unreadable == "file" { 0o644 } else { 0o755 });
    }
    let src = t.join("ro");
    make_tree(&src);
    set_mode(&src.join("sub"), 0o555);
    clone_tree(&src, &t.join("roclone")).unwrap();
    assert!(t.join("roclone/sub/b.txt").exists());
}

#[test]
fn clone_tree_on_a_full_volume_leaves_no_destination() {
    let img = Image::create(Fs::Apfs, 12);
    let tree = img.path().join("tree");
    for d in 0..10 {
        let dir = tree.join(format!("d{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..100 {
            fs::write(dir.join(format!("f{f}")), format!("{d}-{f}")).unwrap();
        }
    }
    let before = snapshot(&tree);
    let filler = fill_until_enospc(img.path());
    let e = assert_code(clone_tree(&tree, &img.path().join("clone")), Code::Unknown);
    assert_eq!(detail_i64(&e, "errno"), Some(libc::ENOSPC as i64));
    assert_eq!(detail_bool(&e, "dst_exists"), Some(false));
    assert!(fs::symlink_metadata(img.path().join("clone")).is_err());
    assert!(filler.exists());
    assert_eq!(snapshot(&tree), before);
    fs::remove_file(&filler).unwrap();
    clone_tree(&tree, &img.path().join("clone")).unwrap();
    assert_eq!(snapshot(&img.path().join("clone")), before);
}

#[test]
fn clone_file_rejects_directories_and_special_files() {
    let t = Tmp::new();
    fs::create_dir(t.join("dir")).unwrap();
    make_fifo(&t.join("fifo"));
    make_socket(&t.join("sock"));
    fs::write(t.join("f"), "x").unwrap();
    fs::write(t.join("there"), "y").unwrap();
    assert_code(clone_file(&t.join("dir"), &t.join("o1")), Code::IsDirectory);
    assert_reason(clone_file(&t.join("fifo"), &t.join("o2")), Code::NotFile, "fifo");
    assert_reason(clone_file(&t.join("sock"), &t.join("o3")), Code::NotFile, "socket");
    assert_reason(clone_file(Path::new("/dev/null"), &t.join("o4")), Code::NotFile, "device");
    assert_code(clone_file(&t.join("missing"), &t.join("o5")), Code::NoSuchPath);
    assert_code(clone_file(&t.join("f"), &t.join("there")), Code::Exists);
    assert_eq!(fs::read(t.join("there")).unwrap(), b"y");
}

#[test]
fn clone_file_clones_symlinks_as_symlinks() {
    let t = Tmp::new();
    fs::write(t.join("target"), "content").unwrap();
    fs::create_dir(t.join("dir")).unwrap();
    symlink("target", t.join("to_file")).unwrap();
    symlink("dir", t.join("to_dir")).unwrap();
    symlink("nowhere", t.join("dangling")).unwrap();
    for n in ["to_file", "to_dir", "dangling"] {
        let dst = t.join(&format!("c_{n}"));
        clone_file(&t.join(n), &dst).unwrap();
        assert!(fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&dst).unwrap(), fs::read_link(t.join(n)).unwrap());
    }
    // An intermediate symlink in src is resolved by the kernel; only the
    // last component is not followed.
    fs::write(t.join("dir/inner"), "inner").unwrap();
    clone_file(&t.join("to_dir/inner"), &t.join("c_inner")).unwrap();
    assert_eq!(fs::read(t.join("c_inner")).unwrap(), b"inner");
    assert!(fs::symlink_metadata(t.join("c_inner")).unwrap().is_file());
}

#[test]
fn name_over_255_utf16_units_is_invalid_input() {
    let t = Tmp::new();
    fs::write(t.join("src"), "x").unwrap();
    let src = t.join("src");
    let ok1 = "\u{3042}".repeat(255);
    assert_eq!(ok1.len(), 765);
    clone_file(&src, &t.join(&ok1)).unwrap();
    let ok2 = "\u{1F600}".repeat(127);
    clone_file(&src, &t.join(&ok2)).unwrap();
    for bad in ["\u{3042}".repeat(256), "\u{1F600}".repeat(128), "a".repeat(256)] {
        let e = assert_reason(clone_file(&src, &t.join(&bad)), Code::InvalidInput, "name_too_long");
        assert_eq!(detail_i64(&e, "errno"), Some(libc::ENAMETOOLONG as i64));
        let raw = fs::File::create(t.join(&bad)).unwrap_err();
        assert_eq!(raw.raw_os_error(), Some(libc::ENAMETOOLONG));
    }
}

/// Build an absolute path of exactly `len` bytes under `base`, creating the
/// intermediate directories; returns the path (its leaf does not exist).
fn path_of_len(base: &Path, len: usize) -> PathBuf {
    let mut p = base.to_path_buf();
    loop {
        let cur = p.as_os_str().len();
        let remaining = len - cur - 1; // for the '/'
        if remaining <= 200 {
            return p.join("f".repeat(remaining));
        }
        p = p.join("d".repeat(150));
        fs::create_dir_all(&p).unwrap();
    }
}

#[test]
fn path_of_1024_bytes_is_invalid_input() {
    let t = Tmp::new();
    fs::write(t.join("src"), "x").unwrap();
    for (len, sub) in [(1023, "a"), (1024, "b"), (1025, "c")] {
        let base = t.join(sub);
        fs::create_dir(&base).unwrap();
        let dst = path_of_len(&base, len);
        assert_eq!(dst.as_os_str().len(), len);
        let r = clone_file(&t.join("src"), &dst);
        if len == 1023 {
            r.unwrap();
            assert_eq!(fs::read(&dst).unwrap(), b"x");
        } else {
            assert_reason(r, Code::InvalidInput, "path_too_long");
        }
    }
}

#[test]
fn clone_is_copy_on_write_and_isolated() {
    let t = Tmp::new();
    let src = t.join("src");
    fs::create_dir(&src).unwrap();
    let base: Vec<Vec<u8>> = (0..5).map(|i| random_bytes(1 << 20, i + 11)).collect();
    for (i, b) in base.iter().enumerate() {
        fs::write(src.join(format!("f{i}")), b).unwrap();
    }
    clone_tree(&src, &t.join("A")).unwrap();
    clone_tree(&src, &t.join("B")).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::scope(|s| {
        for w in 0..2 {
            let p = t.join(&format!("B/f{w}"));
            s.spawn(move || {
                let mut f = fs::OpenOptions::new().write(true).open(&p).unwrap();
                let data = vec![0xEEu8; 5 << 20];
                f.write_all(&data).unwrap();
                f.sync_all().unwrap();
                assert_eq!(fs::read(&p).unwrap(), data);
            });
        }
        for r in 0..12 {
            let dir = if r % 2 == 0 { t.join("A") } else { src.clone() };
            let base = &base;
            let stop = stop.clone();
            s.spawn(move || {
                for round in 0..20 {
                    let i = (r + round) % 5;
                    let mut v = vec![];
                    fs::File::open(dir.join(format!("f{i}"))).unwrap().read_to_end(&mut v).unwrap();
                    assert!(v == base[i], "reader saw foreign bytes");
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                }
            });
        }
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for (i, b) in base.iter().enumerate() {
        assert_eq!(&fs::read(src.join(format!("f{i}"))).unwrap(), b);
    }
}

#[test]
fn clone_then_small_write_allocates_only_the_touched_extent() {
    let img = Image::create(Fs::Apfs, 512);
    let src = img.path().join("big");
    let data = random_bytes(128 << 20, 99);
    fs::write(&src, &data).unwrap();
    fs::File::open(&src).unwrap().sync_all().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let f0 = img.free_bytes();
    clone_file(&src, &img.path().join("clone")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let f1 = img.free_bytes();
    assert!(f0.saturating_sub(f1) < (1 << 20), "clone allocated {} bytes", f0 - f1);
    let c = fs::OpenOptions::new().write(true).open(img.path().join("clone")).unwrap();
    c.write_all_at(&vec![0x55u8; 64 << 10], 1 << 20).unwrap();
    c.sync_all().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let f2 = img.free_bytes();
    assert!(f1.saturating_sub(f2) < (1 << 20), "write allocated {} bytes", f1 - f2);
    let mut head = vec![0u8; 64 << 10];
    fs::File::open(&src).unwrap().read_exact_at(&mut head, 1 << 20).unwrap();
    assert_eq!(head, data[1 << 20..(1 << 20) + (64 << 10)]);
    assert_eq!(
        fs::metadata(&src).unwrap().blocks(),
        fs::metadata(img.path().join("clone")).unwrap().blocks()
    );
}

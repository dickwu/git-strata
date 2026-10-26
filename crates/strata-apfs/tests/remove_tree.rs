//! `remove_tree_rooted` (§8 Task 5, §7 rows remove_tree_rooted_*).
mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

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

fn user() -> String {
    let o = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn sh(args: &[&str]) {
    let o = Command::new(args[0]).args(&args[1..]).output().unwrap();
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Entries under `p` including `p` itself (never following symlinks).
fn count(p: &Path) -> u64 {
    let m = fs::symlink_metadata(p).unwrap();
    let mut n = 1;
    if m.is_dir() {
        for e in fs::read_dir(p).unwrap() {
            n += count(&e.unwrap().path());
        }
    }
    n
}

fn gone(p: &Path) -> bool {
    fs::symlink_metadata(p).is_err()
}

fn stuck_map(r: &RemoveReport) -> BTreeMap<String, (StuckReason, i32, Option<String>)> {
    r.stuck.iter().map(|s| (s.path.as_str().to_string(), (s.reason, s.errno, s.detail.clone()))).collect()
}

#[test]
fn remove_tree_rooted_removes_a_protected_tree() {
    let t = Tmp::new();
    let root = t.join("root");
    let tree = root.join("tree");
    let outside = t.join("outside");
    fs::create_dir_all(outside.join("sub")).unwrap();
    fs::write(outside.join("o.txt"), "OUT").unwrap();
    fs::write(outside.join("sub/o2.txt"), "OUT2").unwrap();
    fs::create_dir_all(&tree).unwrap();
    let w = |p: &str, s: &str| fs::write(tree.join(p), s).unwrap();
    w("f0400", "a");
    w("fuchg", "b");
    w("fuapp", "c");
    w("fhidden", "d");
    w("f100u", "e");
    symlink(&outside, tree.join("l_out")).unwrap();
    symlink("f0400", tree.join("l_in")).unwrap();
    symlink("nope", tree.join("l_dangle")).unwrap();
    symlink("f0400", tree.join("l_uchg")).unwrap();
    make_fifo(&tree.join("fifo"));
    make_socket(&tree.join("sock"));
    w("hl1", "shared");
    fs::hard_link(tree.join("hl1"), tree.join("hl2")).unwrap();
    fs::hard_link(tree.join("hl1"), outside.join("hl3")).unwrap();
    for d in ["d000", "d555", "d100", "d100u", "duchg", "duapp"] {
        fs::create_dir(tree.join(d)).unwrap();
        fs::write(tree.join(d).join("in"), d).unwrap();
    }
    fs::create_dir_all(tree.join("nest/a/b/c/d")).unwrap();
    fs::write(tree.join("nest/a/b/c/d/leaf"), "leaf").unwrap();
    let expected = count(&tree);
    // Protections, innermost first.
    set_mode(&tree.join("f0400"), 0o400);
    chflags(&tree.join("fuchg"), "uchg");
    chflags(&tree.join("fuapp"), "uappnd");
    chflags(&tree.join("fhidden"), "hidden");
    chflags(&tree.join("l_uchg"), "uchg");
    set_mode(&tree.join("f100u"), 0o100);
    chflags(&tree.join("f100u"), "uchg");
    set_mode(&tree.join("d000"), 0o000);
    set_mode(&tree.join("d555"), 0o555);
    set_mode(&tree.join("d100"), 0o100);
    set_mode(&tree.join("d100u"), 0o100);
    chflags(&tree.join("d100u"), "uchg");
    chflags(&tree.join("duchg"), "uchg");
    chflags(&tree.join("duapp"), "uappnd");
    set_mode(&tree.join("nest/a/b"), 0o555);
    let out_before = snapshot(&outside);
    let hl3_mode = mode_of(&outside.join("hl3"));

    let r = remove_tree_rooted(&root, &rp("tree")).unwrap();
    assert_eq!(r.stuck, vec![], "{r:?}");
    assert_eq!(r.stuck_total, 0);
    assert!(r.is_complete());
    assert_eq!(r.removed, expected, "{r:?}");
    assert_eq!(r.flags_cleared, 7, "{r:?}");
    assert_eq!(r.made_writable, 5, "{r:?}");
    assert!(gone(&tree));
    assert_eq!(snapshot(&outside), out_before);
    assert_eq!(fs::read(outside.join("hl3")).unwrap(), b"shared");
    assert_eq!(mode_of(&outside.join("hl3")), hl3_mode);
}

#[test]
fn remove_tree_rooted_takes_any_entry_kind() {
    let t = Tmp::new();
    let root = t.path.clone();
    let outside = Tmp::new();
    fs::create_dir(outside.join("d")).unwrap();
    fs::write(outside.join("d/x"), "X").unwrap();
    type Make = Box<dyn Fn(&Path)>;
    let o = outside.join("d");
    let o2 = o.clone();
    let cases: Vec<(&str, Make, u64)> = vec![
        ("file0400", Box::new(|p| {
            fs::write(p, "a").unwrap();
            set_mode(p, 0o400);
        }), 0),
        ("fileuchg", Box::new(|p| {
            fs::write(p, "a").unwrap();
            chflags(p, "uchg");
        }), 1),
        ("link_out", Box::new(move |p| symlink(&o, p).unwrap()), 0),
        ("link_uchg", Box::new(move |p| {
            symlink(&o2, p).unwrap();
            chflags(p, "uchg");
        }), 1),
        ("dangling", Box::new(|p| symlink("nope", p).unwrap()), 0),
        ("fifo", Box::new(make_fifo), 0),
        ("sock", Box::new(make_socket), 0),
        ("emptydir", Box::new(|p| fs::create_dir(p).unwrap()), 0),
    ];
    let out_before = snapshot(&outside.path);
    for (name, make, cleared) in cases {
        let dir = root.join(format!("p_{name}"));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("sib"), "s").unwrap();
        make(&dir.join("t"));
        let r = remove_tree_rooted(&root, &rp(&format!("p_{name}/t"))).unwrap();
        assert!(r.is_complete(), "{name}: {r:?}");
        assert_eq!(r.removed, 1, "{name}");
        assert_eq!(r.flags_cleared, cleared, "{name}");
        assert!(gone(&dir.join("t")), "{name}");
        assert_eq!(fs::read(dir.join("sib")).unwrap(), b"s", "{name}");
        assert_eq!(snapshot(&outside.path), out_before, "{name}");
    }
    // A flagged file with a second name: left alone.
    fs::create_dir(root.join("hl")).unwrap();
    fs::write(root.join("hl/t"), "h").unwrap();
    fs::hard_link(root.join("hl/t"), root.join("hl/other")).unwrap();
    chflags(&root.join("hl/t"), "uchg");
    let r = remove_tree_rooted(&root, &rp("hl/t")).unwrap();
    assert!(!r.is_complete());
    assert_eq!(r.removed, 0);
    assert_eq!(r.flags_cleared, 0);
    assert_eq!(r.stuck.len(), 1);
    assert_eq!(r.stuck[0].path.as_str(), "hl/t");
    assert_eq!(r.stuck[0].reason, StuckReason::Immutable);
    assert_eq!(r.stuck[0].detail.as_deref(), Some("hard_link"));
    assert!(flags_of(&root.join("hl/t")) & UF_IMMUTABLE != 0);
    assert_eq!(fs::read(root.join("hl/other")).unwrap(), b"h");
    chflags(&root.join("hl/t"), "nouchg");
}

#[test]
fn remove_tree_rooted_reports_what_it_cannot_remove() {
    let t = Tmp::new();
    let root = t.path.clone();
    let tree = root.join("tree");
    fs::create_dir_all(tree.join("sub")).unwrap();
    fs::write(tree.join("a.txt"), "a").unwrap();
    fs::write(tree.join("sub/b.txt"), "b").unwrap();
    symlink("../a.txt", tree.join("sub/l")).unwrap();
    // Nested mount.
    let mut img = Image::create(Fs::Apfs, 32);
    fs::create_dir(img.path().join("md")).unwrap();
    fs::write(img.path().join("mf"), "MOUNTED").unwrap();
    fs::write(img.path().join("md/mf2"), "MOUNTED2").unwrap();
    fs::create_dir(tree.join("mnt")).unwrap();
    img.attach_at(&tree.join("mnt"));
    // ACL-protected file and directory.
    let me = user();
    fs::write(tree.join("aclf"), "acl").unwrap();
    sh(&["chmod", "+a", &format!("user:{me} deny delete"), tree.join("aclf").to_str().unwrap()]);
    fs::create_dir(tree.join("acld")).unwrap();
    fs::write(tree.join("acld/in"), "in").unwrap();
    set_mode(&tree.join("acld"), 0o700);
    sh(&["chmod", "+a", &format!("user:{me} deny list,search"), tree.join("acld").to_str().unwrap()]);
    // Unopenable and flagged.
    fs::create_dir(tree.join("d0000u")).unwrap();
    fs::write(tree.join("d0000u/in"), "in").unwrap();
    set_mode(&tree.join("d0000u"), 0o000);
    chflags(&tree.join("d0000u"), "uchg");
    fs::write(tree.join("f0200u"), "f").unwrap();
    set_mode(&tree.join("f0200u"), 0o200);
    chflags(&tree.join("f0200u"), "uchg");
    let ct_d0 = ctime_ns(&tree.join("d0000u"));
    let ct_acld = ctime_ns(&tree.join("acld"));
    std::thread::sleep(Duration::from_millis(1100));

    let r = remove_tree_rooted(&root, &rp("tree")).unwrap();
    assert!(!r.is_complete());
    assert_eq!(r.stuck_total, 5, "{r:?}");
    let want: BTreeMap<String, (StuckReason, i32, Option<String>)> = [
        ("tree/aclf", (StuckReason::NoPermission, libc::EACCES, None)),
        ("tree/mnt", (StuckReason::OtherDevice, 0, None)),
        ("tree/d0000u", (StuckReason::Immutable, libc::EPERM, Some("UF_IMMUTABLE".to_string()))),
        ("tree/f0200u", (StuckReason::Immutable, libc::EACCES, Some("UF_IMMUTABLE".to_string()))),
        ("tree/acld", (StuckReason::NoPermission, libc::EACCES, None)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(stuck_map(&r), want);
    assert_eq!(r.removed, 4, "{r:?}");
    assert_eq!(r.made_writable, 0);
    assert!(gone(&tree.join("a.txt")) && gone(&tree.join("sub")));
    for p in ["aclf", "mnt/mf", "mnt/md/mf2", "f0200u", "acld", "d0000u"] {
        assert!(!gone(&tree.join(p)), "{p}");
    }
    assert_eq!(fs::read(tree.join("mnt/mf")).unwrap(), b"MOUNTED");
    assert_eq!(fs::read(tree.join("mnt/md/mf2")).unwrap(), b"MOUNTED2");
    assert_eq!(mode_of(&tree.join("d0000u")), 0o000);
    assert!(flags_of(&tree.join("d0000u")) & UF_IMMUTABLE != 0);
    assert_eq!(mode_of(&tree.join("acld")), 0o700);
    assert_eq!(ctime_ns(&tree.join("d0000u")), ct_d0, "d0000u was touched");
    assert_eq!(ctime_ns(&tree.join("acld")), ct_acld, "acld was touched");

    // Lift every obstacle, then the second call finishes.
    sh(&["chmod", "-a", &format!("user:{me} deny delete"), tree.join("aclf").to_str().unwrap()]);
    sh(&["chmod", "-a", &format!("user:{me} deny list,search"), tree.join("acld").to_str().unwrap()]);
    chflags(&tree.join("d0000u"), "nouchg");
    chflags(&tree.join("f0200u"), "nouchg");
    set_mode(&tree.join("d0000u"), 0o700);
    img.detach();
    let left = count(&tree);
    let r = remove_tree_rooted(&root, &rp("tree")).unwrap();
    assert!(r.is_complete(), "{r:?}");
    assert_eq!(r.removed, left);
    assert!(gone(&tree));
}

#[test]
fn remove_tree_rooted_leaves_a_shared_flagged_inode_alone() {
    let t = Tmp::new();
    let root = t.join("root");
    let tree = root.join("tree");
    let outside = t.join("outside");
    fs::create_dir_all(&tree).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(tree.join("h"), "H").unwrap();
    fs::hard_link(tree.join("h"), outside.join("h2")).unwrap();
    chflags(&tree.join("h"), "uchg");
    fs::write(tree.join("g1"), "G").unwrap();
    fs::hard_link(tree.join("g1"), tree.join("g2")).unwrap();
    chflags(&tree.join("g1"), "uappnd");
    fs::write(tree.join("p1"), "p").unwrap();
    fs::write(tree.join("p2"), "p").unwrap();
    let h2 = outside.join("h2");
    assert!(flags_of(&h2) & UF_IMMUTABLE != 0, "flags belong to the inode");
    let (m, f, c) = (mode_of(&h2), flags_of(&h2), ctime_ns(&h2));
    tick();

    let r = remove_tree_rooted(&root, &rp("tree")).unwrap();
    assert!(!r.is_complete());
    let hl = (StuckReason::Immutable, libc::EPERM, Some("hard_link".to_string()));
    let want: BTreeMap<_, _> =
        ["tree/h", "tree/g1", "tree/g2"].iter().map(|k| (k.to_string(), hl.clone())).collect();
    assert_eq!(stuck_map(&r), want);
    assert_eq!(r.stuck_total, 3);
    assert_eq!(r.flags_cleared, 0);
    assert_eq!(r.removed, 2);
    assert_eq!(fs::read(&h2).unwrap(), b"H");
    assert_eq!((mode_of(&h2), flags_of(&h2), ctime_ns(&h2)), (m, f, c));
    chflags(&tree.join("h"), "nouchg");
    chflags(&tree.join("g1"), "nouappnd");
}

/// (relative path, mode, flags, mtime) of every entry, never following links.
fn attrs(p: &Path) -> Vec<(PathBuf, u32, u32, i128)> {
    let mut out = vec![];
    fn go(base: &Path, p: &Path, out: &mut Vec<(PathBuf, u32, u32, i128)>) {
        out.push((p.strip_prefix(base).unwrap().to_path_buf(), mode_of(p), flags_of(p), mtime_ns(p)));
        let m = fs::symlink_metadata(p).unwrap();
        if m.is_dir()
            && let Ok(rd) = fs::read_dir(p)
        {
            let mut v: Vec<_> = rd.map(|e| e.unwrap().path()).collect();
            v.sort();
            for c in v {
                go(base, &c, out);
            }
        }
    }
    go(p, p, &mut out);
    out
}

#[test]
fn remove_tree_rooted_refuses_before_touching_anything() {
    let t = Tmp::new();
    let root = t.join("root");
    let outside = t.join("outside");
    fs::create_dir_all(outside.join("x")).unwrap();
    fs::write(outside.join("x/o"), "O").unwrap();
    fs::create_dir_all(root.join("real/x")).unwrap();
    fs::write(root.join("real/x/r"), "R").unwrap();
    symlink(&outside, root.join("lo")).unwrap();
    symlink(root.join("real"), root.join("li")).unwrap();
    fs::write(root.join("file"), "F").unwrap();
    for d in ["ro", "imm", "app"] {
        fs::create_dir_all(root.join(d).join("x")).unwrap();
        fs::write(root.join(d).join("x/f"), "f").unwrap();
    }
    set_mode(&root.join("ro"), 0o555);
    chflags(&root.join("imm"), "uchg");
    chflags(&root.join("app"), "uappnd");
    let mut img = Image::create(Fs::Apfs, 32);
    fs::write(img.path().join("mf"), "MOUNTED").unwrap();
    fs::create_dir(root.join("mnt")).unwrap();
    img.attach_at(&root.join("mnt"));

    let (sr, ar, so, ao) = (snapshot(&root), attrs(&root), snapshot(&outside), attrs(&outside));
    let check = |what: &str| {
        assert_eq!(snapshot(&root), sr, "{what}");
        assert_eq!(attrs(&root), ar, "{what}");
        assert_eq!(snapshot(&outside), so, "{what}");
        assert_eq!(attrs(&outside), ao, "{what}");
    };
    assert_code(remove_tree_rooted(&root, &rp("missing")), Code::NoSuchPath);
    check("missing");
    assert_reason(remove_tree_rooted(&root, &rp("lo/x")), Code::NotDirectory, "symlink");
    check("lo");
    assert_reason(remove_tree_rooted(&root, &rp("li/x")), Code::NotDirectory, "symlink");
    check("li");
    assert_reason(remove_tree_rooted(&root, &rp("file/x")), Code::NotDirectory, "not_a_directory");
    check("file");
    let e = assert_reason(remove_tree_rooted(&root, &rp("mnt")), Code::CrossVolume, "mount_point");
    assert_ne!(detail_i64(&e, "src_dev"), detail_i64(&e, "dst_dev"));
    assert!(detail_i64(&e, "src_dev").is_some());
    check("mnt");
    assert_eq!(fs::read(root.join("mnt/mf")).unwrap(), b"MOUNTED");
    assert_reason(remove_tree_rooted(&root, &rp("ro/x")), Code::ReadOnly, "no_permission");
    check("ro");
    assert_reason(remove_tree_rooted(&root, &rp("imm/x")), Code::ReadOnly, "immutable");
    check("imm");
    assert_reason(remove_tree_rooted(&root, &rp("app/x")), Code::ReadOnly, "immutable");
    check("app");
    img.detach();
    chflags(&root.join("imm"), "nouchg");
    chflags(&root.join("app"), "nouappnd");
}

#[test]
fn remove_tree_rooted_never_follows_a_swapped_directory() {
    let _c = ClearHook;
    // Round 1: a child directory becomes a symlink before it is opened.
    let t = Tmp::new();
    let outside = Tmp::new();
    fs::create_dir(outside.join("od")).unwrap();
    fs::write(outside.join("od/keep"), "KEEP").unwrap();
    let tree = t.join("tree");
    fs::create_dir_all(tree.join("d/inner")).unwrap();
    fs::write(tree.join("d/f"), "f").unwrap();
    fs::write(tree.join("s1"), "1").unwrap();
    fs::create_dir(tree.join("s2")).unwrap();
    fs::write(tree.join("s2/f"), "2").unwrap();
    let before = snapshot(&outside.path);
    {
        let d = tree.join("d");
        let target = outside.join("od");
        set_before_open(Some(Box::new(move |_, name| {
            if name == "d" && !d.is_symlink() && d.is_dir() {
                fs::remove_dir_all(&d).unwrap();
                symlink(&target, &d).unwrap();
            }
        })));
    }
    let r = remove_tree_rooted(&t.path, &rp("tree")).unwrap();
    set_before_open(None);
    assert!(!r.is_complete());
    let m = stuck_map(&r);
    assert_eq!(m.get("tree/d").map(|v| v.0), Some(StuckReason::Changed), "{r:?}");
    assert_eq!(r.stuck_total, 1);
    assert!(tree.join("d").is_symlink());
    assert!(gone(&tree.join("s1")) && gone(&tree.join("s2")));
    assert_eq!(snapshot(&outside.path), before);

    // Round 2: an already-resolved parent's path becomes a symlink.
    let t = Tmp::new();
    let outside = Tmp::new();
    fs::create_dir_all(outside.join("tree/sub")).unwrap();
    fs::write(outside.join("tree/f"), "OUT").unwrap();
    fs::write(outside.join("tree/sub/g"), "OUT").unwrap();
    fs::create_dir_all(t.join("a/tree/sub")).unwrap();
    fs::write(t.join("a/tree/f"), "in").unwrap();
    fs::write(t.join("a/tree/sub/g"), "in").unwrap();
    let before = snapshot(&outside.path);
    let fired = Rc::new(Cell::new(false));
    {
        let (a, moved, target, fired) = (t.join("a"), t.join("a_orig"), outside.path.clone(), fired.clone());
        set_before_open(Some(Box::new(move |_, name| {
            if name == "tree" && !fired.get() {
                fired.set(true);
                fs::rename(&a, &moved).unwrap();
                symlink(&target, &a).unwrap();
            }
        })));
    }
    let r = remove_tree_rooted(&t.path, &rp("a/tree")).unwrap();
    set_before_open(None);
    assert!(fired.get());
    assert!(r.is_complete(), "{r:?}");
    assert!(gone(&t.join("a_orig/tree")));
    assert_eq!(snapshot(&outside.path), before);
}

fn chain(base: &Path, levels: usize) -> PathBuf {
    let mut p = base.to_path_buf();
    for _ in 0..levels {
        p.push("x");
    }
    fs::create_dir_all(&p).unwrap();
    // A file in every level.
    let mut q = base.to_path_buf();
    for _ in 0..levels {
        q.push("x");
        fs::write(q.join("f"), "f").unwrap();
    }
    p
}

#[test]
fn remove_tree_rooted_depth_limit() {
    let t = Tmp::new();
    chain(&t.join("shallow"), 100);
    let r = remove_tree_rooted(&t.path, &rp("shallow")).unwrap();
    assert!(r.is_complete(), "{:?}", r.stuck);
    assert!(gone(&t.join("shallow")));

    let deep = t.join("deep");
    chain(&deep, 130);
    fs::create_dir_all(deep.join("sh/a")).unwrap();
    fs::write(deep.join("sh/a/f"), "f").unwrap();
    let r = remove_tree_rooted(&t.path, &rp("deep")).unwrap();
    assert!(!r.is_complete());
    assert_eq!(r.stuck_total, 1, "{:?}", r.stuck);
    let lim = MAX_WALK_DEPTH as usize;
    let want = format!("deep{}", "/x".repeat(lim));
    assert_eq!(r.stuck[0].path.as_str(), want);
    assert_eq!(r.stuck[0].reason, StuckReason::TooDeep);
    assert_eq!(r.stuck[0].errno, 0);
    assert!(gone(&deep.join("sh")));
    let at = |n: usize| {
        let mut p = deep.clone();
        for _ in 0..n {
            p.push("x");
        }
        p
    };
    // Levels within the limit lose their files; the too-deep level keeps its own.
    assert!(gone(&at(lim - 1).join("f")));
    assert!(gone(&at(1).join("f")));
    assert!(at(lim).join("f").exists());
    assert!(at(130).join("f").exists());
}

const MARKER: &str = "low-fd-child: 512 wide, 100 deep, removed";

#[test]
fn remove_tree_fits_a_low_descriptor_limit() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "remove_tree_low_descriptor_limit_child", "--nocapture", "--test-threads=1"])
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed\nstdout:\n{so}\nstderr:\n{se}");
    assert!(so.lines().any(|l| l.trim() == MARKER), "marker missing\nstdout:\n{so}\nstderr:\n{se}");
}

fn open_fds() -> usize {
    // read_dir holds one descriptor of its own.
    fs::read_dir("/dev/fd").unwrap().count() - 1
}

#[test]
#[ignore = "child of remove_tree_fits_a_low_descriptor_limit"]
fn remove_tree_low_descriptor_limit_child() {
    let lim = libc::rlimit { rlim_cur: 256, rlim_max: libc::RLIM_INFINITY };
    let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut cur) };
    let lim = libc::rlimit { rlim_max: cur.rlim_max, ..lim };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) }, 0);
    let t = Tmp::new();
    let tree = t.join("tree");
    for i in 0..512 {
        let d = tree.join(format!("w{i}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f"), "f").unwrap();
    }
    chain(&tree.join("deep"), 100);
    let base = open_fds();
    let max = Rc::new(Cell::new(0usize));
    let n = Rc::new(Cell::new(0usize));
    {
        let (max, n) = (max.clone(), n.clone());
        set_before_open(Some(Box::new(move |_, _| {
            n.set(n.get() + 1);
            if n.get() % 7 == 0 || n.get() > 512 {
                max.set(max.get().max(open_fds()));
            }
        })));
    }
    let r = remove_tree_rooted(&t.path, &rp("tree"));
    set_before_open(None);
    let r = r.unwrap();
    assert!(r.is_complete(), "{:?}", r.stuck);
    assert!(gone(&tree));
    assert!(max.get() > base, "sampling saw no descriptors");
    assert!(max.get() <= base + 101 + 4, "max {} base {}", max.get(), base);
    println!("\n{MARKER}");
}

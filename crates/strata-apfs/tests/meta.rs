//! §7 `make_removable_clears_flags_before_mode` and
//! `set_file_meta_changes_metadata_through_a_handle`.
mod common;

use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use strata_apfs::seams::{MetaChange, set_before_meta};
use strata_apfs::*;
use strata_types::Code;

struct ClearMeta;
impl Drop for ClearMeta {
    fn drop(&mut self) {
        set_before_meta(None);
    }
}

/// Precondition helper: a std `fchmod` on the path's handle fails with EPERM.
fn std_fchmod_errno(p: &Path, m: u32) -> Option<i32> {
    let f = fs::File::open(p).unwrap();
    let r = unsafe { libc::fchmod(f.as_raw_fd(), m as libc::mode_t) };
    if r == 0 { None } else { std::io::Error::last_os_error().raw_os_error() }
}

fn std_futimens_errno(p: &Path) -> Option<i32> {
    let f = fs::File::open(p).unwrap();
    let ts = [
        libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
        libc::timespec { tv_sec: 1_000_000_000, tv_nsec: 0 },
    ];
    let r = unsafe { libc::futimens(f.as_raw_fd(), ts.as_ptr()) };
    if r == 0 { None } else { std::io::Error::last_os_error().raw_os_error() }
}

fn at(secs: i64, nsec: u32) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::new(secs as u64, nsec)
    } else {
        UNIX_EPOCH - Duration::new((-secs) as u64, 0) + Duration::new(0, nsec)
    }
}

const T2020: i64 = 1_577_836_800;

#[test]
fn make_removable_clears_flags_before_mode() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("other")).unwrap();

    // (a) 0555 + uchg directory.
    let d = root.join("sealed");
    fs::create_dir(&d).unwrap();
    set_mode(&d, 0o555);
    chflags(&d, "uchg");
    assert_eq!(std_fchmod_errno(&d, 0o755), Some(libc::EPERM), "precondition: fchmod on a uchg dir is EPERM");
    make_removable(&open_rooted(&root, &rp("sealed")).unwrap()).unwrap();
    assert_eq!(flags_of(&d), 0);
    assert_eq!(mode_of(&d), 0o755);
    rename_rooted(&root, &rp("sealed"), &rp("other/sealed"), RenameMode::Excl).unwrap();

    // (b) 0555 without flags; uappnd 0755; hidden preserved; 0400 file.
    let d2 = root.join("d2");
    fs::create_dir(&d2).unwrap();
    set_mode(&d2, 0o555);
    chflags(&d2, "hidden");
    make_removable(&open_rooted(&root, &rp("d2")).unwrap()).unwrap();
    assert_eq!((mode_of(&d2), flags_of(&d2)), (0o755, UF_HIDDEN));
    let d3 = root.join("d3");
    fs::create_dir(&d3).unwrap();
    chflags(&d3, "uappnd,hidden");
    make_removable(&open_rooted(&root, &rp("d3")).unwrap()).unwrap();
    assert_eq!((mode_of(&d3), flags_of(&d3)), (0o755, UF_HIDDEN));
    let f = root.join("f400");
    fs::write(&f, "x").unwrap();
    set_mode(&f, 0o400);
    make_removable(&open_rooted(&root, &rp("f400")).unwrap()).unwrap();
    assert_eq!(mode_of(&f), 0o600);

    // (c) no-op: ctime does not move.
    let f600 = root.join("f600");
    fs::write(&f600, "x").unwrap();
    set_mode(&f600, 0o600);
    for (p, name) in [(&d2, "d2"), (&f600, "f600")] {
        let c = ctime_ns(p);
        tick();
        make_removable(&open_rooted(&root, &rp(name)).unwrap()).unwrap();
        assert_eq!(ctime_ns(p), c, "{name}: no-op make_removable moved ctime");
    }

    // (d) a non-directory with a second name outside the tree.
    for (flags, mode) in [(Some("uchg"), 0o644), (None, 0o400)] {
        let inside = root.join("hl");
        let outside = t.join("outside-hl");
        fs::write(&inside, "x").unwrap();
        set_mode(&inside, mode);
        fs::hard_link(&inside, &outside).unwrap();
        if let Some(fl) = flags {
            chflags(&inside, fl);
        }
        let before = (flags_of(&outside), mode_of(&outside), ctime_ns(&outside));
        tick();
        let e = assert_reason(make_removable(&open_rooted(&root, &rp("hl")).unwrap()), Code::ReadOnly, "hard_link");
        let _ = e;
        assert_eq!((flags_of(&outside), mode_of(&outside), ctime_ns(&outside)), before);
        assert_eq!((flags_of(&inside), mode_of(&inside)), (before.0, before.1));
        chflags(&inside, "nouchg");
        fs::remove_file(&inside).unwrap();
        fs::remove_file(&outside).unwrap();
    }

    // (e) the second name appears after the handle was taken.
    let late = root.join("late");
    fs::write(&late, "x").unwrap();
    set_mode(&late, 0o400);
    let h = open_rooted(&root, &rp("late")).unwrap();
    fs::hard_link(&late, t.join("late2")).unwrap();
    assert_reason(make_removable(&h), Code::ReadOnly, "hard_link");
    assert_eq!(mode_of(&late), 0o400);

    // (g) failure after the first change: uchg is set again between the steps.
    let g = root.join("g");
    fs::create_dir(&g).unwrap();
    set_mode(&g, 0o555);
    chflags(&g, "uchg");
    let _clear = ClearMeta;
    let gp = g.clone();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen2 = seen.clone();
    set_before_meta(Some(Box::new(move |step, change| {
        seen2.borrow_mut().push((step, change));
        if (step, change) == (1, MetaChange::Mode) {
            let f = fs::File::open(&gp).unwrap();
            assert_eq!(unsafe { libc::fchflags(f.as_raw_fd(), UF_IMMUTABLE) }, 0);
        }
    })));
    let e = assert_code(make_removable(&open_rooted(&root, &rp("g")).unwrap()), Code::Unknown);
    set_before_meta(None);
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EPERM as i64));
    assert_eq!(*seen.borrow(), vec![(0, MetaChange::Flags), (1, MetaChange::Mode)]);
    assert_eq!(flags_of(&g), UF_IMMUTABLE);
    assert_eq!(mode_of(&g), 0o555);
}

#[test]
fn set_file_meta_changes_metadata_through_a_handle() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(&root).unwrap();

    // (a) a clone of a 0444 baseline with uchg + hidden and a 2020 mtime.
    let base = t.join("baseline");
    fs::write(&base, "baseline").unwrap();
    set_mode(&base, 0o444);
    set_mtime(&base, T2020, 123_456_789);
    chflags(&base, "uchg,hidden");
    clone_file_rooted(&base, &root, &rp("b"), None).unwrap();
    let b = root.join("b");
    assert_ne!(flags_of(&b) & UF_IMMUTABLE, 0, "clone carries uchg");
    assert_eq!(std_fchmod_errno(&b, 0o644), Some(libc::EPERM), "precondition: fchmod on uchg is EPERM");
    let atime = atime_ns(&b);
    let want = at(T2020 + 100, 987_654_321);
    let h = open_rooted_file(&root, &rp("b")).unwrap();
    set_file_meta(&h, &FileMeta { clear_flags: true, mode: Some(0o644), mtime: Some(want) }).unwrap();
    assert_eq!(flags_of(&b) & !UF_COMPRESSED, 0);
    assert_eq!(mode_of(&b), 0o644);
    assert_eq!(mtime_ns(&b), (T2020 + 100) as i128 * 1_000_000_000 + 987_654_321);
    assert_eq!(atime_ns(&b), atime);
    assert_eq!(fs::read(&b).unwrap(), b"baseline");
    chflags(&base, "nouchg");

    // (b) a compressed file keeps UF_COMPRESSED.
    let sys_src = Path::new("/Library/Documentation/Acknowledgements.rtf");
    assert!(
        sys_src.exists() && flags_of(sys_src) & UF_COMPRESSED != 0,
        "need a compressed system file at {} for this test",
        sys_src.display()
    );
    let c = root.join("compressed.rtf");
    let o = Command::new("ditto").arg(sys_src).arg(&c).output().unwrap();
    assert!(o.status.success());
    assert_ne!(flags_of(&c) & UF_COMPRESSED, 0, "ditto kept compression");
    set_mode(&c, 0o644);
    chflags(&c, "uchg,hidden");
    assert_eq!(flags_of(&c), UF_COMPRESSED | UF_IMMUTABLE | UF_HIDDEN);
    let h = open_rooted_file(&root, &rp("compressed.rtf")).unwrap();
    set_file_meta(&h, &FileMeta { clear_flags: true, ..Default::default() }).unwrap();
    assert_eq!(flags_of(&c), UF_COMPRESSED);
    assert_eq!(fs::read(&c).unwrap(), fs::read(sys_src).unwrap());

    // (c) no-ops leave ctime alone.
    let n = root.join("n");
    fs::write(&n, "n").unwrap();
    set_mode(&n, 0o644);
    let h = open_rooted_file(&root, &rp("n")).unwrap();
    for meta in [
        FileMeta::default(),
        FileMeta { mode: Some(0o644), ..Default::default() },
        FileMeta { clear_flags: true, ..Default::default() },
        FileMeta { mtime: None, ..Default::default() },
    ] {
        let c0 = ctime_ns(&n);
        tick();
        set_file_meta(&h, &meta).unwrap();
        assert_eq!(ctime_ns(&n), c0, "{meta:?}");
    }

    // (d) refusals change nothing.
    make_fifo(&root.join("fifo"));
    let h = open_rooted(&root, &rp("fifo")).unwrap();
    assert_reason(set_file_meta(&h, &FileMeta { mode: Some(0o600), ..Default::default() }), Code::NotFile, "fifo");
    assert_eq!(mode_of(&root.join("fifo")), 0o644);
    let (a, _b) = UnixStream::pair().unwrap();
    let sock = fs::File::from(OwnedFd::from(a));
    assert_reason(set_file_meta(&sock, &FileMeta::default()), Code::NotFile, "socket");
    let null = fs::File::open("/dev/null").unwrap();
    assert_reason(set_file_meta(&null, &FileMeta { mode: Some(0o600), ..Default::default() }), Code::NotFile, "device");
    let two = root.join("two");
    fs::write(&two, "2").unwrap();
    set_mode(&two, 0o644);
    fs::hard_link(&two, t.join("two-out")).unwrap();
    let before = (flags_of(&two), mode_of(&two), mtime_ns(&two), ctime_ns(&two));
    tick();
    let h = open_rooted_file(&root, &rp("two")).unwrap();
    assert_reason(
        set_file_meta(&h, &FileMeta { clear_flags: true, mode: Some(0o600), mtime: Some(at(T2020, 0)) }),
        Code::ReadOnly,
        "hard_link",
    );
    let out = t.join("two-out");
    assert_eq!((flags_of(&out), mode_of(&out), mtime_ns(&out), ctime_ns(&out)), before);

    // (e) the store writer's scenario: the clone dst replaced by a hard link
    // to a 0600 file outside the root.
    let secret = t.join("secret");
    fs::write(&secret, "secret").unwrap();
    set_mode(&secret, 0o600);
    fs::hard_link(&secret, root.join("got")).unwrap();
    let h = open_rooted_file(&root, &rp("got")).unwrap();
    assert_reason(set_file_meta(&h, &FileMeta { mode: Some(0o644), ..Default::default() }), Code::ReadOnly, "hard_link");
    assert_eq!(mode_of(&secret), 0o600);

    // (f) the second name appears after the handle was opened.
    let late = root.join("late");
    fs::write(&late, "l").unwrap();
    set_mode(&late, 0o644);
    let h = open_rooted_file(&root, &rp("late")).unwrap();
    fs::hard_link(&late, t.join("late-out")).unwrap();
    assert_reason(set_file_meta(&h, &FileMeta { mode: Some(0o600), ..Default::default() }), Code::ReadOnly, "hard_link");
    assert_eq!(mode_of(&late), 0o644);

    // (g) directory handles.
    let d = root.join("dir");
    fs::create_dir_all(d.join("child")).unwrap();
    fs::write(d.join("child/x"), "x").unwrap();
    set_mtime(&d, T2020, 0);
    chflags(&d, "uchg,hidden");
    assert!(st(&d).st_nlink >= 3);
    assert_eq!(std_fchmod_errno(&d, 0o555), Some(libc::EPERM));
    assert_eq!(std_futimens_errno(&d), Some(libc::EPERM));
    let child_snap = snapshot(&d.join("child"));
    let atime = atime_ns(&d);
    let h = open_rooted(&root, &rp("dir")).unwrap();
    set_file_meta(&h, &FileMeta { clear_flags: true, mode: Some(0o555), mtime: Some(at(T2020 + 5, 5)) }).unwrap();
    assert_eq!((flags_of(&d), mode_of(&d)), (0, 0o555));
    assert_eq!(mtime_ns(&d), (T2020 + 5) as i128 * 1_000_000_000 + 5);
    assert_eq!(atime_ns(&d), atime);
    assert_eq!(snapshot(&d.join("child")), child_snap);
    let c0 = ctime_ns(&d);
    tick();
    set_file_meta(&h, &FileMeta { mode: Some(0o555), ..Default::default() }).unwrap();
    assert_eq!(ctime_ns(&d), c0, "no-op on a directory moved ctime");
    set_file_meta(&h, &FileMeta { mode: Some(0o4755), ..Default::default() }).unwrap();
    assert_eq!(mode_of(&d), 0o755);
    // deps sealing: rename into place, then 0555 through the handle.
    fs::create_dir_all(root.join("staging/pkg/sub")).unwrap();
    fs::write(root.join("staging/pkg/sub/f"), "f").unwrap();
    fs::create_dir(root.join("sealed")).unwrap();
    rename_rooted(&root, &rp("staging/pkg"), &rp("sealed/pkg"), RenameMode::Excl).unwrap();
    let inner = snapshot(&root.join("sealed/pkg/sub"));
    let h = open_rooted(&root, &rp("sealed/pkg")).unwrap();
    set_file_meta(&h, &FileMeta { mode: Some(0o555), ..Default::default() }).unwrap();
    assert_eq!(mode_of(&root.join("sealed/pkg")), 0o555);
    assert_eq!(snapshot(&root.join("sealed/pkg/sub")), inner);

    // (h) failure after the first change.
    let hf = root.join("hfail");
    fs::write(&hf, "h").unwrap();
    set_mode(&hf, 0o644);
    chflags(&hf, "uchg");
    let mt = mtime_ns(&hf);
    let _clear = ClearMeta;
    let p2 = hf.clone();
    set_before_meta(Some(Box::new(move |step, change| {
        if (step, change) == (1, MetaChange::Mode) {
            let f = fs::File::open(&p2).unwrap();
            assert_eq!(unsafe { libc::fchflags(f.as_raw_fd(), UF_IMMUTABLE) }, 0);
        }
    })));
    let h = open_rooted_file(&root, &rp("hfail")).unwrap();
    let e = assert_code(
        set_file_meta(&h, &FileMeta { clear_flags: true, mode: Some(0o600), mtime: Some(at(T2020, 0)) }),
        Code::Unknown,
    );
    set_before_meta(None);
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EPERM as i64));
    assert_eq!((flags_of(&hf), mode_of(&hf), mtime_ns(&hf)), (UF_IMMUTABLE, 0o644, mt));
    chflags(&hf, "nouchg");

    // (i) out-of-range mtimes are clamped by the kernel, and only set once.
    let r = root.join("range");
    fs::write(&r, "r").unwrap();
    let h = open_rooted_file(&root, &rp("range")).unwrap();
    let year3000 = at(32_503_680_000, 0);
    set_file_meta(&h, &FileMeta { mtime: Some(year3000), ..Default::default() }).unwrap();
    assert_eq!(mtime_ns(&r), i64::MAX as i128);
    let c0 = ctime_ns(&r);
    std::thread::sleep(Duration::from_millis(1100));
    set_file_meta(&h, &FileMeta { mtime: Some(year3000), ..Default::default() }).unwrap();
    assert_eq!(ctime_ns(&r), c0);
    assert_eq!(mtime_ns(&r), i64::MAX as i128);
    let early = at(-9_223_372_037, 0);
    set_file_meta(&h, &FileMeta { mtime: Some(early), ..Default::default() }).unwrap();
    assert_eq!(mtime_ns(&r), i64::MIN as i128);
    let c1 = ctime_ns(&r);
    std::thread::sleep(Duration::from_millis(1100));
    set_file_meta(&h, &FileMeta { mtime: Some(early), ..Default::default() }).unwrap();
    assert_eq!(ctime_ns(&r), c1);
}

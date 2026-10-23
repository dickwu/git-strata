//! `Attrs`, `attrs_of` and the gen count contract (§4.2 C, §5.5, §8 Task 6).
mod common;

use std::ffi::CString;
use std::fs;
use std::io::{BufRead, BufReader, Read as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use strata_apfs::*;
use strata_types::Code;

fn a(root: &Path, p: &str) -> Attrs {
    attrs_of(root, &rp(p)).unwrap_or_else(|e| panic!("attrs_of {p}: {e:?}"))
}

fn cs(p: &Path) -> CString {
    CString::new(p.as_os_str().as_bytes()).unwrap()
}

fn open(p: &Path, flags: i32) -> i32 {
    let fd = unsafe { libc::open(cs(p).as_ptr(), flags | libc::O_CLOEXEC) };
    assert!(fd >= 0, "open {}: {}", p.display(), std::io::Error::last_os_error());
    fd
}

fn close(fd: i32) {
    assert_eq!(unsafe { libc::close(fd) }, 0);
}

fn acc_mode(fd: i32) -> i32 {
    unsafe { libc::fcntl(fd, libc::F_GETFL) & libc::O_ACCMODE }
}

fn pwrite(p: &Path, off: i64, b: &[u8]) {
    let fd = open(p, libc::O_WRONLY);
    assert_eq!(unsafe { libc::pwrite(fd, b.as_ptr().cast(), b.len(), off) }, b.len() as isize);
    close(fd);
}

fn ts(t: SystemTime) -> libc::timespec {
    let d = t.duration_since(UNIX_EPOCH).unwrap();
    libc::timespec { tv_sec: d.as_secs() as i64, tv_nsec: d.subsec_nanos() as i64 }
}

/// Set atime and mtime (no follow).
fn utimes(p: &Path, at: libc::timespec, mt: libc::timespec) {
    let t = [at, mt];
    assert_eq!(unsafe { libc::utimensat(libc::AT_FDCWD, cs(p).as_ptr(), t.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) }, 0);
}

fn atime(p: &Path) -> libc::timespec {
    let s = st(p);
    libc::timespec { tv_sec: s.st_atime, tv_nsec: s.st_atime_nsec }
}

fn sys_time(s: i64, ns: i64) -> SystemTime {
    if s >= 0 { UNIX_EPOCH + Duration::new(s as u64, ns as u32) } else { UNIX_EPOCH - Duration::from_secs(s.unsigned_abs()) + Duration::from_nanos(ns as u64) }
}

fn set_xattr(p: &Path, k: &str, v: &[u8]) {
    let k = CString::new(k).unwrap();
    assert_eq!(unsafe { libc::setxattr(cs(p).as_ptr(), k.as_ptr(), v.as_ptr().cast(), v.len(), 0, libc::XATTR_NOFOLLOW) }, 0);
}

fn ftruncate(p: &Path, len: i64) {
    let fd = open(p, libc::O_WRONLY);
    assert_eq!(unsafe { libc::ftruncate(fd, len) }, 0);
    close(fd);
}

struct Map {
    ptr: *mut libc::c_void,
    len: usize,
}

impl Map {
    fn new(fd: i32, len: usize, prot: i32, flags: i32) -> std::result::Result<Map, i32> {
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, flags, fd, 0) };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap());
        }
        Ok(Map { ptr, len })
    }
    fn write(&self, off: usize, b: u8) {
        unsafe { *(self.ptr as *mut u8).add(off) = b };
    }
    fn sync(&self) {
        assert_eq!(unsafe { libc::msync(self.ptr, self.len, libc::MS_SYNC) }, 0);
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

fn base_attrs() -> Attrs {
    Attrs {
        kind: FileKind::File,
        size: 10,
        mode: 0o644,
        gen_count: 5,
        mtime: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ctime: UNIX_EPOCH + Duration::from_secs(1_700_000_001),
        inode: 42,
    }
}

#[test]
fn attrs_unchanged_since_rules() {
    let b = base_attrs();
    assert_eq!(GEN_COUNT_UNAVAILABLE, 0);
    assert!(b.gen_count_known());
    assert!(b.unchanged_since(&b.clone()));
    let unknown = Attrs { gen_count: GEN_COUNT_UNAVAILABLE, ..b.clone() };
    assert!(!unknown.gen_count_known());
    let cases: Vec<(&str, Attrs, Attrs, bool)> = vec![
        ("later unknown", unknown.clone(), b.clone(), false),
        ("earlier unknown", b.clone(), unknown.clone(), false),
        ("both unknown", unknown.clone(), unknown.clone(), false),
        ("gen differs", Attrs { gen_count: 6, ..b.clone() }, b.clone(), false),
        ("inode differs", Attrs { inode: 43, ..b.clone() }, b.clone(), false),
        ("size differs", Attrs { size: 11, ..b.clone() }, b.clone(), false),
        ("mode differs", Attrs { mode: 0o755, ..b.clone() }, b.clone(), false),
        ("kind differs", Attrs { kind: FileKind::Dir, ..b.clone() }, b.clone(), false),
        (
            "symlink target differs",
            Attrs { kind: FileKind::Symlink { target: "a".into() }, ..b.clone() },
            Attrs { kind: FileKind::Symlink { target: "b".into() }, ..b.clone() },
            false,
        ),
        (
            "same symlink",
            Attrs { kind: FileKind::Symlink { target: "a".into() }, ..b.clone() },
            Attrs { kind: FileKind::Symlink { target: "a".into() }, ..b.clone() },
            true,
        ),
        (
            "times differ only",
            Attrs { mtime: UNIX_EPOCH, ctime: UNIX_EPOCH + Duration::from_secs(9), ..b.clone() },
            b.clone(),
            true,
        ),
        ("all equal", b.clone(), b.clone(), true),
    ];
    for (name, later, earlier, want) in cases {
        assert_eq!(later.unchanged_since(&earlier), want, "{name}");
    }
    // last_changed is max(mtime, ctime) both ways round.
    assert_eq!(b.last_changed(), b.ctime);
    let m = Attrs { mtime: UNIX_EPOCH + Duration::from_secs(1_800_000_000), ..b.clone() };
    assert_eq!(m.last_changed(), m.mtime);
}

fn std_kind(m: &fs::Metadata) -> &'static str {
    let t = m.file_type();
    use std::os::unix::fs::FileTypeExt;
    if t.is_symlink() {
        "symlink"
    } else if t.is_dir() {
        "dir"
    } else if t.is_file() {
        "file"
    } else if t.is_fifo() {
        "fifo"
    } else if t.is_socket() {
        "socket"
    } else {
        "other"
    }
}

#[test]
fn attrs_of_reports_every_field() {
    let t = Tmp::new();
    let root = t.path.clone();
    fs::write(root.join("f"), "hello world").unwrap();
    set_mode(&root.join("f"), 0o4755);
    fs::create_dir(root.join("d")).unwrap();
    fs::write(root.join("d/inner"), "x").unwrap();
    let target = PathBuf::from(std::ffi::OsStr::from_bytes(b"tar\xffget/../x"));
    symlink(&target, root.join("l")).unwrap();
    make_fifo(&root.join("p"));
    make_socket(&root.join("s"));
    for name in ["f", "d", "l", "p", "s", "d/inner"] {
        let at = a(&root, name);
        let m = fs::symlink_metadata(root.join(name)).unwrap();
        let want_kind = match std_kind(&m) {
            "file" => FileKind::File,
            "dir" => FileKind::Dir,
            "symlink" => FileKind::Symlink { target: target.clone() },
            "fifo" => FileKind::Fifo,
            "socket" => FileKind::Socket,
            k => panic!("{k}"),
        };
        assert_eq!(at.kind, want_kind, "{name}");
        let want_size = match want_kind {
            FileKind::File => m.len(),
            FileKind::Symlink { .. } => target.as_os_str().len() as u64,
            _ => 0,
        };
        assert_eq!(at.size, want_size, "{name}");
        assert_eq!(at.mode, m.mode() & 0o7777, "{name}");
        assert_eq!(at.inode, m.ino(), "{name}");
        assert_eq!(at.mtime, sys_time(m.mtime(), m.mtime_nsec()), "{name}");
        assert_eq!(at.ctime, sys_time(m.ctime(), m.ctime_nsec()), "{name}");
        assert!(at.gen_count_known(), "{name}: gen_count {}", at.gen_count);
    }
    assert_eq!(a(&root, "f").mode, 0o4755);
}

#[test]
fn attrs_of_does_not_follow_links() {
    let t = Tmp::new();
    let root = t.path.clone();
    let out = Tmp::new();
    fs::create_dir(out.join("dir")).unwrap();
    fs::write(out.join("dir/f"), "outside").unwrap();
    symlink(out.join("dir"), root.join("l")).unwrap();
    let at = a(&root, "l");
    assert_eq!(at.kind, FileKind::Symlink { target: out.join("dir") });
    assert_eq!(at.inode, fs::symlink_metadata(root.join("l")).unwrap().ino());
    assert_reason(attrs_of(&root, &rp("l/f")), Code::NotDirectory, "symlink");
    assert_code(attrs_of(&root, &rp("missing")), Code::NoSuchPath);
    assert_code(attrs_of(&root, &rp("missing/x")), Code::NoSuchPath);
    fs::write(root.join("one"), "1").unwrap();
    assert_eq!(a(&root, "one").kind, FileKind::File);
}

#[test]
fn gen_count_changes_on_write_not_on_touch() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fs::write(&f, "0123456789").unwrap();
    // (a) writes change it.
    let base = a(&root, "f");
    assert!(base.gen_count_known());
    let at0 = atime(&f);
    pwrite(&f, 0, b"0"); // the same byte
    let w = a(&root, "f");
    assert_ne!(w.gen_count, base.gen_count, "same-byte pwrite");
    assert!(!w.unchanged_since(&base));
    utimes(&f, at0, ts(base.mtime));
    let w2 = a(&root, "f");
    assert_eq!(w2.gen_count, w.gen_count, "restoring times leaves it");
    assert_eq!(w2.mtime, base.mtime);
    assert!(!w2.unchanged_since(&base));
    let mut prev = w2;
    ftruncate(&f, 20);
    let g = a(&root, "f");
    assert_ne!(g.gen_count, prev.gen_count, "ftruncate up");
    assert!(!g.unchanged_since(&prev));
    prev = g;
    let fd = open(&f, libc::O_WRONLY | libc::O_TRUNC);
    close(fd);
    let g = a(&root, "f");
    assert_ne!(g.gen_count, prev.gen_count, "O_TRUNC");
    assert!(!g.unchanged_since(&prev));
    fs::write(&f, "0123456789").unwrap();

    // (b) metadata-only operations leave it.
    let mut cur = a(&root, "f");
    let mut path = f.clone();
    let mut rel = "f".to_string();
    type Step = Box<dyn Fn(&Path)>;
    let steps: Vec<(&str, Step)> = vec![
        ("utimensat", Box::new(|p| utimes(p, ts(UNIX_EPOCH + Duration::from_secs(1_000_000)), ts(UNIX_EPOCH + Duration::from_secs(2_000_000))))),
        ("utimensat back", Box::new(|p| utimes(p, ts(SystemTime::now()), ts(SystemTime::now())))),
        ("chmod +x", Box::new(|p| set_mode(p, 0o755))),
        ("setxattr", Box::new(|p| set_xattr(p, "com.example.k", b"v"))),
        ("chflags", Box::new(|p| chflags(p, "hidden"))),
        ("hard link", Box::new(|p| fs::hard_link(p, p.with_file_name("hl")).unwrap())),
        ("ftruncate same size", Box::new(|p| ftruncate(p, 10))),
        ("open O_WRONLY, no write", Box::new(|p| close(open(p, libc::O_WRONLY)))),
        ("pread", Box::new(|p| {
            let mut s = String::new();
            fs::File::open(p).unwrap().read_to_string(&mut s).unwrap();
        })),
    ];
    for (name, step) in steps {
        step(&path);
        let now = a(&root, &rel);
        assert_eq!(now.gen_count, cur.gen_count, "{name}");
        if name == "chmod +x" {
            assert_eq!(now.mode, 0o755);
            assert!(!now.unchanged_since(&cur), "{name}: mode is part of the version");
        } else {
            assert!(now.unchanged_since(&cur), "{name}");
        }
        cur = now;
    }
    // rename
    fs::rename(&path, root.join("g")).unwrap();
    path = root.join("g");
    rel = "g".into();
    let now = a(&root, &rel);
    assert_eq!(now.gen_count, cur.gen_count, "rename");
    assert!(now.unchanged_since(&cur), "rename");
    let _ = path;
}

/// Write-and-restore-times through a mapping keeps the comparison honest.
fn fill(p: &Path) {
    fs::write(p, vec![b'a'; 4096]).unwrap();
}

#[test]
fn gen_count_is_unknown_while_a_writable_mapping_exists() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fill(&f);
    let base = a(&root, "f");
    assert!(base.gen_count_known(), "a non-zero baseline before mapping");
    let fd = open(&f, libc::O_RDWR);
    assert_eq!(acc_mode(fd), libc::O_RDWR, "the descriptor is O_RDWR");
    let map = Map::new(fd, 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED).unwrap();
    close(fd); // the mapping outlives its descriptor
    let m1 = a(&root, "f");
    assert_eq!(m1.gen_count, GEN_COUNT_UNAVAILABLE, "after mmap");
    assert!(!m1.gen_count_known());
    assert!(!m1.unchanged_since(&base));
    let at0 = atime(&f);
    map.write(0, b'Z');
    let m2 = a(&root, "f");
    assert_eq!(m2.gen_count, GEN_COUNT_UNAVAILABLE, "after writing through the mapping");
    assert!(!m2.unchanged_since(&base));
    map.sync();
    utimes(&f, at0, ts(base.mtime));
    let m3 = a(&root, "f");
    assert_eq!(m3.gen_count, GEN_COUNT_UNAVAILABLE, "after msync");
    assert!(!m3.unchanged_since(&base));
    drop(map);
    let after = a(&root, "f");
    assert!(after.gen_count_known(), "after munmap");
    assert_ne!(after.gen_count, base.gen_count, "the write through the mapping is a new version");
    assert!(!after.unchanged_since(&base));
    assert_eq!(fs::read(&f).unwrap()[0], b'Z');
    // Settled: known and stable again.
    assert_eq!(a(&root, "f").gen_count, after.gen_count);
}

#[test]
fn gen_count_unknown_with_creator_descriptor_still_open() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fill(&f);
    let base = a(&root, "f");
    assert!(base.gen_count_known());
    let fd = open(&f, libc::O_RDWR);
    assert_eq!(acc_mode(fd), libc::O_RDWR);
    let map = Map::new(fd, 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED).unwrap();
    assert_eq!(a(&root, "f").gen_count, GEN_COUNT_UNAVAILABLE);
    map.write(1, b'Y');
    assert_eq!(a(&root, "f").gen_count, GEN_COUNT_UNAVAILABLE);
    assert!(!a(&root, "f").unchanged_since(&base));
    drop(map);
    close(fd);
    let after = a(&root, "f");
    assert!(after.gen_count_known());
    assert!(!after.unchanged_since(&base));
}

const CHILD_ENV: &str = "STRATA_APFS_MAP_CHILD";

/// Child entry for `gen_count_unknown_for_a_mapping_made_by_another_process`:
/// maps the file named in the env var writable+shared, says MAPPED, holds the
/// mapping until stdin closes.
#[test]
fn mapping_child() {
    let Some(p) = std::env::var_os(CHILD_ENV) else { return };
    let fd = open(Path::new(&p), libc::O_RDWR);
    assert_eq!(acc_mode(fd), libc::O_RDWR);
    let map = Map::new(fd, 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED).unwrap();
    close(fd);
    println!("\nMAPPED");
    let mut sink = Vec::new();
    std::io::stdin().read_to_end(&mut sink).unwrap();
    drop(map);
}

#[test]
fn gen_count_unknown_for_a_mapping_made_by_another_process() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fill(&f);
    let base = a(&root, "f");
    assert!(base.gen_count_known());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "mapping_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, &f)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "child exited before mapping");
        if line.trim() == "MAPPED" {
            break;
        }
    }
    let during = a(&root, "f");
    assert_eq!(during.gen_count, GEN_COUNT_UNAVAILABLE, "another process holds a writable mapping");
    assert!(!during.unchanged_since(&base));
    drop(child.stdin.take());
    let mut rest = String::new();
    out.read_to_string(&mut rest).unwrap();
    assert!(child.wait().unwrap().success(), "child failed: {rest}");
    let after = a(&root, "f");
    assert!(after.gen_count_known(), "after the child exits");
}

#[test]
fn gen_count_unknown_for_a_read_only_mapping_of_a_writable_descriptor() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fill(&f);
    let base = a(&root, "f");
    assert!(base.gen_count_known());
    for close_first in [false, true] {
        let before = a(&root, "f");
        let fd = open(&f, libc::O_RDWR);
        assert_eq!(acc_mode(fd), libc::O_RDWR, "the trigger is the descriptor's access mode");
        let map = Map::new(fd, 4096, libc::PROT_READ, libc::MAP_SHARED).unwrap();
        if close_first {
            close(fd);
        }
        let during = a(&root, "f");
        assert_eq!(during.gen_count, GEN_COUNT_UNAVAILABLE, "PROT_READ on O_RDWR, close_first={close_first}");
        assert!(!during.unchanged_since(&before));
        drop(map);
        if !close_first {
            close(fd);
        }
        let after = a(&root, "f");
        assert!(after.gen_count_known());
        assert!(after.gen_count > before.gen_count, "bumped though never written: {} -> {}", before.gen_count, after.gen_count);
        assert_eq!(after.gen_count, before.gen_count + 1);
        assert_eq!(fs::read(&f).unwrap(), vec![b'a'; 4096]);
    }
}

#[test]
fn read_only_and_private_mappings_keep_gen_count_known() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fill(&f);
    let base = a(&root, "f");
    assert!(base.gen_count_known());
    // O_RDONLY, PROT_READ, MAP_SHARED.
    let fd = open(&f, libc::O_RDONLY);
    assert_eq!(acc_mode(fd), libc::O_RDONLY);
    let map = Map::new(fd, 4096, libc::PROT_READ, libc::MAP_SHARED).unwrap();
    let during = a(&root, "f");
    assert_eq!(during.gen_count, base.gen_count);
    assert!(during.unchanged_since(&base));
    // O_RDONLY + PROT_WRITE + MAP_SHARED cannot exist.
    assert_eq!(Map::new(fd, 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED).err(), Some(libc::EACCES));
    drop(map);
    close(fd);
    assert_eq!(a(&root, "f").gen_count, base.gen_count);
    // O_RDWR, PROT_WRITE, MAP_PRIVATE: writes never reach the file.
    let fd = open(&f, libc::O_RDWR);
    assert_eq!(acc_mode(fd), libc::O_RDWR);
    let map = Map::new(fd, 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE).unwrap();
    map.write(0, b'Q');
    let during = a(&root, "f");
    assert!(during.gen_count_known());
    assert_eq!(during.gen_count, base.gen_count);
    drop(map);
    close(fd);
    assert_eq!(a(&root, "f").gen_count, base.gen_count);
    assert_eq!(fs::read(&f).unwrap(), vec![b'a'; 4096]);
}

#[test]
fn gen_count_alone_is_not_a_version() {
    let t = Tmp::new();
    let root = t.path.clone();
    fs::write(root.join("src"), "source").unwrap();
    pwrite(&root.join("src"), 0, b"S");
    let src = a(&root, "src");
    clone_file(&root.join("src"), &root.join("clone")).unwrap();
    let cl = a(&root, "clone");
    assert!(cl.gen_count_known());
    assert!(cl.gen_count <= src.gen_count, "a clone starts low: {} vs {}", cl.gen_count, src.gen_count);
    assert_ne!(cl.inode, src.inode);
    assert!(!cl.unchanged_since(&src));
    // Same gen count, different object, same path.
    fs::write(root.join("f"), "one").unwrap();
    fs::write(root.join("other"), "two").unwrap();
    let before = a(&root, "f");
    let other = a(&root, "other");
    assert_eq!(before.gen_count, other.gen_count, "two fresh files share a gen count");
    fs::rename(root.join("other"), root.join("f")).unwrap();
    let after = a(&root, "f");
    assert_eq!(after.gen_count, before.gen_count);
    assert_ne!(after.inode, before.inode);
    assert!(!after.unchanged_since(&before));
}

#[test]
fn directory_gen_count_tracks_entries_not_content() {
    let t = Tmp::new();
    let root = t.path.clone();
    fs::create_dir(root.join("d")).unwrap();
    let d = root.join("d");
    let mut cur = a(&root, "d");
    assert!(cur.gen_count_known());
    let mut bump = |what: &str, f: &dyn Fn()| {
        f();
        let now = a(&root, "d");
        assert!(now.gen_count > cur.gen_count, "{what}: {} -> {}", cur.gen_count, now.gen_count);
        assert!(!now.unchanged_since(&cur), "{what}");
        cur = now;
    };
    bump("create", &|| fs::write(d.join("x"), "1").unwrap());
    bump("rename", &|| fs::rename(d.join("x"), d.join("y")).unwrap());
    bump("unlink", &|| fs::remove_file(d.join("y")).unwrap());
    bump("mkdir", &|| fs::create_dir(d.join("sub")).unwrap());
    bump("rmdir", &|| fs::remove_dir(d.join("sub")).unwrap());
    fs::write(d.join("c"), "content").unwrap();
    let before = a(&root, "d");
    pwrite(&d.join("c"), 0, b"CONTENT and more");
    let after = a(&root, "d");
    assert_eq!(after.gen_count, before.gen_count, "child content is not the directory's");
    assert!(after.unchanged_since(&before));
}

#[test]
fn attrs_versions_differ_for_same_second_writes() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    // Retry until both writes land in the same second.
    for _ in 0..20 {
        fs::write(&f, "aaaa").unwrap();
        let one = a(&root, "f");
        fs::write(&f, "bbbb").unwrap();
        let two = a(&root, "f");
        let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).unwrap().as_secs();
        if secs(one.mtime) != secs(two.mtime) {
            continue;
        }
        assert_eq!(one.size, two.size);
        assert_ne!(one, two);
        assert!(!two.unchanged_since(&one));
        let ns = |t: SystemTime| t.duration_since(UNIX_EPOCH).unwrap().subsec_nanos();
        assert!(ns(one.mtime) != 0 || ns(two.mtime) != 0, "mtime carries nanoseconds");
        return;
    }
    panic!("never managed two writes in one second");
}

#[test]
fn same_size_write_with_restored_mtime_is_a_new_version() {
    let t = Tmp::new();
    let root = t.path.clone();
    let f = root.join("f");
    fs::write(&f, "version-1").unwrap();
    let base = a(&root, "f");
    let at0 = atime(&f);
    fs::write(&f, "version-2").unwrap();
    utimes(&f, at0, ts(base.mtime));
    let now = a(&root, "f");
    assert_eq!(now.mtime, base.mtime);
    assert_eq!(now.size, base.size);
    assert_ne!(now.gen_count, base.gen_count);
    assert!(!now.unchanged_since(&base));
    // `touch -r`: timestamps only.
    let before = a(&root, "f");
    fs::write(root.join("ref"), "r").unwrap();
    set_mtime(&root.join("ref"), 1_500_000_000, 0);
    let st_ref = st(&root.join("ref"));
    utimes(
        &f,
        libc::timespec { tv_sec: st_ref.st_atime, tv_nsec: st_ref.st_atime_nsec },
        libc::timespec { tv_sec: st_ref.st_mtime, tv_nsec: st_ref.st_mtime_nsec },
    );
    let touched = a(&root, "f");
    assert_ne!(touched.mtime, before.mtime);
    assert!(touched.unchanged_since(&before));
}

const Y2020: i64 = 1_580_000_000;
const Y2017: i64 = 1_490_000_000;

#[test]
fn attrs_last_changed_is_the_later_of_mtime_and_ctime() {
    let t = Tmp::new();
    let root = t.path.clone();
    let y2020 = sys_time(Y2020, 0);
    let recent = || SystemTime::now() - Duration::from_secs(5);
    // Files.
    fs::write(root.join("src"), "s").unwrap();
    set_mtime(&root.join("src"), Y2020, 0);
    clone_file(&root.join("src"), &root.join("c")).unwrap();
    let c = a(&root, "c");
    assert_eq!(c.mtime, y2020);
    assert!(c.ctime > recent());
    assert_eq!(c.last_changed(), c.ctime);
    tick();
    rename_rooted(&root, &rp("c"), &rp("c2"), RenameMode::Excl).unwrap();
    let r = a(&root, "c2");
    assert_eq!(r.mtime, c.mtime);
    assert!(r.ctime > c.ctime);
    assert!(r.last_changed() > c.last_changed());
    assert_eq!((r.inode, r.gen_count), (c.inode, c.gen_count));
    let mut prev = r;
    for what in ["chmod", "chflags"] {
        tick();
        match what {
            "chmod" => set_mode(&root.join("c2"), 0o600),
            _ => chflags(&root.join("c2"), "hidden"),
        }
        let now = a(&root, "c2");
        assert_eq!(now.mtime, prev.mtime, "{what}");
        assert!(now.ctime > prev.ctime, "{what}");
        prev = now;
    }
    tick();
    set_mtime(&root.join("c2"), Y2017, 0);
    let old = a(&root, "c2");
    assert_eq!(old.mtime, sys_time(Y2017, 0));
    assert!(old.last_changed() >= prev.ctime);
    assert!(old.last_changed() > recent());
    tick();
    pwrite(&root.join("c2"), 0, b"w");
    let w = a(&root, "c2");
    assert!(w.mtime > old.ctime && w.ctime > old.ctime);
    // Directories.
    let s = root.join("tree");
    fs::create_dir_all(s.join("full/inner")).unwrap();
    fs::create_dir(s.join("empty")).unwrap();
    fs::write(s.join("full/f"), "f").unwrap();
    fs::write(s.join("full/inner/g"), "g").unwrap();
    for p in ["full/inner/g", "full/f", "full/inner", "full", "empty", ""] {
        set_mtime(&if p.is_empty() { s.clone() } else { s.join(p) }, Y2020, 0);
    }
    clone_tree(&s, &root.join("ct")).unwrap();
    for p in ["ct/full/f", "ct/full/inner/g", "ct/empty"] {
        assert_eq!(a(&root, p).mtime, y2020, "{p}");
    }
    for p in ["ct", "ct/full", "ct/full/inner"] {
        assert!(a(&root, p).mtime > recent(), "{p}: a directory with children");
    }
    for p in ["ct", "ct/full", "ct/full/inner", "ct/full/f", "ct/full/inner/g", "ct/empty"] {
        assert!(a(&root, p).last_changed() > recent(), "{p}");
    }
    fs::create_dir(root.join("other")).unwrap();
    let before = a(&root, "ct/empty");
    tick();
    rename_rooted(&root, &rp("ct/empty"), &rp("other/empty"), RenameMode::Excl).unwrap();
    let moved = a(&root, "other/empty");
    assert_eq!(moved.mtime, before.mtime);
    assert!(moved.ctime > before.ctime);
    let (x0, y0) = (a(&root, "ct/full"), a(&root, "other/empty"));
    tick();
    rename_rooted(&root, &rp("ct/full"), &rp("other/empty"), RenameMode::Swap).unwrap();
    let (x1, y1) = (a(&root, "other/empty"), a(&root, "ct/full"));
    assert_eq!((x1.inode, y1.inode), (x0.inode, y0.inode));
    assert_eq!((x1.mtime, y1.mtime), (x0.mtime, y0.mtime));
    assert!(x1.ctime > x0.ctime && y1.ctime > y0.ctime);
}

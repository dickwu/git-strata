mod common;

use std::cell::Cell;
use std::fs;
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, symlink};
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::*;
use strata_apfs::seams::set_before_open;
use strata_apfs::*;
use strata_types::{Code, RepoPath};

fn read_all(mut f: fs::File) -> String {
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}

struct ClearHook;
impl Drop for ClearHook {
    fn drop(&mut self) {
        set_before_open(None);
    }
}

/// Run `name` (a test in this binary) in a child process with a deadline.
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

fn is_child() -> bool {
    std::env::var_os("STRATA_APFS_CHILD").is_some()
}

#[test]
fn rooted_open_opens_files_and_directories() {
    let t = Tmp::new();
    fs::create_dir_all(t.join("dir/sub")).unwrap();
    fs::write(t.join("dir/sub/file"), "hello").unwrap();
    fs::write(t.join("top"), "top").unwrap();
    assert_eq!(read_all(open_rooted(&t.path, &rp("dir/sub/file")).unwrap()), "hello");
    assert_eq!(read_all(open_rooted_file(&t.path, &rp("top")).unwrap()), "top");
    let d = open_rooted(&t.path, &rp("dir/sub")).unwrap();
    assert!(d.metadata().unwrap().is_dir());
    let name = std::ffi::CString::new("file").unwrap();
    let fd = unsafe { libc::openat(d.as_raw_fd(), name.as_ptr(), libc::O_RDONLY) };
    assert!(fd >= 0);
    unsafe { libc::close(fd) };
    assert_code(open_rooted_file(&t.path, &rp("dir")), Code::IsDirectory);
    for bad in ["", ".", "./dir/file", "../file", "dir/../file", "/abs"] {
        assert!(RepoPath::try_from(bad.to_string()).is_err(), "{bad}");
    }
    // An ancestor of the root that is a symlink (/tmp -> /private/tmp).
    let base = tempfile::Builder::new().prefix("strata-rooted").tempdir_in("/tmp").unwrap();
    let root = Path::new("/tmp").join(base.path().file_name().unwrap());
    fs::write(root.join("f"), "via tmp").unwrap();
    assert_eq!(read_all(open_rooted_file(&root, &rp("f")).unwrap()), "via tmp");
}

#[test]
fn open_root_returns_the_root_handle() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("child"), "c").unwrap();
    let h = open_root(&root).unwrap();
    assert!(h.metadata().unwrap().is_dir());
    let name = std::ffi::CString::new("child").unwrap();
    let fd = unsafe { libc::openat(h.as_raw_fd(), name.as_ptr(), libc::O_RDONLY) };
    assert!(fd >= 0);
    unsafe { libc::close(fd) };

    let base = tempfile::Builder::new().prefix("strata-root").tempdir_in("/tmp").unwrap();
    let via_tmp = Path::new("/tmp").join(base.path().file_name().unwrap());
    assert!(open_root(&via_tmp).unwrap().metadata().unwrap().is_dir());

    symlink(&root, t.join("lnk")).unwrap();
    let before = snapshot(&root);
    assert_reason(open_root(&t.join("lnk")), Code::NotDirectory, "symlink");
    fs::write(t.join("file"), "f").unwrap();
    assert_reason(open_root(&t.join("file")), Code::NotDirectory, "not_a_directory");
    assert_code(open_root(&t.join("missing")), Code::NoSuchPath);
    let r = root.to_str().unwrap();
    let l = t.join("lnk");
    let l = l.to_str().unwrap();
    for bad in [
        format!("{r}/"),
        format!("{r}//"),
        format!("{r}/."),
        format!("{r}/./sub"),
        format!("{r}/../root"),
        "repo".into(),
        "a/b".into(),
        ".hidden".into(),
        format!("{l}/"),
        format!("{l}//"),
        format!("{l}/."),
    ] {
        assert_reason(open_root(Path::new(&bad)), Code::InvalidInput, "root_not_canonical");
    }
    assert_eq!(snapshot(&root), before);
}

#[test]
fn rooted_open_treats_colon_and_backslash_names_literally() {
    let t = Tmp::new();
    fs::create_dir_all(t.join("dir")).unwrap();
    fs::create_dir_all(t.join("a")).unwrap();
    let names = ["a:b", "C:relative.txt", "dir/name:stream", "a\\b.txt", "a/b.txt", "with space", "cafe\u{301}"];
    for (i, n) in names.iter().enumerate() {
        fs::write(t.path.join(n), format!("content {i}")).unwrap();
    }
    for (i, n) in names.iter().enumerate() {
        let got = read_all(open_rooted_file(&t.path, &rp(n)).unwrap());
        assert_eq!(got, format!("content {i}"), "{n}");
    }
}

#[test]
fn rooted_open_fifo_is_nonblocking_and_symlinks_are_refused() {
    run_child("fifo_child", 20);
}

#[test]
fn fifo_child() {
    if !is_child() {
        return;
    }
    let t = Tmp::new();
    let outside = Tmp::new();
    fs::write(outside.join("secret"), "SECRET").unwrap();
    fs::write(t.join("inside"), "inside").unwrap();
    make_fifo(&t.join("fifo"));
    symlink(outside.join("secret"), t.join("out_link")).unwrap();
    symlink("inside", t.join("in_link")).unwrap();
    let mut f = open_rooted(&t.path, &rp("fifo")).unwrap();
    assert!(f.metadata().unwrap().file_type().is_fifo());
    let mut buf = [0u8; 8];
    match f.read(&mut buf) {
        Ok(0) => {}
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        other => panic!("unexpected fifo read {other:?}"),
    }
    assert_reason(open_rooted_file(&t.path, &rp("fifo")), Code::NotFile, "fifo");
    for l in ["out_link", "in_link"] {
        assert_reason(open_rooted(&t.path, &rp(l)), Code::NotFile, "symlink");
        assert_reason(open_rooted_file(&t.path, &rp(l)), Code::NotFile, "symlink");
    }
}

#[test]
fn rooted_open_rejects_a_directory_swapped_for_a_link() {
    let _c = ClearHook;
    let outside = Tmp::new();
    fs::write(outside.join(".env"), "SECRET").unwrap();
    for target_inside in [false, true] {
        let t = Tmp::new();
        fs::create_dir_all(t.join("dir")).unwrap();
        fs::write(t.join("dir/.env"), "ok").unwrap();
        fs::create_dir_all(t.join("other")).unwrap();
        fs::write(t.join("other/.env"), "other").unwrap();
        let root = t.path.clone();
        let target = if target_inside { t.join("other") } else { outside.path.clone() };
        set_before_open(Some(Box::new(move |_, name| {
            if name == "dir" && root.join("dir").is_dir() && !root.join("dir").is_symlink() {
                fs::remove_dir_all(root.join("dir")).unwrap();
                symlink(&target, root.join("dir")).unwrap();
            }
        })));
        assert_reason(open_rooted(&t.path, &rp("dir/.env")), Code::NotDirectory, "symlink");
        set_before_open(None);
    }
}

#[test]
fn rooted_open_keeps_the_parent_handle_after_a_swap() {
    let _c = ClearHook;
    let t = Tmp::new();
    let outside = Tmp::new();
    fs::write(outside.join("file"), "OUTSIDE").unwrap();
    fs::create_dir_all(t.join("dir")).unwrap();
    fs::write(t.join("dir/file"), "original").unwrap();
    let root = t.path.clone();
    let out = outside.path.clone();
    set_before_open(Some(Box::new(move |step, name| {
        if step == 1 && name == "file" {
            fs::rename(root.join("dir"), root.join("moved")).unwrap();
            symlink(&out, root.join("dir")).unwrap();
        }
    })));
    assert_eq!(read_all(open_rooted_file(&t.path, &rp("dir/file")).unwrap()), "original");
    set_before_open(None);
    assert_reason(open_rooted_file(&t.path, &rp("dir/file")), Code::NotDirectory, "symlink");
}

#[test]
fn rooted_open_pins_the_root() {
    let _c = ClearHook;
    let t = Tmp::new();
    let outside = Tmp::new();
    fs::create_dir_all(outside.join("sub")).unwrap();
    fs::write(outside.join("sub/file"), "OUTSIDE").unwrap();
    let root = t.join("root");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/file"), "original").unwrap();
    let (r2, o2, t2) = (root.clone(), outside.path.clone(), t.path.clone());
    set_before_open(Some(Box::new(move |step, _| {
        if step == 0 {
            fs::rename(&r2, t2.join("root.moved")).unwrap();
            symlink(&o2, &r2).unwrap();
        }
    })));
    assert_eq!(read_all(open_rooted_file(&root, &rp("sub/file")).unwrap()), "original");
    set_before_open(None);
    assert_reason(open_rooted_file(&root, &rp("sub/file")), Code::NotDirectory, "symlink");
}

#[test]
fn rooted_open_rejects_a_final_symlink_swap() {
    let _c = ClearHook;
    let outside = Tmp::new();
    fs::write(outside.join("secret"), "SECRET").unwrap();
    for file_only in [false, true] {
        let t = Tmp::new();
        fs::create_dir_all(t.join("d")).unwrap();
        fs::write(t.join("d/target"), "ok").unwrap();
        let (root, sec) = (t.path.clone(), outside.join("secret"));
        let fired = Rc::new(Cell::new(false));
        let f2 = fired.clone();
        set_before_open(Some(Box::new(move |_, name| {
            if name == "target" {
                fs::remove_file(root.join("d/target")).unwrap();
                symlink(&sec, root.join("d/target")).unwrap();
                f2.set(true);
            }
        })));
        let r = if file_only {
            open_rooted_file(&t.path, &rp("d/target"))
        } else {
            open_rooted(&t.path, &rp("d/target"))
        };
        let e = assert_reason(r, Code::NotFile, "symlink");
        assert_eq!(detail_i64(&e, "errno"), Some(libc::ELOOP as i64));
        assert!(fired.get());
        set_before_open(None);
    }
}

#[test]
fn rooted_open_file_refuses_a_fifo_swapped_in() {
    run_child("fifo_swap_child", 20);
}

#[test]
fn fifo_swap_child() {
    if !is_child() {
        return;
    }
    let _c = ClearHook;
    let t = Tmp::new();
    fs::write(t.join("target"), "ok").unwrap();
    let root = t.path.clone();
    set_before_open(Some(Box::new(move |_, name| {
        if name == "target" && root.join("target").is_file() {
            fs::remove_file(root.join("target")).unwrap();
            make_fifo(&root.join("target"));
        }
    })));
    assert_reason(open_rooted_file(&t.path, &rp("target")), Code::NotFile, "fifo");
    set_before_open(None);
    let f = open_rooted(&t.path, &rp("target")).unwrap();
    assert!(f.metadata().unwrap().file_type().is_fifo());
}

#[test]
fn rooted_open_error_codes() {
    let t = Tmp::new();
    fs::write(t.join("file"), "x").unwrap();
    make_socket(&t.join("sock"));
    fs::write(t.join("locked"), "x").unwrap();
    set_mode(&t.join("locked"), 0o000);
    assert_code(open_rooted(&t.path, &rp("missing")), Code::NoSuchPath);
    assert_code(open_rooted(&t.path, &rp("missing/deeper")), Code::NoSuchPath);
    assert_reason(open_rooted(&t.path, &rp("file/x")), Code::NotDirectory, "not_a_directory");
    assert_reason(open_rooted(&t.path, &rp("sock")), Code::NotFile, "socket");
    assert_reason(open_rooted(Path::new("/dev"), &rp("null")), Code::NotFile, "device");
    let e = assert_code(open_rooted(&t.path, &rp("locked")), Code::Unknown);
    assert_eq!(detail_i64(&e, "errno"), Some(libc::EACCES as i64));
    set_mode(&t.join("locked"), 0o644);
    let nfd = "cafe\u{301}";
    fs::write(t.path.join(nfd), "nfd").unwrap();
    assert_eq!(read_all(open_rooted_file(&t.path, &rp("caf\u{e9}")).unwrap()), "nfd");
}

#[test]
fn rooted_open_name_too_long_is_invalid_input() {
    let t = Tmp::new();
    let wide = "\u{3042}".repeat(255);
    fs::write(t.join(&wide), "wide").unwrap();
    assert_eq!(read_all(open_rooted_file(&t.path, &rp(&wide)).unwrap()), "wide");
    let ok = "a".repeat(255);
    fs::create_dir(t.join(&ok)).unwrap();
    fs::write(t.join(&format!("{ok}/{ok}")), "deep").unwrap();
    assert_eq!(read_all(open_rooted_file(&t.path, &rp(&format!("{ok}/{ok}"))).unwrap()), "deep");
    let bad = "\u{3042}".repeat(256);
    for p in [bad.clone(), format!("{ok}/{bad}"), format!("{bad}/x")] {
        for file_only in [false, true] {
            let r = if file_only { open_rooted_file(&t.path, &rp(&p)) } else { open_rooted(&t.path, &rp(&p)) };
            let e = assert_reason(r, Code::InvalidInput, "name_too_long");
            assert_eq!(detail_i64(&e, "errno"), Some(libc::ENAMETOOLONG as i64));
        }
    }
}

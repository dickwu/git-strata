mod common;

use std::fs;
use std::io;
use std::path::Path;

use common::*;
use strata_apfs::*;
use strata_types::Code;

fn os(e: i32) -> io::Error {
    io::Error::from_raw_os_error(e)
}

#[test]
fn errno_mapping_table() {
    let p = Path::new("/x/y");
    let both = [
        (libc::ENOENT, Code::NoSuchPath),
        (libc::EEXIST, Code::Exists),
        (libc::ENOTDIR, Code::NotDirectory),
        (libc::EISDIR, Code::IsDirectory),
        (libc::ENOTEMPTY, Code::NotEmpty),
        (libc::EXDEV, Code::CrossVolume),
        (libc::ENOTSUP, Code::Unsupported),
        (libc::EOPNOTSUPP, Code::Unsupported),
        (libc::ENAMETOOLONG, Code::InvalidInput),
        (libc::EINVAL, Code::InvalidInput),
        (libc::ENOSPC, Code::Unknown),
        (libc::EIO, Code::Unknown),
        (libc::EDEADLK, Code::Unknown),
        (libc::EMFILE, Code::Unknown),
        (libc::ELOOP, Code::Unknown),
    ];
    for (errno, code) in both {
        for side in [IoSide::Read, IoSide::Write] {
            let e = classify_io_error(&os(errno), side, p);
            assert_eq!(e.code, code, "errno {errno} {side:?}");
            assert_eq!(detail_i64(&e, "errno"), Some(errno as i64));
            assert_eq!(detail_str(&e, "path").as_deref(), Some("/x/y"));
        }
    }
    for errno in [libc::EACCES, libc::EPERM] {
        let e = classify_io_error(&os(errno), IoSide::Write, p);
        assert_eq!(e.code, Code::ReadOnly);
        assert_eq!(detail_str(&e, "reason").as_deref(), Some("no_permission"));
        assert_eq!(classify_io_error(&os(errno), IoSide::Read, p).code, Code::Unknown);
    }
    let e = classify_io_error(&os(libc::EROFS), IoSide::Write, p);
    assert_eq!(e.code, Code::ReadOnly);
    assert_eq!(detail_str(&e, "reason").as_deref(), Some("read_only_volume"));
    assert_eq!(classify_io_error(&os(libc::EROFS), IoSide::Read, p).code, Code::Unknown);
    let e = classify_io_error(&os(libc::EBUSY), IoSide::Write, p);
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(detail_str(&e, "reason").as_deref(), Some("busy"));

    for (errno, _) in both {
        assert_eq!(classify_io_error(&os(errno), IoSide::AfterWrite, p).code, Code::Unknown);
    }
    for errno in [libc::EACCES, libc::EPERM, libc::EROFS, libc::EBUSY] {
        let e = classify_io_error(&os(errno), IoSide::AfterWrite, p);
        assert_eq!(e.code, Code::Unknown);
        assert_eq!(detail_i64(&e, "errno"), Some(errno as i64));
    }
    let custom = io::Error::other("no errno");
    assert_eq!(classify_io_error(&custom, IoSide::Write, p).code, Code::Unknown);
}

fn check(err: io::Error, expect_read: Code, expect_write: Code, path: &Path) {
    assert!(err.raw_os_error().is_some(), "{err:?}");
    assert_eq!(classify_io_error(&err, IoSide::Read, path).code, expect_read, "{err:?} read");
    assert_eq!(classify_io_error(&err, IoSide::Write, path).code, expect_write, "{err:?} write");
    assert_eq!(classify_io_error(&err, IoSide::AfterWrite, path).code, Code::Unknown, "{err:?} after");
}

#[test]
fn classify_io_error_matches_real_syscalls() {
    let t = Tmp::new();
    let p = t.join("missing");
    check(fs::File::open(&p).unwrap_err(), Code::NoSuchPath, Code::NoSuchPath, &p);

    fs::write(t.join("f"), "x").unwrap();
    let e = fs::OpenOptions::new().write(true).create_new(true).open(t.join("f")).unwrap_err();
    check(e, Code::Exists, Code::Exists, &t.join("f"));
    check(fs::File::open(t.join("f/x")).unwrap_err(), Code::NotDirectory, Code::NotDirectory, &t.join("f/x"));

    fs::create_dir(t.join("d")).unwrap();
    check(fs::write(t.join("d"), "x").unwrap_err(), Code::IsDirectory, Code::IsDirectory, &t.join("d"));
    fs::write(t.join("d/c"), "x").unwrap();
    check(fs::remove_dir(t.join("d")).unwrap_err(), Code::NotEmpty, Code::NotEmpty, &t.join("d"));

    let a = Image::create(Fs::Apfs, 32);
    let b = Image::create(Fs::Apfs, 32);
    fs::write(a.path().join("f"), "x").unwrap();
    let e = fs::rename(a.path().join("f"), b.path().join("f")).unwrap_err();
    check(e, Code::CrossVolume, Code::CrossVolume, b.path());

    let hfs = Image::create(Fs::Hfs, 32);
    fs::write(hfs.path().join("f"), "x").unwrap();
    let src = std::ffi::CString::new(hfs.path().join("f").to_str().unwrap()).unwrap();
    let dst = std::ffi::CString::new(hfs.path().join("g").to_str().unwrap()).unwrap();
    let r = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    assert_eq!(r, -1);
    check(io::Error::last_os_error(), Code::Unsupported, Code::Unsupported, hfs.path());

    let long = t.join(&"a".repeat(256));
    check(fs::File::create(&long).unwrap_err(), Code::InvalidInput, Code::InvalidInput, &long);

    fs::write(t.join("locked"), "x").unwrap();
    set_mode(&t.join("locked"), 0o000);
    let e = fs::File::open(t.join("locked")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EACCES));
    check(e, Code::Unknown, Code::ReadOnly, &t.join("locked"));

    let mut ro = Image::create(Fs::Apfs, 32);
    ro.reattach_read_only();
    let e = fs::File::create(ro.path().join("new")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EROFS));
    let c = classify_io_error(&e, IoSide::Write, ro.path());
    assert_eq!(c.code, Code::ReadOnly);
    assert_eq!(detail_str(&c, "reason").as_deref(), Some("read_only_volume"));
    check(e, Code::Unknown, Code::ReadOnly, ro.path());

    fs::write(t.join("u"), "x").unwrap();
    chflags(&t.join("u"), "uchg");
    let e = fs::remove_file(t.join("u")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EPERM));
    let c = classify_io_error(&e, IoSide::Write, &t.join("u"));
    assert_eq!(detail_str(&c, "reason").as_deref(), Some("no_permission"));
    check(e, Code::Unknown, Code::ReadOnly, &t.join("u"));
    chflags(&t.join("u"), "nouchg");

    let full = Image::create(Fs::Apfs, 12);
    fill_until_enospc(full.path());
    let e = fs::write(full.path().join("more"), vec![0u8; 4 << 20]).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::ENOSPC));
    check(e, Code::Unknown, Code::Unknown, full.path());
}

#[test]
fn creating_a_non_utf8_name_fails_with_eilseq() {
    let t = Tmp::new();
    let mut b = t.path.as_os_str().as_encoded_bytes().to_vec();
    b.extend_from_slice(b"/bad\xffname\0");
    let fd = unsafe { libc::open(b.as_ptr() as *const _, libc::O_CREAT | libc::O_WRONLY, 0o644) };
    assert_eq!(fd, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EILSEQ));
}

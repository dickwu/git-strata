//! `usage_of` on APFS (the test TMPDIR must be on APFS, like every test of
//! this crate): clones share extents, private size counts them nowhere,
//! allocated size once per file, hard links once.

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;

use common::*;
use strata_apfs::*;

const MB: u64 = 1 << 20;

fn write_random(p: &std::path::Path, n: usize) {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_ne_bytes());
    }
    v.truncate(n);
    let mut f = fs::File::create(p).unwrap();
    f.write_all(&v).unwrap();
    f.sync_all().unwrap();
}

#[test]
fn a_single_file_is_logical_allocated_and_private() {
    let t = Tmp::new();
    let f = t.path.join("one.bin");
    write_random(&f, 4 * MB as usize);
    let u = usage_of(&f).unwrap();
    assert_eq!(u.files, 1);
    assert_eq!(u.logical, 4 * MB);
    assert!(u.allocated >= 4 * MB, "{u:?}");
    assert!(u.private_exact, "APFS reports PRIVATESIZE: {u:?}");
    assert_eq!(u.private, u.allocated, "an unshared file is all private: {u:?}");
    assert_eq!(u.shared(), 0);
}

#[test]
fn a_clone_shares_every_extent_until_written() {
    let t = Tmp::new();
    let src = t.path.join("src");
    fs::create_dir(&src).unwrap();
    write_random(&src.join("big.bin"), 8 * MB as usize);
    fs::write(src.join("small.txt"), b"hello").unwrap();
    let before = usage_of(&src).unwrap();
    assert_eq!(before.files, 2);
    assert_eq!(before.private, before.allocated, "{before:?}");

    let dst = t.path.join("dst");
    clone_tree(&src, &dst).unwrap();
    let both = usage_of(&t.path).unwrap();
    assert_eq!(both.files, 4);
    assert_eq!(both.logical, 2 * before.logical);
    // naive (du-style) counts the shared extents once per file
    assert_eq!(both.allocated, 2 * before.allocated, "{both:?}");
    // private counts them nowhere: neither copy owns them
    let big_alloc = usage_of(&src.join("big.bin")).unwrap().allocated;
    assert!(both.private <= 2 * (before.allocated - big_alloc) + 2 * 4096, "{both:?}");
    let clone = usage_of(&dst).unwrap();
    assert_eq!(clone.private, usage_of(&dst.join("small.txt")).unwrap().private, "the big clone is fully shared: {clone:?}");
    assert!(clone.shared() >= 8 * MB, "{clone:?}");

    // write 1 MiB into the clone: those blocks become private to it
    let f = fs::OpenOptions::new().write(true).open(dst.join("big.bin")).unwrap();
    f.write_all_at(&vec![7u8; MB as usize], 0).unwrap();
    f.sync_all().unwrap();
    drop(f);
    let written = usage_of(&dst.join("big.bin")).unwrap();
    assert!(written.private >= MB && written.private < 8 * MB, "{written:?}");
    assert_eq!(written.logical, 8 * MB);
}

#[test]
fn hard_links_count_once_and_symlinks_are_not_followed() {
    let t = Tmp::new();
    let d = t.path.join("d");
    fs::create_dir_all(d.join("sub")).unwrap();
    write_random(&d.join("a.bin"), MB as usize);
    fs::hard_link(d.join("a.bin"), d.join("sub/b.bin")).unwrap();
    let outside = t.path.join("outside");
    fs::create_dir(&outside).unwrap();
    write_random(&outside.join("x.bin"), 2 * MB as usize);
    std::os::unix::fs::symlink(&outside, d.join("link")).unwrap();
    let u = usage_of(&d).unwrap();
    assert_eq!(u.files, 1, "{u:?}");
    assert_eq!(u.other, 1);
    assert_eq!(u.dirs, 2);
    assert!(u.logical < 2 * MB, "the symlink target is not counted: {u:?}");
    // the top-level symlink itself is not followed either
    let l = usage_of(&d.join("link")).unwrap();
    assert_eq!((l.files, l.other), (0, 1));
}

#[test]
fn stop_cancels_and_a_missing_path_is_no_such_path() {
    let t = Tmp::new();
    fs::create_dir_all(t.path.join("a/b")).unwrap();
    let e = usage_with(&t.path, &|| true).unwrap_err();
    assert_eq!(e.code, strata_types::Code::Busy);
    let e = usage_of(&t.path.join("nope")).unwrap_err();
    assert_eq!(e.code, strata_types::Code::NoSuchPath);
}

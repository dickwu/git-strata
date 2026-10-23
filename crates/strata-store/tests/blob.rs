//! store.md §7, Task 2: content-addressed blobs.

mod support;

use std::cell::Cell;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::macos::fs::MetadataExt as MacMeta;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use strata_apfs::seams::set_before_open;
use strata_store::Seam;
use strata_types::{BlobId, Code, RepoPath};
use support::*;

const EMPTY: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
const ABC: &str = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";
/// Computed with an independent pure-Python BLAKE3 reference implementation
/// that reproduces EMPTY and ABC (no b3sum on this machine).
const MILLION_A: &str = "616f575a1b58d4c9797d4217b9730ae5e6eb319d76edef6549b46f4efe31ff8b";
const UF_IMMUTABLE: u32 = 0x2;

fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn bid(s: &str) -> BlobId {
    BlobId::try_from(s.to_string()).unwrap()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            out.extend(files_under(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out.sort();
    out
}

fn blob_files(env: &Env) -> Vec<PathBuf> {
    files_under(&env.store_dir().join("blobs"))
}

fn tmp_files(env: &Env) -> Vec<PathBuf> {
    files_under(&env.store_dir().join("tmp"))
}

fn blob_file(env: &Env, id: &BlobId) -> PathBuf {
    env.store_dir().join("blobs").join(&id.as_str()[..2]).join(id.as_str())
}

fn write(p: &Path, b: &[u8]) {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(p, b).unwrap();
}

fn work(env: &Env) -> PathBuf {
    let w = env.dir.path().join("work");
    fs::create_dir_all(&w).unwrap();
    w.canonicalize().unwrap()
}

fn hash(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}

fn chflags(p: &Path, flags: u32) {
    let c = CString::new(p.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::lchflags(c.as_ptr(), flags) }, 0, "chflags {}", p.display());
}

fn flags(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().st_flags()
}

fn mode(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().mode() & 0o7777
}

fn set_xattr(p: &Path, name: &str, v: &[u8]) {
    let c = CString::new(p.as_os_str().as_bytes()).unwrap();
    let n = CString::new(name).unwrap();
    let r = unsafe { libc::setxattr(c.as_ptr(), n.as_ptr(), v.as_ptr().cast(), v.len(), 0, 0) };
    assert_eq!(r, 0, "setxattr {name}");
}

fn xattrs(p: &Path) -> Vec<String> {
    let c = CString::new(p.as_os_str().as_bytes()).unwrap();
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::listxattr(c.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), libc::XATTR_NOFOLLOW) };
    assert!(n >= 0);
    buf[..n as usize]
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

fn mkfifo(p: &Path) {
    let c = CString::new(p.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
}

fn set_mtime(p: &Path, t: SystemTime) {
    let f = OpenOptions::new().read(true).open(p).unwrap();
    f.set_modified(t).unwrap();
}

fn y2020() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800)
}

fn near_now(t: SystemTime) -> bool {
    let now = SystemTime::now();
    let d = now.duration_since(t).unwrap_or_else(|e| e.duration());
    d < Duration::from_secs(60)
}

fn flip_bit(p: &Path, off: u64) {
    fs::set_permissions(p, fs::Permissions::from_mode(0o644)).unwrap();
    let f = OpenOptions::new().read(true).write(true).open(p).unwrap();
    let mut b = [0u8];
    f.read_exact_at(&mut b, off).unwrap();
    b[0] ^= 1;
    f.write_all_at(&b, off).unwrap();
    drop(f);
    fs::set_permissions(p, fs::Permissions::from_mode(0o444)).unwrap();
}

fn raw_blob_row(env: &Env, id: &str, size: i64) {
    let (c, _) = raw(&env.db(), false);
    c.execute("INSERT INTO blobs (id, size, created_ns) VALUES (?1, ?2, 0)", rusqlite::params![id, size]).unwrap();
}

fn blob_rows(env: &Env) -> i64 {
    bare(&env.db()).query_row("SELECT count(*) FROM blobs", [], |r| r.get(0)).unwrap()
}

/// Run `f` on the n-th (1-based) `before_open` of `name` on this thread.
fn on_open(name: &'static str, nth: usize, f: impl Fn() + 'static) {
    let seen = Rc::new(Cell::new(0usize));
    set_before_open(Some(Box::new(move |_step, n| {
        if n.as_bytes() == name.as_bytes() {
            seen.set(seen.get() + 1);
            if seen.get() == nth {
                f();
            }
        }
    })));
}

fn clear_hook() {
    set_before_open(None);
}

// ------------------------------------------------------------------ tests

#[test]
fn blob_put_is_idempotent() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    write(&w.join("one"), b"same content");
    write(&w.join("two"), b"same content");
    let a = s.put_blob(&w.join("one")).unwrap();
    let b = s.put_blob(&w.join("one")).unwrap();
    let c = s.put_blob(&w.join("two")).unwrap();
    let d = s.put_bytes(b"same content").unwrap();
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a, d);
    assert_eq!(blob_files(&env), vec![blob_file(&env, &a)]);
    // the stored copy is never re-read: 000 permissions do not matter
    fs::set_permissions(blob_file(&env, &a), fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(s.put_blob(&w.join("two")).unwrap(), a);
    assert_eq!(s.put_bytes(b"same content").unwrap(), a);
    assert_eq!(blob_files(&env).len(), 1);
    assert!(tmp_files(&env).is_empty());
}

#[test]
fn blob_id_ignores_delivery_shape() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 + i / 13) as u8).collect();
    write(&w.join("whole"), &data);
    // chunks written out of order
    {
        let f = File::create(w.join("chunks")).unwrap();
        let chunk = 4096;
        let mut offs: Vec<usize> = (0..data.len()).step_by(chunk).collect();
        offs.reverse();
        offs.swap(1, 5);
        for o in offs {
            let end = (o + chunk).min(data.len());
            f.write_all_at(&data[o..end], o as u64).unwrap();
        }
    }
    // through a shared mapping
    {
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(w.join("mapped")).unwrap();
        f.set_len(data.len() as u64).unwrap();
        let mut m = unsafe { memmap2::MmapMut::map_mut(&f).unwrap() };
        m.copy_from_slice(&data);
        m.flush().unwrap();
    }
    let ids = [
        s.put_blob(&w.join("whole")).unwrap(),
        s.put_blob(&w.join("chunks")).unwrap(),
        s.put_blob(&w.join("mapped")).unwrap(),
        s.put_bytes(&data).unwrap(),
    ];
    for id in &ids {
        assert_eq!(id.as_str(), hash(&data));
    }
    assert_eq!(blob_files(&env).len(), 1);
}

#[test]
fn blob_known_answer_vectors() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let million = vec![b'a'; 1_000_000];
    for (name, bytes, want) in [("empty", &b""[..], EMPTY), ("abc", &b"abc"[..], ABC), ("million", &million[..], MILLION_A)] {
        write(&w.join(name), bytes);
        assert_eq!(s.put_blob(&w.join(name)).unwrap().as_str(), want, "{name} via put_blob");
        assert_eq!(s.put_bytes(bytes).unwrap().as_str(), want, "{name} via put_bytes");
    }
}

#[test]
fn empty_file_is_a_valid_blob() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    write(&w.join("e"), b"");
    let id = s.put_blob(&w.join("e")).unwrap();
    assert_eq!(id.as_str(), EMPTY);
    s.get_blob(&id, &w.join("out")).unwrap();
    assert_eq!(fs::metadata(w.join("out")).unwrap().len(), 0);
    assert_eq!(s.read_blob(&id, 0).unwrap(), Vec::<u8>::new());
    s.verify_blob(&id).unwrap();
}

#[test]
fn stored_blob_is_never_rewritten() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let a = s.put_bytes(b"first").unwrap();
    let p = blob_file(&env, &a);
    let ino = fs::metadata(&p).unwrap().ino();
    for i in 0..20 {
        s.put_bytes(format!("other {i}").as_bytes()).unwrap();
    }
    write(&w.join("f"), b"another");
    s.put_blob(&w.join("f")).unwrap();
    assert_eq!(fs::metadata(&p).unwrap().ino(), ino);
    assert_eq!(fs::read(&p).unwrap(), b"first");
    // same content again only refreshes atime and mtime
    set_mtime(&p, y2020());
    let ctime_before = fs::metadata(&p).unwrap().ctime();
    assert_eq!(s.put_bytes(b"first").unwrap(), a);
    let md = fs::metadata(&p).unwrap();
    assert_eq!(md.ino(), ino);
    assert!(near_now(md.modified().unwrap()));
    assert!(near_now(md.accessed().unwrap()));
    assert!(md.ctime() >= ctime_before);
    assert_eq!(fs::read(&p).unwrap(), b"first");
    assert_eq!(mode(&p), 0o444);
}

#[test]
fn blob_identity_changes_with_any_byte() {
    let env = env();
    let s = env.open();
    let base: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let orig = s.put_bytes(&base).unwrap();
    let mut ids = vec![orig.clone()];
    for at in [0, base.len() / 2, base.len() - 1] {
        let mut b = base.clone();
        b[at] ^= 0x80;
        ids.push(s.put_bytes(&b).unwrap());
    }
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 4, "{ids:?}");
}

#[test]
fn blob_roundtrip_is_byte_exact() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let mib: Vec<u8> = (0..1u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    for (name, bytes) in [("zero", &b""[..]), ("one", &b"x"[..]), ("mib", &mib[..])] {
        write(&w.join(name), bytes);
        let id = s.put_blob(&w.join(name)).unwrap();
        let out = w.join(format!("{name}.out"));
        s.get_blob(&id, &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), bytes, "{name}");
    }
    assert_eq!(blob_rows(&env), 0, "no checkpoint registered any blob");
    let id = s.put_bytes(b"0123456789").unwrap();
    assert_eq!(s.read_blob(&id, 10).unwrap(), b"0123456789");
    let e = s.read_blob(&id, 9).err().unwrap();
    assert_code(&e, Code::TooLarge);
    assert_eq!(detail(&e, "limit").and_then(|v| v.as_str()), Some("read_max"));
}

fn same_file_bytes(a: &Path, b: &Path) {
    let fa = File::open(a).unwrap();
    let fb = File::open(b).unwrap();
    let len = fa.metadata().unwrap().len();
    assert_eq!(len, fb.metadata().unwrap().len());
    let mut ba = vec![0u8; 8 << 20];
    let mut bb = vec![0u8; 8 << 20];
    let mut off = 0;
    while off < len {
        let n = ((len - off) as usize).min(ba.len());
        fa.read_exact_at(&mut ba[..n], off).unwrap();
        fb.read_exact_at(&mut bb[..n], off).unwrap();
        assert!(ba[..n] == bb[..n], "differ near offset {off}");
        off += n as u64;
    }
}

#[test]
fn large_and_sparse_blobs_roundtrip() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    // 500 000 000 dense bytes
    let dense = w.join("dense");
    {
        let mut f = File::create(&dense).unwrap();
        let block: Vec<u8> = (0..(1u32 << 20)).map(|i| (i ^ (i >> 9)) as u8).collect();
        let mut left = 500_000_000usize;
        let mut k = 0u8;
        while left > 0 {
            let n = left.min(block.len());
            let mut b = block[..n].to_vec();
            b[0] = k;
            f.write_all(&b).unwrap();
            left -= n;
            k = k.wrapping_add(1);
        }
    }
    let id = s.put_blob(&dense).unwrap();
    let out = w.join("dense.out");
    s.get_blob(&id, &out).unwrap();
    same_file_bytes(&dense, &out);
    s.verify_blob(&id).unwrap();
    // sparse: data islands in a 1 GiB file
    let sparse = w.join("sparse");
    {
        let f = File::create(&sparse).unwrap();
        f.set_len(1 << 30).unwrap();
        for off in [0u64, 300 << 20, (1 << 30) - 5] {
            f.write_all_at(b"island", off.min((1 << 30) - 6)).unwrap();
        }
    }
    let id = s.put_blob(&sparse).unwrap();
    let out = w.join("sparse.out");
    s.get_blob(&id, &out).unwrap();
    same_file_bytes(&sparse, &out);
    let f = File::open(&out).unwrap();
    let mut b = [1u8; 6];
    f.read_exact_at(&mut b, 100 << 20).unwrap();
    assert_eq!(b, [0u8; 6], "holes read as zeros");
}

#[test]
fn same_bytes_new_path_or_mode_reuse_blob() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    write(&w.join("a/x"), b"payload");
    let id = s.put_blob(&w.join("a/x")).unwrap();
    // new path (rename), new inode (copy), new mode
    fs::rename(w.join("a/x"), w.join("moved")).unwrap();
    assert_eq!(s.put_blob(&w.join("moved")).unwrap(), id);
    fs::copy(w.join("moved"), w.join("copy")).unwrap();
    assert_ne!(fs::metadata(w.join("copy")).unwrap().ino(), fs::metadata(w.join("moved")).unwrap().ino());
    assert_eq!(s.put_blob(&w.join("copy")).unwrap(), id);
    fs::set_permissions(w.join("copy"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(s.put_blob(&w.join("copy")).unwrap(), id);
    assert_eq!(blob_files(&env).len(), 1);
    write(&w.join("diff"), b"payloaD");
    assert_ne!(s.put_blob(&w.join("diff")).unwrap(), id);
    assert_eq!(blob_files(&env).len(), 2);
}

#[test]
fn blob_files_are_content_only() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let src = w.join("src");
    write(&src, b"content only");
    set_xattr(&src, "com.apple.quarantine", b"0081;00000000;Test;");
    set_xattr(&src, "user.strata.test", b"v");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    chflags(&src, UF_IMMUTABLE);
    let id = s.put_blob(&src).unwrap();
    chflags(&src, 0);
    let p = blob_file(&env, &id);
    let left: Vec<String> = xattrs(&p).into_iter().filter(|n| n != "com.apple.provenance").collect();
    assert!(left.is_empty(), "blob kept {left:?}");
    assert_eq!(flags(&p), 0);
    assert_eq!(mode(&p), 0o444);
    let out = w.join("out");
    s.get_blob(&id, &out).unwrap();
    assert_eq!(mode(&out), 0o644);
    assert_eq!(flags(&out), 0);
    assert!(near_now(fs::metadata(&out).unwrap().modified().unwrap()));
    let left: Vec<String> = xattrs(&out).into_iter().filter(|n| n != "com.apple.provenance").collect();
    assert!(left.is_empty(), "get copied {left:?}");
    assert_eq!(fs::read(&out).unwrap(), b"content only");
}

#[test]
fn symlink_blob_stores_target_text() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    write(&w.join("a/b"), b"the link's target, not to be stored");
    fs::create_dir(w.join("d")).unwrap();
    symlink("../a/b", w.join("d/link")).unwrap();
    let id = s.put_blob(&w.join("d/link")).unwrap();
    assert_eq!(id.as_str(), hash(b"../a/b"));
    let p = blob_file(&env, &id);
    assert!(fs::symlink_metadata(&p).unwrap().file_type().is_file());
    assert_eq!(fs::read(&p).unwrap(), b"../a/b");
    assert_eq!(s.read_blob(&id, 100).unwrap(), b"../a/b");
    assert_eq!(s.put_blob_rooted(&w, &rp("d/link")).unwrap(), id);
    assert_eq!(blob_files(&env).len(), 1);
    assert!(tmp_files(&env).is_empty());
}

#[test]
fn put_blob_refuses_directory_and_special_files() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    fs::create_dir(w.join("dir")).unwrap();
    mkfifo(&w.join("fifo"));
    for (name, code) in [("dir", Code::IsDirectory), ("fifo", Code::NotFile), ("missing", Code::NoSuchPath)] {
        let e = s.put_blob(&w.join(name)).err().unwrap();
        assert_code(&e, code);
        let e = s.put_blob_rooted(&w, &rp(name)).err().unwrap();
        assert_code(&e, code);
        assert!(blob_files(&env).is_empty(), "{name}");
        assert!(tmp_files(&env).is_empty(), "{name}");
    }
}

#[test]
fn get_blob_refuses_existing_dst() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let id = s.put_bytes(b"new").unwrap();
    write(&w.join("dst"), b"old");
    let ino = fs::metadata(w.join("dst")).unwrap().ino();
    let e = s.get_blob(&id, &w.join("dst")).err().unwrap();
    assert_code(&e, Code::Exists);
    assert_eq!(fs::read(w.join("dst")).unwrap(), b"old");
    assert_eq!(fs::metadata(w.join("dst")).unwrap().ino(), ino);
}

#[test]
fn get_blob_missing_is_corrupt_and_leaves_no_dst() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let id = s.put_bytes(b"gone").unwrap();
    fs::remove_file(blob_file(&env, &id)).unwrap();
    let e = s.get_blob(&id, &w.join("dst")).err().unwrap();
    assert_reason(&e, Code::Corrupt, "blob_missing");
    assert!(!w.join("dst").exists());
    assert_reason(&s.read_blob(&id, 100).err().unwrap(), Code::Corrupt, "blob_missing");
    assert_reason(&s.verify_blob(&id).err().unwrap(), Code::Corrupt, "blob_missing");
}

#[test]
fn missing_or_mismatched_blob_is_corrupt_never_new() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    // file only, no row: get and read work
    let ok_id = s.put_bytes(b"file only").unwrap();
    s.get_blob(&ok_id, &w.join("ok")).unwrap();
    assert_eq!(s.read_blob(&ok_id, 100).unwrap(), b"file only");
    // not there
    let missing = bid(&hash(b"never stored"));
    for e in [
        s.get_blob(&missing, &w.join("m")).err().unwrap(),
        s.read_blob(&missing, 100).err().unwrap(),
        s.verify_blob(&missing).err().unwrap(),
    ] {
        assert_reason(&e, Code::Corrupt, "blob_missing");
    }
    // not a regular file
    let dir_id = s.put_bytes(b"becomes a dir").unwrap();
    let p = blob_file(&env, &dir_id);
    fs::remove_file(&p).unwrap();
    fs::create_dir(&p).unwrap();
    for e in [
        s.get_blob(&dir_id, &w.join("d")).err().unwrap(),
        s.read_blob(&dir_id, 100).err().unwrap(),
        s.verify_blob(&dir_id).err().unwrap(),
    ] {
        assert_reason(&e, Code::Corrupt, "blob_type");
    }
    assert!(!w.join("d").exists());
    // length differs from the row
    let len_id = s.put_bytes(b"twelve bytes").unwrap();
    raw_blob_row(&env, len_id.as_str(), 11);
    for e in [
        s.get_blob(&len_id, &w.join("l")).err().unwrap(),
        s.read_blob(&len_id, 100).err().unwrap(),
        s.verify_blob(&len_id).err().unwrap(),
    ] {
        assert_reason(&e, Code::Corrupt, "blob_size");
    }
    assert!(!w.join("l").exists());
    // content differs from the id
    let bad = s.put_bytes(b"flip me").unwrap();
    flip_bit(&blob_file(&env, &bad), 0);
    assert_reason(&s.verify_blob(&bad).err().unwrap(), Code::Corrupt, "blob_mismatch");
}

#[test]
fn get_blob_is_trusted_verify_detects_flipped_bit() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let id = s.put_bytes(b"trusted read").unwrap();
    flip_bit(&blob_file(&env, &id), 3);
    s.get_blob(&id, &w.join("out")).unwrap();
    assert_ne!(fs::read(w.join("out")).unwrap(), b"trusted read");
    let e = s.verify_blob(&id).err().unwrap();
    assert_reason(&e, Code::Corrupt, "blob_mismatch");
    assert_eq!(detail(&e, "expected").and_then(|v| v.as_str()), Some(id.as_str()));
    let actual = detail(&e, "actual").and_then(|v| v.as_str()).unwrap();
    assert_eq!(actual, hash(&fs::read(blob_file(&env, &id)).unwrap()));
}

#[test]
fn put_reuse_does_not_reread_existing_copy() {
    let env = env();
    let s = env.open();
    let id = s.put_bytes(b"stored once").unwrap();
    flip_bit(&blob_file(&env, &id), 0);
    assert_eq!(s.put_bytes(b"stored once").unwrap(), id);
    assert_reason(&s.verify_blob(&id).err().unwrap(), Code::Corrupt, "blob_mismatch");
}

#[test]
fn put_reuse_with_length_mismatch_is_corrupt() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let id = s.put_bytes(b"full length").unwrap();
    let p = blob_file(&env, &id);
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    OpenOptions::new().write(true).open(&p).unwrap().set_len(4).unwrap();
    write(&w.join("src"), b"full length");
    for e in [s.put_blob(&w.join("src")).err().unwrap(), s.put_bytes(b"full length").err().unwrap()] {
        assert_reason(&e, Code::Corrupt, "blob_size");
        let kept = PathBuf::from(detail(&e, "path").and_then(|v| v.as_str()).unwrap());
        assert!(kept.starts_with(env.store_dir().join("tmp")), "{}", kept.display());
        assert_eq!(fs::read(&kept).unwrap(), b"full length");
    }
    assert_eq!(fs::read(&p).unwrap(), b"full");
}

#[test]
fn put_restores_missing_blob_file() {
    let env = env();
    let s = env.open();
    let id = s.put_bytes(b"restore me").unwrap();
    raw_blob_row(&env, id.as_str(), 10);
    fs::remove_file(blob_file(&env, &id)).unwrap();
    assert_reason(&s.verify_blob(&id).err().unwrap(), Code::Corrupt, "blob_missing");
    assert_eq!(s.put_bytes(b"restore me").unwrap(), id);
    assert_eq!(fs::read(blob_file(&env, &id)).unwrap(), b"restore me");
    s.verify_blob(&id).unwrap();
}

#[test]
fn stale_temp_files_do_not_block_put() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let tmp = env.store_dir().join("tmp");
    for i in 0..50 {
        write(&tmp.join(format!("put-{i:032x}")), format!("stale {i}").as_bytes());
    }
    write(&tmp.join("unrelated.txt"), b"not ours");
    write(&tmp.join("init-0123.db"), b"old init");
    let before = snapshot(&tmp);
    write(&w.join("src"), b"fresh");
    s.arm(Seam::TempNameTaken);
    let id = s.put_blob(&w.join("src")).unwrap();
    assert_eq!(fs::read(blob_file(&env, &id)).unwrap(), b"fresh");
    let after = snapshot(&tmp);
    for (k, v) in &before {
        assert_eq!(after.get(k), Some(v), "{} changed", k.display());
    }
    // the only new name is the one the seam occupied, untouched
    let new: Vec<_> = after.iter().filter(|(k, _)| !before.contains_key(*k)).collect();
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(new[0].1, &Snap::File(b"taken".to_vec()));
    // put_bytes too
    s.arm(Seam::TempNameTaken);
    let id2 = s.put_bytes(b"fresh bytes").unwrap();
    assert_eq!(fs::read(blob_file(&env, &id2)).unwrap(), b"fresh bytes");
}

#[test]
fn concurrent_puts_of_same_content_store_one_blob() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let data: Vec<u8> = (0..2_000_000u32).map(|i| (i >> 3) as u8).collect();
    for i in 0..8 {
        write(&w.join(format!("f{i}")), &data);
    }
    let ids: Vec<BlobId> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..8).map(|i| {
            let s = &s;
            let p = w.join(format!("f{i}"));
            sc.spawn(move || s.put_blob(&p).unwrap())
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(ids.iter().all(|i| *i == ids[0]));
    assert_eq!(blob_files(&env), vec![blob_file(&env, &ids[0])]);
    assert_eq!(s.read_blob(&ids[0], u64::MAX).unwrap(), data);
    assert!(tmp_files(&env).is_empty());
}

#[test]
fn put_blob_orders_blob_before_later_writes() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let c0 = s.sync_counts();
    s.put_bytes(b"one").unwrap();
    let c1 = s.sync_counts();
    assert_eq!((c1.blob_barriers - c0.blob_barriers, c1.dir_barriers - c0.dir_barriers), (1, 1));
    write(&w.join("two"), b"two");
    s.put_blob(&w.join("two")).unwrap();
    let c2 = s.sync_counts();
    assert_eq!((c2.blob_barriers - c1.blob_barriers, c2.dir_barriers - c1.dir_barriers), (1, 1));
    // duplicates: no barrier at all
    s.put_bytes(b"one").unwrap();
    s.put_blob(&w.join("two")).unwrap();
    let c3 = s.sync_counts();
    assert_eq!((c3.blob_barriers, c3.dir_barriers), (c2.blob_barriers, c2.dir_barriers));
}

#[test]
fn put_rename_failure_is_not_written() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    write(&w.join("src"), b"rename fails once");
    s.arm(Seam::BlobRenameFails { errno: libc::EIO });
    let e = s.put_blob(&w.join("src")).err().unwrap();
    assert_code(&e, Code::Unknown);
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("not_written"));
    assert!(blob_files(&env).is_empty());
    let tmp = tmp_files(&env);
    assert_eq!(tmp.len(), 1);
    assert_eq!(fs::read(&tmp[0]).unwrap(), b"rename fails once");
    let id = s.put_blob(&w.join("src")).unwrap();
    assert_eq!(fs::read(blob_file(&env, &id)).unwrap(), b"rename fails once");
}

#[test]
fn put_blob_rooted_never_follows_a_swapped_parent() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let x = env.dir.path().join("outside");
    write(&w.join("a/b/f"), b"inside");
    write(&x.join("f"), b"outside");
    let id = s.put_blob_rooted(&w, &rp("a/b/f")).unwrap();
    assert_eq!(id.as_str(), hash(b"inside"));
    let blobs0 = blob_files(&env);
    let swap = {
        let w = w.clone();
        let x = x.clone();
        move || {
            fs::rename(w.join("a/b"), w.join("a/b.real")).unwrap();
            symlink(&x, w.join("a/b")).unwrap();
        }
    };
    let unswap = |w: &Path| {
        fs::remove_file(w.join("a/b")).unwrap();
        fs::rename(w.join("a/b.real"), w.join("a/b")).unwrap();
    };
    // swapped before the call
    swap();
    let e = s.put_blob_rooted(&w, &rp("a/b/f")).err().unwrap();
    assert_reason(&e, Code::NotDirectory, "symlink");
    assert_eq!(blob_files(&env), blobs0);
    assert!(tmp_files(&env).is_empty());
    // control: the by-path put follows the link and stores X's content
    let followed = s.put_blob(&w.join("a/b/f")).unwrap();
    assert_eq!(followed.as_str(), hash(b"outside"));
    fs::remove_file(blob_file(&env, &followed)).unwrap();
    unswap(&w);
    // swapped after attrs_of, before clone_from_rooted opens `b`
    on_open("b", 2, swap.clone());
    let e = s.put_blob_rooted(&w, &rp("a/b/f")).err().unwrap();
    clear_hook();
    assert_reason(&e, Code::NotDirectory, "symlink");
    assert_eq!(blob_files(&env), blobs0);
    assert!(tmp_files(&env).is_empty());
    assert_eq!(fs::read(x.join("f")).unwrap(), b"outside");
    unswap(&w);
    // swapped before clone_from_rooted opens `a` (its first level)
    let swap_a = {
        let w = w.clone();
        let x = x.clone();
        move || {
            fs::rename(w.join("a"), w.join("a.real")).unwrap();
            fs::create_dir_all(x.join("b")).unwrap();
            fs::write(x.join("b/f"), b"outside").unwrap();
            symlink(&x, w.join("a")).unwrap();
        }
    };
    on_open("a", 2, swap_a);
    let e = s.put_blob_rooted(&w, &rp("a/b/f")).err().unwrap();
    clear_hook();
    assert_reason(&e, Code::NotDirectory, "symlink");
    assert_eq!(blob_files(&env), blobs0);
    assert!(tmp_files(&env).is_empty());
}

#[test]
fn put_blob_rooted_refuses_a_swapped_kind_before_cloning() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    write(&w.join("f"), b"a file");
    // → directory with a file in it
    {
        let w2 = w.clone();
        on_open("f", 2, move || {
            fs::remove_file(w2.join("f")).unwrap();
            fs::create_dir(w2.join("f")).unwrap();
            fs::write(w2.join("f/inner"), b"inner").unwrap();
        });
        let e = s.put_blob_rooted(&w, &rp("f")).err().unwrap();
        clear_hook();
        assert_code(&e, Code::IsDirectory);
        assert!(tmp_files(&env).is_empty());
        assert!(blob_files(&env).is_empty());
        assert_eq!(fs::read(w.join("f/inner")).unwrap(), b"inner");
        fs::remove_dir_all(w.join("f")).unwrap();
    }
    // → FIFO without a writer: refused, never hangs
    {
        write(&w.join("f"), b"a file");
        let w2 = w.clone();
        on_open("f", 2, move || {
            fs::remove_file(w2.join("f")).unwrap();
            mkfifo(&w2.join("f"));
        });
        let e = s.put_blob_rooted(&w, &rp("f")).err().unwrap();
        clear_hook();
        assert_code(&e, Code::NotFile);
        assert!(tmp_files(&env).is_empty());
        fs::remove_file(w.join("f")).unwrap();
    }
    // → symlink
    {
        write(&w.join("f"), b"a file");
        write(&w.join("target"), b"target");
        let w2 = w.clone();
        on_open("f", 2, move || {
            fs::remove_file(w2.join("f")).unwrap();
            symlink("target", w2.join("f")).unwrap();
        });
        let e = s.put_blob_rooted(&w, &rp("f")).err().unwrap();
        clear_hook();
        assert_reason(&e, Code::NotFile, "symlink");
        assert!(tmp_files(&env).is_empty());
        assert!(blob_files(&env).is_empty());
    }
}

#[test]
fn get_blob_rooted_roundtrip_matches_get_blob() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    fs::create_dir(w.join("a")).unwrap();
    let id = s.put_bytes(b"both ways").unwrap();
    assert_eq!(blob_rows(&env), 0);
    let check = |p: &Path| {
        assert_eq!(fs::read(p).unwrap(), b"both ways");
        assert_eq!(mode(p), 0o644);
        assert_eq!(flags(p), 0);
        assert!(near_now(fs::metadata(p).unwrap().modified().unwrap()));
    };
    s.get_blob(&id, &w.join("p")).unwrap();
    s.get_blob_rooted(&id, &w, &rp("a/f")).unwrap();
    check(&w.join("p"));
    check(&w.join("a/f"));
    // the blob file carries uchg: flags are cleared before the mode is set
    let bf = blob_file(&env, &id);
    chflags(&bf, UF_IMMUTABLE);
    let r1 = s.get_blob(&id, &w.join("p2"));
    let r2 = s.get_blob_rooted(&id, &w, &rp("a/f2"));
    chflags(&bf, 0);
    r1.unwrap();
    r2.unwrap();
    check(&w.join("p2"));
    check(&w.join("a/f2"));
    // dst exists (also a dangling symlink): Exists, untouched
    let e = s.get_blob_rooted(&id, &w, &rp("a/f")).err().unwrap();
    assert_code(&e, Code::Exists);
    symlink(env.dir.path().join("nowhere"), w.join("a/dangling")).unwrap();
    let e = s.get_blob_rooted(&id, &w, &rp("a/dangling")).err().unwrap();
    assert_code(&e, Code::Exists);
    assert!(!env.dir.path().join("nowhere").exists());
    // missing parent
    let e = s.get_blob_rooted(&id, &w, &rp("nope/f")).err().unwrap();
    assert_code(&e, Code::NoSuchPath);
    // missing blob file
    fs::remove_file(&bf).unwrap();
    let e = s.get_blob_rooted(&id, &w, &rp("a/f3")).err().unwrap();
    assert_reason(&e, Code::Corrupt, "blob_missing");
    assert!(!w.join("a/f3").exists());
}

#[test]
fn get_blob_rooted_never_writes_through_a_swapped_parent() {
    let env = env();
    let s = env.open();
    let w = work(&env);
    let x = env.dir.path().join("outside");
    write(&x.join("keep"), b"keep");
    fs::set_permissions(x.join("keep"), fs::Permissions::from_mode(0o600)).unwrap();
    set_mtime(&x.join("keep"), y2020());
    fs::create_dir_all(w.join("a/b")).unwrap();
    let id = s.put_bytes(b"payload").unwrap();
    let x_listing = || listing(&x);
    let x0 = x_listing();
    let swap = {
        let w = w.clone();
        let x = x.clone();
        move || {
            fs::rename(w.join("a/b"), w.join("a/b.real")).unwrap();
            symlink(&x, w.join("a/b")).unwrap();
        }
    };
    let unswap = |w: &Path| {
        fs::remove_file(w.join("a/b")).unwrap();
        fs::rename(w.join("a/b.real"), w.join("a/b")).unwrap();
    };
    // swapped before the call
    swap();
    let e = s.get_blob_rooted(&id, &w, &rp("a/b/f")).err().unwrap();
    assert_reason(&e, Code::NotDirectory, "symlink");
    assert_eq!(x_listing(), x0);
    // control: by-path get follows the link and lands in X
    s.get_blob(&id, &w.join("a/b/f")).unwrap();
    assert!(x.join("f").exists());
    fs::remove_file(x.join("f")).unwrap();
    unswap(&w);
    // swapped before clone_file_rooted opens `b`
    on_open("b", 1, swap.clone());
    let e = s.get_blob_rooted(&id, &w, &rp("a/b/f")).err().unwrap();
    clear_hook();
    assert_reason(&e, Code::NotDirectory, "symlink");
    assert_eq!(x_listing(), x0);
    unswap(&w);
    // swapped after the clone, before open_rooted_file opens `b`
    on_open("b", 2, swap.clone());
    let e = s.get_blob_rooted(&id, &w, &rp("a/b/f")).err().unwrap();
    clear_hook();
    assert_code(&e, Code::Unknown);
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("partial"));
    assert_eq!(fs::read(w.join("a/b.real/f")).unwrap(), b"payload", "the clone stays with the moved dir");
    assert_eq!(x_listing(), x0);
    assert_eq!(mode(&x.join("keep")), 0o600);
    assert_eq!(fs::metadata(x.join("keep")).unwrap().modified().unwrap(), y2020());
    fs::remove_file(w.join("a/b")).unwrap();
    fs::remove_dir_all(w.join("a/b.real")).unwrap();
    fs::create_dir(w.join("a/b")).unwrap();
    // f replaced by a hard link to an outside 0600 file before open_rooted_file opens it
    let z = env.dir.path().join("z");
    write(&z, b"zzz");
    fs::set_permissions(&z, fs::Permissions::from_mode(0o600)).unwrap();
    set_mtime(&z, y2020());
    {
        let w2 = w.clone();
        let z2 = z.clone();
        on_open("f", 2, move || {
            fs::remove_file(w2.join("a/b/f")).unwrap();
            fs::hard_link(&z2, w2.join("a/b/f")).unwrap();
        });
    }
    let e = s.get_blob_rooted(&id, &w, &rp("a/b/f")).err().unwrap();
    clear_hook();
    assert_code(&e, Code::Unknown);
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("partial"));
    assert_eq!(mode(&z), 0o600);
    assert_eq!(flags(&z), 0);
    assert_eq!(fs::metadata(&z).unwrap().modified().unwrap(), y2020());
    assert_eq!(fs::read(&z).unwrap(), b"zzz");
}

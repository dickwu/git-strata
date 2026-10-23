mod common;

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use common::*;
use strata_apfs::*;
use strata_types::Code;

#[test]
fn same_volume_tells_system_volume_from_data_volume() {
    let t = Tmp::new();
    fs::write(t.join("a"), "a").unwrap();
    fs::write(t.join("b"), "b").unwrap();
    let sys = Path::new("/usr/bin/true");
    assert_eq!(
        fs::metadata(sys).unwrap().dev(),
        fs::metadata(t.join("a")).unwrap().dev(),
        "firmlinked system and data volumes share st_dev on this machine"
    );
    assert!(!same_volume(sys, &t.join("a")).unwrap());
    assert!(same_volume(&t.join("a"), &t.join("b")).unwrap());
    let img = Image::create(Fs::Apfs, 32);
    assert!(!same_volume(&t.path, img.path()).unwrap());
}

#[test]
fn same_volume_follows_links_and_reports_missing_paths() {
    let name = format!("strata-sv-{}", std::process::id());
    let via_tmp = PathBuf::from("/tmp").join(&name);
    fs::write(&via_tmp, "x").unwrap();
    let real = PathBuf::from("/private/tmp").join(&name);
    assert!(same_volume(&via_tmp, &real).unwrap());
    fs::remove_file(&via_tmp).unwrap();

    let home = std::env::var("HOME").unwrap();
    if home.starts_with("/Users/") {
        let data = PathBuf::from("/System/Volumes/Data").join(home.trim_start_matches('/'));
        assert!(same_volume(Path::new(&home), &data).unwrap());
    }

    let t = Tmp::new();
    let img = Image::create(Fs::Apfs, 32);
    fs::create_dir(img.path().join("d")).unwrap();
    std::os::unix::fs::symlink(img.path().join("d"), t.join("lnk")).unwrap();
    fs::write(t.join("f"), "f").unwrap();
    assert!(!same_volume(&t.join("lnk"), &t.join("f")).unwrap());
    assert!(same_volume(&t.join("lnk"), &img.path().join("d")).unwrap());

    let e = same_volume(&t.join("missing"), &t.join("f")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}

#[test]
fn nearest_existing_ancestor_climbs_to_the_first_existing_path() {
    let t = Tmp::new();
    fs::create_dir(t.join("d")).unwrap();
    assert_eq!(nearest_existing_ancestor(&t.join("d")).unwrap(), t.join("d"));
    assert_eq!(nearest_existing_ancestor(&t.join("d/leaf")).unwrap(), t.join("d"));
    assert_eq!(nearest_existing_ancestor(&t.join("d/a/b/c/d/e")).unwrap(), t.join("d"));
    fs::write(t.join("file"), "x").unwrap();
    assert_eq!(nearest_existing_ancestor(&t.join("file/x/y")).unwrap(), t.join("file"));
    std::os::unix::fs::symlink("nowhere", t.join("dang")).unwrap();
    assert_eq!(nearest_existing_ancestor(&t.join("dang/x")).unwrap(), t.path);

    let name = format!("strata-nea-{}", std::process::id());
    let via_tmp = PathBuf::from("/tmp").join(&name).join("x");
    assert_eq!(nearest_existing_ancestor(&via_tmp).unwrap(), PathBuf::from("/tmp"));

    let rel = nearest_existing_ancestor(Path::new("rel/x")).unwrap_err();
    assert_eq!(rel.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(classify_io_error(&rel, IoSide::Read, Path::new("rel/x")).code, Code::Unknown);

    fs::create_dir(t.join("locked")).unwrap();
    set_mode(&t.join("locked"), 0o000);
    let e = nearest_existing_ancestor(&t.join("locked/x/y")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    let c = classify_io_error(&e, IoSide::Read, &t.join("locked/x/y"));
    assert_eq!(c.code, Code::Unknown);
    assert_eq!(detail_i64(&c, "errno"), Some(libc::EACCES as i64));
    set_mode(&t.join("locked"), 0o755);

    let img = Image::create(Fs::Apfs, 32);
    fs::write(img.path().join("f"), "f").unwrap();
    let anc = nearest_existing_ancestor(&img.path().join("new/deeper")).unwrap();
    assert!(same_volume(&anc, &img.path().join("f")).unwrap());
    assert!(!same_volume(&anc, &t.join("file")).unwrap());
}

#[test]
fn volume_caps_per_filesystem() {
    let apfs = Image::create(Fs::Apfs, 32);
    let c = volume_caps(apfs.path()).unwrap();
    assert_eq!(c.fs_type, "apfs");
    assert_eq!(c.clone, Some(true));
    assert!(!c.case_sensitive);
    assert!(c.write_gen_count);

    let cs = Image::create(Fs::ApfsCaseSensitive, 32);
    let c = volume_caps(cs.path()).unwrap();
    assert_eq!(c.fs_type, "apfs");
    assert!(c.case_sensitive);

    let hfs = Image::create(Fs::Hfs, 32);
    let c = volume_caps(hfs.path()).unwrap();
    assert_eq!(c.fs_type, "hfs");
    assert_eq!(c.clone, None);

    let dos = Image::create(Fs::MsDos, 64);
    let c = volume_caps(dos.path()).unwrap();
    assert_eq!(c.fs_type, "msdos");
    assert_eq!(c.clone, Some(false));
    assert!(!c.write_gen_count);

    let e = volume_caps(&apfs.path().join("missing")).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath);
}

//! "A symlink is recreated, never cloned" (review N6, §8 Task 6):
//! `attrs_of` + `symlink_rooted`, never `clone_from_rooted` / a by-path clone.
mod common;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

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

fn readlink_bytes(p: &Path) -> Vec<u8> {
    fs::read_link(p).unwrap().as_os_str().as_bytes().to_vec()
}

#[test]
fn symlink_recipe_recreates_a_workspace_symlink() {
    let ws_t = Tmp::new();
    let dst_t = Tmp::new();
    let store = Tmp::new();
    let out = Tmp::new();
    let ws = ws_t.path.clone();
    let dst = dst_t.path.clone();
    // Outside the root: a directory whose contents must never be read.
    fs::create_dir(out.join("dir")).unwrap();
    fs::write(out.join("dir/secret"), "outside").unwrap();
    fs::write(ws.join("file"), "in tree").unwrap();
    let links: Vec<(&str, PathBuf)> = vec![
        ("to_file", "file".into()),
        ("to_outside", out.join("dir")),
        ("dangling", "nope/nothing".into()),
        ("relative", "../x".into()),
        ("non_utf8", PathBuf::from(OsStr::from_bytes(b"t\xffr\xfe"))),
    ];
    for (name, target) in &links {
        symlink(target, ws.join(name)).unwrap();
    }
    let out_before = snapshot(&out.path);
    let out_st = st(&out.join("dir"));
    for (name, target) in &links {
        let at = attrs_of(&ws, &rp(name)).unwrap();
        let FileKind::Symlink { target: got } = at.kind.clone() else { panic!("{name}: {:?}", at.kind) };
        assert_eq!(got.as_os_str().as_bytes(), target.as_os_str().as_bytes(), "{name}");
        symlink_rooted(&dst, &rp(name), got.as_os_str()).unwrap();
        assert_eq!(readlink_bytes(&dst.join(name)), readlink_bytes(&ws.join(name)), "{name}");
        // The same link handed to clone_from_rooted is refused, nothing made.
        let cd = store.join(&format!("{name}_f"));
        assert_reason(clone_from_rooted(&ws, &rp(name), &cd, CloneKind::File), Code::NotFile, "symlink");
        assert!(fs::symlink_metadata(&cd).is_err());
        let cd = store.join(&format!("{name}_d"));
        assert_reason(clone_from_rooted(&ws, &rp(name), &cd, CloneKind::Dir), Code::NotDirectory, "symlink");
        assert!(fs::symlink_metadata(&cd).is_err());
    }
    assert_eq!(snapshot(&out.path), out_before);
    let after = st(&out.join("dir"));
    assert_eq!((after.st_mtime, after.st_mtime_nsec, after.st_ctime, after.st_ctime_nsec), (out_st.st_mtime, out_st.st_mtime_nsec, out_st.st_ctime, out_st.st_ctime_nsec));

    // A parent swapped for a symlink to outside: the recipe reads nothing.
    fs::create_dir(ws.join("d")).unwrap();
    symlink("file", ws.join("d/link")).unwrap();
    fs::create_dir(out.join("evil")).unwrap();
    symlink("outside-target", out.join("evil/link")).unwrap();
    let swap = {
        let ws = ws.clone();
        let evil = out.join("evil");
        move || {
            fs::rename(ws.join("d"), ws.join("d.real")).unwrap();
            symlink(&evil, ws.join("d")).unwrap();
        }
    };
    {
        let _g = ClearHook;
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let f2 = fired.clone();
        let swap = swap.clone();
        set_before_open(Some(Box::new(move |_, n| {
            if n == "d" && !f2.get() {
                f2.set(true);
                swap();
            }
        })));
        assert_reason(attrs_of(&ws, &rp("d/link")), Code::NotDirectory, "symlink");
        assert!(fired.get());
    }
    // Contrast: a by-path clone follows the swapped middle component and
    // copies the outside link (§4.2 A) — why the recipe goes through attrs_of.
    clone_file_rooted(&ws.join("d/link"), &dst, &rp("by_path"), None).unwrap();
    assert_eq!(readlink_bytes(&dst.join("by_path")), b"outside-target");
}

//! `rooted_roots_must_be_canonical`: every function that takes a root refuses
//! a non-canonical one before touching anything. The table grows as the
//! functions land (§8 Task 3 onwards).
mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::*;
use strata_apfs::*;
use strata_types::{Code, StrataError};

type Call = Box<dyn Fn(&Path) -> std::result::Result<(), StrataError>>;

fn calls(src_file: PathBuf, src_dir: PathBuf) -> Vec<(&'static str, Call)> {
    let (sf, sf2, sd) = (src_file.clone(), src_file, src_dir);
    vec![
        ("open_root", Box::new(|r| open_root(r).map(drop))),
        ("open_rooted", Box::new(|r| open_rooted(r, &rp("f")).map(drop))),
        ("open_rooted_file", Box::new(|r| open_rooted_file(r, &rp("f")).map(drop))),
        ("clone_file_rooted", Box::new(move |r| clone_file_rooted(&sf, r, &rp("dst"), None))),
        ("clone_file_rooted(mode)", Box::new(move |r| clone_file_rooted(&sf2, r, &rp("dst"), Some(0o644)))),
        ("clone_tree_rooted", Box::new(move |r| clone_tree_rooted(&sd, r, &rp("dst")))),
        ("create_file_rooted", Box::new(|r| create_file_rooted(r, &rp("dst"), 0o644).map(drop))),
        ("mkdir_rooted(false)", Box::new(|r| mkdir_rooted(r, &rp("dst"), 0o755, false))),
        ("mkdir_rooted(true)", Box::new(|r| mkdir_rooted(r, &rp("dst/a"), 0o755, true))),
        ("symlink_rooted", Box::new(|r| symlink_rooted(r, &rp("dst"), "f".as_ref()))),
        ("rename_rooted(Excl)", Box::new(|r| rename_rooted(r, &rp("f"), &rp("dst"), RenameMode::Excl))),
        ("rename_rooted(Swap)", Box::new(|r| rename_rooted(r, &rp("f"), &rp("sub"), RenameMode::Swap))),
        ("remove_rooted(File)", Box::new(|r| remove_rooted(r, &rp("f"), RemoveKind::File))),
        ("remove_rooted(EmptyDir)", Box::new(|r| remove_rooted(r, &rp("sub"), RemoveKind::EmptyDir))),
    ]
}

/// Calls that must succeed on a canonical root containing a file `f`.
fn ok_calls() -> Vec<(&'static str, Call)> {
    vec![
        ("open_root", Box::new(|r| open_root(r).map(drop))),
        ("open_rooted_file", Box::new(|r| open_rooted_file(r, &rp("f")).map(drop))),
        ("create_file_rooted", Box::new(|r| create_file_rooted(r, &rp("created"), 0o644).map(drop))),
    ]
}

#[test]
fn rooted_roots_must_be_canonical() {
    let t = Tmp::new();
    let root = t.join("root");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("f"), "f").unwrap();
    let target = t.join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("f"), "t").unwrap();
    symlink(&target, t.join("lnk")).unwrap();
    let r = root.to_str().unwrap().to_string();
    let l = t.join("lnk").to_str().unwrap().to_string();
    let bad: Vec<PathBuf> = [
        format!("{l}/"),
        format!("{r}/"),
        format!("{r}/."),
        format!("{r}/.."),
        format!("{r}/sub/../sub"),
        String::new(),
        "repo".into(),
        "a/b".into(),
        "./repo".into(),
        ".hidden".into(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    fs::write(t.join("srcfile"), "s").unwrap();
    fs::create_dir(t.join("srcdir")).unwrap();
    let (sr, st) = (snapshot(&root), snapshot(&target));
    for b in &bad {
        for (name, call) in calls(t.join("srcfile"), t.join("srcdir")) {
            let e = call(b).expect_err(name);
            assert_eq!(e.code, Code::InvalidInput, "{name} {b:?}: {e:?}");
            assert_eq!(detail_str(&e, "reason").as_deref(), Some("root_not_canonical"), "{name} {b:?}");
        }
        assert_eq!(snapshot(&root), sr);
        assert_eq!(snapshot(&target), st);
    }
    // Canonical roots work, including dot-led names that are not `.`/`..`
    // and a root whose ancestor is a symlink.
    let base = tempfile::Builder::new().prefix("strata-roots").tempdir_in("/tmp").unwrap();
    let via_tmp = Path::new("/tmp").join(base.path().file_name().unwrap());
    let mut good = vec![root.clone(), via_tmp.clone()];
    for d in ["sessions/.repos/id1/repo.git", ".staging/abc123", "..hidden", "a..b", "..."] {
        good.push(t.join(d));
    }
    for g in &good {
        fs::create_dir_all(g).unwrap();
        if !g.join("f").exists() {
            fs::write(g.join("f"), "f").unwrap();
        }
        for (name, call) in ok_calls() {
            call(g).unwrap_or_else(|e| panic!("{name} {g:?}: {e:?}"));
        }
    }
}

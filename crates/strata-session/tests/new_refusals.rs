//! session.md §8 Task 1: `new` refuses invalid requests before writing.

mod support;

use std::fs;
use std::path::Path;

use strata_session::{NewReport, Result};
use strata_types::{Code, Config, Paths, StrataError};
use support::*;

/// `new` got past every refusal and built the session.
fn reached_build(r: &Result<NewReport>) -> bool {
    r.is_ok()
}

/// Nothing of a session anywhere: no row, no dirs under sessions/ (and
/// .repos/), no pins.
fn assert_no_trace(fx: &Fx) {
    assert_eq!(fx.session_rows(), 0, "a session row was written");
    assert!(fx.session_dirs().is_empty(), "dirs under sessions/: {:?}", fx.session_dirs());
    for rc in fx.config.repos.values() {
        if rc.path.is_dir() {
            let pins = fx.sb.git(&rc.path, &["for-each-ref", "refs/strata/pins/"]);
            assert!(pins.is_empty(), "pins written: {pins}");
        }
    }
}

fn assert_snaps(fx: &Fx, before: &[(String, MainSnap)], orig: &[(String, std::path::PathBuf)]) {
    for ((n, s), (_, p)) in before.iter().zip(orig) {
        assert_eq!(&MainSnap::at(&fx.sb, p), s, "main checkout {n} changed");
    }
}

fn detail_u64(e: &StrataError, k: &str) -> Option<u64> {
    e.detail.as_ref().and_then(|d| d.get(k)).and_then(|v| v.as_u64())
}

#[test]
fn new_typed_refusals_leave_no_trace() {
    type Setup = fn(&mut Fx);
    type Case = (&'static str, Setup, Code, &'static [(&'static str, &'static str)]);
    let cases: Vec<Case> = vec![
        ("unknown repo", |_| {}, Code::NoSuchRepo, &[("repo", "nope")]),
        (
            "missing path",
            |fx| {
                let gone = fx.sb.root.join("gone");
                fx.cfg_mut("tool").path = gone;
            },
            Code::NoSuchPath,
            &[("repo", "tool"), ("field", "path")],
        ),
        (
            "seed cache is a file",
            |fx| fs::write(fx.main("tool").join(".cache"), b"x").unwrap(),
            Code::NotDirectory,
            &[("repo", "tool"), ("field", "seed_caches"), ("path", ".cache"), ("reason", "not_a_directory")],
        ),
        (
            "explicit seed is a directory",
            |fx| {
                fs::create_dir_all(fx.main("tool").join("conf.d")).unwrap();
                fx.cfg_mut("tool").seed_files = Some(rps(&[".env", "conf.d"]));
            },
            Code::IsDirectory,
            &[("repo", "tool"), ("field", "seed_files"), ("path", "conf.d")],
        ),
        (
            "explicit seed is a FIFO",
            |fx| {
                mkfifo(&fx.main("front").join(".env"));
                fx.cfg_mut("front").seed_files = Some(rps(&[".env"]));
            },
            Code::NotFile,
            &[("repo", "front"), ("field", "seed_files"), ("path", ".env"), ("file_type", "fifo")],
        ),
        (
            "path is a symlink",
            |fx| {
                let link = fx.sb.root.join("tool-link");
                std::os::unix::fs::symlink(fx.main("tool"), &link).unwrap();
                fx.cfg_mut("tool").path = link;
            },
            Code::NotDirectory,
            &[("repo", "tool"), ("field", "path"), ("reason", "symlink")],
        ),
        (
            "no origin/main",
            |fx| {
                let origin = fx.sb.root.join("front-origin.git");
                fx.sb.git(&origin, &["update-ref", "-d", "refs/heads/main"]);
            },
            Code::NoSuchBranch,
            &[("branch", "main")],
        ),
    ];
    for (name, setup, code, details) in cases {
        let mut fx = Fx::new();
        let orig: Vec<(String, std::path::PathBuf)> = fx.names.iter().map(|n| (n.clone(), fx.main(n))).collect();
        setup(&mut fx);
        let before: Vec<(String, MainSnap)> = orig.iter().map(|(n, p)| (n.clone(), MainSnap::at(&fx.sb, p))).collect();
        let s = fx.sessions();
        let repos = if name == "unknown repo" { vec![fx.rn("tool"), fx.rn("nope")] } else { vec![fx.rn("tool")] };
        let e = s.new(Some("gwd-1"), &repos).expect_err(name);
        assert_eq!(e.code, code, "{name}: {e:?}");
        for (k, v) in details {
            assert_eq!(detail_str(&e, k), Some(*v), "{name}: detail.{k} in {e:?}");
        }
        if name == "explicit seed is a directory" {
            assert_eq!(detail_u64(&e, "index"), Some(1));
        }
        drop(s);
        assert_no_trace(&fx);
        assert_snaps(&fx, &before, &orig);
    }
}

#[test]
fn new_passes_lower_codes_through_unchanged() {
    let fx = Fx::new();
    let origin = fx.sb.root.join("tool-origin.git");
    fx.sb.git(&origin, &["update-ref", "-d", "refs/heads/main"]);
    let want = fx.gits[&fx.rn("tool")].fetch("origin", "main").unwrap_err();
    assert_eq!(want.code, Code::NoSuchBranch);
    let s = fx.sessions();
    let got = s.new(None, &[fx.rn("front")]).unwrap_err();
    assert_eq!(got, want, "gitio's error reaches the caller verbatim");
    assert_no_trace(&fx);
}

#[test]
fn new_passes_gitlink_unsupported_through_unchanged() {
    let fx = Fx::new();
    let main = fx.main("tool");
    let head = fx.sb.head(&main);
    fx.sb.git(&main, &["update-index", "--add", "--cacheinfo", &format!("160000,{head},sub")]);
    fx.sb.git(&main, &["commit", "-q", "-m", "gitlink"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    let git = &fx.gits[&fx.rn("tool")];
    let tip = git.fetch("origin", "main").unwrap();
    // gitio refuses the gitlink when reading the tree.
    let want = git.tree_of(&tip).unwrap_err();
    assert_eq!(want.code, Code::Unsupported);
    let s = fx.sessions();
    let got = s.new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(got, want, "gitio's Unsupported reaches the caller verbatim");
    assert_no_trace(&fx);
    let staging = fx.root().join("bases/tool/.staging");
    assert!(!staging.exists() || fs::read_dir(&staging).unwrap().count() == 0, "staging left behind");
}

#[test]
fn new_validates_ticket() {
    let fx = Fx::new();
    let s = fx.sessions();
    let tool = [fx.rn("tool")];
    let long = "a".repeat(65);
    for bad in ["../foreign", "", long.as_str(), "gwd 9246", "gwd:9246"] {
        let e = s.new(Some(bad), &tool).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{bad:?}: {e:?}");
        assert_eq!(detail_str(&e, "field"), Some("ticket"));
    }
    for reserved in ["s-k3q7xm2a", "S-K3Q7XM2A"] {
        let e = s.new(Some(reserved), &tool).unwrap_err();
        assert_eq!(e.code, Code::ReservedName, "{reserved}: {e:?}");
    }
    assert_eq!(fx.lock.holds().len(), 0, "refused tickets never reach the lock");
    assert_no_trace(&fx);
    for ok in ["9246", "gwd-9246", "a.b_c-d"] {
        let r = s.new(Some(ok), &tool);
        assert!(reached_build(&r), "{ok}: {r:?}");
    }
}

#[test]
fn new_refuses_empty_repo_list() {
    let fx = Fx::new();
    let e = fx.sessions().new(None, &[]).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(detail_str(&e, "field"), Some("repos"));
}

/// paths.root on another volume than the checkouts.
#[test]
#[ignore = "needs hdiutil attach (a disk image), which needs a human-approved environment"]
fn new_refuses_cross_volume_root() {
    let sb = Sandbox::new();
    let img = DiskImage::new(&sb, "strata-xvol", "APFS");
    let main = sb.init_project("tool");
    sb.bare_origin(&main);
    let mut repos = std::collections::BTreeMap::new();
    repos.insert(repo_name("tool"), sb.config(&main));
    let config = Config { paths: Paths { root: img.mount.join("strata") }, repos, dev_ports: Default::default() };
    let fx = Fx::from_parts_unchecked(sb, config);
    let e = fx.sessions().new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(e.code, Code::CrossVolume, "{e:?}");
    assert_eq!(detail_str(&e, "repo"), Some("tool"));
    assert!(fx.session_dirs().is_empty());
}

/// paths.root on a volume that is not APFS.
#[test]
#[ignore = "needs hdiutil attach (a disk image), which needs a human-approved environment"]
fn new_refuses_non_apfs_root() {
    let sb = Sandbox::new();
    let img = DiskImage::new(&sb, "strata-hfs", "HFS+");
    let main_parent = img.mount.join("work");
    fs::create_dir_all(&main_parent).unwrap();
    let main = main_parent.join("tool");
    fs::create_dir_all(&main).unwrap();
    sb.git(&main, &["init", "-q"]);
    sb.commit(&main, &[(".gitignore", b"node_modules/\n"), ("package.json", PACKAGE_JSON.as_bytes())], "initial");
    sb.bare_origin(&main);
    let mut repos = std::collections::BTreeMap::new();
    repos.insert(repo_name("tool"), sb.config(&main));
    let config = Config { paths: Paths { root: img.mount.join("strata") }, repos, dev_ports: Default::default() };
    let fx = Fx::from_parts_unchecked(sb, config);
    let e = fx.sessions().new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
    assert!(detail_str(&e, "fs_type").is_some());
    assert!(fx.session_dirs().is_empty());
}

#[test]
fn new_refuses_existing_session_dir_before_any_effect() {
    for sub in ["", ".repos/"] {
        let fx = Fx::new();
        let id = sid("s-k3q7xm2a");
        let dir = fx.root().join("sessions").join(format!("{sub}{}", id.as_str()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep"), b"mine").unwrap();
        let s = fx.sessions();
        s.set_next_id(id.clone());
        let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, Code::Exists, "{e:?}");
        assert_eq!(reason(&e), Some("directory"));
        assert_eq!(detail_str(&e, "session"), Some(id.as_str()));
        assert_eq!(fs::read(dir.join("keep")).unwrap(), b"mine");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(fx.lock.holds().is_empty(), "no fetch before the id check");
        assert_eq!(fx.session_rows(), 0);
        assert!(!fx.root().join("bases").join("tool").exists() || fs::read_dir(fx.root().join("bases/tool")).unwrap().count() == 0);
        assert!(fx.pins("tool").is_empty());
        let remotes = fx.sb.git(&fx.main("tool"), &["for-each-ref", "refs/strata/remotes/"]);
        assert!(remotes.is_empty(), "fetched: {remotes}");
    }
}

#[test]
fn new_refuses_id_with_existing_store_row() {
    let fx = Fx::new();
    let id = sid("s-k3q7xm2a");
    let s = fx.sessions();
    s.set_next_id(id.clone());
    s.new(None, &[fx.rn("tool")]).unwrap();
    // Both directories gone, the row stays: the id is still taken.
    for sub in ["", ".repos/"] {
        fs::remove_dir_all(fx.root().join("sessions").join(format!("{sub}{}", id.as_str()))).unwrap();
    }
    let holds = fx.lock.holds().len();
    let installs = fx.sb.install_count();
    s.set_next_id(id.clone());
    let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(reason(&e), Some("store_row"));
    assert_eq!(detail_str(&e, "session"), Some(id.as_str()));
    assert_eq!(fx.lock.holds().len(), holds, "no lock taken");
    assert_eq!(fx.sb.install_count(), installs);
    assert_eq!(fx.session_rows(), 1);
    assert!(fx.session_dirs().is_empty(), "{:?}", fx.session_dirs());
}

/// Four spellings whose literal paths do not nest but whose real ones do.
#[test]
fn new_refuses_root_aliased_into_checkout() {
    // 1. root through a symlink into a checkout
    {
        let sb = Sandbox::new();
        let main = sb.init_project("tool");
        sb.bare_origin(&main);
        std::os::unix::fs::symlink(&main, sb.root.join("link")).unwrap();
        let fx = aliased(sb, &main, |sb| sb.root.join("link/strata"));
        expect_nesting(&fx, "checkout", &main);
    }
    // 2. root spelled with another case (case-insensitive volume only)
    {
        let sb = Sandbox::new();
        let main = sb.init_project("tool");
        sb.bare_origin(&main);
        let caps = strata_apfs::volume_caps(&sb.root).unwrap();
        if caps.case_sensitive {
            eprintln!("skipping the case spelling: {} is case-sensitive", sb.root.display());
        } else {
            let fx = aliased(sb, &main, |sb| sb.root.join("TOOL/strata"));
            expect_nesting(&fx, "checkout", &main);
        }
    }
    // 3. the configured checkout is a linked worktree; root lies in the
    //    common dir it shares with the user's main checkout
    {
        let sb = Sandbox::new();
        let main = sb.init_project("tool");
        sb.bare_origin(&main);
        let linked = sb.root.join("linked");
        sb.git(&main, &["worktree", "add", "-q", "-b", "wt", linked.to_str().unwrap()]);
        let fx = aliased(sb, &linked, |sb| sb.root.join("tool/.git/strata"));
        expect_nesting(&fx, "common_dir", &main.join(".git"));
    }
    // 4. a repo path that reaches into root through a symlinked parent
    {
        let sb = Sandbox::new();
        let root = sb.root.join("strata");
        Store_open_close(&root);
        let main = sb.init_project("strata/bases/tool");
        sb.bare_origin(&main);
        std::os::unix::fs::symlink(&root, sb.root.join("lnk")).unwrap();
        let path = sb.root.join("lnk/bases/tool");
        let fx = aliased(sb, &path, |_| root.clone());
        expect_nesting(&fx, "checkout", &main);
    }
}

#[allow(non_snake_case)]
fn Store_open_close(root: &Path) {
    strata_store::Store::open(root, &[]).unwrap().close().unwrap();
}

fn aliased(sb: Sandbox, path: &Path, root: impl Fn(&Sandbox) -> std::path::PathBuf) -> Fx {
    let mut repos = std::collections::BTreeMap::new();
    repos.insert(repo_name("tool"), sb.config(path));
    let config = Config { paths: Paths { root: root(&sb) }, repos, dev_ports: Default::default() };
    Fx::from_parts_unchecked(sb, config)
}

fn expect_nesting(fx: &Fx, key: &str, real: &Path) {
    let before = MainSnap::at(&fx.sb, &fx.main("tool"));
    let dirs_before = snapshot(&fx.sb.root).len();
    let e = fx.sessions().new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(detail_str(&e, "rule"), Some("nesting"), "{e:?}");
    assert_eq!(detail_str(&e, "repo"), Some("tool"));
    assert!(detail_str(&e, "root").is_some());
    let got = detail_str(&e, key).unwrap_or_else(|| panic!("detail.{key} in {e:?}"));
    assert!(
        Path::new(got).to_string_lossy().eq_ignore_ascii_case(&real.to_string_lossy()),
        "detail.{key} = {got}, want {}",
        real.display()
    );
    assert_eq!(MainSnap::at(&fx.sb, &fx.main("tool")), before, "nothing written in the checkout");
    assert_eq!(snapshot(&fx.sb.root).len(), dirs_before, "no new entries anywhere");
    assert!(fx.lock.holds().is_empty(), "no fetch");
}

#[test]
fn new_refuses_symlinked_repo_path() {
    let mut fx = Fx::new();
    let real = fx.main("tool");
    let link = fx.sb.root.join("tool-link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    fx.cfg_mut("tool").path = link.clone();
    {
        let e = fx.sessions().new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, Code::NotDirectory);
        assert_eq!(detail_str(&e, "repo"), Some("tool"));
        assert_eq!(detail_str(&e, "field"), Some("path"));
        assert_eq!(detail_str(&e, "path"), link.to_str());
        assert_eq!(reason(&e), Some("symlink"));
        assert_no_trace(&fx);
    }
    // the root as a symlink
    let real_root = fx.root();
    fx.cfg_mut("tool").path = real.clone();
    let root_link = fx.sb.root.join("root-link");
    std::os::unix::fs::symlink(&real_root, &root_link).unwrap();
    fx.config.paths.root = root_link.clone();
    {
        let e = fx.sessions().new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, Code::NotDirectory);
        assert_eq!(detail_str(&e, "field"), Some("paths.root"));
        assert_eq!(reason(&e), Some("symlink"));
    }
    fx.config.paths.root = real_root;
    assert_no_trace(&fx);
    let r = fx.sessions().new(None, &[fx.rn("tool")]);
    assert!(reached_build(&r), "after the fix: {r:?}");
}

#[test]
fn new_refuses_fifo_seed_before_writing() {
    let mut fx = Fx::new();
    let fifo = fx.main("tool").join(".env");
    mkfifo(&fifo);
    fx.cfg_mut("tool").seed_files = Some(rps(&[".env"]));
    let first = sid("s-aaaaaaaa");
    {
        let s = fx.sessions();
        s.set_next_id(first.clone());
        // A FIFO with no writer: opening it for reading would block forever.
        let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, Code::NotFile);
        assert_eq!(detail_str(&e, "file_type"), Some("fifo"));
        assert_eq!(detail_str(&e, "field"), Some("seed_files"));
    }
    assert_no_trace(&fx);
    fs::remove_file(&fifo).unwrap();
    fs::write(&fifo, b"TOKEN=1\n").unwrap();
    // An explicit seed must be ignored (§5.3 step 4): via the main checkout's info/exclude.
    fs::write(fx.main("tool").join(".git/info/exclude"), b".env\n").unwrap();
    let s = fx.sessions();
    let r = s.new(None, &[fx.rn("tool")]);
    assert!(reached_build(&r), "after the fix: {r:?}");
    if let Ok(rep) = &r {
        assert_ne!(rep.manifest.id, first, "a fresh id, not the refused one");
    }
}

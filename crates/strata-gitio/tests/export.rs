mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use common::*;
use strata_gitio::{EXPORT_TEMP_PREFIX, Git, SyncEvent};
use strata_types::{Code, TreeOid};

fn toid(s: &str) -> TreeOid {
    TreeOid::try_from(s.trim().to_string()).unwrap()
}

fn blob(sb: &Sandbox, repo: &Path, content: &[u8]) -> String {
    sb.git_stdin(repo, &["hash-object", "-w", "--stdin"], content)
}

fn mktree(sb: &Sandbox, repo: &Path, entries: &[(&str, &str, &str, &[u8])]) -> String {
    let mut input = Vec::new();
    for (mode, kind, o, name) in entries {
        input.extend_from_slice(format!("{mode} {kind} {o}\t").as_bytes());
        input.extend_from_slice(name);
        input.push(0);
    }
    sb.git_stdin(repo, &["mktree", "-z"], &input)
}

#[derive(Debug, PartialEq, Eq)]
enum Node {
    File { bytes: Vec<u8>, exec: bool },
    Link(PathBuf),
    Dir,
}

/// Everything under `dir` except a top-level `.git`.
fn listing(dir: &Path) -> BTreeMap<PathBuf, Node> {
    let mut out = BTreeMap::new();
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<PathBuf, Node>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if rel == Path::new(".git") {
                continue;
            }
            let m = fs::symlink_metadata(&p).unwrap();
            if m.file_type().is_symlink() {
                out.insert(rel, Node::Link(fs::read_link(&p).unwrap()));
            } else if m.is_dir() {
                out.insert(rel, Node::Dir);
                walk(base, &p, out);
            } else {
                out.insert(rel, Node::File { bytes: fs::read(&p).unwrap(), exec: m.permissions().mode() & 0o111 != 0 });
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn temps(dir: &Path) -> Vec<String> {
    entries(dir).into_iter().filter(|n| n.starts_with(EXPORT_TEMP_PREFIX)).collect()
}

/// A clone of `repo` with `commit` checked out by plain git.
fn checkout_clone(sb: &Sandbox, repo: &Path, name: &str, commit: &str, cfg: &[&str]) -> PathBuf {
    let dst = sb.path(name);
    let mut args = vec!["clone", "-q", "--no-checkout"];
    for c in cfg {
        args.push("-c");
        args.push(c);
    }
    let (r, d) = (repo.to_str().unwrap().to_string(), dst.to_str().unwrap().to_string());
    args.push(&r);
    args.push(&d);
    sb.git(&sb.root, &args);
    sb.git(&dst, &["checkout", "-q", commit]);
    dst
}

fn add_rot13_filter(sb: &Sandbox) {
    let mut cfg = fs::read_to_string(sb.home.join(".gitconfig")).unwrap();
    cfg.push_str("[filter \"rot13\"]\n\tsmudge = tr A-Za-z N-ZA-Mn-za-m\n\tclean = tr A-Za-z N-ZA-Mn-za-m\n");
    fs::write(sb.home.join(".gitconfig"), cfg).unwrap();
}

/// The mixed repository of `export_tree_matches_git_checkout_byte_for_byte`.
fn mixed_repo(sb: &Sandbox, name: &str, autocrlf: bool) -> (PathBuf, String) {
    let repo = sb.init_repo(name);
    if autocrlf {
        sb.git(&repo, &["config", "core.autocrlf", "true"]);
    }
    sb.write(&repo, ".gitignore", b"*.log\n");
    sb.write(
        &repo,
        ".gitattributes",
        b"*.crlf text eol=crlf\n*.u16 text working-tree-encoding=UTF-16\n*.id ident\n*.rot filter=rot13\n*.txt text\n",
    );
    sb.write(&repo, "a.crlf", b"l1\nl2\n");
    sb.write(&repo, "c.id", b"$Id$\nbody\n");
    sb.write(&repo, "d.rot", b"Hello Secret\n");
    sb.write(&repo, "plain.txt", b"x\ny\n");
    sb.write(&repo, "bin.dat", &[0, 1, 2, 255, b'\n', b'\r', b'\n']);
    sb.write(&repo, "empty.txt", b"");
    sb.write(&repo, "run.sh", b"#!/bin/sh\necho hi\n");
    fs::set_permissions(repo.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("nowhere", repo.join("dangling")).unwrap();
    symlink("self", repo.join("self")).unwrap();
    symlink("../../../../etc/passwd", repo.join("outside")).unwrap();
    sb.write(&repo, "a/b/c/d/e/f/g.txt", b"deep\n");
    sb.write(&repo, "caf\u{e9}/na\u{ef}ve.txt", b"unicode\n");
    sb.git(&repo, &["add", "-A"]);
    // working-tree-encoding: the blob is UTF-8; checkout writes UTF-16.
    let u = sb.git_stdin(&repo, &["hash-object", "-w", "--no-filters", "--stdin"], "h\u{e9}llo\n".as_bytes());
    sb.git(&repo, &["update-index", "--add", "--cacheinfo", &format!("100644,{u},b.u16")]);
    sb.git(&repo, &["commit", "-qm", "mixed"]);
    let head = sb.head(&repo);
    (repo, head)
}

fn assert_like_checkout(dst: &Path, co: &Path) {
    let a = listing(dst);
    let b = listing(co);
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (k, v) in &a {
        assert_eq!(v, &b[k], "{}", k.display());
    }
    assert!(!dst.join(".git").exists());
    for (k, v) in &a {
        let p = dst.join(k);
        match v {
            Node::Dir => assert!(fs::read_dir(&p).unwrap().next().is_some(), "empty dir {}", k.display()),
            Node::File { .. } => assert_eq!(fs::metadata(&p).unwrap().nlink(), 1, "{}", k.display()),
            Node::Link(_) => {}
        }
    }
}

#[test]
fn export_tree_matches_git_checkout_byte_for_byte() {
    let sb = Sandbox::new();
    add_rot13_filter(&sb);
    for (name, autocrlf) in [("mixed", false), ("mixed-crlf", true)] {
        let (repo, head) = mixed_repo(&sb, name, autocrlf);
        let git = sb.open(&repo);
        let tree = sb.tree_of(&repo, "HEAD");
        git.take_git_calls();
        let dst = sb.path(&format!("out/{name}"));
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        git.export_tree(&toid(&tree), &dst).unwrap();
        let cfg: &[&str] = if autocrlf { &["core.autocrlf=true"] } else { &[] };
        let co = checkout_clone(&sb, &repo, &format!("clone-{name}"), &head, cfg);
        assert_like_checkout(&dst, &co);
        // The conversions really happened.
        assert_eq!(fs::read(dst.join("a.crlf")).unwrap(), b"l1\r\nl2\r\n");
        let rot: &[u8] = if autocrlf { b"Hello Secret\r\n" } else { b"Hello Secret\n" };
        assert_eq!(fs::read(dst.join("d.rot")).unwrap(), rot);
        assert_ne!(fs::read(dst.join("b.u16")).unwrap(), "h\u{e9}llo\n".as_bytes());
        assert!(fs::read_to_string(dst.join("c.id")).unwrap().starts_with("$Id: "));
        if autocrlf {
            assert_eq!(fs::read(dst.join("plain.txt")).unwrap(), b"x\r\ny\r\n");
        } else {
            assert_eq!(fs::read(dst.join("plain.txt")).unwrap(), b"x\ny\n");
        }
        assert_eq!(fs::read_link(dst.join("outside")).unwrap(), Path::new("../../../../etc/passwd"));
        let calls = git.take_git_calls();
        let ci = find_call(&calls, "checkout-index").expect("checkout-index ran");
        assert_eq!(env_of(ci, "LC_ALL"), std::env::var("LC_ALL").ok(), "gitio sets no LC_ALL for checkout-index");
        assert!(env_of(ci, "GIT_INDEX_FILE").is_some_and(|i| !i.starts_with(repo.join(".git").to_str().unwrap())));
        assert_eq!(temps(dst.parent().unwrap()), Vec::<String>::new());
    }

    // The empty tree exports as an empty directory.
    let repo = sb.path("mixed");
    let empty = sb.git_stdin(&repo, &["mktree"], b"");
    let git = sb.open(&repo);
    let dst = sb.path("out/empty");
    git.export_tree(&toid(&empty), &dst).unwrap();
    assert!(dst.is_dir());
    assert_eq!(entries(&dst), Vec::<String>::new());
}

#[test]
fn export_filter_failure_matches_checkout() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.write(&repo, ".gitattributes", b"*.f filter=nope\n");
    sb.write(&repo, "x.f", b"raw\n");
    sb.git(&repo, &["add", "-A"]);
    sb.git(&repo, &["commit", "-qm", "filtered"]);
    let head = sb.head(&repo);
    let tree = toid(&sb.tree_of(&repo, "HEAD"));
    sb.git(&repo, &["config", "filter.nope.smudge", "definitely-not-a-command-strata"]);
    sb.git(&repo, &["config", "filter.nope.required", "true"]);
    let git = sb.open(&repo);
    let out = sb.path("out");
    fs::create_dir(&out).unwrap();
    let dst = out.join("dst");
    let e = git.export_tree(&tree, &dst).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert!(!dst.exists());
    let t = temps(&out);
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(detail(&e, "temp").as_deref(), Some(out.join(&t[0]).to_str().unwrap()));
    assert_eq!(detail(&e, "tree").as_deref(), Some(tree.as_str()));

    sb.git(&repo, &["config", "--unset", "filter.nope.required"]);
    let dst2 = out.join("dst2");
    git.export_tree(&tree, &dst2).unwrap();
    assert_eq!(fs::read(dst2.join("x.f")).unwrap(), b"raw\n");
    let co = checkout_clone(&sb, &repo, "clone", &head, &["filter.nope.smudge=definitely-not-a-command-strata"]);
    assert_like_checkout(&dst2, &co);
}

#[test]
fn export_tree_refuses_existing_destination_untouched() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let git = sb.open(&repo);
    let tree = toid(&sb.tree_of(&repo, "HEAD"));
    let out = sb.path("out");
    fs::create_dir(&out).unwrap();
    let dst = out.join("dst");
    fs::create_dir(&dst).unwrap();
    fs::write(dst.join("f"), b"keep me\n").unwrap();
    let before = (snapshot(&out), fs::metadata(dst.join("f")).unwrap().mtime_nsec(), fs::metadata(&dst).unwrap().mtime_nsec());
    let e = git.export_tree(&tree, &dst).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    let after = (snapshot(&out), fs::metadata(dst.join("f")).unwrap().mtime_nsec(), fs::metadata(&dst).unwrap().mtime_nsec());
    assert_eq!(before, after);
    assert_eq!(entries(&out), vec!["dst"]);
    // A file or a dangling symlink in the way is refused the same way.
    fs::write(out.join("file"), b"f").unwrap();
    symlink("nowhere", out.join("link")).unwrap();
    assert_eq!(git.export_tree(&tree, &out.join("file")).unwrap_err().code, Code::Exists);
    assert_eq!(git.export_tree(&tree, &out.join("link")).unwrap_err().code, Code::Exists);
    assert_eq!(entries(&out), vec!["dst", "file", "link"]);
}

#[test]
fn export_typed_refusals_write_nothing() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let git = sb.open(&repo);
    let good = sb.tree_of(&repo, "HEAD");
    let head = sb.head(&repo);
    let b = blob(&sb, &repo, b"x\n");
    let cfg = mktree(&sb, &repo, &[("100644", "blob", &b, b"config")]);
    let long = "a".repeat(256);
    sb.git(&repo, &["config", "core.ignorecase", "true"]);
    let bad = [
        ("gitlink", mktree(&sb, &repo, &[("160000", "commit", &head, b"sub")]), Code::Unsupported),
        ("nottext", mktree(&sb, &repo, &[("100644", "blob", &b, b"\xfe.txt")]), Code::NotText),
        (
            "collision",
            mktree(&sb, &repo, &[("100644", "blob", &b, b"README"), ("100644", "blob", &b, b"readme")]),
            Code::Unsupported,
        ),
        ("dotgit", mktree(&sb, &repo, &[("040000", "tree", &cfg, b".git")]), Code::Unsupported),
        ("overlong", mktree(&sb, &repo, &[("100644", "blob", &b, long.as_bytes())]), Code::InvalidInput),
    ];
    let out = sb.path("out");
    fs::create_dir(&out).unwrap();
    for (tag, t, code) in &bad {
        let e = git.export_tree(&toid(t), &out.join(tag)).unwrap_err();
        assert_eq!(&e.code, code, "{tag}: {e:?}");
        assert_eq!(entries(&out), Vec::<String>::new(), "{tag}");
    }
    let good = toid(&good);
    // A dst name whose temp name exceeds 255 UTF-16 units.
    let e = git.export_tree(&good, &out.join("n".repeat(240))).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(entries(&out), Vec::<String>::new());
    // Parent missing; parent a symlink; parent a file.
    let e = git.export_tree(&good, &out.join("missing/dst")).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    let target = sb.path("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, out.join("via-link")).unwrap();
    let e = git.export_tree(&good, &out.join("via-link/dst")).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(entries(&target), Vec::<String>::new());
    fs::write(out.join("file"), b"").unwrap();
    let e = git.export_tree(&good, &out.join("file/dst")).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(entries(&out), vec!["file", "via-link"]);
    // Inside the main checkout, its work tree or its common dir.
    let before = snapshot(&repo);
    for d in [repo.join("exported"), repo.join(".git/exported"), repo.join(".git/refs/x")] {
        let e = git.export_tree(&good, &d).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{}: {e:?}", d.display());
    }
    assert_eq!(snapshot(&repo), before);
    // Relative destinations and names that are not a single component.
    assert_eq!(git.export_tree(&good, Path::new("rel/dst")).unwrap_err().code, Code::InvalidInput);
    assert_eq!(git.export_tree(&good, &out.join("..")).unwrap_err().code, Code::InvalidInput);
    assert_eq!(entries(&out), vec!["file", "via-link"]);
}

#[test]
fn interrupted_export_keeps_temp_and_never_creates_destination() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.write(&repo, ".gitattributes", b"*.k filter=killer\n");
    sb.write(&repo, "a.k", b"data\n");
    sb.write(&repo, "b.txt", b"b\n");
    sb.git(&repo, &["add", "-A"]);
    sb.git(&repo, &["commit", "-qm", "k"]);
    let tree = toid(&sb.tree_of(&repo, "HEAD"));
    sb.git(&repo, &["config", "filter.killer.smudge", "kill -9 $PPID"]);
    let git = sb.open(&repo);
    let out = sb.path("out");
    fs::create_dir(&out).unwrap();
    let dst = out.join("dst");
    let e = git.export_tree(&tree, &dst).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert!(!dst.exists());
    let t = temps(&out);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].starts_with(".export-dst-"));
    let kept = snapshot(&out.join(&t[0]));
    assert!(kept.iter().any(|(p, _)| p == Path::new("index")));

    sb.git(&repo, &["config", "--unset", "filter.killer.smudge"]);
    git.export_tree(&tree, &dst).unwrap();
    assert_eq!(fs::read(dst.join("b.txt")).unwrap(), b"b\n");
    assert_eq!(temps(&out), t, "the old temp stays; the new one is gone");
    assert_eq!(snapshot(&out.join(&t[0])), kept);
}

#[test]
fn concurrent_export_never_overwrites_destination() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.commit_file(&repo, "dir/f.txt", b"f\n", "more");
    let tree = toid(&sb.tree_of(&repo, "HEAD"));
    let git_a = sb.open(&repo);
    let git_b = sb.open(&repo);
    let out = sb.path("out");
    fs::create_dir(&out).unwrap();
    let dst = out.join("dst");
    let pause = git_a.pause_export_before_rename();
    let (ga, ta, da) = (git_a.clone(), tree.clone(), dst.clone());
    let a = std::thread::spawn(move || ga.export_tree(&ta, &da));
    pause.wait_until_paused();
    assert!(!dst.exists());
    git_b.export_tree(&tree, &dst).unwrap();
    let ino = fs::metadata(&dst).unwrap().ino();
    let b_snapshot = snapshot(&dst);
    pause.release();
    let e = a.join().unwrap().unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(fs::metadata(&dst).unwrap().ino(), ino);
    assert_eq!(snapshot(&dst), b_snapshot);
    let t = temps(&out);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(out.join(&t[0]).join("tree/dir/f.txt").is_file(), "A's complete temp is kept");
    assert_eq!(detail(&e, "temp").as_deref(), Some(out.join(&t[0]).to_str().unwrap()));
    // An export that starts after dst exists is refused and writes nothing.
    let before = entries(&out);
    assert_eq!(git_b.export_tree(&tree, &dst).unwrap_err().code, Code::Exists);
    assert_eq!(entries(&out), before);
}

#[test]
#[ignore = "slow: 100k files; run before release"]
fn export_large_tree_is_complete() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let b = blob(&sb, &repo, b"same content\n");
    let mut leaf = Vec::new();
    for i in 0..1000 {
        leaf.extend_from_slice(format!("100644 blob {b}\tf{i:04}.txt\0").as_bytes());
    }
    let leaf = sb.git_stdin(&repo, &["mktree", "-z"], &leaf);
    let mut top = Vec::new();
    for d in 0..100 {
        top.extend_from_slice(format!("040000 tree {leaf}\td{d:03}\0").as_bytes());
    }
    let top = sb.git_stdin(&repo, &["mktree", "-z"], &top);
    let n = sb.git(&repo, &["ls-tree", "-r", "--name-only", &top]).lines().count();
    assert_eq!(n, 100_000);
    let git = sb.open(&repo);
    let dst = sb.path("big");
    git.export_tree(&toid(&top), &dst).unwrap();
    let files = listing(&dst).values().filter(|v| matches!(v, Node::File { .. })).count();
    assert_eq!(files, n);
}

#[test]
fn export_is_durable_before_rename() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.write(&repo, "a/b/f", b"f\n");
    sb.write(&repo, "a/g", b"g\n");
    symlink("README.md", repo.join("link")).unwrap();
    sb.git(&repo, &["add", "-A"]);
    sb.git(&repo, &["commit", "-qm", "dirs"]);
    let tree = toid(&sb.tree_of(&repo, "HEAD"));
    let git: Git = sb.open(&repo);
    let out = sb.path("out");
    fs::create_dir(&out).unwrap();
    let dst = out.join("dst");
    git.take_sync_events();
    git.export_tree(&tree, &dst).unwrap();
    let ev = git.take_sync_events();
    let r = ev.iter().position(|e| matches!(e, SyncEvent::RenameExcl { .. })).expect("renamed");
    let SyncEvent::RenameExcl { from, to } = &ev[r] else { unreachable!() };
    assert_eq!(to, &dst);
    assert_eq!(from.file_name().unwrap(), "tree");
    let work = from.clone();
    assert!(work.parent().unwrap().file_name().unwrap().to_str().unwrap().starts_with(".export-dst-"));
    assert_eq!(work.parent().unwrap().parent().unwrap(), out);
    let before = &ev[..r];
    for rel in ["README.md", "a/b/f", "a/g", "a/b", "a", ""] {
        let p = if rel.is_empty() { work.clone() } else { work.join(rel) };
        assert!(before.contains(&SyncEvent::Fsync(p.clone())), "fsync of {} before rename: {ev:?}", p.display());
    }
    assert!(!before.contains(&SyncEvent::Fsync(work.join("link"))), "symlinks are not opened");
    let full = before.iter().position(|e| e == &SyncEvent::FullFsync(work.clone())).expect("full_fsync of T/tree");
    assert!(before[..full].iter().all(|e| matches!(e, SyncEvent::Fsync(_))));
    assert_eq!(before.iter().filter(|e| matches!(e, SyncEvent::FullFsync(_))).count(), 1);
    assert_eq!(&ev[r + 1..], &[SyncEvent::FullFsync(out.clone())]);
    assert_eq!(temps(&out), Vec::<String>::new());
}

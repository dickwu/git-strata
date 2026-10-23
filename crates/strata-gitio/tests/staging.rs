//! Task 8: `attach_staging`, `stage`, `commit_tree` (§5.6, §5.9, §5.10).

mod common;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use common::*;
use strata_gitio::Git;
use strata_types::{Code, CommitOid, RepoPath, TreeOid};

fn coid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}

fn toid(s: &str) -> TreeOid {
    TreeOid::try_from(s.trim().to_string()).unwrap()
}

fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn mkfifo(p: &Path) {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
}

const ROT13: &str = "tr A-Za-z N-ZA-Mn-za-m";

/// A publish staging tree: `head` exported to `<root>/st/tree` and
/// attached with its private git dir at `<root>/st/git`.
struct Staging {
    sb: Sandbox,
    main: PathBuf,
    git: Git,
    head: String,
    tree: PathBuf,
    gitdir: PathBuf,
}

impl Staging {
    fn ls(&self, tree: &str) -> String {
        self.sb.git(&self.main, &["ls-tree", "-r", tree])
    }
    fn index_bytes(&self) -> Vec<u8> {
        fs::read(self.gitdir.join("index")).unwrap()
    }
}

fn staging_with(prepare: impl FnOnce(&Sandbox, &Path)) -> Staging {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    prepare(&sb, &main);
    let git = sb.open(&main);
    let head = sb.head(&main);
    fs::create_dir(sb.path("st")).unwrap();
    let tree = sb.path("st/tree");
    let gitdir = sb.path("st/git");
    git.export_tree(&toid(&sb.tree_of(&main, &head)), &tree).unwrap();
    git.attach_staging(&tree, &gitdir, &coid(&head)).unwrap();
    Staging { sb, main, git, head, tree, gitdir }
}

fn main_state(sb: &Sandbox, main: &Path) -> (Vec<u8>, String, String, String) {
    (
        fs::read(main.join(".git/index")).unwrap(),
        fs::read_to_string(main.join(".git/HEAD")).unwrap(),
        sb.git(main, &["worktree", "list", "--porcelain"]),
        sb.git(main, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags", "refs/remotes"]),
    )
}

fn filters(sb: &Sandbox, main: &Path) {
    sb.git(main, &["config", "filter.rot.smudge", ROT13]);
    sb.git(main, &["config", "filter.rot.clean", ROT13]);
    sb.write(main, ".gitattributes", b"*.r filter=rot\n*.txt eol=crlf\n");
    sb.write(main, "secret.r", b"Hello World\n");
    sb.write(main, "notes.txt", b"one\ntwo\n");
    sb.write(main, "bin/run.sh", b"#!/bin/sh\necho hi\n");
    fs::set_permissions(main.join("bin/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("notes.txt", main.join("link")).unwrap();
    sb.git(main, &["add", "."]);
    sb.git(main, &["commit", "-q", "-m", "filters"]);
}

#[test]
fn index_writers_refuse_the_main_checkout() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let linked = sb.path("linked");
    sb.git(&main, &["worktree", "add", "-q", "--detach", linked.to_str().unwrap()]);
    let git = sb.open(&main);
    let head = coid(&sb.head(&main));
    let before = fs::read(main.join(".git/index")).unwrap();
    let invalid = |r: strata_gitio::Result<()>, what: &str| {
        let e = r.unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{what}: {e:?}");
    };
    // stage and push
    git.update_ref("refs/strata/publish/main", &head, None).unwrap();
    for t in [&main, &linked] {
        invalid(git.stage(t, &[rp("README.md")]).map(|_| ()), "stage");
        let r = git.push(t, "origin", "refs/strata/publish/main", "main", std::time::Duration::from_secs(30));
        invalid(r.map(|_| ()), "push");
    }
    // attach_staging: main / linked worktree as staging, gitdir in the common dir.
    let gd = sb.path("gd");
    invalid(git.attach_staging(&main, &gd, &head), "attach_staging main");
    invalid(git.attach_staging(&linked, &gd, &head), "attach_staging linked");
    assert!(!gd.exists());
    fs::create_dir(sb.path("st")).unwrap();
    let st = sb.path("st/tree");
    git.export_tree(&toid(&sb.tree_of(&main, "HEAD")), &st).unwrap();
    invalid(git.attach_staging(&st, &main.join(".git/strata-staging"), &head), "gitdir in common");
    invalid(git.attach_staging(&st, &main.join("sub-git"), &head), "gitdir in work tree");
    assert!(!main.join(".git/strata-staging").exists() && !main.join("sub-git").exists());
    assert!(!st.join(".git").exists());
    // ensure_shadow / attach_worktree inside the main checkout.
    invalid(git.ensure_shadow(&main.join("s.git")).map(|_| ()), "ensure_shadow");
    let shadow = git.ensure_shadow(&sb.path("s.git")).unwrap();
    let inner = main.join("inner-tree");
    fs::create_dir(&inner).unwrap();
    invalid(git.attach_worktree(&shadow, 1, &head, &inner, &sb.path("ws")).map(|_| ()), "tree_dir");
    let tmp = sb.path("tmp");
    git.export_tree(&toid(&sb.tree_of(&main, "HEAD")), &tmp).unwrap();
    invalid(git.attach_worktree(&shadow, 1, &head, &tmp, &main.join("ws")).map(|_| ()), "workspace");
    assert!(!tmp.join(".git").exists());
    assert_eq!(fs::read(main.join(".git/index")).unwrap(), before);
}

#[test]
fn attach_staging_commits_land_in_main_store() {
    let s = staging_with(|_, _| {});
    let sb = &s.sb;
    let before = main_state(sb, &s.main);
    // Layout.
    assert_eq!(fs::read(s.tree.join(".git")).unwrap(), format!("gitdir: {}\n", s.gitdir.display()).into_bytes());
    assert_eq!(fs::read_to_string(s.gitdir.join("HEAD")).unwrap(), format!("{}\n", s.head));
    assert_eq!(fs::read_to_string(s.gitdir.join("commondir")).unwrap(), format!("{}\n", s.git.common_dir().display()));
    assert_eq!(fs::read_to_string(s.gitdir.join("gitdir")).unwrap(), format!("{}/.git\n", s.tree.display()));
    // git sees a detached, clean checkout of head.
    assert_eq!(sb.git_output(&s.tree, &["symbolic-ref", "-q", "HEAD"]).status.code(), Some(1));
    assert_eq!(sb.git(&s.tree, &["rev-parse", "HEAD"]), s.head);
    assert_eq!(sb.git(&s.tree, &["status", "--porcelain"]), "");

    fs::write(s.tree.join("new.txt"), "new\n").unwrap();
    fs::write(s.tree.join("README.md"), "changed\n").unwrap();
    let t = s.git.stage(&s.tree, &[rp("new.txt"), rp("README.md")]).unwrap();
    let c = s.git.commit_tree(&t, &coid(&s.head), "publish\n").unwrap();
    assert_eq!(sb.git(&s.main, &["cat-file", "-t", c.as_str()]), "commit");
    assert_eq!(sb.git(&s.main, &["rev-parse", &format!("{}^{{tree}}", c.as_str())]), t.as_str());
    assert_eq!(sb.git(&s.main, &["cat-file", "-p", &format!("{}:new.txt", c.as_str())]), "new");
    s.git.update_ref("refs/strata/publish/main", &c, None).unwrap();
    assert_eq!(s.git.read_ref("refs/strata/publish/main").unwrap(), Some(c.clone()));
    assert!(sb.git(&s.main, &["fsck", "--connectivity-only", "--no-dangling"]).is_empty());
    assert_eq!(main_state(sb, &s.main), before);
    assert!(!sb.git(&s.main, &["worktree", "list"]).contains("st/tree"));
    assert_eq!(sb.git(&s.main, &["status", "--porcelain"]), "");

    // A strata workspace is not a staging tree.
    let shadow = s.git.ensure_shadow(&sb.path("s.git")).unwrap();
    let ws = sb.path("ws");
    let tmp = sb.path("tmp");
    s.git.export_tree(&toid(&sb.tree_of(&s.main, "HEAD")), &tmp).unwrap();
    s.git.attach_worktree(&shadow, 1, &coid(&s.head), &tmp, &ws).unwrap();
    fs::rename(&tmp, &ws).unwrap();
    let e = s.git.stage(&ws, &[rp("README.md")]).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");

    // Refusals: existing .git / gitdir, missing staging.
    let head = coid(&s.head);
    assert_eq!(s.git.attach_staging(&s.tree, &sb.path("st/git2"), &head).unwrap_err().code, Code::Exists);
    let t2 = sb.path("st/tree2");
    s.git.export_tree(&toid(&sb.tree_of(&s.main, "HEAD")), &t2).unwrap();
    assert_eq!(s.git.attach_staging(&t2, &s.gitdir, &head).unwrap_err().code, Code::Exists);
    assert!(!t2.join(".git").exists());
    assert_eq!(s.git.attach_staging(&sb.path("st/none"), &sb.path("st/g3"), &head).unwrap_err().code, Code::NoSuchPath);
    fs::write(sb.path("st/file"), "x").unwrap();
    assert_eq!(s.git.attach_staging(&sb.path("st/file"), &sb.path("st/g3"), &head).unwrap_err().code, Code::NotDirectory);
    assert_eq!(s.git.attach_staging(Path::new("st/tree2"), &sb.path("st/g3"), &head).unwrap_err().code, Code::InvalidInput);
    let missing = coid("0123456789012345678901234567890123456789");
    assert_eq!(s.git.attach_staging(&t2, &sb.path("st/g3"), &missing).unwrap_err().code, Code::NoSuchCommit);
    assert!(!sb.path("st/g3").exists());
    // A gitfile that is not ours / a damaged commondir: stage refuses.
    s.git.attach_staging(&t2, &sb.path("st/g3"), &head).unwrap();
    fs::write(sb.path("st/g3/commondir"), "/nowhere\n").unwrap();
    assert_eq!(s.git.stage(&t2, &[rp("README.md")]).unwrap_err().code, Code::InvalidInput);
    fs::remove_file(sb.path("st/g3/commondir")).unwrap();
    mkfifo(&sb.path("st/g3/commondir"));
    assert_eq!(s.git.stage(&t2, &[rp("README.md")]).unwrap_err().code, Code::InvalidInput);
}

#[test]
fn stage_after_export_roundtrips_to_same_tree() {
    let s = staging_with(filters);
    // The blob is the cleaned (rot13) form; the export is smudged back.
    assert_eq!(fs::read(s.tree.join("secret.r")).unwrap(), b"Hello World\n");
    let blob = s.sb.git(&s.main, &["rev-parse", &format!("{}:secret.r", s.head)]);
    assert_eq!(s.sb.git(&s.main, &["cat-file", "-p", &blob]), "Uryyb Jbeyq");
    assert_eq!(fs::read(s.tree.join("notes.txt")).unwrap(), b"one\r\ntwo\r\n");
    let orig = s.sb.tree_of(&s.main, &s.head);
    let all: Vec<RepoPath> = s.sb.git(&s.main, &["ls-tree", "-r", "--name-only", &orig]).lines().map(rp).collect();
    let t = s.git.stage(&s.tree, &all).unwrap();
    assert_eq!(t.as_str(), orig);
    let o = s
        .sb
        .cmd(&s.tree)
        .env("GIT_INDEX_FILE", s.gitdir.join("index"))
        .arg(format!("--git-dir={}", s.gitdir.display()))
        .arg("write-tree")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), orig);
}

#[test]
fn stage_applies_clean_filters() {
    let s = staging_with(filters);
    fs::write(s.tree.join("secret.r"), b"Uryyb Gurer\n").unwrap();
    fs::write(s.tree.join("notes.txt"), b"one\r\ntwo\r\nthree\r\n").unwrap();
    s.git.take_git_calls();
    let t = s.git.stage(&s.tree, &[rp("secret.r"), rp("notes.txt")]).unwrap();
    let calls = s.git.take_git_calls();
    let ui: Vec<_> = calls.iter().filter(|c| args_of(c).iter().any(|a| a == "update-index")).collect();
    assert!(!ui.is_empty());
    for c in ui {
        assert_eq!(env_of(c, "LC_ALL"), None, "{:?}", args_of(c));
        assert_eq!(env_of(c, "GIT_INDEX_FILE").map(PathBuf::from), Some(s.gitdir.join("index")));
    }
    let blob = |tree: &str, p: &str| s.sb.git(&s.main, &["rev-parse", &format!("{tree}:{p}")]);
    assert_eq!(s.sb.git(&s.main, &["cat-file", "-p", &blob(t.as_str(), "secret.r")]), "Hello There");
    let notes = s.sb.git_output(&s.main, &["cat-file", "-p", &blob(t.as_str(), "notes.txt")]).stdout;
    assert_eq!(notes, b"one\ntwo\nthree\n");
    // Same as `git add` of the same bytes in an ordinary checkout.
    fs::write(s.main.join("secret.r"), b"Uryyb Gurer\n").unwrap();
    fs::write(s.main.join("notes.txt"), b"one\r\ntwo\r\nthree\r\n").unwrap();
    s.sb.git(&s.main, &["add", "secret.r", "notes.txt"]);
    for p in ["secret.r", "notes.txt"] {
        assert_eq!(blob(t.as_str(), p), s.sb.git(&s.main, &["rev-parse", &format!(":{p}")]), "{p}");
    }
}

#[test]
fn stage_handles_deletions_and_type_transitions() {
    let s = staging_with(|sb, main| {
        sb.write(main, "del.txt", b"bye\n");
        sb.write(main, "a", b"file a\n");
        sb.write(main, "d/x", b"in d\n");
        sb.write(main, "f", b"plain f\n");
        symlink("README.md", main.join("l")).unwrap();
        sb.git(main, &["add", "."]);
        sb.git(main, &["commit", "-q", "-m", "shapes"]);
    });
    let human = s.sb.path("human");
    s.sb.git(&s.main, &["worktree", "add", "-q", "--detach", human.to_str().unwrap()]);
    for root in [&s.tree, &human] {
        fs::remove_file(root.join("del.txt")).unwrap();
        fs::remove_file(root.join("a")).unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/b"), "now a dir\n").unwrap();
        fs::remove_dir_all(root.join("d")).unwrap();
        fs::write(root.join("d"), "now a file\n").unwrap();
        fs::remove_file(root.join("f")).unwrap();
        symlink("README.md", root.join("f")).unwrap();
        fs::remove_file(root.join("l")).unwrap();
        fs::write(root.join("l"), "now regular\n").unwrap();
    }
    s.sb.git(&human, &["add", "-A"]);
    let want = s.sb.git(&human, &["write-tree"]);
    let batch: Vec<RepoPath> = ["l", "del.txt", "a", "a/b", "d", "d/x", "f", "a"].into_iter().map(rp).collect();
    let t = s.git.stage(&s.tree, &batch).unwrap();
    assert_eq!(s.ls(t.as_str()), s.ls(&want));
    assert_eq!(t.as_str(), want);
}

#[test]
fn stage_never_adds_unlisted_paths() {
    let s = staging_with(|sb, main| {
        sb.commit_file(main, ".gitignore", b"*.log\n", "ignore");
    });
    fs::create_dir(s.tree.join("a")).unwrap();
    for (p, b) in [("a/b", "b\n"), ("a/c", "c\n"), ("a/x.log", "log\n"), ("README.md", "edited\n")] {
        fs::write(s.tree.join(p), b).unwrap();
    }
    let orig = s.sb.tree_of(&s.main, &s.head);
    let t = s.git.stage(&s.tree, &[rp("a/b")]).unwrap();
    let got = s.ls(t.as_str());
    let before = s.ls(&orig);
    let extra: Vec<&str> = got.lines().filter(|l| !before.lines().any(|b| b == *l)).collect();
    assert_eq!(extra.len(), 1, "{got}");
    assert!(extra[0].ends_with("\ta/b"), "{got}");
    assert_eq!(got.lines().count(), before.lines().count() + 1);
    assert_eq!(
        s.sb.git(&s.main, &["rev-parse", &format!("{}:README.md", t.as_str())]),
        s.sb.git(&s.main, &["rev-parse", &format!("{orig}:README.md")])
    );
    // A batch naming an ignored file stages it (paths are explicit), dirs are never recursed.
    let t2 = s.git.stage(&s.tree, &[rp("a/x.log")]).unwrap();
    assert!(s.ls(t2.as_str()).contains("\ta/x.log"));
    assert!(!s.ls(t2.as_str()).contains("\ta/c"));
}

#[test]
fn stage_refuses_special_files_before_writing() {
    let s = staging_with(|_, _| {});
    fs::write(s.tree.join("README.md"), "changed\n").unwrap();
    mkfifo(&s.tree.join("pipe"));
    let before = s.index_bytes();
    s.git.take_git_calls();
    let e = s.git.stage(&s.tree, &[rp("README.md"), rp("pipe")]).unwrap_err();
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert_eq!(s.index_bytes(), before);
    assert!(s.git.take_git_calls().iter().all(|c| !args_of(c).iter().any(|a| a == "update-index")));
}

#[test]
fn commit_tree_creates_single_parent_commit() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let parent = sb.commit_file(&main, "x", b"x\n", "second");
    let git = sb.open(&main);
    let mut gc = fs::read_to_string(sb.home.join(".gitconfig")).unwrap();
    gc.push_str("[commit]\n\tgpgSign = true\n[gpg]\n\tprogram = /nonexistent/gpg\n");
    fs::write(sb.home.join(".gitconfig"), gc).unwrap();
    let refs = |sb: &Sandbox| sb.git(&main, &["for-each-ref"]);
    let (r0, h0) = (refs(&sb), fs::read(main.join(".git/HEAD")).unwrap());
    let tree = toid(&sb.tree_of(&main, "HEAD~1"));
    let msg = "subject line\n\n# not a comment\n  indented  \n\n\ntrailing blank lines\n\n";
    git.take_git_calls();
    let c = git.commit_tree(&tree, &coid(&parent), msg).unwrap();
    let raw = sb.git_output(&main, &["cat-file", "commit", c.as_str()]).stdout;
    let raw = String::from_utf8(raw).unwrap();
    let (hdr, body) = raw.split_once("\n\n").unwrap();
    assert_eq!(body, msg);
    let lines: Vec<&str> = hdr.lines().collect();
    assert_eq!(lines[0], format!("tree {}", tree.as_str()));
    assert_eq!(lines[1], format!("parent {parent}"));
    assert!(lines[2].starts_with("author Strata Test <test@example.com> "), "{hdr}");
    assert!(lines[3].starts_with("committer Strata Test <test@example.com> "), "{hdr}");
    assert_eq!(lines.len(), 4, "{hdr}");
    let calls = git.take_git_calls();
    let ct = find_call(&calls, "commit-tree").unwrap();
    let a = args_of(ct);
    assert!(a.windows(2).any(|w| w[0] == "-c" && w[1] == "user.useConfigOnly=true"), "{a:?}");
    assert_eq!(env_of(ct, "LC_ALL").as_deref(), Some("C"));
    assert_eq!(refs(&sb), r0);
    assert_eq!(fs::read(main.join(".git/HEAD")).unwrap(), h0);

    // Missing tree / parent.
    let zero_t = toid("0123456789012345678901234567890123456789");
    let e = git.commit_tree(&zero_t, &coid(&parent), "m").unwrap_err();
    assert_eq!(e.code, Code::NoSuchCommit, "{e:?}");
    let e = git.commit_tree(&tree, &coid("0123456789012345678901234567890123456789"), "m").unwrap_err();
    assert_eq!(e.code, Code::NoSuchCommit, "{e:?}");
    assert_eq!(refs(&sb), r0);
}

#[test]
fn commit_tree_without_identity_is_invalid_input() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let refs = sb.git(&main, &["for-each-ref"]);
    let tree = toid(&sb.tree_of(&main, "HEAD"));
    let parent = coid(&sb.head(&main));
    for (name, cfg) in [("no-email", "[user]\n\tname = Someone\n"), ("no-name", "[user]\n\temail = a@b.c\n")] {
        let home = sb.path(name);
        fs::create_dir(&home).unwrap();
        fs::write(home.join(".gitconfig"), cfg).unwrap();
        git.set_env("HOME", Some(home.as_os_str()));
        git.set_env("EMAIL", Some(OsStr::new("auto@example.com")));
        let e = git.commit_tree(&tree, &parent, "m\n").unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{name}: {e:?}");
        assert_eq!(reason(&e).as_deref(), Some("git-identity"), "{name}");
    }
    assert_eq!(sb.git(&main, &["for-each-ref"]), refs);
}

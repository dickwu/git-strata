mod common;

use std::fs;
use std::path::Path;

use common::*;
use strata_gitio::Git;
use strata_types::{Code, CommitOid, RepoPath, TreeOid};

fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}

fn toid(s: &str) -> TreeOid {
    TreeOid::try_from(s.trim().to_string()).unwrap()
}

fn set_ignorecase(sb: &Sandbox, repo: &Path, v: bool) {
    sb.git(repo, &["config", "core.ignorecase", if v { "true" } else { "false" }]);
}

fn blob(sb: &Sandbox, repo: &Path, content: &[u8]) -> String {
    sb.git_stdin(repo, &["hash-object", "-w", "--stdin"], content)
}

/// `git mktree -z` from (mode, type, oid, name) entries.
fn mktree(sb: &Sandbox, repo: &Path, entries: &[(&str, &str, &str, &[u8])]) -> String {
    let mut input = Vec::new();
    for (mode, kind, o, name) in entries {
        input.extend_from_slice(format!("{mode} {kind} {o}\t").as_bytes());
        input.extend_from_slice(name);
        input.push(0);
    }
    sb.git_stdin(repo, &["mktree", "-z"], &input)
}

fn commit_of(sb: &Sandbox, repo: &Path, tree: &str) -> CommitOid {
    oid(&sb.git(repo, &["commit-tree", tree, "-m", "t"]))
}

fn loose_object(repo: &Path, o: &str) -> std::path::PathBuf {
    repo.join(".git/objects").join(&o[..2]).join(&o[2..])
}

fn ls_tree_count(git: &Git) -> usize {
    git.take_git_calls().iter().filter(|c| args_of(c).iter().any(|a| a == "ls-tree")).count()
}

#[test]
fn replace_refs_do_not_change_tree_of() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let c1 = sb.head(&repo);
    let t1 = sb.tree_of(&repo, "HEAD");
    let c2 = sb.commit_file(&repo, "other.txt", b"other\n", "second");
    sb.git(&repo, &["replace", &c1, &c2]);
    assert_ne!(sb.tree_of(&repo, &c1), t1, "git itself honours the replacement");
    let git = sb.open(&repo);
    assert_eq!(git.tree_of(&oid(&c1)).unwrap().as_str(), t1);
}

#[test]
fn gitlink_in_base_tree_is_unsupported() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let head = sb.head(&repo);
    let b = blob(&sb, &repo, b"x\n");
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, b"a.txt"), ("160000", "commit", &head, b"sub")]);
    let c = commit_of(&sb, &repo, &t);
    let git = sb.open(&repo);
    let e = git.tree_of(&c).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
    assert_eq!(detail(&e, "path").as_deref(), Some("sub"));
    let e = git.tree_contains(&toid(&t), &rp("a.txt")).unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
}

#[test]
fn missing_blob_refused_before_export() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let b = blob(&sb, &repo, b"only here, about to vanish\n");
    let keep = blob(&sb, &repo, b"keep\n");
    let inner = mktree(&sb, &repo, &[("100644", "blob", &b, b"gone.txt")]);
    let t = mktree(&sb, &repo, &[("100644", "blob", &keep, b"keep.txt"), ("040000", "tree", &inner, b"dir")]);
    let c = commit_of(&sb, &repo, &t);
    fs::remove_file(loose_object(&repo, &b)).unwrap();
    let ls = sb.git_output(&repo, &["ls-tree", "-r", "-l", &t]);
    assert!(ls.status.success() && String::from_utf8_lossy(&ls.stdout).contains("BAD"));
    let git = sb.open(&repo);
    let e = git.tree_of(&c).unwrap_err();
    assert_eq!(e.code, Code::NoSuchCommit, "{e:?}");
    assert_eq!(detail(&e, "path").as_deref(), Some("dir/gone.txt"));

    // A missing subtree.
    let b2 = blob(&sb, &repo, b"in a subtree\n");
    let inner2 = mktree(&sb, &repo, &[("100644", "blob", &b2, b"f.txt")]);
    let t2 = mktree(&sb, &repo, &[("100644", "blob", &keep, b"keep.txt"), ("040000", "tree", &inner2, b"sub")]);
    let c2 = commit_of(&sb, &repo, &t2);
    fs::remove_file(loose_object(&repo, &inner2)).unwrap();
    let e = git.tree_of(&c2).unwrap_err();
    assert_eq!(e.code, Code::NoSuchCommit, "{e:?}");
    // And a commit that is not there at all.
    let e = git.tree_of(&oid("1234567890123456789012345678901234567890")).unwrap_err();
    assert_eq!(e.code, Code::NoSuchCommit);
}

#[test]
fn non_utf8_tree_path_is_not_text() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let b = blob(&sb, &repo, b"x\n");
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, b"ok.txt"), ("100644", "blob", &b, b"\xff.txt")]);
    let c = commit_of(&sb, &repo, &t);
    let git = sb.open(&repo);
    let e = git.tree_of(&c).unwrap_err();
    assert_eq!(e.code, Code::NotText, "{e:?}");
}

#[test]
fn colliding_paths_are_unsupported() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let b = blob(&sb, &repo, b"x\n");
    let git = sb.open(&repo);

    // ASCII case: refused only when the repository ignores case.
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, b"README"), ("100644", "blob", &b, b"readme")]);
    let c = commit_of(&sb, &repo, &t);
    set_ignorecase(&sb, &repo, true);
    let e = git.tree_of(&c).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
    let paths = e.detail.as_ref().unwrap()["paths"].as_array().unwrap().clone();
    let mut paths: Vec<&str> = paths.iter().map(|v| v.as_str().unwrap()).collect();
    paths.sort();
    assert_eq!(paths, vec!["README", "readme"]);
    set_ignorecase(&sb, &repo, false);
    assert_eq!(git.tree_of(&c).unwrap().as_str(), t);

    // NFC-equal names collide in any repository.
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, "e\u{301}.txt".as_bytes()), ("100644", "blob", &b, "\u{e9}.txt".as_bytes())]);
    let e = git.tree_of(&commit_of(&sb, &repo, &t)).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");

    // A name that is both a symlink and a directory.
    let inner = mktree(&sb, &repo, &[("100644", "blob", &b, b"b")]);
    let t = mktree(&sb, &repo, &[("120000", "blob", &b, b"a"), ("040000", "tree", &inner, b"a")]);
    let e = git.tree_of(&commit_of(&sb, &repo, &t)).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");

    // Unicode-only case differences are not gitio's to judge.
    set_ignorecase(&sb, &repo, true);
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, "\u{c9}.txt".as_bytes()), ("100644", "blob", &b, "\u{e9}.txt".as_bytes())]);
    assert_eq!(git.tree_of(&commit_of(&sb, &repo, &t)).unwrap().as_str(), t);
}

#[test]
fn dot_git_component_is_unsupported() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let b = blob(&sb, &repo, b"x\n");
    let git = sb.open(&repo);
    let cfg = mktree(&sb, &repo, &[("100644", "blob", &b, b"config")]);
    let t = mktree(&sb, &repo, &[("040000", "tree", &cfg, b".GIT")]);
    let e = git.tree_of(&commit_of(&sb, &repo, &t)).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
    assert_eq!(detail(&e, "path").as_deref(), Some(".GIT/config"));
    let x = mktree(&sb, &repo, &[("100644", "blob", &b, b"x")]);
    let dg = mktree(&sb, &repo, &[("040000", "tree", &x, b".git")]);
    let t = mktree(&sb, &repo, &[("040000", "tree", &dg, b"sub")]);
    let e = git.tree_of(&commit_of(&sb, &repo, &t)).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
    assert_eq!(detail(&e, "path").as_deref(), Some("sub/.git/x"));
}

#[test]
fn overlong_component_refused_before_writing() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let b = blob(&sb, &repo, b"x\n");
    let git = sb.open(&repo);
    let ok = "a".repeat(255);
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, ok.as_bytes())]);
    assert_eq!(git.tree_of(&commit_of(&sb, &repo, &t)).unwrap().as_str(), t);
    let long = "a".repeat(256);
    let t = mktree(&sb, &repo, &[("100644", "blob", &b, long.as_bytes())]);
    let e = git.tree_of(&commit_of(&sb, &repo, &t)).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
}

#[test]
fn tree_validation_is_cached_per_tree() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let c = oid(&sb.head(&repo));
    let t = sb.tree_of(&repo, "HEAD");
    let git = sb.open(&repo);
    git.take_git_calls();
    assert_eq!(git.tree_of(&c).unwrap().as_str(), t);
    let calls = git.take_git_calls();
    let ls = find_call(&calls, "ls-tree").expect("first tree_of validates");
    assert_eq!(env_of(ls, "LC_ALL").as_deref(), Some("C"));
    let rv = find_call(&calls, "rev-parse").expect("rev-parse ran");
    assert_eq!(env_of(rv, "LC_ALL").as_deref(), Some("C"));
    assert_eq!(git.tree_of(&c).unwrap().as_str(), t);
    assert_eq!(ls_tree_count(&git), 0);
    let other = oid(&sb.git(&repo, &["commit-tree", &t, "-p", c.as_str(), "-m", "same tree"]));
    assert_eq!(git.tree_of(&other).unwrap().as_str(), t);
    assert_eq!(ls_tree_count(&git), 0);
    // Refusals are remembered too.
    let b = blob(&sb, &repo, b"x\n");
    let bad = mktree(&sb, &repo, &[("160000", "commit", c.as_str(), b"sub"), ("100644", "blob", &b, b"f")]);
    let bc = commit_of(&sb, &repo, &bad);
    assert_eq!(git.tree_of(&bc).unwrap_err().code, Code::Unsupported);
    git.take_git_calls();
    assert_eq!(git.tree_of(&bc).unwrap_err().code, Code::Unsupported);
    assert_eq!(ls_tree_count(&git), 0);
}

#[test]
fn tree_contains_answers_from_the_validated_tree() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    set_ignorecase(&sb, &repo, false);
    sb.write(&repo, "dir/b.txt", b"b\n");
    std::os::unix::fs::symlink("README.md", repo.join("link")).unwrap();
    sb.git(&repo, &["add", "dir/b.txt", "link"]);
    sb.git(&repo, &["commit", "-qm", "more"]);
    let t = toid(&sb.tree_of(&repo, "HEAD"));
    let git = sb.open(&repo);
    for (p, want) in [
        ("README.md", true),
        ("dir/b.txt", true),
        ("link", true),
        ("dir", true),
        ("dir/x", false),
        ("nope", false),
        ("readme.md", false),
        ("DIR", false),
        ("link/x", false),
    ] {
        assert_eq!(git.tree_contains(&t, &rp(p)).unwrap(), want, "{p}");
    }
    git.take_git_calls();
    assert!(git.tree_contains(&t, &rp("dir")).unwrap());
    assert_eq!(ls_tree_count(&git), 0);

    set_ignorecase(&sb, &repo, true);
    for (p, want) in [("readme.md", true), ("DIR", true), ("Dir/B.TXT", true), ("LINK", true), ("nope", false)] {
        assert_eq!(git.tree_contains(&t, &rp(p)).unwrap(), want, "{p}");
    }

    let head = sb.head(&repo);
    let b = blob(&sb, &repo, b"x\n");
    let bad = mktree(&sb, &repo, &[("160000", "commit", &head, b"sub"), ("100644", "blob", &b, b"f")]);
    let e = git.tree_contains(&toid(&bad), &rp("f")).unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert_eq!(e, git.tree_of(&commit_of(&sb, &repo, &bad)).unwrap_err());
}

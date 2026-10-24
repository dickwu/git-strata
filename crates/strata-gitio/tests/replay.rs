//! `Git::replay_commits` (daemon.md D18).

mod common;

use std::fs;

use common::*;
use strata_types::{Code, CommitOid};

fn oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

/// main: base a; branch "chain": a → c1 (x.txt) → c2 (y.txt); branch
/// "origin": a → o1 (README.md changed). Returns (repo, a, c, o).
fn diverged(sb: &Sandbox) -> (std::path::PathBuf, String, String, String) {
    let repo = sb.init_repo("r");
    let a = sb.head(&repo);
    sb.git(&repo, &["checkout", "-q", "-b", "chain"]);
    sb.commit_file(&repo, "x.txt", b"x\n", "c1 adds x");
    let c = sb.commit_file(&repo, "y.txt", b"y\n", "c2 adds y\n\nbody line");
    sb.git(&repo, &["checkout", "-q", "-b", "origin", &a]);
    let o = sb.commit_file(&repo, "README.md", b"hello\nfrom origin\n", "o1");
    (repo, a, c, o)
}

fn refs(sb: &Sandbox, repo: &std::path::Path) -> String {
    sb.git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

#[test]
fn replays_range_oldest_first_onto_onto() {
    let sb = Sandbox::new();
    let (repo, a, c, o) = diverged(&sb);
    let g = sb.open(&repo);
    let before = refs(&sb, &repo);
    let tip = g.replay_commits(&oid(&a), &oid(&c), &oid(&o)).unwrap();
    assert_eq!(refs(&sb, &repo), before, "no ref moved");
    let log = sb.git(&repo, &["log", "--format=%s", &format!("{o}..{}", tip.as_str())]);
    assert_eq!(log, "c2 adds y\nc1 adds x");
    assert_eq!(sb.git(&repo, &["rev-parse", &format!("{}~2", tip.as_str())]), o);
    assert_eq!(sb.git(&repo, &["log", "-1", "--format=%B", tip.as_str()]), "c2 adds y\n\nbody line");
    let dir = sb.path("out");
    sb.archive_tree_into(&repo, tip.as_str(), &dir);
    assert_eq!(fs::read(dir.join("README.md")).unwrap(), b"hello\nfrom origin\n");
    assert_eq!(fs::read(dir.join("x.txt")).unwrap(), b"x\n");
    assert_eq!(fs::read(dir.join("y.txt")).unwrap(), b"y\n");
}

#[test]
fn range_excludes_base_and_empty_range_is_onto() {
    let sb = Sandbox::new();
    let (repo, a, c, o) = diverged(&sb);
    let g = sb.open(&repo);
    // exclusive_base = c1: only c2 is replayed.
    let c1 = sb.git(&repo, &["rev-parse", &format!("{c}~1")]);
    let tip = g.replay_commits(&oid(&c1), &oid(&c), &oid(&o)).unwrap();
    assert_eq!(sb.git(&repo, &["log", "--format=%s", &format!("{o}..{}", tip.as_str())]), "c2 adds y");
    assert_eq!(g.replay_commits(&oid(&c), &oid(&c), &oid(&o)).unwrap(), oid(&o));
    let _ = a;
}

#[test]
fn merge_commit_becomes_single_parent() {
    let sb = Sandbox::new();
    let (repo, a, _c, o) = diverged(&sb);
    sb.git(&repo, &["checkout", "-q", "-b", "side", &a]);
    sb.commit_file(&repo, "side.txt", b"s\n", "side");
    sb.git(&repo, &["checkout", "-q", "chain"]);
    sb.git(&repo, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    let m = sb.head(&repo);
    let g = sb.open(&repo);
    let tip = g.replay_commits(&oid(&a), &oid(&m), &oid(&o)).unwrap();
    let log = sb.git(&repo, &["log", "--format=%s %P", &format!("{o}..{}", tip.as_str())]);
    assert_eq!(log.lines().count(), 3, "first-parent only: c1, c2, merge: {log}");
    for l in log.lines() {
        assert_eq!(l.rsplit(' ').next().unwrap().len(), 40, "single parent: {l}");
    }
    let dir = sb.path("out");
    sb.archive_tree_into(&repo, tip.as_str(), &dir);
    assert_eq!(fs::read(dir.join("side.txt")).unwrap(), b"s\n");
}

#[test]
fn conflict_writes_no_ref() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("r");
    let a = sb.head(&repo);
    sb.git(&repo, &["checkout", "-q", "-b", "chain"]);
    let c = sb.commit_file(&repo, "README.md", b"chain\n", "chain edit");
    sb.git(&repo, &["checkout", "-q", "-b", "origin", &a]);
    let o = sb.commit_file(&repo, "README.md", b"origin\n", "origin edit");
    let g = sb.open(&repo);
    let before = refs(&sb, &repo);
    let e = g.replay_commits(&oid(&a), &oid(&c), &oid(&o)).unwrap_err();
    assert_eq!(e.code, Code::Conflict, "{e:?}");
    assert_eq!(refs(&sb, &repo), before);
}

#[test]
fn identity_from_config_only() {
    let sb = Sandbox::new();
    let (repo, a, c, o) = diverged(&sb);
    fs::write(sb.home.join(".gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
    let g = sb.open(&repo);
    let e = g.replay_commits(&oid(&a), &oid(&c), &oid(&o)).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(reason(&e).as_deref(), Some("git-identity"));
}

#[test]
fn never_pushes_or_writes_refs() {
    let sb = Sandbox::new();
    let (repo, a, c, o) = diverged(&sb);
    let g = sb.open(&repo);
    g.take_git_calls();
    g.replay_commits(&oid(&a), &oid(&c), &oid(&o)).unwrap();
    for call in g.take_git_calls() {
        let args = args_of(&call);
        for bad in ["push", "update-ref", "send-pack", "symbolic-ref", "--force"] {
            assert!(!args.iter().any(|x| x == bad), "{args:?}");
        }
    }
}

#[test]
fn commits_between_lists_the_range_newest_first() {
    let sb = Sandbox::new();
    let (repo, a, c, o) = diverged(&sb);
    let g = sb.open(&repo);
    let before = refs(&sb, &repo);
    let (v, more) = g.commits_between(&oid(&a), &oid(&c), 10).unwrap();
    assert!(!more);
    assert_eq!(v.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(), vec!["c2 adds y", "c1 adds x"]);
    assert_eq!(v[0].0, oid(&c));
    let (v, more) = g.commits_between(&oid(&a), &oid(&c), 1).unwrap();
    assert!(more);
    assert_eq!(v.len(), 1);
    assert!(g.commits_between(&oid(&c), &oid(&c), 10).unwrap().0.is_empty());
    // o is not in a..c: only what c adds over o.
    assert_eq!(g.commits_between(&oid(&o), &oid(&c), 10).unwrap().0.len(), 2);
    assert_eq!(refs(&sb, &repo), before, "read-only");
}

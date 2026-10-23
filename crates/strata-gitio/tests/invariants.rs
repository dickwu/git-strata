//! Task 11: the cross-cutting invariant of §9 (invariants 2, 3, 5): every
//! gitio API leaves the main checkout alone except `objects/` and
//! `refs/strata/*`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use strata_types::{CommitOid, PushOutcome, RepoPath, TreeOid};

fn coid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}

fn toid(s: &str) -> TreeOid {
    TreeOid::try_from(s.trim().to_string()).unwrap()
}

fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

/// Everything under the main checkout (work tree and `.git`), minus the
/// two places gitio may write.
fn main_snapshot(main: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    snapshot(main)
        .into_iter()
        .filter(|(p, _)| !p.starts_with(".git/objects") && !p.starts_with(".git/refs/strata"))
        .collect()
}

/// Every path under `dir`, without reading any (a daemon socket would not
/// read).
fn names(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if fs::symlink_metadata(&p).unwrap().is_dir() {
            out.extend(names(&p));
        }
        out.push(p);
    }
    out
}

fn diff_paths(a: &[(PathBuf, Vec<u8>)], b: &[(PathBuf, Vec<u8>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (p, bytes) in a {
        match b.iter().find(|(q, _)| q == p) {
            None => out.push(format!("removed {}", p.display())),
            Some((_, c)) if c != bytes => out.push(format!("changed {}", p.display())),
            _ => {}
        }
    }
    for (p, _) in b {
        if !a.iter().any(|(q, _)| q == p) {
            out.push(format!("added {}", p.display()));
        }
    }
    out
}

#[test]
fn main_checkout_untouched_except_strata_refs() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.commit_file(&main, ".gitignore", b"*.log\n", "ignore logs");
    sb.commit_file(&main, "src/a.txt", b"a\n", "add a");
    sb.bare_remote(&main, "remote.git");
    sb.git(&main, &["fetch", "-q", "origin"]);
    sb.git(&main, &["tag", "v1"]);
    sb.git(&main, &["stash", "list"]);
    // The main checkout is dirty: a staged change, an unstaged change and
    // untracked and ignored files.
    sb.write(&main, "src/a.txt", b"staged\n");
    sb.git(&main, &["add", "src/a.txt"]);
    sb.write(&main, "src/a.txt", b"unstaged\n");
    sb.write(&main, "untracked.txt", b"u\n");
    sb.write(&main, "debug.log", b"l\n");
    let head = sb.head(&main);
    let tree = toid(&sb.tree_of(&main, "HEAD"));
    // Turn on fsmonitor and the split index for every git that reads the
    // user config, after the fixture's own git calls.
    sb.git(&sb.root, &["config", "--global", "core.fsmonitor", "true"]);
    sb.git(&sb.root, &["config", "--global", "core.splitIndex", "true"]);

    let before = main_snapshot(&main);
    let objects_before = snapshot(&main.join(".git/objects")).len();
    assert!(before.iter().any(|(p, _)| p == Path::new(".git/index")));

    // The per-workspace APIs.
    let git = sb.open(&main);
    let tip = git.fetch("origin", "main").unwrap();
    assert_eq!(tip, coid(&head));
    assert_eq!(git.tree_of(&coid(&head)).unwrap(), tree);
    assert!(git.tree_contains(&tree, &rp("src/a.txt")).unwrap());
    let exp = sb.path("export");
    git.export_tree(&tree, &exp).unwrap();
    assert!(git.is_ignored_in_export(&exp, &tree, &rp("x.log")).unwrap());
    let shadow = git.ensure_shadow(&sb.path("s.git")).unwrap();
    let ws = sb.path("ws");
    let tmp = sb.path("tmp-1");
    git.export_tree(&tree, &tmp).unwrap();
    git.attach_worktree(&shadow, 1, &coid(&head), &tmp, &ws).unwrap();
    fs::rename(&tmp, &ws).unwrap();
    assert_eq!(git.shadow_drift(&shadow, &ws).unwrap(), vec![]);
    assert!(git.read_index(&ws).unwrap().is_tracked(&rp("src/a.txt")));
    assert!(git.is_ignored(&ws, &rp("y.log")).unwrap());
    assert!(!git.is_ignored(&ws, &rp("src/a.txt")).unwrap());

    // The publish APIs, on a staging tree elsewhere.
    fs::create_dir(sb.path("st")).unwrap();
    let staging = sb.path("st/tree");
    git.export_tree(&tree, &staging).unwrap();
    git.attach_staging(&staging, &sb.path("st/git"), &coid(&head)).unwrap();
    fs::write(staging.join("src/a.txt"), b"published\n").unwrap();
    let new_tree = git.stage(&staging, &[rp("src/a.txt")]).unwrap();
    let commit = git.commit_tree(&new_tree, &coid(&head), "publish").unwrap();
    git.update_ref("refs/strata/publish/main", &commit, None).unwrap();
    let out = git.push(&staging, "origin", "refs/strata/publish/main", "main", Duration::from_secs(120)).unwrap();
    assert_eq!(out, PushOutcome::Pushed);

    // No fsmonitor socket or shared index appeared; only objects/ and
    // refs/strata/* changed.
    for p in names(&main) {
        let n = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!n.starts_with("fsmonitor--daemon") && !n.starts_with("sharedindex."), "{}", p.display());
    }
    let after = main_snapshot(&main);
    let changes = diff_paths(&before, &after);
    assert!(changes.is_empty(), "main checkout changed outside objects/ and refs/strata/: {changes:#?}");
    assert!(snapshot(&main.join(".git/objects")).len() > objects_before, "the publish wrote no objects?");
    let strata = sb.git(&main, &["for-each-ref", "--format=%(refname)", "refs/strata"]);
    assert!(strata.contains("refs/strata/publish/main") && strata.contains("refs/strata/remotes/origin/main"), "{strata}");
    // No fsmonitor daemon was left watching the main checkout.
    let st = sb.cmd(&main).args(["fsmonitor--daemon", "status"]).output().unwrap();
    assert!(!st.status.success(), "fsmonitor daemon running: {}", String::from_utf8_lossy(&st.stdout));
    // The human's own view: staged and unstaged changes are as they were.
    let o = sb.cmd(&main).args(["-c", "core.fsmonitor=false", "diff", "--cached", "--name-only"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "src/a.txt\n");
}

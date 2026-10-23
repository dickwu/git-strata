mod common;

use std::path::Path;

use common::*;
use strata_types::Code;

#[test]
fn command_sets_safety_flags() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let git = sb.open(&repo);
    let calls = git.take_git_calls();
    assert!(!calls.is_empty());
    let mut saw_rev_parse = false;
    for c in &calls {
        let a = args_of(c);
        assert_eq!(a[0], "--no-replace-objects", "{a:?}");
        assert!(a.windows(2).any(|w| w[0] == "-c" && w[1] == "core.fsmonitor=false"), "{a:?}");
        assert_eq!(env_of(c, "GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(env_of(c, "GIT_NO_LAZY_FETCH").as_deref(), Some("1"));
        assert_eq!(env_of(c, "GIT_OPTIONAL_LOCKS").as_deref(), Some("0"));
        assert!(!c.stdin_piped);
        if a.iter().any(|x| x == "rev-parse") {
            saw_rev_parse = true;
            assert_eq!(env_of(c, "LC_ALL").as_deref(), Some("C"));
        }
    }
    assert!(saw_rev_parse);
    assert!(git.take_git_calls().is_empty());
}

#[test]
fn open_resolves_repo_id_from_main_checkout_only() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let git = sb.open(&repo);
    let expected = std::fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(git.common_dir(), expected);
    assert_eq!(git.repo_id(), strata_types::RepoId::new(expected.clone()));

    let wt = sb.path("linked");
    sb.git(&repo, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    assert_eq!(sb.open(&wt).repo_id(), git.repo_id());

    let alias = sb.path("alias");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();
    assert_eq!(sb.open(&alias).repo_id(), git.repo_id());

    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let e = sb.try_open(&repo.join("sub")).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);

    let shadow = git.ensure_shadow(&sb.path("shadow.git")).unwrap();
    let ws = sb.path("ws");
    let tmp = sb.path("ws-tmp");
    let head = sb.head(&repo);
    let tree = strata_types::TreeOid::try_from(sb.tree_of(&repo, "HEAD")).unwrap();
    git.export_tree(&tree, &tmp).unwrap();
    let commit = strata_types::CommitOid::try_from(head).unwrap();
    git.attach_worktree(&shadow, 1, &commit, &tmp, &ws).unwrap();
    std::fs::rename(&tmp, &ws).unwrap();
    let e = sb.try_open(&ws).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e).as_deref(), Some("strata-workspace"));
    assert!(!format!("{e:?}").contains("shadow.git"), "{e:?}");
}

#[test]
fn open_refuses_non_repository_bare_and_missing() {
    let sb = Sandbox::new();
    let plain = sb.path("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(sb.try_open(&plain).unwrap_err().code, Code::InvalidInput);

    let bare = sb.path("bare.git");
    sb.git(&sb.root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    assert_eq!(sb.try_open(&bare).unwrap_err().code, Code::InvalidInput);

    assert_eq!(sb.try_open(Path::new(&sb.path("missing"))).unwrap_err().code, Code::NoSuchPath);
}

#[test]
fn sha256_repository_is_unsupported() {
    let sb = Sandbox::new();
    let repo = sb.init_repo_with("sha256", &["--object-format=sha256"]);
    assert_eq!(sb.try_open(&repo).unwrap_err().code, Code::Unsupported);
    let ok = sb.init_repo("sha1");
    sb.open(&ok);
}

#[test]
fn reftable_repository_is_unsupported() {
    let sb = Sandbox::new();
    let repo = sb.init_repo_with("reftable", &["--ref-format=reftable"]);
    assert_eq!(sb.try_open(&repo).unwrap_err().code, Code::Unsupported);
    let ok = sb.init_repo_with("files", &["--ref-format=files"]);
    sb.open(&ok);
}

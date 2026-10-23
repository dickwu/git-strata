mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use strata_gitio::remote_ref;
use strata_types::{Code, CommitOid};

fn oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}

fn rev(sb: &Sandbox, repo: &Path, r: &str) -> Option<String> {
    let o = sb.git_output(repo, &["rev-parse", "--verify", "--quiet", r]);
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn empty_commit(sb: &Sandbox, repo: &Path, msg: &str) -> String {
    sb.git(repo, &["commit", "-q", "--allow-empty", "-m", msg]);
    sb.head(repo)
}

#[test]
fn option_like_arguments_are_refused_not_executed() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.bare_remote(&repo, "remote.git");
    let git = sb.open(&repo);
    git.take_git_calls();
    let f = sb.path("pwned");
    let evil = format!("--upload-pack=touch {}", f.display());
    let e = git.fetch(&evil, "main").unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    let e = git.fetch("origin", "-x").unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    for bad in [".hidden", "a/b", "a b", ""] {
        assert_eq!(git.fetch(bad, "main").unwrap_err().code, Code::InvalidInput, "{bad}");
    }
    for bad in ["a..b", "x:y", "x*", "@{u}", ""] {
        assert_eq!(git.fetch("origin", bad).unwrap_err().code, Code::InvalidInput, "{bad}");
    }
    assert!(!f.exists());
    assert!(find_call(&git.take_git_calls(), "fetch").is_none());
    // A revision is only accepted as a 40-hex commit id.
    let e = CommitOid::try_from("--help".to_string()).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(git.read_ref("--help").unwrap_err().code, Code::InvalidInput);
    assert!(!f.exists());
}

#[test]
fn fetch_writes_only_the_strata_tracking_ref() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let bare = sb.bare_remote(&repo, "remote.git");
    sb.git(&repo, &["fetch", "-q", "origin"]);
    let fetch_head = fs::read(repo.join(".git/FETCH_HEAD")).unwrap();
    let tracking = rev(&sb, &repo, "refs/remotes/origin/main").unwrap();
    let tags_before = sb.git(&repo, &["tag", "-l"]);

    // The remote moves on and gains a tag on its new tip.
    let other = sb.path("other");
    sb.git(&sb.path(""), &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
    let tip = sb.commit_file(&other, "new.txt", b"new\n", "remote work");
    sb.git(&other, &["tag", "v9"]);
    sb.git(&other, &["push", "-q", "origin", "main", "v9"]);

    let git = sb.open(&repo);
    git.take_git_calls();
    let got = git.fetch("origin", "main").unwrap();
    assert_eq!(got.as_str(), tip);
    assert_eq!(remote_ref("origin", "main"), "refs/strata/remotes/origin/main");
    assert_eq!(rev(&sb, &repo, "refs/strata/remotes/origin/main").as_deref(), Some(tip.as_str()));
    assert_eq!(git.read_ref("refs/strata/remotes/origin/main").unwrap().unwrap().as_str(), tip);
    assert_eq!(rev(&sb, &repo, "refs/remotes/origin/main").unwrap(), tracking);
    assert_eq!(fs::read(repo.join(".git/FETCH_HEAD")).unwrap(), fetch_head);
    assert_eq!(sb.git(&repo, &["tag", "-l"]), tags_before);
    assert!(rev(&sb, &repo, "refs/tags/v9").is_none());

    let calls = git.take_git_calls();
    let call = find_call(&calls, "fetch").expect("fetch ran");
    assert_eq!(env_of(call, "LC_ALL").as_deref(), Some("C"));
    assert!(!call.stdin_piped);
    let a = args_of(call);
    for flag in ["--no-tags", "--no-write-fetch-head", "--refmap=", "--end-of-options"] {
        assert!(a.iter().any(|x| x == flag), "{flag} missing: {a:?}");
    }
}

#[test]
fn fetch_missing_branch_is_no_such_branch() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.bare_remote(&repo, "remote.git");
    let empty = sb.path("empty.git");
    sb.git(&sb.path(""), &["init", "-q", "--bare", empty.to_str().unwrap()]);
    sb.git(&repo, &["remote", "add", "empty", empty.to_str().unwrap()]);
    let git = sb.open(&repo);
    let e = git.fetch("origin", "nope").unwrap_err();
    assert_eq!(e.code, Code::NoSuchBranch, "{e:?}");
    let e = git.fetch("empty", "main").unwrap_err();
    assert_eq!(e.code, Code::NoSuchBranch, "{e:?}");
    assert!(git.read_ref("refs/strata/remotes/empty/main").unwrap().is_none());
    // A remote that is not configured is the caller's mistake.
    let e = git.fetch("nosuch", "main").unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
}

#[test]
fn fetch_timeout_is_unknown_and_moves_nothing() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let bare = sb.bare_remote(&repo, "remote.git");
    let git = sb.open(&repo);
    let first = git.fetch("origin", "main").unwrap();

    let other = sb.path("other");
    sb.git(&sb.path(""), &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
    sb.commit_file(&other, "new.txt", b"new\n", "remote work");
    sb.git(&other, &["push", "-q", "origin", "main"]);

    let pidfile = sb.path("upload-pack.pid");
    let script = sb.path("hang-upload-pack");
    fs::write(&script, format!("#!/bin/sh\necho $$ > '{}'\nsleep 60\n", pidfile.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    sb.git(&repo, &["config", "remote.origin.uploadpack", script.to_str().unwrap()]);

    git.set_fetch_timeout(Duration::from_secs(2));
    let t0 = Instant::now();
    let e = git.fetch("origin", "main").unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert!(t0.elapsed() < Duration::from_secs(20));
    assert_eq!(git.read_ref("refs/strata/remotes/origin/main").unwrap(), Some(first));

    let pid: i32 = fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // SAFETY: signal 0 only probes for existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if !alive {
            break;
        }
        assert!(Instant::now() < deadline, "fake upload-pack {pid} survived");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn ancestry_answers_come_from_git() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let base = sb.head(&repo);
    let a1 = empty_commit(&sb, &repo, "a1");
    sb.git(&repo, &["checkout", "-q", "-b", "side", &base]);
    let b1 = empty_commit(&sb, &repo, "b1");
    sb.git(&repo, &["merge", "-q", "--no-ff", "--no-edit", &a1]);
    let m = sb.head(&repo);
    // Criss-cross: two best common ancestors.
    sb.git(&repo, &["checkout", "-q", "-b", "x", &a1]);
    let x1 = empty_commit(&sb, &repo, "x1");
    sb.git(&repo, &["checkout", "-q", "-b", "y", &b1]);
    let y1 = empty_commit(&sb, &repo, "y1");
    sb.git(&repo, &["checkout", "-q", "x"]);
    sb.git(&repo, &["merge", "-q", "--no-ff", "--no-edit", &y1]);
    let x2 = sb.head(&repo);
    sb.git(&repo, &["checkout", "-q", "y"]);
    sb.git(&repo, &["merge", "-q", "--no-ff", "--no-edit", &x1]);
    let y2 = sb.head(&repo);
    // Unrelated history.
    sb.git(&repo, &["checkout", "-q", "--orphan", "lone"]);
    let lone = empty_commit(&sb, &repo, "lone");

    let git = sb.open(&repo);
    let all = [&base, &a1, &b1, &m, &x1, &y1, &x2, &y2, &lone];
    for p in all {
        for q in all {
            let o = sb.git_output(&repo, &["merge-base", "--is-ancestor", p, q]);
            let want = o.status.code() == Some(0);
            assert_eq!(git.is_ancestor(&oid(p), &oid(q)).unwrap(), want, "{p} {q}");
            let o = sb.git_output(&repo, &["merge-base", p, q]);
            let want = o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let got = git.merge_base(&oid(p), &oid(q)).unwrap().map(|c| c.as_str().to_string());
            assert_eq!(got, want, "{p} {q}");
        }
    }
    assert!(git.merge_base(&oid(&base), &oid(&lone)).unwrap().is_none());
    let both = sb.git(&repo, &["merge-base", "--all", &x2, &y2]);
    assert_eq!(both.lines().count(), 2);
    let missing = oid("1234567890123456789012345678901234567890");
    assert_eq!(git.is_ancestor(&missing, &oid(&base)).unwrap_err().code, Code::NoSuchCommit);
    assert_eq!(git.merge_base(&oid(&base), &missing).unwrap_err().code, Code::NoSuchCommit);
}

#[test]
fn read_ref_absent_is_none() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let git = sb.open(&repo);
    assert!(git.read_ref("refs/strata/nothing/here").unwrap().is_none());
    assert!(git.read_ref("refs/strata/remotes/origin/main").unwrap().is_none());
}

fn two_commits(sb: &Sandbox, repo: &Path) -> (CommitOid, CommitOid) {
    let c1 = sb.head(repo);
    let c2 = empty_commit(sb, repo, "second");
    (oid(&c1), oid(&c2))
}

#[test]
fn update_ref_cas_refuses_moved_ref() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let (c1, c2) = two_commits(&sb, &repo);
    let git = sb.open(&repo);
    let name = "refs/strata/sessions/s1/base";
    git.update_ref(name, &c1, None).unwrap();
    git.take_git_calls();
    let e = git.update_ref(name, &c2, Some(&c2)).unwrap_err();
    assert_eq!(e.code, Code::HeadMoved, "{e:?}");
    assert_eq!(detail(&e, "expected").as_deref(), Some(c2.as_str()));
    assert_eq!(detail(&e, "actual").as_deref(), Some(c1.as_str()));
    assert_eq!(detail(&e, "ref").as_deref(), Some(name));
    assert_eq!(git.read_ref(name).unwrap(), Some(c1.clone()));
    let calls = git.take_git_calls();
    let call = find_call(&calls, "update-ref").expect("update-ref ran");
    assert_eq!(env_of(call, "LC_ALL").as_deref(), Some("C"));
    git.update_ref(name, &c2, Some(&c1)).unwrap();
    assert_eq!(git.read_ref(name).unwrap(), Some(c2));
}

#[test]
fn update_ref_create_requires_absent() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let (c1, c2) = two_commits(&sb, &repo);
    let git = sb.open(&repo);
    let name = "refs/strata/remotes/origin/main";
    git.update_ref(name, &c1, None).unwrap();
    assert_eq!(git.read_ref(name).unwrap(), Some(c1.clone()));
    let e = git.update_ref(name, &c2, None).unwrap_err();
    assert_eq!(e.code, Code::HeadMoved, "{e:?}");
    assert_eq!(detail(&e, "actual").as_deref(), Some(c1.as_str()));
    assert_eq!(git.read_ref(name).unwrap(), Some(c1));
}

#[test]
fn refs_outside_strata_namespace_are_refused() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    sb.bare_remote(&repo, "remote.git");
    let (c1, c2) = two_commits(&sb, &repo);
    let git = sb.open(&repo);
    let before = sb.git(&repo, &["for-each-ref"]);
    let head = sb.head(&repo);
    git.take_git_calls();
    for name in ["refs/heads/main", "refs/remotes/origin/main", "HEAD", "refs/strata", "refs/strata/", "refs/strata/../heads/main"] {
        assert_eq!(git.read_ref(name).unwrap_err().code, Code::InvalidInput, "{name}");
        assert_eq!(git.update_ref(name, &c1, Some(&c2)).unwrap_err().code, Code::InvalidInput, "{name}");
        assert_eq!(git.update_ref(name, &c1, None).unwrap_err().code, Code::InvalidInput, "{name}");
        assert_eq!(git.delete_ref(name, &c2).unwrap_err().code, Code::InvalidInput, "{name}");
    }
    assert!(git.take_git_calls().is_empty());
    assert_eq!(sb.git(&repo, &["for-each-ref"]), before);
    assert_eq!(sb.head(&repo), head);
}

#[test]
fn delete_ref_is_compare_and_swap() {
    let sb = Sandbox::new();
    let repo = sb.init_repo("main");
    let (c1, c2) = two_commits(&sb, &repo);
    let git = sb.open(&repo);
    let name = "refs/strata/sessions/s1/base";
    git.update_ref(name, &c1, None).unwrap();
    let e = git.delete_ref(name, &c2).unwrap_err();
    assert_eq!(e.code, Code::HeadMoved, "{e:?}");
    assert_eq!(git.read_ref(name).unwrap(), Some(c1.clone()));
    git.delete_ref(name, &c1).unwrap();
    assert!(git.read_ref(name).unwrap().is_none());
    // Deleting what is already gone is a moved ref too.
    let e = git.delete_ref(name, &c1).unwrap_err();
    assert_eq!(e.code, Code::HeadMoved, "{e:?}");
}

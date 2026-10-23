//! Task 9: `push` (§5.11).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use strata_gitio::{Git, remote_ref};
use strata_types::{Code, CommitOid, PushOutcome, RepoPath, TreeOid};

const T: Duration = Duration::from_secs(60);
const PUBLISH: &str = "refs/strata/publish/main";

fn coid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}

fn hook(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

struct Env {
    sb: Sandbox,
    main: PathBuf,
    remote: PathBuf,
    git: Git,
    head: String,
    staging: PathBuf,
}

impl Env {
    /// One more publish commit on top of `parent`, pointed at by `PUBLISH`.
    fn publish(&self, parent: &str, file: &str) -> String {
        fs::write(self.staging.join(file), format!("{file}\n")).unwrap();
        let t = self.git.stage(&self.staging, &[RepoPath::try_from(file.to_string()).unwrap()]).unwrap();
        let c = self.git.commit_tree(&t, &coid(parent), &format!("publish {file}\n")).unwrap();
        let old = self.git.read_ref(PUBLISH).unwrap();
        self.git.update_ref(PUBLISH, &c, old.as_ref()).unwrap();
        c.as_str().to_string()
    }
    fn remote_branch(&self, b: &str) -> Option<String> {
        let o = self.sb.git_output(&self.remote, &["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")]);
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }
    fn local(&self, r: &str) -> Option<String> {
        self.git.read_ref(r).unwrap().map(|c| c.as_str().to_string())
    }
    fn push(&self, dst: &str, timeout: Duration) -> PushOutcome {
        self.git.push(&self.staging, "origin", PUBLISH, dst, timeout).unwrap()
    }
}

fn env() -> Env {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let remote = sb.bare_remote(&main, "remote.git");
    let git = sb.open(&main);
    let head = sb.head(&main);
    git.fetch("origin", "main").unwrap();
    fs::create_dir(sb.path("st")).unwrap();
    let staging = sb.path("st/tree");
    git.export_tree(&TreeOid::try_from(sb.tree_of(&main, &head)).unwrap(), &staging).unwrap();
    git.attach_staging(&staging, &sb.path("st/git"), &coid(&head)).unwrap();
    Env { sb, main, remote, git, head, staging }
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn push_unknown_on_timeout() {
    let e = env();
    let pidfile = e.sb.path("hook.pid");
    hook(&e.remote.join("hooks"), "pre-receive", &format!("echo $$ > {}\nexec sleep 30\n", pidfile.display()));
    let c = e.publish(&e.head, "a.txt");
    let before = (e.local(PUBLISH), e.local(&remote_ref("origin", "main")));
    let t0 = Instant::now();
    assert_eq!(e.push("main", Duration::from_secs(2)), PushOutcome::Unknown);
    assert!(t0.elapsed() < Duration::from_secs(20));
    let pid: i32 = fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!pid_alive(pid), "the hung receive-pack's hook survived");
    assert_eq!((e.local(PUBLISH), e.local(&remote_ref("origin", "main"))), before);
    assert_eq!(e.local(PUBLISH).as_deref(), Some(c.as_str()));
    assert_eq!(e.remote_branch("main").as_deref(), Some(e.head.as_str()));
}

#[test]
fn push_without_ref_status_is_unknown() {
    let e = env();
    hook(&e.remote.join("hooks"), "reference-transaction", "cat >/dev/null\nif [ \"$1\" = committed ]; then kill -9 $PPID; fi\nexit 0\n");
    let c = e.publish(&e.head, "a.txt");
    let tracking = e.local(&remote_ref("origin", "main"));
    assert_eq!(e.push("main", T), PushOutcome::Unknown);
    // The remote did take it: only the status line can say "refused".
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()));
    assert_eq!(e.local(&remote_ref("origin", "main")), tracking);
}

#[test]
fn push_refused_carries_remote_message() {
    let e = env();
    // Non-fast-forward: the remote moved on.
    let other = e.sb.path("other");
    e.sb.git(&e.sb.root, &["clone", "-q", e.remote.to_str().unwrap(), other.to_str().unwrap()]);
    let moved = e.sb.commit_file(&other, "o.txt", b"o\n", "elsewhere");
    e.sb.git(&other, &["push", "-q", "origin", "main"]);
    // With the remote tip known locally git says non-fast-forward (else "fetch first").
    e.git.fetch("origin", "main").unwrap();
    e.publish(&e.head, "a.txt");
    match e.push("main", T) {
        PushOutcome::Refused(m) => assert!(m.contains("non-fast-forward"), "{m}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(e.remote_branch("main").as_deref(), Some(moved.as_str()));
    // pre-receive declines, with its own words.
    hook(&e.remote.join("hooks"), "pre-receive", "echo 'policy: no publishing today' >&2\nexit 1\n");
    e.publish(&moved, "b.txt");
    match e.push("main", T) {
        PushOutcome::Refused(m) => {
            assert!(m.contains("pre-receive hook declined"), "{m}");
            assert!(m.contains("policy: no publishing today"), "{m}");
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(e.remote_branch("main").as_deref(), Some(moved.as_str()));
    assert_eq!(e.local(&remote_ref("origin", "main")).as_deref(), Some(moved.as_str()));
}

#[test]
fn push_pushed_fast_forward_new_and_up_to_date() {
    let e = env();
    let human_tracking = e.sb.git(&e.main, &["rev-parse", "refs/remotes/origin/main"]);
    let c1 = e.publish(&e.head, "a.txt");
    assert_eq!(e.push("main", T), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("main").as_deref(), Some(c1.as_str()));
    assert_eq!(e.local(&remote_ref("origin", "main")).as_deref(), Some(c1.as_str()));
    // New branch.
    assert_eq!(e.push("feature/x", T), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("feature/x").as_deref(), Some(c1.as_str()));
    assert_eq!(e.local(&remote_ref("origin", "feature/x")).as_deref(), Some(c1.as_str()));
    // Up to date ("=").
    assert_eq!(e.push("main", T), PushOutcome::Pushed);
    assert_eq!(e.local(&remote_ref("origin", "main")).as_deref(), Some(c1.as_str()));
    // The human's remote-tracking ref never moves.
    assert_eq!(e.sb.git(&e.main, &["rev-parse", "refs/remotes/origin/main"]), human_tracking);
    assert!(e.sb.git_output(&e.main, &["rev-parse", "--verify", "-q", "refs/remotes/origin/feature/x"]).stdout.is_empty());
    // A concurrent fetch (simulated by the human's pre-push hook) wins.
    hook(
        &e.main.join(".git/hooks"),
        "pre-push",
        &format!("cat >/dev/null\ngit update-ref {} {}\n", remote_ref("origin", "main"), e.head),
    );
    let c2 = e.publish(&c1, "b.txt");
    assert_eq!(e.push("main", T), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("main").as_deref(), Some(c2.as_str()));
    assert_eq!(e.local(&remote_ref("origin", "main")).as_deref(), Some(e.head.as_str()));
}

#[test]
fn push_sets_strata_publish_and_never_forces() {
    let e = env();
    e.publish(&e.head, "a.txt");
    e.git.take_git_calls();
    assert_eq!(e.push("main", T), PushOutcome::Pushed);
    let calls = e.git.take_git_calls();
    let p = find_call(&calls, "push").expect("a push call");
    assert_eq!(env_of(p, "STRATA_PUBLISH").as_deref(), Some("1"));
    assert_eq!(env_of(p, "LC_ALL"), None);
    let a = args_of(p);
    for bad in ["--force", "--force-with-lease", "--no-verify", "-f"] {
        assert!(!a.iter().any(|x| x.starts_with(bad)), "{a:?}");
    }
    assert!(!a.iter().any(|x| x.starts_with('+')), "{a:?}");
    assert!(a.iter().any(|x| x == "--porcelain"), "{a:?}");
    assert!(a.iter().any(|x| x == &format!("{PUBLISH}:refs/heads/main")), "{a:?}");
    assert!(!a.iter().any(|x| x == "origin"), "pushes to the URL, not the remote name: {a:?}");
    assert_eq!(p.cwd, e.staging);

    // Refusals send nothing.
    let code = |r: strata_gitio::Result<PushOutcome>| r.unwrap_err().code;
    assert_eq!(code(e.git.push(&e.staging, "origin", "refs/heads/main", "main", T)), Code::InvalidInput);
    assert_eq!(code(e.git.push(&e.staging, "origin", "refs/strata/publish/none", "main", T)), Code::NoSuchCommit);
    assert_eq!(code(e.git.push(&e.main, "origin", PUBLISH, "main", T)), Code::InvalidInput);
    assert_eq!(code(e.git.push(&e.staging, "-origin", PUBLISH, "main", T)), Code::InvalidInput);
    assert_eq!(code(e.git.push(&e.staging, "origin", PUBLISH, "bad..branch", T)), Code::InvalidInput);
    assert_eq!(code(e.git.push(&e.staging, "origin", PUBLISH, "-main", T)), Code::InvalidInput);
    assert!(find_call(&e.git.take_git_calls(), "push").is_none());
}

#[test]
fn push_runs_human_pre_push_in_staging_root() {
    let e = env();
    let log = e.sb.path("hook.log");
    hook(
        &e.main.join(".git/hooks"),
        "pre-push",
        &format!("cat >/dev/null\n{{ pwd -P; echo \"$GIT_WORK_TREE\"; echo \"$STRATA_PUBLISH\"; }} > {}\nexit 1\n", log.display()),
    );
    e.publish(&e.head, "a.txt");
    assert_eq!(e.push("main", T), PushOutcome::Unknown);
    let got = fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(lines[0], e.staging.to_str().unwrap(), "{got}");
    assert_eq!(fs::canonicalize(lines[1]).unwrap(), e.staging, "{got}");
    assert_eq!(lines[2], "1", "{got}");
    assert_eq!(e.remote_branch("main").as_deref(), Some(e.head.as_str()));
    assert_eq!(e.local(&remote_ref("origin", "main")).as_deref(), Some(e.head.as_str()));
}

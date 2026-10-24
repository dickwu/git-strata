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

/// Review-3 R4: `push_url` is the destination a push goes to, read from
/// the local config only: the remote's `url`, then a `pushurl` once set (and
/// a push then lands there), `InvalidInput` for an unknown or malformed
/// remote name.
#[test]
fn push_url_names_the_push_destination() {
    let e = env();
    let url = e.sb.git_output(&e.main, &["config", "remote.origin.url"]);
    let url = String::from_utf8_lossy(&url.stdout).trim().to_string();
    assert_eq!(e.git.push_url("origin").unwrap(), url);

    let b = e.sb.path("other.git");
    assert!(e.sb.git_output(&e.main, &["init", "-q", "--bare", b.to_str().unwrap()]).status.success());
    assert!(e.sb.git_output(&e.main, &["push", "-q", b.to_str().unwrap(), "main"]).status.success());
    let b_url = b.display().to_string();
    assert!(e.sb.git_output(&e.main, &["config", "remote.origin.pushurl", &b_url]).status.success());
    assert_eq!(e.git.push_url("origin").unwrap(), b_url);
    let c = e.publish(&e.head, "r4.txt");
    assert_eq!(e.push("main", T), PushOutcome::Pushed);
    let at_b = e.sb.git_output(&b, &["rev-parse", "refs/heads/main"]);
    assert_eq!(String::from_utf8_lossy(&at_b.stdout).trim(), c, "the push went to the push URL");
    assert_eq!(e.remote_branch("main").as_deref(), Some(e.head.as_str()), "not to the fetch URL");

    let err = e.git.push_url("nope").unwrap_err();
    assert_eq!(err.code, Code::InvalidInput, "{err:?}");
    assert_eq!(err.detail.as_ref().unwrap()["reason"], "no-such-remote");
    assert_eq!(e.git.push_url("-x").unwrap_err().code, Code::InvalidInput);
}

/// Review-4 F1: `push_published_to` / `push_recorded_to` send to exactly
/// the URL given, whatever origin's push URL says now; tracking ref and
/// anchor still move as for `push_published`. An empty URL is refused
/// before anything is sent.
#[test]
fn push_to_sends_to_the_given_url_not_the_config() {
    let e = env();
    let a_url = e.sb.git_output(&e.main, &["config", "remote.origin.url"]);
    let a_url = String::from_utf8_lossy(&a_url.stdout).trim().to_string();
    let b = e.sb.path("other.git");
    assert!(e.sb.git_output(&e.main, &["init", "-q", "--bare", b.to_str().unwrap()]).status.success());
    assert!(e.sb.git_output(&e.main, &["push", "-q", b.to_str().unwrap(), "main"]).status.success());
    assert!(e.sb.git_output(&e.main, &["config", "remote.origin.pushurl", b.to_str().unwrap()]).status.success());
    let anchor = "refs/strata/anchor/main";

    let c = e.publish(&e.head, "f1.txt");
    let err = e.git.push_published_to(&e.staging, "origin", "", PUBLISH, "main", anchor, T).unwrap_err();
    assert_eq!(err.code, Code::InvalidInput, "{err:?}");
    assert_eq!(err.detail.as_ref().unwrap()["reason"], "empty-url");
    assert_eq!(e.remote_branch("main").as_deref(), Some(e.head.as_str()), "nothing sent");
    assert_eq!(e.git.push_published_to(&e.staging, "origin", &a_url, PUBLISH, "main", anchor, T).unwrap(), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()), "the push went to the given URL (A)");
    let at_b = e.sb.git_output(&b, &["rev-parse", "refs/heads/main"]);
    assert_eq!(String::from_utf8_lossy(&at_b.stdout).trim(), e.head, "not to the configured push URL (B)");
    assert_eq!(e.git.read_ref(&remote_ref("origin", "main")).unwrap(), Some(coid(&c)));
    assert_eq!(e.git.read_ref(anchor).unwrap(), Some(coid(&c)));

    let d = e.publish(&c, "f1b.txt");
    assert_eq!(e.git.push_recorded_to(&e.staging, "origin", &a_url, PUBLISH, "main", anchor, &[], T).unwrap(), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("main").as_deref(), Some(d.as_str()));
}

/// Review-5 N1: the URL a caller resolved is pushed to as given — git does
/// not rewrite it again with `url.*.insteadOf` / `pushInsteadOf` (gitio
/// pins it). The only config that would beat the pin (a rule matching the
/// URL exactly, a remote named after it) is refused before anything is
/// sent, as is a URL that cannot be pinned.
#[test]
fn push_to_is_pinned_against_url_rewriting() {
    let e = env();
    let out = e.sb.git_output(&e.main, &["config", "remote.origin.url"]);
    let a_url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let a = std::path::Path::new(&a_url);
    let alt = e.sb.path("alt");
    fs::create_dir(&alt).unwrap();
    let alt_repo = alt.join(a.file_name().unwrap());
    assert!(e.sb.git_output(&e.main, &["clone", "-q", "--bare", &a_url, alt_repo.to_str().unwrap()]).status.success());
    let at = |p: &std::path::Path| {
        let o = e.sb.git_output(p, &["rev-parse", "refs/heads/main"]);
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let config = |args: &[&str]| assert!(e.sb.git_output(&e.main, &[&["config"][..], args].concat()).status.success());
    let anchor = "refs/strata/anchor/main";

    // A prefix rule that would send A to alt/<name>: the pin wins.
    config(&[&format!("url.{}/.insteadOf", alt.display()), &format!("{}/", a.parent().unwrap().display())]);
    config(&[&format!("url.{}/.pushInsteadOf", alt.display()), &format!("{}/", a.parent().unwrap().display())]);
    assert!(e.git.push_url_exact(&a_url).unwrap());
    let c = e.publish(&e.head, "n1.txt");
    assert_eq!(e.git.push_published_to(&e.staging, "origin", &a_url, PUBLISH, "main", anchor, T).unwrap(), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()), "the push went to A as given");
    assert_eq!(at(&alt_repo), e.head, "not to the rewritten URL");

    // A rule matching A exactly ties the pin: refused, nothing sent.
    let b = e.sb.path("other.git");
    assert!(e.sb.git_output(&e.main, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]).status.success());
    config(&[&format!("url.{}.pushInsteadOf", b.display()), &a_url]);
    assert!(!e.git.push_url_exact(&a_url).unwrap());
    let d = e.publish(&c, "n1b.txt");
    let err = e.git.push_recorded_to(&e.staging, "origin", &a_url, PUBLISH, "main", anchor, &[], T).unwrap_err();
    assert_eq!(err.code, Code::InvalidInput, "{err:?}");
    assert_eq!(err.detail.as_ref().unwrap()["reason"], "url-rewritten");
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()), "nothing sent to A");
    assert_eq!(at(&b), c, "nothing sent to B");
    assert_eq!(e.git.read_ref(anchor).unwrap(), Some(coid(&c)), "nothing recorded");

    // A remote named after the URL: refused, nothing sent.
    config(&["remote.host:n1.git.url", &a_url]);
    assert!(!e.git.push_url_exact("host:n1.git").unwrap());
    let err = e.git.push_published_to(&e.staging, "origin", "host:n1.git", PUBLISH, "main", anchor, T).unwrap_err();
    assert_eq!(err.detail.as_ref().unwrap()["reason"], "url-is-remote-name", "{err:?}");
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()), "nothing sent");

    // A URL that cannot be pinned.
    let err = e.git.push_published_to(&e.staging, "origin", "a\nb", PUBLISH, "main", anchor, T).unwrap_err();
    assert_eq!(err.detail.as_ref().unwrap()["reason"], "bad-url", "{err:?}");
    assert!(!e.git.push_url_exact("a\nb").unwrap());
    let _ = d;
}

/// Review-6 hole 1: git also reads remotes from the legacy files
/// `<common>/remotes/<name>` ("URL: <B>") and `<common>/branches/<name>`
/// ("<B>#<branch>"). A push URL that is a bare name with such a file —
/// `origin`'s own URL here, as the daemon would resolve it — is a remote
/// name, not a URL: refused before anything is sent, and not "exact".
#[test]
fn push_to_refuses_a_bare_name_with_a_legacy_remote_file() {
    let e = env();
    let out = e.sb.git_output(&e.main, &["config", "remote.origin.url"]);
    let a_url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let b = e.sb.path("other.git");
    assert!(e.sb.git_output(&e.main, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]).status.success());
    let at_b = || String::from_utf8_lossy(&e.sb.git_output(&b, &["rev-parse", "refs/heads/main"]).stdout).trim().to_string();
    let anchor = "refs/strata/anchor/main";
    let common = e.main.join(".git");
    let c = e.publish(&e.head, "h1.txt");

    for (dir, name, body) in [
        ("remotes", "legacy", format!("URL: {}\n", b.display())),
        ("branches", "legacy2", format!("{}#main\n", b.display())),
    ] {
        fs::create_dir_all(common.join(dir)).unwrap();
        fs::write(common.join(dir).join(name), body).unwrap();
        assert!(e.sb.git_output(&e.main, &["config", "remote.origin.url", name]).status.success());
        assert_eq!(e.git.push_url("origin").unwrap(), name);
        assert!(!e.git.push_url_exact(name).unwrap(), "{dir}/{name}");
        let err = e.git.push_published_to(&e.staging, "origin", name, PUBLISH, "main", anchor, T).unwrap_err();
        assert_eq!(err.code, Code::InvalidInput, "{dir}: {err:?}");
        assert_eq!(err.detail.as_ref().unwrap()["reason"], "url-is-remote-name", "{dir}: {err:?}");
        assert_eq!(at_b(), e.head, "{dir}: nothing sent to B");
        assert_eq!(e.remote_branch("main").as_deref(), Some(e.head.as_str()), "{dir}: nothing sent to A");
        assert_eq!(e.git.read_ref(anchor).unwrap(), None, "{dir}: nothing recorded");
        fs::remove_file(common.join(dir).join(name)).unwrap();
    }
    // Without the file the same bare name is just a (missing) path.
    assert!(e.git.push_url_exact("legacy").unwrap());
    let _ = c;
}

/// Review-6 hole 2: a rule added after the pre-send check and before
/// `git push` reads the config (here: an exact `pushInsteadOf` sending A to
/// B, removed again once the push returns, so no later re-check sees it)
/// cannot redirect the push. The push process itself checks the URL it is
/// about to send to (the guard `pre-push`, D25): refused, nothing sent to
/// A or B, nothing recorded, and the guard leaves nothing behind.
#[test]
fn push_to_rule_added_between_check_and_push_is_refused() {
    let e = env();
    let out = e.sb.git_output(&e.main, &["config", "remote.origin.url"]);
    let a_url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let b = e.sb.path("other.git");
    assert!(e.sb.git_output(&e.main, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]).status.success());
    let at_b = || String::from_utf8_lossy(&e.sb.git_output(&b, &["rev-parse", "refs/heads/main"]).stdout).trim().to_string();
    let anchor = "refs/strata/anchor/main";
    let rule = format!("url.{}.pushInsteadOf", b.display());
    let tracking = e.local(&remote_ref("origin", "main"));
    e.publish(&e.head, "h2.txt");

    let pause = e.git.pause_push_after_check();
    let r = std::thread::scope(|s| {
        let h = s.spawn(|| e.git.push_published_to(&e.staging, "origin", &a_url, PUBLISH, "main", anchor, T));
        pause.wait_until_paused();
        assert!(e.sb.git_output(&e.main, &["config", &rule, &a_url]).status.success());
        pause.release();
        h.join().unwrap()
    });
    assert!(e.sb.git_output(&e.main, &["config", "--unset", &rule]).status.success());

    let err = r.unwrap_err();
    assert_eq!(err.code, Code::InvalidInput, "{err:?}");
    assert_eq!(err.detail.as_ref().unwrap()["reason"], "url-rewritten", "{err:?}");
    assert_eq!(at_b(), e.head, "nothing sent to B");
    assert_eq!(e.remote_branch("main").as_deref(), Some(e.head.as_str()), "nothing sent to A");
    assert_eq!(e.git.read_ref(anchor).unwrap(), None, "nothing recorded");
    assert_eq!(e.local(&remote_ref("origin", "main")), tracking);
    // The rule is gone again: a re-check after the push sees nothing.
    assert!(e.git.push_url_exact(&a_url).unwrap());
    let left: Vec<_> = fs::read_dir(e.sb.path("st/git"))
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("strata-push-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // The same push without the rule goes to A, guard and all.
    let c = e.local(PUBLISH).unwrap();
    assert_eq!(e.git.push_published_to(&e.staging, "origin", &a_url, PUBLISH, "main", anchor, T).unwrap(), PushOutcome::Pushed);
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()));
    assert_eq!(at_b(), e.head);
}

/// Review-7: the guard (D25) hands the human's `pre-push` the environment
/// it had before: git commands inside that hook do not see the guard's
/// `core.hooksPath`, so a nested `git hook run pre-commit` finds the human's
/// own `pre-commit`, and no guard variables leak into the hook.
#[test]
fn push_human_pre_push_runs_nested_hooks_without_the_guard_override() {
    let e = env();
    let hooks = e.main.join(".git/hooks");
    let ran = e.sb.path("pre-commit.ran");
    let log = e.sb.path("pre-push.log");
    hook(&hooks, "pre-commit", &format!("echo ok > {}\n", ran.display()));
    hook(
        &hooks,
        "pre-push",
        &format!(
            "cat >/dev/null\n{{ echo \"hooksPath=[$(git config --get core.hooksPath)]\"; echo \"count=${{GIT_CONFIG_COUNT-unset}} seen=${{STRATA_PUSH_GUARD_SEEN-unset}} user=${{STRATA_USER_PRE_PUSH-unset}}\"; }} > {}\ngit hook run pre-commit || exit 1\n",
            log.display()
        ),
    );
    let c = e.publish(&e.head, "r7.txt");
    assert_eq!(e.push("main", T), PushOutcome::Pushed, "{:?}", fs::read_to_string(&log));
    assert_eq!(e.remote_branch("main").as_deref(), Some(c.as_str()));
    assert!(ran.exists(), "the nested pre-commit ran");
    let got = fs::read_to_string(&log).unwrap();
    assert_eq!(got, "hooksPath=[]\ncount=unset seen=unset user=unset\n");
}

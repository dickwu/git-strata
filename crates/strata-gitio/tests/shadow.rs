//! Task 7: shadow repositories, `attach_worktree`, `worktree_gitdir` (§5.6).

mod common;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
use strata_gitio::{Git, MirrorDrift, Shadow};
use strata_types::{Code, CommitOid, RepoPath, StrataError, TreeOid};

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
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0, "mkfifo {}", p.display());
}

/// Paths and bytes plus mtimes (nanoseconds) of everything under `dir`.
fn snapshot_mtimes(dir: &Path) -> Vec<(PathBuf, Vec<u8>, i64, i64)> {
    snapshot(dir)
        .into_iter()
        .map(|(rel, b)| {
            let m = fs::symlink_metadata(dir.join(&rel)).unwrap();
            (rel, b, m.mtime(), m.mtime_nsec())
        })
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// Run `f` on another thread; fail when it takes longer than `secs`.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).expect("call hung (watchdog)")
}

/// A main checkout, its `Git`, a shadow repository and generation 1 of a
/// workspace living at `ws`.
struct Ctx {
    sb: Sandbox,
    main: PathBuf,
    git: Git,
    shadow: Shadow,
    ws: PathBuf,
    admin: PathBuf,
    head: String,
}

impl Ctx {
    fn tree(&self, head: &str) -> TreeOid {
        toid(&self.sb.tree_of(&self.main, head))
    }

    /// Export `head`'s tree to a fresh temp tree named `name`.
    fn temp_tree(&self, name: &str, head: &str) -> PathBuf {
        let tmp = self.sb.path(name);
        self.git.export_tree(&self.tree(head), &tmp).unwrap();
        tmp
    }

    /// Attach generation `n` on a fresh temp tree (not moved).
    fn attach(&self, n: u64, name: &str) -> (PathBuf, strata_gitio::Result<PathBuf>) {
        let tmp = self.temp_tree(name, &self.head);
        let r = self.git.attach_worktree(&self.shadow, n, &coid(&self.head), &tmp, &self.ws);
        (tmp, r)
    }

    /// Swap `tmp` into the workspace path.
    fn swap_in(&self, tmp: &Path) {
        let old = self.sb.path(&format!("old-{}", tmp.file_name().unwrap().to_string_lossy()));
        if self.ws.exists() {
            fs::rename(&self.ws, &old).unwrap();
        }
        fs::rename(tmp, &self.ws).unwrap();
    }

    fn ws_git(&self, args: &[&str]) -> String {
        self.sb.git(&self.ws, args)
    }

    fn drift(&self) -> Vec<MirrorDrift> {
        self.git.shadow_drift(&self.shadow, &self.ws).unwrap()
    }
}

fn setup_with(prepare: impl FnOnce(&Sandbox, &Path)) -> Ctx {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    prepare(&sb, &main);
    let git = sb.open(&main);
    let shadow = git.ensure_shadow(&sb.path("s.git")).unwrap();
    let head = sb.head(&main);
    let ws = sb.path("ws");
    let tmp = sb.path("tmp-1");
    git.export_tree(&toid(&sb.tree_of(&main, "HEAD")), &tmp).unwrap();
    let admin = git.attach_worktree(&shadow, 1, &coid(&head), &tmp, &ws).unwrap();
    fs::rename(&tmp, &ws).unwrap();
    Ctx { sb, main, git, shadow, ws, admin, head }
}

fn setup() -> Ctx {
    setup_with(|_, _| {})
}

/// The main checkout's index, config, HEAD, ref and worktree listings, and hook files.
type MainState = (Vec<u8>, Vec<u8>, String, String, String, Vec<(PathBuf, Vec<u8>)>);

fn main_state(sb: &Sandbox, main: &Path) -> MainState {
    let hooks = main.join(".git/hooks");
    (
        fs::read(main.join(".git/index")).unwrap(),
        fs::read(main.join(".git/config")).unwrap(),
        fs::read_to_string(main.join(".git/HEAD")).unwrap(),
        sb.git(main, &["for-each-ref"]),
        sb.git(main, &["worktree", "list", "--porcelain"]),
        if hooks.exists() { snapshot(&hooks) } else { Vec::new() },
    )
}

#[test]
fn exported_workspace_git_status_is_clean() {
    let ctx = setup_with(|sb, main| {
        sb.git(main, &["config", "filter.d.smudge", "sed s/CLEAN/SMUDGED/"]);
        sb.git(main, &["config", "filter.d.clean", "sed s/SMUDGED/CLEAN/"]);
        sb.git(main, &["config", "core.autocrlf", "true"]);
        sb.write(main, ".gitattributes", b"*.f filter=d\n*.txt eol=crlf\n");
        sb.write(main, "a.f", b"CLEAN\n");
        sb.write(main, "b.txt", b"one\ntwo\n");
        sb.git(main, &["add", "."]);
        sb.git(main, &["commit", "-q", "-m", "filters"]);
    });
    assert_eq!(fs::read(ctx.ws.join("a.f")).unwrap(), b"SMUDGED\r\n");
    assert_eq!(fs::read(ctx.ws.join("b.txt")).unwrap(), b"one\r\ntwo\r\n");
    assert_eq!(ctx.ws_git(&["status", "--porcelain"]), "");
}

#[test]
fn ensure_shadow_builds_the_layout() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    // Odd values the writer has to quote or escape.
    let odd: &[(&str, &str)] = &[
        ("filter.d.clean", "cat # not a comment"),
        ("filter.d.smudge", " leading and trailing "),
        ("diff.x.textconv", "say \"hi\" \\ there"),
        ("merge.m.driver", "line1\nline2\tx;y"),
        ("filter.Sub\"q.process", "plain"),
    ];
    for (k, v) in odd {
        sb.git(&main, &["config", k, v]);
    }
    sb.git(&main, &["config", "core.autocrlf", "input"]);
    sb.git(&main, &["config", "core.excludesFile", "/nowhere/excl"]);
    sb.git(&main, &["config", "remote.origin.url", "/nowhere/remote"]);
    sb.git(&main, &["config", "branch.main.remote", "origin"]);
    sb.git(&main, &["config", "include.path", "/nowhere/inc"]);
    sb.git(&main, &["config", "extensions.noop", "true"]);
    sb.git(&main, &["config", "core.hooksPath", "/nowhere/hooks"]);
    fs::write(main.join(".git/config"), [fs::read(main.join(".git/config")).unwrap(), b"[filter \"d\"]\n\trequired\n".to_vec()].concat()).unwrap();
    sb.write(&main, ".git/info/attributes", b"*.bin binary\n");
    // Global defaults that must not leak into the shadow.
    let tpl = sb.path("tpl");
    fs::create_dir_all(tpl.join("hooks")).unwrap();
    fs::write(tpl.join("hooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(tpl.join("description"), "tpl\n").unwrap();
    let gc = sb.home.join(".gitconfig");
    let mut g = fs::read_to_string(&gc).unwrap();
    g.push_str(&format!("[init]\n\tdefaultObjectFormat = sha256\n\tdefaultRefFormat = reftable\n\ttemplateDir = {}\n", tpl.display()));
    fs::write(&gc, g).unwrap();

    let git = sb.open(&main);
    let dir = sb.path("s.git");
    let shadow = git.ensure_shadow(&dir).unwrap();
    assert_eq!(shadow.dir(), fs::canonicalize(&dir).unwrap());
    assert_eq!(sb.git(&dir, &["rev-parse", "--show-object-format"]), "sha1");
    assert_eq!(sb.git(&dir, &["rev-parse", "--show-ref-format"]), "files");
    assert!(!dir.join("description").exists());
    assert_eq!(names(&dir.join("hooks")), vec!["pre-push"]);
    assert_eq!(
        fs::read(dir.join("objects/info/alternates")).unwrap(),
        format!("{}\n", fs::canonicalize(main.join(".git/objects")).unwrap().display()).into_bytes()
    );
    assert_eq!(fs::read(dir.join("info/exclude")).unwrap(), fs::read(main.join(".git/info/exclude")).unwrap());
    assert_eq!(fs::read(dir.join("info/attributes")).unwrap(), b"*.bin binary\n");

    // Config: exactly the expected keys, values read back verbatim by git.
    let list = |args: &[&str], cwd: &Path| -> Vec<(String, Option<String>)> {
        let o = sb.git_output(cwd, args);
        assert!(o.status.success());
        o.stdout
            .split(|&c| c == 0)
            .filter(|r| !r.is_empty())
            .map(|r| {
                let s = String::from_utf8(r.to_vec()).unwrap();
                match s.split_once('\n') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (s, None),
                }
            })
            .collect()
    };
    let main_list = list(&["config", "--local", "--null", "--list"], &main);
    let copied = |k: &str| {
        [
            "core.ignorecase",
            "core.precomposeunicode",
            "core.autocrlf",
            "core.eol",
            "core.safecrlf",
            "core.symlinks",
            "core.filemode",
            "core.checkroundtripencoding",
            "core.excludesfile",
            "core.attributesfile",
        ]
        .contains(&k)
            || ["filter.", "diff.", "merge."].iter().any(|p| k.starts_with(p))
    };
    let real = shadow.dir().to_path_buf();
    let mut want = vec![
        ("core.repositoryformatversion".to_string(), Some("0".to_string())),
        ("core.bare".to_string(), Some("true".to_string())),
    ];
    want.extend(main_list.iter().filter(|(k, _)| copied(k)).cloned());
    want.push(("core.hookspath".into(), Some(format!("{}/hooks", real.display()))));
    want.push(("strata.shadow".into(), Some("true".into())));
    let cfg = dir.join("config");
    let got = list(&["config", "--file", cfg.to_str().unwrap(), "--null", "--list"], &sb.root);
    assert_eq!(got, want);
    for (k, v) in odd {
        let o = sb.git_output(&sb.root, &["config", "--file", cfg.to_str().unwrap(), "--get", k]);
        assert_eq!(String::from_utf8_lossy(&o.stdout), format!("{v}\n"), "{k}");
    }
    let text = fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("\tshadow = true\n"), "{text}");
    assert!(text.contains(&format!("\thooksPath = {}/hooks\n", real.display())), "{text}");
    assert!(text.contains("\tclean = \"cat # not a comment\"\n"), "{text}");
    assert!(text.contains("\tsmudge = \" leading and trailing \"\n"), "{text}");
    assert!(text.contains("\ttextconv = say \\\"hi\\\" \\\\ there\n"), "{text}");
    assert!(text.contains("\tdriver = \"line1\\nline2\\tx;y\"\n"), "{text}");
    assert!(text.contains("\trequired\n"), "{text}");
    assert!(text.contains("\tautocrlf = input\n"), "{text}");
    for bad in ["remote", "branch", "include", "extensions", "/nowhere/hooks"] {
        assert!(!text.contains(bad), "{bad} in {text}");
    }

    // The pre-push guard.
    let hook = dir.join("hooks/pre-push");
    assert_eq!(fs::metadata(&hook).unwrap().permissions().mode() & 0o777, 0o755);
    let o = std::process::Command::new(&hook).env_remove("STRATA_PUBLISH").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "git-strata: push refused in a strata workspace; publish with git-strata publish\n"
    );
    let o = std::process::Command::new(&hook).env("STRATA_PUBLISH", "1").output().unwrap();
    assert_eq!(o.status.code(), Some(0));

    // A main checkout without info/attributes gives a shadow without one.
    fs::remove_file(main.join(".git/info/attributes")).unwrap();
    let s2 = git.ensure_shadow(&sb.path("s2.git")).unwrap();
    assert!(!s2.dir().join("info/attributes").exists());
}

#[test]
fn ensure_shadow_is_idempotent_and_never_rewrites_mirrors() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let dir = sb.path("s.git");
    let a = git.ensure_shadow(&dir).unwrap();
    let before = snapshot_mtimes(&dir);
    fs::write(main.join(".git/info/exclude"), "changed\n").unwrap();
    sb.git(&main, &["config", "core.autocrlf", "true"]);
    let b = git.ensure_shadow(&dir).unwrap();
    assert_eq!(a, b);
    assert_eq!(snapshot_mtimes(&dir), before);

    // Half-built (no marker): Exists, untouched.
    let half = sb.path("half.git");
    sb.git(&sb.root, &["init", "-q", "--bare", half.to_str().unwrap()]);
    fs::write(half.join("objects/info/alternates"), format!("{}\n", main.join(".git/objects").display())).unwrap();
    let snap = snapshot_mtimes(&half);
    assert_eq!(git.ensure_shadow(&half).unwrap_err().code, Code::Exists);
    assert_eq!(snapshot_mtimes(&half), snap);

    // Marker, but alternates of another repo.
    let other = sb.init_repo("other");
    let foreign = sb.path("foreign.git");
    sb.git(&sb.root, &["init", "-q", "--bare", foreign.to_str().unwrap()]);
    fs::write(foreign.join("objects/info/alternates"), format!("{}\n", other.join(".git/objects").display())).unwrap();
    sb.git(&foreign, &["config", "strata.shadow", "true"]);
    let snap = snapshot_mtimes(&foreign);
    assert_eq!(git.ensure_shadow(&foreign).unwrap_err().code, Code::Exists);
    assert_eq!(snapshot_mtimes(&foreign), snap);

    // Inside the main checkout.
    for p in [main.join("inner.git"), main.join(".git/inner")] {
        let e = git.ensure_shadow(&p).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{e:?}");
        assert!(!p.exists());
    }
    // Relative, missing parent.
    assert_eq!(git.ensure_shadow(Path::new("rel.git")).unwrap_err().code, Code::InvalidInput);
    assert_eq!(git.ensure_shadow(&sb.path("nope/s.git")).unwrap_err().code, Code::NoSuchPath);
}

#[test]
fn shadow_at_reopens_without_writing() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let dir = sb.path("s.git");
    let made = sb.open(&main).ensure_shadow(&dir).unwrap();
    let before = snapshot_mtimes(&dir);
    let git = sb.open(&main); // a daemon restart
    let s = git.shadow_at(&dir).unwrap();
    assert_eq!(s, made);
    assert_eq!(snapshot_mtimes(&dir), before);
    let ws = sb.path("ws");
    assert!(git.shadow_drift(&s, &ws).unwrap().is_empty());
    assert_eq!(snapshot_mtimes(&dir), before);
    let head = sb.head(&main);
    let tmp = sb.path("tmp");
    git.export_tree(&toid(&sb.tree_of(&main, "HEAD")), &tmp).unwrap();
    let admin = git.attach_worktree(&s, 1, &coid(&head), &tmp, &ws).unwrap();
    assert_eq!(admin, s.dir().join("worktrees/1"));

    let cfg = dir.join("config");
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(&cfg, text.replace("\tshadow = true\n", "\tshadow = \"true\"\n")).unwrap();
    assert_eq!(sb.open(&main).shadow_at(&dir).unwrap(), made);
}

#[test]
fn shadow_at_missing_dir_is_no_such_path_and_creates_nothing() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let parent = sb.path("p");
    fs::create_dir(&parent).unwrap();
    let dir = parent.join("s.git");
    assert_eq!(git.shadow_at(&dir).unwrap_err().code, Code::NoSuchPath);
    assert!(!dir.exists());
    assert!(names(&parent).is_empty());

    let nomark = sb.path("nomark.git");
    sb.git(&sb.root, &["init", "-q", "--bare", nomark.to_str().unwrap()]);
    fs::write(nomark.join("objects/info/alternates"), format!("{}\n", main.join(".git/objects").display())).unwrap();
    let other = sb.init_repo("other");
    let foreign = sb.path("foreign.git");
    sb.git(&sb.root, &["init", "-q", "--bare", foreign.to_str().unwrap()]);
    fs::write(foreign.join("objects/info/alternates"), format!("{}\n", other.join(".git/objects").display())).unwrap();
    sb.git(&foreign, &["config", "strata.shadow", "true"]);
    let falsy = sb.path("falsy.git");
    sb.git(&sb.root, &["init", "-q", "--bare", falsy.to_str().unwrap()]);
    fs::write(falsy.join("objects/info/alternates"), format!("{}\n", main.join(".git/objects").display())).unwrap();
    sb.git(&falsy, &["config", "strata.shadow", "false"]);
    for d in [&nomark, &foreign, &falsy] {
        let snap = snapshot_mtimes(d);
        let e = git.shadow_at(d).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{} {e:?}", d.display());
        assert_eq!(snapshot_mtimes(d), snap);
    }
    let file = sb.path("file.git");
    fs::write(&file, "x").unwrap();
    assert_eq!(git.shadow_at(&file).unwrap_err().code, Code::InvalidInput);
    assert_eq!(fs::read(&file).unwrap(), b"x");
    // A config line git rejects: Unknown bad-config.
    let badcfg = sb.path("bad.git");
    sb.git(&sb.root, &["init", "-q", "--bare", badcfg.to_str().unwrap()]);
    fs::write(badcfg.join("objects/info/alternates"), format!("{}\n", main.join(".git/objects").display())).unwrap();
    fs::write(badcfg.join("config"), "[strata]\n\tshadow = true\n[[broken\n").unwrap();
    let e = git.shadow_at(&badcfg).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e).as_deref(), Some("bad-config"));
}

#[test]
fn shadow_drift_reports_changed_mirrors() {
    let ctx = setup_with(|sb, main| {
        sb.git(main, &["config", "core.autocrlf", "input"]);
        sb.git(main, &["config", "filter.d.clean", "cat"]);
        sb.git(main, &["config", "filter.d.smudge", "cat"]);
    });
    assert_eq!(ctx.drift(), vec![]);
    let before = snapshot(ctx.shadow.dir());
    fs::write(ctx.main.join(".git/info/exclude"), "new rule\n").unwrap();
    fs::write(ctx.main.join(".git/info/attributes"), "*.x text\n").unwrap();
    ctx.sb.git(&ctx.main, &["config", "core.autocrlf", "true"]);
    ctx.sb.git(&ctx.main, &["config", "--unset", "filter.d.clean"]);
    assert_eq!(
        ctx.drift(),
        vec![
            MirrorDrift::Exclude,
            MirrorDrift::Attributes,
            MirrorDrift::Config { key: "core.autocrlf".into() },
            MirrorDrift::Config { key: "filter.d.clean".into() },
        ]
    );
    assert_eq!(snapshot(ctx.shadow.dir()), before);
}

fn guard(hooks_path: Option<String>, hook_replaced: bool) -> MirrorDrift {
    MirrorDrift::PrePushGuardDisabled { hooks_path, hook_replaced }
}

#[test]
fn shadow_drift_reports_a_disabled_pre_push_guard() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let real = sb.path("real");
    fs::create_dir(&real).unwrap();
    symlink(&real, sb.path("link")).unwrap();
    let shadow = git.ensure_shadow(&sb.path("link/s.git")).unwrap();
    assert_eq!(shadow.dir(), real.join("s.git"));
    let ws = sb.path("ws");
    let tmp = sb.path("tmp");
    git.export_tree(&toid(&sb.tree_of(&main, "HEAD")), &tmp).unwrap();
    git.attach_worktree(&shadow, 1, &coid(&sb.head(&main)), &tmp, &ws).unwrap();
    fs::rename(&tmp, &ws).unwrap();
    let drift = |want: Vec<MirrorDrift>| {
        let before = snapshot(shadow.dir());
        assert_eq!(git.shadow_drift(&shadow, &ws).unwrap(), want);
        assert_eq!(snapshot(shadow.dir()), before);
    };
    drift(vec![]);

    sb.git(&ws, &["config", "core.hooksPath", ".husky/_"]);
    drift(vec![guard(Some(format!("{}/.husky/_", ws.display())), false)]);

    sb.git(&ws, &["config", "--unset", "core.hooksPath"]);
    drift(vec![]);

    let elsewhere = sb.path("user-hooks");
    fs::create_dir(&elsewhere).unwrap();
    sb.git(&sb.root, &["config", "--global", "core.hooksPath", elsewhere.to_str().unwrap()]);
    drift(vec![guard(Some(elsewhere.display().to_string()), false)]);

    let pinned = format!("{}/hooks", shadow.dir().display());
    sb.git(&ws, &["config", "core.hooksPath", &pinned]);
    drift(vec![]);
    fs::write(shadow.dir().join("hooks/pre-push"), "#!/bin/sh\nexit 0\n").unwrap();
    drift(vec![guard(None, true)]);
}

/// A fresh workspace for one case of the effective-hooks-path test.
fn hooks_case() -> (Ctx, PathBuf, PathBuf) {
    let ctx = setup();
    let elsewhere = ctx.sb.path("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let inc = ctx.sb.path("inc.cfg");
    fs::write(&inc, format!("[core]\n\thooksPath = {}\n", elsewhere.display())).unwrap();
    (ctx, elsewhere, inc)
}

fn drift_unchanged(ctx: &Ctx) -> strata_gitio::Result<Vec<MirrorDrift>> {
    let (s, w) = (snapshot(ctx.shadow.dir()), snapshot(&ctx.ws));
    let r = ctx.git.shadow_drift(&ctx.shadow, &ctx.ws);
    assert_eq!(snapshot(ctx.shadow.dir()), s);
    if ctx.ws.exists() {
        assert_eq!(snapshot(&ctx.ws), w);
    }
    r
}

#[test]
fn shadow_drift_follows_the_effective_hooks_path() {
    let pinned_list = |ctx: &Ctx| ctx.sb.git(ctx.shadow.dir(), &["config", "--local", "--get", "core.hooksPath"]);

    // include.path in the shadow config.
    let (ctx, elsewhere, inc) = hooks_case();
    ctx.ws_git(&["config", "include.path", inc.to_str().unwrap()]);
    let d = drift_unchanged(&ctx).unwrap();
    assert_eq!(
        d,
        vec![MirrorDrift::Config { key: "include.path".into() }, guard(Some(elsewhere.display().to_string()), false)]
    );
    assert_eq!(pinned_list(&ctx), format!("{}/hooks", ctx.shadow.dir().display()));

    // includeIf gitdir: on the admin dirs.
    let (ctx, _elsewhere, inc) = hooks_case();
    let key = format!("includeIf.gitdir:{}/worktrees/**.path", ctx.shadow.dir().display());
    ctx.ws_git(&["config", &key, inc.to_str().unwrap()]);
    let d = drift_unchanged(&ctx).unwrap();
    let want = format!("includeif.gitdir:{}/worktrees/**.path", ctx.shadow.dir().display());
    assert!(d.contains(&MirrorDrift::Config { key: want }), "{d:?}");

    // worktreeConfig plus a per-worktree hooksPath.
    let (ctx, elsewhere, _inc) = hooks_case();
    ctx.ws_git(&["config", "extensions.worktreeConfig", "true"]);
    ctx.ws_git(&["config", "--worktree", "core.hooksPath", elsewhere.to_str().unwrap()]);
    let d = drift_unchanged(&ctx).unwrap();
    assert_eq!(
        d,
        vec![
            MirrorDrift::Config { key: "extensions.worktreeconfig".into() },
            guard(Some(elsewhere.display().to_string()), false)
        ]
    );

    // worktreeConfig alone, include in config.worktree.
    let (ctx, _elsewhere, _inc) = hooks_case();
    ctx.ws_git(&["config", "extensions.worktreeConfig", "true"]);
    let harmless = ctx.sb.path("harmless.cfg");
    fs::write(&harmless, "[user]\n\tname = x\n").unwrap();
    fs::write(ctx.admin.join("config.worktree"), format!("[include]\n\tpath = {}\n", harmless.display())).unwrap();
    assert_eq!(drift_unchanged(&ctx).unwrap(), vec![MirrorDrift::Config { key: "extensions.worktreeconfig".into() }]);

    // Pinned key present: user-level hooksPath, directly or via includeIf gitdir.
    let (ctx, elsewhere, inc) = hooks_case();
    ctx.sb.git(&ctx.sb.root, &["config", "--global", "core.hooksPath", elsewhere.to_str().unwrap()]);
    assert_eq!(drift_unchanged(&ctx).unwrap(), vec![]);
    ctx.sb.git(&ctx.sb.root, &["config", "--global", "--unset", "core.hooksPath"]);
    let key = format!("includeIf.gitdir:{}/worktrees/**.path", ctx.shadow.dir().display());
    ctx.sb.git(&ctx.sb.root, &["config", "--global", &key, inc.to_str().unwrap()]);
    assert_eq!(drift_unchanged(&ctx).unwrap(), vec![]);

    // gitfile pointed at the main checkout.
    let (ctx, _e, _i) = hooks_case();
    fs::write(ctx.ws.join(".git"), format!("gitdir: {}/.git\n", ctx.main.display())).unwrap();
    assert_eq!(drift_unchanged(&ctx).unwrap(), vec![guard(Some(format!("{}/.git/hooks", ctx.main.display())), false)]);

    // No .git: mirrors only.
    fs::remove_file(ctx.ws.join(".git")).unwrap();
    fs::write(ctx.main.join(".git/info/exclude"), "changed\n").unwrap();
    assert_eq!(drift_unchanged(&ctx).unwrap(), vec![MirrorDrift::Exclude]);

    // Broken gitfile: Unknown hooks-lookup.
    fs::write(ctx.ws.join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
    let e = drift_unchanged(&ctx).unwrap_err();
    assert_eq!((e.code, reason(&e).as_deref()), (Code::Unknown, Some("hooks-lookup")), "{e:?}");

    // An empty per-worktree hooksPath git refuses: Unknown hooks-lookup.
    let (ctx, _e, _i) = hooks_case();
    ctx.ws_git(&["config", "extensions.worktreeConfig", "true"]);
    fs::write(ctx.admin.join("config.worktree"), "[core]\n\thooksPath = \n").unwrap();
    let e = drift_unchanged(&ctx).unwrap_err();
    assert_eq!((e.code, reason(&e).as_deref()), (Code::Unknown, Some("hooks-lookup")), "{e:?}");

    // An include of a FIFO in the shadow config: Unknown timeout (~10 s).
    let (ctx, _e, _i) = hooks_case();
    let fifo = ctx.sb.path("fifo");
    mkfifo(&fifo);
    ctx.ws_git(&["config", "include.path", fifo.to_str().unwrap()]);
    let t = Instant::now();
    let e = drift_unchanged(&ctx).unwrap_err();
    assert_eq!((e.code, reason(&e).as_deref()), (Code::Unknown, Some("timeout")), "{e:?}");
    assert!(t.elapsed() < Duration::from_secs(30));
}

#[test]
fn attach_worktree_makes_git_work_in_workspace() {
    let ctx = setup_with(|sb, main| {
        sb.commit_file(main, ".gitignore", b"node_modules/\n", "ignore");
        fs::write(main.join(".git/info/exclude"), ".env\n").unwrap();
        sb.bare_remote(main, "remote.git");
    });
    let (sb, ws) = (&ctx.sb, &ctx.ws);
    let main_before = main_state(sb, &ctx.main);
    fs::write(ws.join(".env"), "SECRET=1\n").unwrap();
    sb.write(ws, "node_modules/x", b"x\n");

    let tree = ctx.tree(&ctx.head);
    let exp = sb.path("exp");
    ctx.git.export_tree(&tree, &exp).unwrap();
    for p in [".env", "node_modules/x", "README.md"] {
        assert_eq!(
            ctx.git.is_ignored_in_export(&exp, &tree, &rp(p)).unwrap(),
            ctx.git.is_ignored(ws, &rp(p)).unwrap(),
            "{p}"
        );
    }
    assert!(ctx.git.is_ignored(ws, &rp(".env")).unwrap());

    assert_eq!(ctx.ws_git(&["status", "--porcelain"]), "");
    assert!(sb.git_output(ws, &["symbolic-ref", "-q", "HEAD"]).status.code() == Some(1), "detached");
    assert_eq!(ctx.ws_git(&["rev-parse", "HEAD"]), ctx.head);
    assert!(!sb.git_output(ws, &["rev-parse", "--abbrev-ref", "@{upstream}"]).status.success());
    assert_eq!(ctx.ws_git(&["log", "-1", "--format=%H"]), ctx.head);
    assert_eq!(PathBuf::from(ctx.ws_git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])), ctx.shadow.dir());
    assert!(!sb.git_output(ws, &["fetch", "origin"]).status.success());
    assert!(!sb.git_output(ws, &["rev-parse", "--verify", "-q", "origin/main"]).status.success());

    sb.commit_file(ws, "agent.txt", b"agent\n", "agent work");
    let c = ctx.ws_git(&["rev-parse", "HEAD"]);
    assert!(ctx.shadow.dir().join(format!("objects/{}/{}", &c[..2], &c[2..])).is_file());
    assert!(!sb.git_output(&ctx.main, &["cat-file", "-e", &c]).status.success());

    let remote = sb.path("remote.git");
    let o = sb.git_output(ws, &["push", remote.to_str().unwrap(), "HEAD:refs/heads/x"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("git-strata: push refused"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!sb.git_output(&remote, &["rev-parse", "--verify", "-q", "refs/heads/x"]).status.success());

    assert_eq!(main_state(sb, &ctx.main), main_before);
    assert!(!sb.git(&ctx.main, &["worktree", "list"]).contains("ws"));
}

#[test]
fn user_hooks_path_cannot_disable_the_pre_push_guard() {
    let ctx = setup_with(|sb, main| {
        sb.bare_remote(main, "remote.git");
    });
    let empty = ctx.sb.path("empty-hooks");
    fs::create_dir(&empty).unwrap();
    ctx.sb.git(&ctx.sb.root, &["config", "--global", "core.hooksPath", empty.to_str().unwrap()]);
    let remote = ctx.sb.path("remote.git");
    let o = ctx.sb.git_output(&ctx.ws, &["push", remote.to_str().unwrap(), "HEAD:refs/heads/x"]);
    assert_eq!(o.status.code(), Some(1), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!ctx.sb.git_output(&remote, &["rev-parse", "--verify", "-q", "refs/heads/x"]).status.success());
    assert_eq!(
        ctx.ws_git(&["config", "--show-origin", "--get", "core.hooksPath"]),
        format!("file:{}/config\t{}/hooks", ctx.shadow.dir().display(), ctx.shadow.dir().display())
    );
}

#[test]
fn attach_worktree_builds_a_fresh_admin_dir_per_generation() {
    let ctx = setup();
    let gen1 = snapshot(&ctx.admin);
    let head2 = ctx.sb.commit_file(&ctx.main, "two.txt", b"2\n", "two");
    let tmp = ctx.sb.path("tmp-2");
    ctx.git.export_tree(&ctx.tree(&head2), &tmp).unwrap();
    let a2 = ctx.git.attach_worktree(&ctx.shadow, 2, &coid(&head2), &tmp, &ctx.ws).unwrap();
    assert_eq!(a2, ctx.shadow.dir().join("worktrees/2"));
    assert_eq!(fs::read_to_string(a2.join("HEAD")).unwrap(), format!("{head2}\n"));
    let o = ctx
        .sb
        .cmd(&ctx.sb.root)
        .env("GIT_INDEX_FILE", a2.join("index"))
        .arg(format!("--git-dir={}", ctx.shadow.dir().display()))
        .arg("write-tree")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), ctx.sb.tree_of(&ctx.main, &head2));
    assert_eq!(snapshot(&ctx.admin), gen1);
    assert_eq!(fs::read(tmp.join(".git")).unwrap(), format!("gitdir: {}/worktrees/2\n", ctx.shadow.dir().display()).into_bytes());
    assert!(fs::symlink_metadata(tmp.join(".git")).unwrap().is_file());
    assert_eq!(fs::read_to_string(a2.join("gitdir")).unwrap(), format!("{}/.git\n", ctx.ws.display()));
    assert_eq!(fs::read_to_string(a2.join("commondir")).unwrap(), "../..\n");
    assert_eq!(fs::read_to_string(a2.join("locked")).unwrap(), "git-strata");
    ctx.swap_in(&tmp);
    assert_eq!(ctx.ws_git(&["status", "--porcelain"]), "");
    assert_eq!(ctx.ws_git(&["rev-parse", "HEAD"]), head2);

    // Existing worktrees/2: Exists, nothing written.
    let tmp3 = ctx.sb.path("tmp-3");
    ctx.git.export_tree(&ctx.tree(&head2), &tmp3).unwrap();
    let (s, t) = (snapshot(ctx.shadow.dir()), snapshot(&tmp3));
    let e = ctx.git.attach_worktree(&ctx.shadow, 2, &coid(&head2), &tmp3, &ctx.ws).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!((snapshot(ctx.shadow.dir()), snapshot(&tmp3)), (s.clone(), t.clone()));
    let e = ctx.git.attach_worktree(&ctx.shadow, 0, &coid(&head2), &tmp3, &ctx.ws).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!((snapshot(ctx.shadow.dir()), snapshot(&tmp3)), (s, t));
}

#[test]
fn attach_refuses_existing_dot_git() {
    let ctx = setup();
    let wts = snapshot(&ctx.shadow.dir().join("worktrees"));
    for (i, kind) in ["file", "dir", "symlink"].into_iter().enumerate() {
        let tmp = ctx.temp_tree(&format!("t-{kind}"), &ctx.head);
        match kind {
            "file" => fs::write(tmp.join(".git"), "gitdir: /x\n").unwrap(),
            "dir" => fs::create_dir(tmp.join(".git")).unwrap(),
            _ => symlink(ctx.main.join(".git"), tmp.join(".git")).unwrap(),
        }
        let t = snapshot(&tmp);
        let e = ctx.git.attach_worktree(&ctx.shadow, 2 + i as u64, &coid(&ctx.head), &tmp, &ctx.ws).unwrap_err();
        assert_eq!(e.code, Code::Exists, "{kind} {e:?}");
        assert_eq!(snapshot(&tmp), t);
        assert_eq!(snapshot(&ctx.shadow.dir().join("worktrees")), wts);
    }
}

#[test]
fn attach_at_move_refuses_a_symlinked_worktrees_dir() {
    let ctx = setup_with(|sb, main| {
        sb.git(main, &["worktree", "add", "-q", "--detach", sb.path("human-wt").to_str().unwrap()]);
    });
    let mwt = ctx.main.join(".git/worktrees");
    let (before, list) = (snapshot(&mwt), ctx.sb.git(&ctx.main, &["worktree", "list", "--porcelain"]));
    let wts = ctx.shadow.dir().join("worktrees");
    let aside = ctx.shadow.dir().join("worktrees-aside");
    fs::rename(&wts, &aside).unwrap();
    symlink(&mwt, &wts).unwrap();
    let (tmp, r) = ctx.attach(2, "tmp-2");
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(snapshot(&mwt), before);
    assert_eq!(ctx.sb.git(&ctx.main, &["worktree", "list", "--porcelain"]), list);
    assert!(fs::symlink_metadata(tmp.join(".git")).is_err());
    fs::remove_file(&wts).unwrap();
    fs::rename(&aside, &wts).unwrap();
    let a2 = ctx.git.attach_worktree(&ctx.shadow, 2, &coid(&ctx.head), &tmp, &ctx.ws).unwrap();
    assert!(a2.join("index").is_file());
}

#[test]
fn attach_index_copy_refuses_a_swapped_admin_dir() {
    let ctx = setup_with(|sb, main| {
        sb.write(main, "staged.txt", b"human\n");
        sb.git(main, &["add", "staged.txt"]);
    });
    let idx = fs::read(ctx.main.join(".git/index")).unwrap();
    let dotgit = names(&ctx.main.join(".git"));
    let tmp = ctx.temp_tree("tmp-2", &ctx.head);
    ctx.git.take_git_calls();
    let pause = ctx.git.pause_attach_before_index();
    let (g, s, h, t, w) = (ctx.git.clone(), ctx.shadow.clone(), coid(&ctx.head), tmp.clone(), ctx.ws.clone());
    let th = std::thread::spawn(move || g.attach_worktree(&s, 2, &h, &t, &w));
    pause.wait_until_paused();
    let a2 = ctx.shadow.dir().join("worktrees/2");
    fs::rename(&a2, ctx.shadow.dir().join("aside-2")).unwrap();
    symlink(ctx.main.join(".git"), &a2).unwrap();
    pause.release();
    let e = th.join().unwrap().unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(fs::read(ctx.main.join(".git/index")).unwrap(), idx);
    assert_eq!(ctx.sb.git(&ctx.main, &["status", "--porcelain"]), "A  staged.txt");
    assert_eq!(names(&ctx.main.join(".git")), dotgit);

    let calls = ctx.git.take_git_calls();
    let want_idx = tmp.join(".git/index");
    let mut seen = 0;
    for c in &calls {
        let args = args_of(c);
        let rt = args.iter().any(|a| a == "read-tree");
        if !(rt || args.iter().any(|a| a == "update-index")) {
            continue;
        }
        seen += 1;
        assert_eq!(env_of(c, "GIT_INDEX_FILE").map(PathBuf::from), Some(want_idx.clone()), "{args:?}");
        assert!(args.contains(&format!("--git-dir={}", ctx.shadow.dir().display())), "{args:?}");
        assert!(!args.iter().any(|a| a.contains("worktrees/")), "{args:?}");
        if rt {
            assert!(args.contains(&format!("--index-output={}", want_idx.display())), "{args:?}");
        }
    }
    assert_eq!(seen, 2, "{calls:?}");

    let (_tmp3, r) = ctx.attach(3, "tmp-3");
    r.unwrap();
}

#[test]
fn attach_readback_refuses_a_swapped_staging() {
    let ctx = setup_with(|sb, main| {
        sb.write(main, "staged.txt", b"human\n");
        sb.git(main, &["add", "staged.txt"]);
    });
    let idx = fs::read(ctx.main.join(".git/index")).unwrap();
    let other_index = ctx.admin.join("index");
    let mut n = 1u64;
    let mut case = |name: &str, at_read_tree: bool, meddle: &dyn Fn(&Path, &Path)| -> (PathBuf, strata_gitio::Result<PathBuf>) {
        n += 1;
        let parent = ctx.sb.path(&format!("p-{name}"));
        fs::create_dir(&parent).unwrap();
        let tmp = parent.join("tree");
        ctx.git.export_tree(&ctx.tree(&ctx.head), &tmp).unwrap();
        let pause = if at_read_tree { ctx.git.pause_attach_before_read_tree() } else { ctx.git.pause_attach_before_readback() };
        let (g, s, h, t, w) = (ctx.git.clone(), ctx.shadow.clone(), coid(&ctx.head), tmp.clone(), ctx.ws.clone());
        let th = std::thread::spawn(move || g.attach_worktree(&s, n, &h, &t, &w));
        pause.wait_until_paused();
        meddle(&parent, &tmp);
        pause.release();
        let r = th.join().unwrap();
        assert_eq!(fs::read(ctx.main.join(".git/index")).unwrap(), idx, "{name}: main index changed");
        if r.is_err() {
            assert!(!ctx.shadow.dir().join(format!("worktrees/{n}/index")).exists(), "{name}");
        }
        (tmp, r)
    };
    let expect = |r: strata_gitio::Result<PathBuf>, why: &str, name: &str| {
        let e = r.unwrap_err();
        assert_eq!((e.code, reason(&e).as_deref()), (Code::Unknown, Some(why)), "{name}: {e:?}");
    };
    let main_root = ctx.main.clone();

    // (a) .git swapped for a dir with another index.
    let (_, r) = case("a", false, &|p, t| {
        fs::rename(t.join(".git"), p.join("old-git")).unwrap();
        fs::create_dir(t.join(".git")).unwrap();
        fs::copy(&other_index, t.join(".git/index")).unwrap();
    });
    expect(r, "staging-changed", "a");
    // (b) .git moved away and back.
    let (_, r) = case("b", false, &|p, t| {
        fs::rename(t.join(".git"), p.join("g")).unwrap();
        fs::rename(p.join("g"), t.join(".git")).unwrap();
    });
    expect(r, "staging-changed", "b");
    // (c) tree_dir moved away and back.
    let (_, r) = case("c", false, &|p, t| {
        fs::rename(t, p.join("away")).unwrap();
        fs::rename(p.join("away"), t).unwrap();
    });
    expect(r, "staging-changed", "c");
    // (d) a new parent with a same-named tree and .git/index.
    let oi = other_index.clone();
    let (_, r) = case("d", false, &move |p, _t| {
        let old = p.with_file_name(format!("{}-old", p.file_name().unwrap().to_string_lossy()));
        fs::rename(p, &old).unwrap();
        fs::create_dir_all(p.join("tree/.git")).unwrap();
        fs::copy(&oi, p.join("tree/.git/index")).unwrap();
    });
    expect(r, "staging-changed", "d");
    // (e) the index replaced by a symlink to the main index.
    let mr = main_root.clone();
    let (_, r) = case("e", false, &move |_p, t| {
        fs::remove_file(t.join(".git/index")).unwrap();
        symlink(mr.join(".git/index"), t.join(".git/index")).unwrap();
    });
    expect(r, "index-read-back-refused", "e");
    // (f) nothing touched: success.
    let (tmp, r) = case("f", false, &|_p, _t| {});
    let admin = r.unwrap();
    assert!(admin.join("index").is_file());
    assert_eq!(fs::read(tmp.join(".git")).unwrap(), format!("gitdir: {}\n", admin.display()).into_bytes());
    // (g) a new parent whose `tree` is a symlink to the main work tree.
    let mr = main_root.clone();
    let (_, r) = case("g", false, &move |p, _t| {
        let old = p.with_file_name(format!("{}-old", p.file_name().unwrap().to_string_lossy()));
        fs::rename(p, &old).unwrap();
        fs::create_dir(p).unwrap();
        symlink(&mr, p.join("tree")).unwrap();
    });
    expect(r, "staging-changed", "g");
    // (h) same, before read-tree, and read-tree fails (tree object gone).
    let mr = main_root.clone();
    let tree = ctx.sb.tree_of(&ctx.main, &ctx.head);
    let (_, r) = case("h", true, &move |p, _t| {
        let old = p.with_file_name(format!("{}-old", p.file_name().unwrap().to_string_lossy()));
        fs::rename(p, &old).unwrap();
        fs::create_dir(p).unwrap();
        symlink(&mr, p.join("tree")).unwrap();
        fs::remove_file(mr.join(format!(".git/objects/{}/{}", &tree[..2], &tree[2..]))).unwrap();
    });
    expect(r, "staging-changed", "h");
}

#[test]
fn attach_runs_no_planted_hook_and_starts_no_fsmonitor() {
    let ctx = setup();
    let marker = ctx.sb.path("marker");
    let hook = ctx.shadow.dir().join("hooks/post-index-change");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    ctx.sb.git(&ctx.sb.root, &["config", "--global", "core.fsmonitor", "true"]);
    ctx.sb.git(&ctx.sb.root, &["config", "--global", "core.splitIndex", "true"]);
    let tmp = ctx.temp_tree("tmp-2", &ctx.head);
    ctx.git.take_git_calls();
    ctx.git.attach_worktree(&ctx.shadow, 2, &coid(&ctx.head), &tmp, &ctx.ws).unwrap();
    assert!(!marker.exists());
    for d in [ctx.shadow.dir().to_path_buf(), tmp.clone()] {
        for (p, _) in snapshot(&d) {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!n.starts_with("fsmonitor--daemon") && !n.starts_with("sharedindex."), "{}", p.display());
        }
    }
    let calls = ctx.git.take_git_calls();
    for sub in ["read-tree", "update-index"] {
        let c = calls.iter().find(|c| args_of(c).iter().any(|a| a == sub)).unwrap_or_else(|| panic!("no {sub}"));
        let a = args_of(c).join(" ");
        for f in ["-c core.hooksPath=/dev/null", "-c core.fsmonitor=false", "-c core.splitIndex=false"] {
            assert!(a.contains(f), "{sub}: {a}");
        }
    }
}

fn gitdir_err(git: &Git, dir: &Path) -> StrataError {
    git.worktree_gitdir(dir).unwrap_err()
}

#[test]
fn worktree_gitdir_reads_the_gitfile_without_following_links() {
    let ctx = setup();
    let git = &ctx.git;
    // A just-attached temp tree.
    let tmp = ctx.temp_tree("tmp-2", &ctx.head);
    let pause = git.pause_attach_before_index();
    let (g, s, h, t, w) = (git.clone(), ctx.shadow.clone(), coid(&ctx.head), tmp.clone(), ctx.ws.clone());
    let th = std::thread::spawn(move || g.attach_worktree(&s, 2, &h, &t, &w));
    pause.wait_until_paused();
    assert_eq!(git.worktree_gitdir(&tmp).unwrap(), None);
    pause.release();
    let a2 = th.join().unwrap().unwrap();
    assert_eq!(git.worktree_gitdir(&tmp).unwrap().unwrap().as_os_str().as_encoded_bytes(), a2.as_os_str().as_encoded_bytes());
    assert_eq!(git.worktree_gitdir(&ctx.ws).unwrap(), Some(ctx.admin.clone()));

    let d = ctx.sb.path("d");
    fs::create_dir(&d).unwrap();
    assert_eq!(git.worktree_gitdir(&d).unwrap(), None);
    assert_eq!(git.worktree_gitdir(&ctx.sb.path("missing")).unwrap(), None);

    let case = |bytes: &[u8]| -> strata_gitio::Result<Option<PathBuf>> {
        let _ = fs::remove_file(d.join(".git"));
        fs::write(d.join(".git"), bytes).unwrap();
        let before = snapshot_mtimes(&d);
        let r = git.worktree_gitdir(&d);
        assert_eq!(snapshot_mtimes(&d), before);
        r
    };
    let some = |s: &str| Some(PathBuf::from(s));
    assert_eq!(case(b"gitdir: /a b /c \r\n").unwrap(), some("/a b /c "));
    assert_eq!(case(b"gitdir:  /x\n").unwrap(), Some(d.join(" /x")));
    let r = case(b"gitdir: ../x\n").unwrap().unwrap();
    assert_eq!(r.as_os_str().as_encoded_bytes(), format!("{}/../x", d.display()).as_bytes());
    let slash = format!("gitdir: {}/worktrees/1/\n", ctx.shadow.dir().display());
    let r = case(slash.as_bytes()).unwrap().unwrap();
    assert_eq!(r.as_os_str().as_encoded_bytes(), format!("{}/worktrees/1/", ctx.shadow.dir().display()).as_bytes());

    let long = format!("gitdir: /{}\n", "a".repeat(1100));
    let mut nul = b"gitdir: /a".to_vec();
    nul.push(0);
    nul.extend_from_slice(b"b\n");
    for bad in [b"gitdir:x\n".to_vec(), b"gitdir: \n".to_vec(), b"gitdir: ".to_vec(), b"hello\n".to_vec(), nul, long.into_bytes()] {
        let e = case(&bad).unwrap_err();
        assert_eq!((e.code, reason(&e).as_deref()), (Code::InvalidInput, Some("bad-gitfile")), "{bad:?} {e:?}");
    }

    // Non-regular .git.
    fs::remove_file(d.join(".git")).unwrap();
    let target = ctx.sb.path("secret-gitfile");
    fs::write(&target, "gitdir: /very/secret/place\n").unwrap();
    symlink(&target, d.join(".git")).unwrap();
    let e = gitdir_err(git, &d);
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert!(!format!("{e:?}").contains("/very/secret/place"));
    fs::remove_file(d.join(".git")).unwrap();
    fs::create_dir(d.join(".git")).unwrap();
    assert_eq!(gitdir_err(git, &d).code, Code::IsDirectory);
    assert_eq!(gitdir_err(git, &ctx.main).code, Code::IsDirectory);
    fs::remove_dir(d.join(".git")).unwrap();
    mkfifo(&d.join(".git"));
    let (g, dd) = (git.clone(), d.clone());
    let e = within(5, move || g.worktree_gitdir(&dd)).unwrap_err();
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    let link = ctx.sb.path("tree-link");
    symlink(&ctx.ws, &link).unwrap();
    assert_eq!(gitdir_err(git, &link).code, Code::NotDirectory);
    assert_eq!(gitdir_err(git, Path::new("rel/tree")).code, Code::InvalidInput);
}

#[test]
fn shadow_reads_never_hang_on_non_regular_files() {
    let w = |ctx: &Ctx| -> (Git, Shadow, PathBuf) { (ctx.git.clone(), ctx.shadow.clone(), ctx.ws.clone()) };

    // Shadow config is a FIFO.
    let ctx = setup();
    let cfg = ctx.shadow.dir().join("config");
    fs::remove_file(&cfg).unwrap();
    mkfifo(&cfg);
    let (g, s, _) = w(&ctx);
    let e = within(5, move || g.shadow_at(s.dir())).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");

    // alternates is a symlink to the same bytes.
    let ctx = setup();
    let alt = ctx.shadow.dir().join("objects/info/alternates");
    let copy = ctx.sb.path("alt-copy");
    fs::copy(&alt, &copy).unwrap();
    fs::remove_file(&alt).unwrap();
    symlink(&copy, &alt).unwrap();
    let (g, s, _) = w(&ctx);
    let e = within(5, move || g.shadow_at(s.dir())).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");

    // Shadow info/exclude a FIFO, then a symlink to the same bytes.
    let ctx = setup();
    let ex = ctx.shadow.dir().join("info/exclude");
    let copy = ctx.sb.path("exclude-copy");
    fs::copy(&ex, &copy).unwrap();
    fs::remove_file(&ex).unwrap();
    mkfifo(&ex);
    let (g, s, ws) = w(&ctx);
    assert_eq!(within(5, move || g.shadow_drift(&s, &ws)).unwrap(), vec![MirrorDrift::Exclude]);
    let (g, _, ws) = w(&ctx);
    assert_eq!(within(5, move || g.is_ignored(&ws, &rp("x"))).unwrap_err().code, Code::Unknown);
    fs::remove_file(&ex).unwrap();
    symlink(&copy, &ex).unwrap();
    let (g, s, ws) = w(&ctx);
    assert_eq!(within(5, move || g.shadow_drift(&s, &ws)).unwrap(), vec![MirrorDrift::Exclude]);
    let (g, _, ws) = w(&ctx);
    assert_eq!(within(5, move || g.is_ignored(&ws, &rp("x"))).unwrap_err().code, Code::Unknown);

    // Main info/exclude a symlink to the same bytes, then a FIFO.
    let ctx = setup();
    let mex = ctx.main.join(".git/info/exclude");
    let copy = ctx.sb.path("main-exclude");
    fs::copy(&mex, &copy).unwrap();
    fs::remove_file(&mex).unwrap();
    symlink(&copy, &mex).unwrap();
    let (g, s, ws) = w(&ctx);
    assert_eq!(within(5, move || g.shadow_drift(&s, &ws)).unwrap(), vec![]);
    let s2 = ctx.git.ensure_shadow(&ctx.sb.path("s2.git")).unwrap();
    let m = fs::symlink_metadata(s2.dir().join("info/exclude")).unwrap();
    assert!(m.is_file());
    assert_eq!(fs::read(s2.dir().join("info/exclude")).unwrap(), fs::read(&copy).unwrap());
    fs::remove_file(&mex).unwrap();
    mkfifo(&mex);
    let (g, s, ws) = w(&ctx);
    assert_eq!(within(5, move || g.shadow_drift(&s, &ws)).unwrap_err().code, Code::Unknown);
    let (g, s3) = (ctx.git.clone(), ctx.sb.path("s3.git"));
    let s3c = s3.clone();
    let e = within(5, move || g.ensure_shadow(&s3c)).unwrap_err();
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert!(!s3.exists());
    fs::remove_file(&mex).unwrap();

    // Admin index a FIFO.
    let ctx = setup();
    let ix = ctx.admin.join("index");
    fs::remove_file(&ix).unwrap();
    mkfifo(&ix);
    let (g, _, ws) = w(&ctx);
    assert_eq!(within(5, move || g.read_index(&ws)).unwrap_err().code, Code::Unknown);
    let (g, _, ws) = w(&ctx);
    assert_eq!(within(5, move || g.is_ignored(&ws, &rp("x"))).unwrap_err().code, Code::Unknown);

    // worktrees/<n> a symlink to a human linked worktree's admin dir.
    let ctx = setup_with(|sb, main| {
        sb.git(main, &["worktree", "add", "-q", "--detach", sb.path("human").to_str().unwrap()]);
    });
    fs::rename(&ctx.admin, ctx.shadow.dir().join("aside")).unwrap();
    symlink(ctx.main.join(".git/worktrees/human"), &ctx.admin).unwrap();
    let (g, _, ws) = w(&ctx);
    assert_eq!(within(5, move || g.read_index(&ws)).unwrap_err().code, Code::Unknown);
}

#[test]
fn read_index_reads_ignorecase_from_a_strata_built_shadow() {
    let ctx = setup_with(|sb, main| {
        sb.git(main, &["config", "core.ignorecase", "true"]);
    });
    let info = ctx.git.read_index(&ctx.ws).unwrap();
    assert!(info.ignorecase);
    assert!(info.is_tracked(&rp("readme.MD")));
    let cfg = ctx.shadow.dir().join("config");
    let text = fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("\tignorecase = true\n"), "{text}");
    for (line, want) in [
        ("\tignorecase = \"true\"\n", true),
        ("\tignorecase = true # x\n", true),
        ("\tignorecase\n", true),
        ("\tignorecase = false\n", false),
    ] {
        fs::write(&cfg, text.replace("\tignorecase = true\n", line)).unwrap();
        assert_eq!(ctx.git.read_index(&ctx.ws).unwrap().ignorecase, want, "{line:?}");
    }
    fs::write(&cfg, &text).unwrap();
    fs::write(ctx.ws.join(".git"), format!("gitdir: {}/\n", ctx.admin.display())).unwrap();
    assert!(ctx.git.read_index(&ctx.ws).unwrap().ignorecase);
}

#[test]
fn sessions_never_share_refs() {
    let ctx = setup();
    let sb = &ctx.sb;
    let b = ctx.git.ensure_shadow(&sb.path("b.git")).unwrap();
    let wsb = sb.path("ws-b");
    let tmp = ctx.temp_tree("tmp-b", &ctx.head);
    ctx.git.attach_worktree(&b, 1, &coid(&ctx.head), &tmp, &wsb).unwrap();
    fs::rename(&tmp, &wsb).unwrap();
    let refs = |d: &Path| sb.git(d, &["for-each-ref"]);
    let (b_before, main_before) = (refs(b.dir()), refs(&ctx.main));

    sb.commit_file(&ctx.ws, "a.txt", b"a\n", "local");
    fs::write(ctx.ws.join("README.md"), "dirty\n").unwrap();
    ctx.ws_git(&["stash", "-q"]);
    ctx.ws_git(&["branch", "agent-branch"]);
    ctx.ws_git(&["tag", "agent-tag"]);
    let a_refs = refs(ctx.shadow.dir());
    for r in ["refs/stash", "refs/heads/agent-branch", "refs/tags/agent-tag"] {
        assert!(a_refs.contains(r), "{a_refs}");
    }
    assert_eq!(refs(b.dir()), b_before);
    assert_eq!(refs(&ctx.main), main_before);
    assert_eq!(sb.git(&wsb, &["status", "--porcelain"]), "");
}

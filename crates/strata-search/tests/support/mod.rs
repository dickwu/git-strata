//! Shared fixtures for the search tests (copied from strata-session tests/support, plus the search pieces at the end). Every
//! repository, store and fake program lives in a temp dir; every git the tests start
//! gets a sandbox HOME / XDG_CONFIG_HOME and GIT_CONFIG_NOSYSTEM=1.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use strata_deps::InstallEnv;
use strata_gitio::Git;
use strata_types::{PublishTarget, RepoConfig, RepoName, RepoPath};

pub fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

pub fn rps(v: &[&str]) -> Vec<RepoPath> {
    v.iter().map(|s| rp(s)).collect()
}

pub fn repo_name(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}

/// A temp dir with a sandbox HOME, a fake `bun` and helpers for git.
pub struct Sandbox {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub xdg: PathBuf,
    /// Directory holding the fake `bun`.
    pub bin: PathBuf,
    /// Where the fake `bun` records its runs.
    pub state: PathBuf,
}

/// Sealed sets and trash entries are 0555: make every directory writable
/// again so the `TempDir` can remove the sandbox (no leaked temp dirs).
impl Drop for Sandbox {
    fn drop(&mut self) {
        fn writable(p: &Path) {
            let Ok(m) = fs::symlink_metadata(p) else {
                return;
            };
            if !m.is_dir() {
                return;
            }
            let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    writable(&e.path());
                }
            }
        }
        writable(&self.root);
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::Builder::new()
            .prefix("strata-search-")
            .tempdir()
            .expect("tempdir");
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(&xdg).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Strata Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let bin = root.join("bin");
        let state = root.join("bun-state");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&state).unwrap();
        let sb = Sandbox {
            _dir: dir,
            root,
            home,
            xdg,
            bin,
            state,
        };
        sb.set_bun_version("1.3.3");
        sb.set_install_script(DEFAULT_INSTALL);
        sb
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Env for `Git::open_with_env`.
    pub fn git_env(&self) -> Vec<(&'static str, Option<&OsStr>)> {
        vec![
            ("HOME", Some(self.home.as_os_str())),
            ("XDG_CONFIG_HOME", Some(self.xdg.as_os_str())),
            ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
        ]
    }

    pub fn open(&self, root: &Path) -> Git {
        Git::open_with_env(root, &self.git_env())
            .unwrap_or_else(|e| panic!("open {}: {e:?}", root.display()))
    }

    pub fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        c.current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        c
    }

    pub fn git_output(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(cwd).args(args).output().expect("spawn git")
    }

    /// Run git, assert success, return trimmed stdout.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.git_output(cwd, args);
        assert!(
            o.status.success(),
            "git {args:?} in {} failed: {}",
            cwd.display(),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim_end().to_string()
    }

    pub fn write(&self, dir: &Path, rel: &str, content: &[u8]) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Write, add and commit files; returns the new HEAD.
    pub fn commit(&self, repo: &Path, files: &[(&str, &[u8])], msg: &str) -> String {
        for (rel, content) in files {
            self.write(repo, rel, content);
            self.git(repo, &["add", "-f", "--", rel]);
        }
        self.git(repo, &["commit", "-q", "--allow-empty", "-m", msg]);
        self.head(repo)
    }

    pub fn head(&self, repo: &Path) -> String {
        self.git(repo, &["rev-parse", "HEAD"])
    }

    pub fn tree_of(&self, repo: &Path, rev: &str) -> String {
        self.git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    /// A main checkout `name` with the standard bun project committed.
    pub fn init_project(&self, name: &str) -> PathBuf {
        let p = self.root.join(name);
        fs::create_dir_all(&p).unwrap();
        self.git(&p, &["init", "-q"]);
        self.commit(
            &p,
            &[
                (".gitignore", b".DS_Store\nnode_modules/\n.cache/\n"),
                ("package.json", PACKAGE_JSON.as_bytes()),
                ("bun.lock", b"lock v1\n"),
                ("patches/a.patch", b"patch a\n"),
                ("src/index.ts", b"export {}\n"),
            ],
            "initial",
        );
        p
    }

    /// Extract `treeish` of `repo` into `dir` without any `.git`.
    pub fn archive_into(&self, repo: &Path, treeish: &str, dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let o = self.git_output(repo, &["archive", "--format=tar", treeish]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let mut tar = Command::new("tar")
            .arg("-xf")
            .arg("-")
            .arg("-C")
            .arg(dir)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        tar.stdin.take().unwrap().write_all(&o.stdout).unwrap();
        assert!(tar.wait().unwrap().success());
    }

    /// A bare `origin` seeded from `repo`'s main branch.
    pub fn bare_origin(&self, repo: &Path) -> PathBuf {
        let name = repo.file_name().unwrap().to_string_lossy();
        let p = self.root.join(format!("{name}-origin.git"));
        self.git(&self.root, &["init", "-q", "--bare", p.to_str().unwrap()]);
        self.git(repo, &["remote", "add", "origin", p.to_str().unwrap()]);
        self.git(repo, &["push", "-q", "origin", "main"]);
        p
    }

    /// The PATH installs run with.
    pub fn install_path(&self) -> String {
        format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.bin.display())
    }

    pub fn install_env(&self) -> InstallEnv {
        InstallEnv {
            path: self.install_path(),
            timeout: Duration::from_secs(60),
        }
    }

    /// Write `bin/<name>` atomically (new inode) with `body` and mode 0755.
    pub fn write_program(&self, name: &str, body: &str) {
        let tmp = self.bin.join(format!(".{name}.tmp"));
        fs::write(&tmp, body).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&tmp, self.bin.join(name)).unwrap();
    }

    /// The fake `bun`: `--version` prints `version`; anything else records
    /// its run under `state/` and sources `state/install.sh` in its cwd.
    pub fn set_bun_version(&self, version: &str) {
        let st = self.state.display();
        self.write_program(
            "bun",
            &format!(
                r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "{version}"; exit 0; fi
STATE="{st}"
n=$(cat "$STATE/count" 2>/dev/null || echo 0)
n=$((n+1))
echo "$n" > "$STATE/count"
pwd -P > "$STATE/cwd.$n"
env > "$STATE/env.$n"
echo "$*" > "$STATE/args.$n"
stat -L -f '%Z' /dev/fd/0 > "$STATE/stdin.$n" 2>/dev/null
echo $$ > "$STATE/pid.$n"
perl -MTime::HiRes=time -e 'printf "%.6f\n", time' > "$STATE/start.$n"
. "$STATE/install.sh"
rc=$?
perl -MTime::HiRes=time -e 'printf "%.6f\n", time' > "$STATE/end.$n"
exit $rc
"#
            ),
        );
    }

    /// The body `bun install` runs (sourced; `$STATE` and `$n` are set).
    pub fn set_install_script(&self, body: &str) {
        fs::write(self.state.join("install.sh"), body).unwrap();
    }

    /// How many installs the fake `bun` has run.
    pub fn install_count(&self) -> u32 {
        fs::read_to_string(self.state.join("count"))
            .map(|s| s.trim().parse().unwrap())
            .unwrap_or(0)
    }

    pub fn install_file(&self, what: &str, n: u32) -> String {
        fs::read_to_string(self.state.join(format!("{what}.{n}"))).unwrap_or_default()
    }

    /// Repo configuration for a bun project at `path`.
    pub fn config(&self, path: &Path) -> RepoConfig {
        RepoConfig {
            path: path.to_path_buf(),
            default_branch: "main".into(),
            deps_dirs: rps(&["node_modules"]),
            seed_caches: rps(&[".cache"]),
            seed_files: None,
            install_inputs: rps(&["package.json", "bun.lock", "patches"]),
            install: Some("bun install --frozen-lockfile".into()),
            checks: vec![],
            publish: PublishTarget {
                branch: "main".into(),
                push: true,
                confirm: false,
            },
            deploy_on_push: false,
            pin_unpushed: true,
            check_siblings: Vec::new(),
        }
    }
}

pub const PACKAGE_JSON: &str = r#"{
  "name": "tool",
  "version": "1.0.87",
  "dependencies": {
    "x": "^1.0.0"
  }
}
"#;

/// Default install: one package plus a `.bin` link, like a tiny bun install.
pub const DEFAULT_INSTALL: &str = r#"mkdir -p node_modules/x node_modules/.bin || exit 1
echo 'module.exports = 1' > node_modules/x/index.js
echo '{"name":"x","version":"1.0.0"}' > node_modules/x/package.json
ln -sf ../x/index.js node_modules/.bin/x
echo "installed"
"#;

// ---- store, the repo write lock double, the BeforeCheckpoint double ----

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;

use strata_deps::RepoWriteLock;
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_session::{BeforeCheckpoint, Sessions};
use strata_store::Store;
use strata_types::{CheckpointReason, Code, Config, Paths, SessionId, StrataError};

thread_local! {
    static HELD: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

type Hook = Box<dyn FnMut(&RepoName) + Send>;

/// One `with_write` critical section, as recorded.
#[derive(Clone, Debug)]
pub struct Hold {
    pub repo: RepoName,
    pub thread: ThreadId,
}

/// The repo write lock double (§8 Task 1): one lock per repo, records every
/// hold, a bounded wait that ends in `Busy`, and a second lock on the same
/// thread fails at once (panics) instead of hanging.
pub struct TestLock {
    held: Mutex<BTreeMap<String, ThreadId>>,
    cv: Condvar,
    pub wait: Mutex<Duration>,
    /// The next n calls return `Busy` without waiting.
    pub busy_next: AtomicU32,
    pub holds: Mutex<Vec<Hold>>,
    pub on_enter: Mutex<Option<Hook>>,
}

impl Default for TestLock {
    fn default() -> Self {
        TestLock {
            held: Mutex::new(BTreeMap::new()),
            cv: Condvar::new(),
            wait: Mutex::new(Duration::from_secs(60)),
            busy_next: AtomicU32::new(0),
            holds: Mutex::new(Vec::new()),
            on_enter: Mutex::new(None),
        }
    }
}

impl TestLock {
    pub fn holds(&self) -> Vec<Hold> {
        self.holds.lock().unwrap().clone()
    }

    pub fn holds_of(&self, repo: &str) -> usize {
        self.holds().iter().filter(|h| h.repo.as_str() == repo).count()
    }

    pub fn set_wait(&self, d: Duration) {
        *self.wait.lock().unwrap() = d;
    }

    pub fn set_busy(&self, n: u32) {
        self.busy_next.store(n, Ordering::SeqCst);
    }

    pub fn on_enter(&self, h: impl FnMut(&RepoName) + Send + 'static) {
        *self.on_enter.lock().unwrap() = Some(Box::new(h));
    }

    /// Hold `repo`'s lock from the test until the guard drops.
    pub fn hold(&self, repo: &str) -> HoldGuard<'_> {
        let mut held = self.held.lock().unwrap();
        while held.contains_key(repo) {
            held = self.cv.wait(held).unwrap();
        }
        held.insert(repo.to_string(), std::thread::current().id());
        HoldGuard { lock: self, repo: repo.to_string() }
    }
}

pub struct HoldGuard<'a> {
    lock: &'a TestLock,
    repo: String,
}

impl Drop for HoldGuard<'_> {
    fn drop(&mut self) {
        self.lock.held.lock().unwrap().remove(&self.repo);
        self.lock.cv.notify_all();
    }
}

impl RepoWriteLock for TestLock {
    fn with_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        let mine = HELD.with(|h| h.borrow().clone());
        assert!(
            mine.is_empty(),
            "RepoWriteLock re-entered on one thread: holding {mine:?}, asked for {repo}"
        );
        if self.busy_next.load(Ordering::SeqCst) > 0 {
            self.busy_next.fetch_sub(1, Ordering::SeqCst);
            return Err(StrataError::new(Code::Busy, format!("repo {repo} write lock is busy")));
        }
        let wait = *self.wait.lock().unwrap();
        let deadline = std::time::Instant::now() + wait;
        let mut held = self.held.lock().unwrap();
        while held.contains_key(repo.as_str()) {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(StrataError::new(Code::Busy, format!("repo {repo} write lock is busy")));
            }
            held = self.cv.wait_timeout(held, deadline - now).unwrap().0;
        }
        held.insert(repo.as_str().to_string(), std::thread::current().id());
        drop(held);
        HELD.with(|h| h.borrow_mut().insert(repo.as_str().to_string()));
        self.holds.lock().unwrap().push(Hold { repo: repo.clone(), thread: std::thread::current().id() });
        let hook = self.on_enter.lock().unwrap().take();
        if let Some(mut h) = hook {
            h(repo);
            let mut s = self.on_enter.lock().unwrap();
            if s.is_none() {
                *s = Some(h);
            }
        }
        let r = f();
        HELD.with(|h| h.borrow_mut().remove(repo.as_str()));
        self.held.lock().unwrap().remove(repo.as_str());
        self.cv.notify_all();
        Ok(r)
    }
}

/// The `BeforeCheckpoint` double: no Tracker, forwards to
/// `overlay::checkpoint` and counts the calls.
#[derive(Default)]
pub struct CountingBefore {
    pub calls: AtomicU32,
    pub reasons: Mutex<Vec<CheckpointReason>>,
}

impl CountingBefore {
    pub fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl BeforeCheckpoint for CountingBefore {
    fn checkpoint(
        &self,
        ws: &WorkspaceRef<'_>,
        base: &Base,
        reason: CheckpointReason,
    ) -> strata_session::Result<Checkpointed> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reasons.lock().unwrap().push(reason.clone());
        strata_overlay::checkpoint(ws, base, None, reason)
    }
}

/// Repos, origins, config, Git handles, store, lock and before-checkpoint:
/// everything `Sessions::open` borrows.
pub struct Fx {
    pub sb: Sandbox,
    pub names: Vec<String>,
    pub config: Config,
    pub gits: BTreeMap<RepoName, Git>,
    pub env: InstallEnv,
    pub lock: TestLock,
    pub before: CountingBefore,
    store: Option<Arc<Store>>,
}

impl Fx {
    /// `tool` and `front`, both bun projects with an origin.
    pub fn new() -> Fx {
        Fx::with(&["front", "tool"])
    }

    pub fn with(names: &[&str]) -> Fx {
        let sb = Sandbox::new();
        let mut repos = BTreeMap::new();
        for n in names {
            let main = sb.init_project(n);
            sb.bare_origin(&main);
            repos.insert(repo_name(n), sb.config(&main));
        }
        let root = sb.root.join("strata");
        let config = Config { paths: Paths { root }, repos };
        Fx::from_parts(sb, config)
    }

    /// Open the store and the Git handles for `config`.
    pub fn from_parts(sb: Sandbox, config: Config) -> Fx {
        let checkouts: Vec<&Path> = config.repos.values().map(|r| r.path.as_path()).collect();
        let store = Store::open(&config.paths.root, &checkouts).expect("open store");
        let mut fx = Fx {
            names: config.repos.keys().map(|r| r.as_str().to_string()).collect(),
            gits: BTreeMap::new(),
            env: sb.install_env(),
            lock: TestLock::default(),
            before: CountingBefore::default(),
            store: Some(Arc::new(store)),
            config,
            sb,
        };
        fx.reopen_gits();
        fx
    }

    pub fn reopen_gits(&mut self) {
        self.gits = self
            .config
            .repos
            .iter()
            .map(|(n, r)| (n.clone(), self.sb.open(&r.path)))
            .collect();
    }

    pub fn sessions(&self) -> Sessions<'_> {
        Sessions::open(&self.config, &self.gits, self.store(), &self.lock, &self.env, &self.before)
            .expect("Sessions::open")
    }

    pub fn store(&self) -> &Store {
        self.store.as_ref().expect("store is open")
    }

    pub fn close_store(&mut self) {
        let s = self.store.take().expect("store is open");
        Arc::try_unwrap(s).ok().expect("store still shared").close().unwrap();
    }

    pub fn reopen_store(&mut self) {
        assert!(self.store.is_none());
        let checkouts: Vec<&Path> = self.config.repos.values().map(|r| r.path.as_path()).collect();
        // Other test threads fork children (git, install scripts) that hold
        // the old LOCK description between fork and exec, so the flock can
        // read Busy for a moment after close: retry briefly.
        let deadline = Instant::now() + Duration::from_secs(5);
        let st = loop {
            match Store::open(&self.config.paths.root, &checkouts) {
                Ok(s) => break s,
                Err(e) if e.code == Code::Busy && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("reopen store: {e:?}"),
            }
        };
        self.store = Some(Arc::new(st));
    }

    pub fn root(&self) -> PathBuf {
        self.config.paths.root.clone()
    }

    pub fn main(&self, repo: &str) -> PathBuf {
        self.config.repos[&repo_name(repo)].path.clone()
    }

    pub fn cfg_mut(&mut self, repo: &str) -> &mut RepoConfig {
        self.config.repos.get_mut(&repo_name(repo)).unwrap()
    }

    pub fn rn(&self, repo: &str) -> RepoName {
        repo_name(repo)
    }

    pub fn all(&self) -> Vec<RepoName> {
        self.names.iter().map(|n| repo_name(n)).collect()
    }

    /// Entries under `<root>/sessions` (including `.repos/*`), sorted.
    pub fn session_dirs(&self) -> Vec<String> {
        let s = self.root().join("sessions");
        let mut v = Vec::new();
        if let Ok(rd) = fs::read_dir(&s) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n == ".repos" {
                    if let Ok(rd) = fs::read_dir(e.path()) {
                        for e in rd.flatten() {
                            v.push(format!(".repos/{}", e.file_name().to_string_lossy()));
                        }
                    }
                } else {
                    v.push(n);
                }
            }
        }
        v.sort();
        v
    }

    pub fn session_rows(&self) -> usize {
        self.store().sessions().unwrap().len()
    }

    /// Every ref in `repo`'s main checkout except the remote-tracking ref
    /// gitio's fetch writes.
    pub fn refs(&self, repo: &str) -> String {
        self.sb
            .git(&self.main(repo), &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .filter(|l| !l.starts_with("refs/strata/remotes/"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn pins(&self, repo: &str) -> String {
        self.sb.git(&self.main(repo), &["for-each-ref", "--format=%(refname) %(objectname)", "refs/strata/pins/"])
    }

    /// One critical section under `repo`'s lock from the test, with the
    /// lock's current bounded wait.
    pub fn lock_write(&self, repo: &RepoName, mut f: impl FnMut()) -> strata_deps::Result<()> {
        self.lock.with_write(repo, &mut || {
            f();
            Ok(())
        })?
    }

    /// What must not change in a main checkout when `new` refuses.
    pub fn main_snap(&self, repo: &str) -> MainSnap {
        MainSnap::take(self, repo)
    }
}

impl Default for Fx {
    fn default() -> Self {
        Self::new()
    }
}

/// (relative path, mtime s, mtime ns, ctime s, ctime ns, contents)
pub type TreeEntry = (String, i64, i64, i64, i64, Vec<u8>);

/// Worktree entries (mtime, ctime, bytes), index, config, hooks, info and
/// refs of a main checkout.
#[derive(Debug, PartialEq, Eq)]
pub struct MainSnap {
    pub tree: Vec<TreeEntry>,
    pub git: Vec<Entry>,
    pub index: (i64, i64, Vec<u8>),
    pub refs: String,
}

impl MainSnap {
    pub fn take(fx: &Fx, repo: &str) -> MainSnap {
        MainSnap::at(&fx.sb, &fx.main(repo))
    }

    pub fn at(sb: &Sandbox, main: &Path) -> MainSnap {
        let main = main.to_path_buf();
        let mut tree = Vec::new();
        fn go(base: &Path, d: &Path, out: &mut Vec<TreeEntry>) {
            let Ok(rd) = fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel == ".git" {
                    continue;
                }
                let m = fs::symlink_metadata(&p).unwrap();
                let bytes = if m.is_file() { fs::read(&p).unwrap_or_default() } else { Vec::new() };
                out.push((rel, m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec(), bytes));
                if m.is_dir() {
                    go(base, &p, out);
                }
            }
        }
        go(&main, &main, &mut tree);
        tree.sort();
        let common = sb.git(&main, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        let g = PathBuf::from(common);
        let gitdir = PathBuf::from(sb.git(&main, &["rev-parse", "--absolute-git-dir"]));
        let mut git = Vec::new();
        for sub in ["hooks", "info"] {
            for mut e in snapshot(&g.join(sub)) {
                e.0 = format!("{sub}/{}", e.0);
                git.push(e);
            }
        }
        git.push(("config".into(), "file".into(), 0, 0, String::new(), fs::read(g.join("config")).unwrap()));
        let im = fs::metadata(gitdir.join("index")).unwrap();
        MainSnap {
            tree,
            git,
            index: (im.mtime(), im.mtime_nsec(), fs::read(gitdir.join("index")).unwrap()),
            refs: sb
                .git(&main, &["for-each-ref", "--format=%(refname) %(objectname)"])
                .lines()
                .filter(|l| !l.starts_with("refs/strata/remotes/"))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

pub fn sid(s: &str) -> SessionId {
    SessionId::try_from(s.to_string()).unwrap()
}

pub fn detail_str<'a>(e: &'a StrataError, key: &str) -> Option<&'a str> {
    e.detail.as_ref().and_then(|d| d.get(key)).and_then(|v| v.as_str())
}

pub fn reason(e: &StrataError) -> Option<&str> {
    detail_str(e, "reason")
}

pub fn mkfifo(p: &Path) {
    let st = Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

pub type Entry = (String, String, u64, u32, String, Vec<u8>);

/// Every entry under `dir`: path, kind, size, mode, link target, bytes.
pub fn snapshot(dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    fn go(base: &Path, d: &Path, out: &mut Vec<Entry>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd {
            let e = e.unwrap();
            let p = e.path();
            let m = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let ft = m.file_type();
            let (kind, link, bytes) = if ft.is_symlink() {
                ("link", fs::read_link(&p).unwrap().to_string_lossy().into_owned(), Vec::new())
            } else if ft.is_dir() {
                ("dir", String::new(), Vec::new())
            } else if ft.is_file() {
                ("file", String::new(), fs::read(&p).unwrap_or_default())
            } else {
                ("special", String::new(), Vec::new())
            };
            out.push((rel, kind.to_string(), if ft.is_dir() { 0 } else { m.len() }, m.permissions().mode(), link, bytes));
            if ft.is_dir() {
                go(base, &p, out);
            }
        }
    }
    go(dir, dir, &mut out);
    out.sort();
    out
}

/// A disk image attached for the duration of a test (hdiutil); detached on
/// drop. Tests that use it are `#[ignore]`: attaching needs a human-approved
/// environment.
pub struct DiskImage {
    pub mount: PathBuf,
    dev: String,
}

impl DiskImage {
    pub fn new(sb: &Sandbox, name: &str, fs_type: &str) -> DiskImage {
        let img = sb.root.join(format!("{name}.dmg"));
        let mount = sb.root.join(format!("{name}-mnt"));
        fs::create_dir_all(&mount).unwrap();
        let st = Command::new("hdiutil")
            .args(["create", "-quiet", "-size", "64m", "-fs", fs_type, "-volname", name])
            .arg(&img)
            .status()
            .unwrap();
        assert!(st.success(), "hdiutil create");
        let out = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-noautoopen", "-mountpoint"])
            .arg(&mount)
            .arg(&img)
            .output()
            .unwrap();
        assert!(out.status.success(), "hdiutil attach: {}", String::from_utf8_lossy(&out.stderr));
        let dev = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().next())
            .unwrap()
            .to_string();
        DiskImage { mount: fs::canonicalize(&mount).unwrap(), dev }
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil").args(["detach", "-force", "-quiet", &self.dev]).status();
    }
}

impl Fx {
    /// `from_parts`, for configurations whose store root is somewhere a
    /// checkout-volume check would refuse (the store is opened without
    /// checkouts).
    pub fn from_parts_unchecked(sb: Sandbox, config: Config) -> Fx {
        let store = Store::open(&config.paths.root, &[]).expect("open store");
        let mut fx = Fx {
            names: config.repos.keys().map(|r| r.as_str().to_string()).collect(),
            gits: BTreeMap::new(),
            env: sb.install_env(),
            lock: TestLock::default(),
            before: CountingBefore::default(),
            store: Some(Arc::new(store)),
            config,
            sb,
        };
        fx.reopen_gits();
        fx
    }
}

// ------------------------------------------------------------ search pieces

use strata_overlay::Overlay;
use strata_search::OverlaySource;
use strata_session::NewReport;
use strata_types::{BaseGen, WorkspaceState};

/// A fake `OverlaySource`: tests put the overlay they want here (usually a
/// real `strata_overlay::checkpoint` of the workspace, see `Fx::capture`).
#[derive(Default)]
pub struct FakeOverlays {
    pub map: Mutex<BTreeMap<(SessionId, RepoName), Overlay>>,
    pub full: std::sync::atomic::AtomicBool,
    /// Run (once) at the start of the next `last`: lets a test act in the
    /// middle of an attach / refresh.
    pub hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl FakeOverlays {
    pub fn set(&self, id: &SessionId, repo: &str, o: Overlay) {
        self.map.lock().unwrap().insert((id.clone(), repo_name(repo)), o);
    }
    pub fn clear(&self, id: &SessionId, repo: &str) {
        self.map.lock().unwrap().remove(&(id.clone(), repo_name(repo)));
    }
    pub fn on_next_last(&self, f: impl FnOnce() + Send + 'static) {
        *self.hook.lock().unwrap() = Some(Box::new(f));
    }
}

impl OverlaySource for FakeOverlays {
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay> {
        let hook = self.hook.lock().unwrap().take();
        if let Some(h) = hook {
            h();
        }
        self.map.lock().unwrap().get(&(session.clone(), repo.clone())).cloned()
    }
    fn needs_full(&self, _session: &SessionId, _repo: &RepoName) -> bool {
        self.full.load(Ordering::SeqCst)
    }
}

impl Fx {
    /// A new session over `repos` (default: all).
    pub fn new_session(&self, repos: &[&str]) -> NewReport {
        let rs: Vec<RepoName> = repos.iter().map(|r| repo_name(r)).collect();
        self.sessions().new(None, &rs).expect("new session")
    }

    pub fn ws(&self, rep: &NewReport, repo: &str) -> WorkspaceState {
        rep.manifest.repos[&repo_name(repo)].clone()
    }

    /// The overlay of the workspace right now, from a real checkpoint.
    pub fn capture(&self, id: &SessionId, repo: &str) -> Overlay {
        let r = repo_name(repo);
        let st = self.sessions().status(id).expect("status");
        let ws = st.manifest.repos[&r].clone();
        self.capture_on(id, repo, &ws.path, &ws.base)
    }

    pub fn capture_on(&self, id: &SessionId, repo: &str, root: &Path, g: &BaseGen) -> Overlay {
        let r = repo_name(repo);
        let wr = WorkspaceRef {
            session: id,
            repo: &r,
            root,
            config: &self.config.repos[&r],
            git: &self.gits[&r],
            store: self.store(),
        };
        let manifest = self.store().manifest(&r, &g.tree).expect("manifest").expect("manifest row");
        let base = Base { r#gen: g.clone(), manifest: Arc::new(manifest), dir: self.store().base_dir(g) };
        let c = strata_overlay::checkpoint(&wr, &base, None, CheckpointReason::Manual { message: "search test".into() })
            .expect("checkpoint");
        Overlay { base: c.checkpoint.base, entries: c.checkpoint.entries }
    }
}

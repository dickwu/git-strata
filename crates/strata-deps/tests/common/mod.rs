//! Shared fixtures for the deps tests (deps.md §7). Every repository,
//! store and fake program lives in a temp dir; every git the tests start
//! gets a sandbox HOME / XDG_CONFIG_HOME and GIT_CONFIG_NOSYSTEM=1.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

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
            let Ok(m) = fs::symlink_metadata(p) else { return };
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
            .prefix("strata-deps-")
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
            self.git(repo, &["add", "--", rel]);
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
        let p = self.root.join("origin.git");
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

// ---- Task 3+: store, context and the repo write lock double ----

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use strata_deps::{Ctx, RepoWriteLock};
use strata_store::Store;
use strata_types::{Code, Paths, StrataError};

type Hook = Box<dyn FnMut() + Send>;

/// The recording `RepoWriteLock` double (§7): a real mutex, `busy`
/// injected `Busy` returns, enter/exit hooks; released even when a panic
/// passes through.
#[derive(Default)]
pub struct RecLock {
    m: Mutex<()>,
    /// Return `Busy` (without running `f`) this many more times.
    pub busy: AtomicU32,
    /// Calls that ran `f`.
    pub enters: AtomicU32,
    /// Calls that returned `Busy`.
    pub busies: AtomicU32,
    pub on_enter: Mutex<Option<Hook>>,
    pub on_exit: Mutex<Option<Hook>>,
}

impl RecLock {
    pub fn enters(&self) -> u32 {
        self.enters.load(Ordering::SeqCst)
    }

    pub fn set_busy(&self, n: u32) {
        self.busy.store(n, Ordering::SeqCst);
    }

    pub fn on_enter(&self, h: impl FnMut() + Send + 'static) {
        *self.on_enter.lock().unwrap() = Some(Box::new(h));
    }

    pub fn on_exit(&self, h: impl FnMut() + Send + 'static) {
        *self.on_exit.lock().unwrap() = Some(Box::new(h));
    }

    fn call(slot: &Mutex<Option<Hook>>) {
        let h = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(mut h) = h {
            h();
            let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
            if s.is_none() {
                *s = Some(h);
            }
        }
    }
}

impl RepoWriteLock for RecLock {
    fn with_write(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_deps::Result<()>,
    ) -> strata_deps::Result<strata_deps::Result<()>> {
        let left = self.busy.load(Ordering::SeqCst);
        if left > 0 {
            self.busy.store(left - 1, Ordering::SeqCst);
            self.busies.fetch_add(1, Ordering::SeqCst);
            return Err(StrataError::new(
                Code::Busy,
                format!("repo {} write lock is busy", repo.as_str()),
            ));
        }
        let _g = self.m.lock().unwrap_or_else(|p| p.into_inner());
        self.enters.fetch_add(1, Ordering::SeqCst);
        Self::call(&self.on_enter);
        let r = f();
        Self::call(&self.on_exit);
        Ok(r)
    }
}

/// A sandbox with a main checkout `tool`, a store at `<root>/strata` and
/// everything a `Ctx` borrows.
pub struct Fx {
    pub sb: Sandbox,
    pub main: PathBuf,
    pub paths: Paths,
    pub repo: RepoName,
    pub cfg: RepoConfig,
    pub git: Git,
    pub env: InstallEnv,
    pub lock: RecLock,
    store: Option<Arc<Store>>,
}

impl Default for Fx {
    fn default() -> Self {
        Self::new()
    }
}

impl Fx {
    pub fn new() -> Fx {
        let sb = Sandbox::new();
        let main = sb.init_project("tool");
        let root = sb.root.join("strata");
        let store = Store::open(&root, &[&main]).expect("open store");
        let git = sb.open(&main);
        let cfg = sb.config(&main);
        let env = sb.install_env();
        Fx {
            main,
            paths: Paths { root },
            repo: repo_name("tool"),
            cfg,
            git,
            env,
            lock: RecLock::default(),
            store: Some(Arc::new(store)),
            sb,
        }
    }

    pub fn store(&self) -> &Store {
        self.store.as_ref().expect("store is open")
    }

    pub fn store_arc(&self) -> Arc<Store> {
        self.store.as_ref().expect("store is open").clone()
    }

    pub fn close_store(&mut self) {
        let s = self.store.take().expect("store is open");
        Arc::try_unwrap(s)
            .ok()
            .expect("store still shared")
            .close()
            .unwrap();
    }

    pub fn reopen_store(&mut self) {
        assert!(self.store.is_none());
        self.store = Some(Arc::new(
            Store::open(&self.paths.root, &[&self.main]).expect("reopen store"),
        ));
    }

    pub fn ctx(&self) -> Ctx<'_> {
        Ctx {
            store: self.store(),
            paths: &self.paths,
            repo: &self.repo,
            cfg: &self.cfg,
            git: &self.git,
            env: &self.env,
            locks: &self.lock,
        }
    }

    /// The main checkout's HEAD exported to `<root>/trees/<name>`.
    pub fn tree(&self, name: &str) -> PathBuf {
        let dir = self.sb.root.join("trees").join(name);
        self.sb.archive_into(&self.main, "HEAD", &dir);
        dir
    }

    /// A tree whose `package.json` differs by `tag` (its own deps set).
    pub fn tree_tagged(&self, name: &str, tag: &str) -> PathBuf {
        let dir = self.tree(name);
        fs::write(
            dir.join("package.json"),
            format!("{{\n  \"name\": \"tool\",\n  \"dependencies\": {{ \"x\": \"^1.0.0\", \"{tag}\": \"1.0.0\" }}\n}}\n"),
        )
        .unwrap();
        dir
    }

    pub fn deps_root(&self) -> PathBuf {
        self.paths.deps()
    }

    pub fn repo_dir(&self) -> PathBuf {
        self.paths.deps().join("tool")
    }

    pub fn lock_path(&self, id: &strata_types::DepsSetId) -> PathBuf {
        self.repo_dir()
            .join(".locks")
            .join(format!("{}.lock", id.as_str()))
    }

    pub fn trash(&self) -> PathBuf {
        self.paths.root.join("store").join("trash")
    }

    pub fn trash_count(&self) -> usize {
        fs::read_dir(self.trash()).map(|d| d.count()).unwrap_or(0)
    }

    pub fn staging_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(self.repo_dir().join(".staging"))
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

/// `(relative path, kind, size, mode, readlink, bytes)` of every entry under
/// `dir`, sorted; for before/after comparisons.
pub type Entry = (String, String, u64, u32, String, Vec<u8>);

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
                (
                    "link",
                    fs::read_link(&p).unwrap().to_string_lossy().into_owned(),
                    Vec::new(),
                )
            } else if ft.is_dir() {
                ("dir", String::new(), Vec::new())
            } else if ft.is_file() {
                ("file", String::new(), fs::read(&p).unwrap_or_default())
            } else {
                ("special", String::new(), Vec::new())
            };
            out.push((
                rel,
                kind.to_string(),
                if ft.is_dir() { 0 } else { m.len() },
                m.permissions().mode(),
                link,
                bytes,
            ));
            if ft.is_dir() {
                go(base, &p, out);
            }
        }
    }
    go(dir, dir, &mut out);
    out.sort();
    out
}

/// Make a sealed (0555) tree writable again and delete it.
pub fn force_remove(p: &Path) {
    let _ = Command::new("chmod").arg("-R").arg("u+w").arg(p).status();
    let _ = fs::remove_dir_all(p);
}

pub fn wait_for_file(p: &Path, limit: Duration) {
    let start = std::time::Instant::now();
    while !p.exists() {
        assert!(
            start.elapsed() < limit,
            "timed out waiting for {}",
            p.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

pub fn group_alive(pgid: i32) -> bool {
    // SAFETY: as above.
    unsafe { libc::killpg(pgid, 0) == 0 }
}

pub fn detail_str<'a>(e: &'a StrataError, key: &str) -> Option<&'a str> {
    e.detail
        .as_ref()
        .and_then(|d| d.get(key))
        .and_then(|v| v.as_str())
}

pub fn reason(e: &StrataError) -> Option<&str> {
    detail_str(e, "reason")
}

pub fn mode_of(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

/// The repo config with no Sandbox (for worker processes).
pub fn bun_config(path: &Path) -> RepoConfig {
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
    }
}

pub const WORKER_ENV: &str = "STRATA_DEPS_WORKER";

pub fn worker_mode() -> Option<String> {
    std::env::var(WORKER_ENV).ok()
}

/// Re-run this test binary's test `name` as a worker in its own process
/// group, with `vars`.
pub fn spawn_worker(name: &str, mode: &str, vars: &[(&str, &OsStr)]) -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(WORKER_ENV, mode)
        .stdin(Stdio::null())
        .process_group(0);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.spawn().expect("spawn worker")
}

/// Worker mode "sock": bind a Unix socket named `$STRATA_SOCK` relative to
/// the worker's cwd (sockaddr paths are short; the sandbox path is not).
pub fn sock_worker() -> ! {
    let name = std::env::var("STRATA_SOCK").unwrap();
    let l = std::os::unix::net::UnixListener::bind(&name).expect("bind socket");
    std::mem::forget(l);
    std::process::exit(0);
}

/// Create a Unix socket at `dir/name` via a worker of this test binary.
pub fn make_socket(dir: &Path, name: &str) {
    let st = Command::new(std::env::current_exe().unwrap())
        .args(["worker", "--exact", "--nocapture", "--test-threads=1"])
        .env(WORKER_ENV, "sock")
        .env("STRATA_SOCK", name)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
    let m = fs::symlink_metadata(dir.join(name)).unwrap();
    assert!(std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()));
}

pub fn mkfifo(p: &Path) {
    let st = Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

/// Attributes of every entry under `root/dir` (gen counts included).
pub fn apfs_walk(root: &Path, dir: &str) -> Vec<(String, strata_apfs::Attrs)> {
    let opts = strata_apfs::WalkOptions {
        start: Some(rp(dir)),
        ..Default::default()
    };
    let all = |_: &Path, _: &strata_apfs::Attrs| true;
    let mut v: Vec<(String, strata_apfs::Attrs)> = strata_apfs::walk_with(root, &opts, &all)
        .map(|r| {
            let (p, a) = r.unwrap();
            (p.as_str().to_string(), a)
        })
        .collect();
    v.push((
        dir.to_string(),
        strata_apfs::attrs_of(root, &rp(dir)).unwrap(),
    ));
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

impl Fx {
    /// The main checkout's `node_modules` like a finished install.
    pub fn main_deps(&self) {
        let nm = self.main.join("node_modules");
        fs::create_dir_all(nm.join("a")).unwrap();
        fs::create_dir_all(nm.join(".bin")).unwrap();
        fs::write(nm.join("a/index.js"), b"module.exports = 'a'\n").unwrap();
        fs::write(nm.join("a/package.json"), b"{\"name\":\"a\"}\n").unwrap();
        std::os::unix::fs::symlink("../a/index.js", nm.join(".bin/a")).unwrap();
    }
}

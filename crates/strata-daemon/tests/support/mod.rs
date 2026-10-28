//! Shared fixtures for the daemon tests (daemon.md §7, §8). The sandbox
//! part is the publish test support's: every repository, store, config dir
//! and fake program lives in a temp dir; every git gets a sandbox HOME /
//! XDG_CONFIG_HOME and GIT_CONFIG_NOSYSTEM=1. The socket lives in a short
//! temp dir under /tmp (sockaddr_un is 104 bytes on macOS).
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
            .prefix("strata-daemon-")
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

// ---- the daemon fixture ----

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use strata_confirm::Confirmer;
use strata_confirm::seams::{FakeClock, PromptLog, ScriptedAuth, Verdict};
use strata_daemon::startup::{self, Core, Options};
use strata_overlay::{Base, WorkspaceRef};
use strata_types::{Batch, Checkpoint, CheckpointId, CheckpointReason, Config, Paths, ReleaseGroup, SessionId};

/// A sandbox with projects, bare origins, a config dir and a socket dir.
pub struct Dfx {
    pub sb: Sandbox,
    pub config: Config,
    pub config_dir: PathBuf,
    pub sock_dir: tempfile::TempDir,
    pub alerts: Arc<Mutex<Vec<String>>>,
    pub lock_wait: Duration,
    /// `Options::reconcile_every` (production value unless a test shortens it).
    pub reconcile_every: Duration,
}

impl Dfx {
    pub fn new() -> Dfx {
        Dfx::with(&["tool"])
    }

    pub fn with(names: &[&str]) -> Dfx {
        let sb = Sandbox::new();
        let mut repos = BTreeMap::new();
        for n in names {
            let p = sb.init_project(n);
            sb.bare_origin(&p);
            repos.insert(repo_name(n), sb.config(&p));
        }
        let config = Config { paths: Paths { root: sb.path("strata") }, repos, dev_ports: Default::default() };
        let config_dir = sb.path("cfg");
        let sock_dir = tempfile::Builder::new().prefix("sd-").tempdir_in("/tmp").expect("sock dir");
        let fx = Dfx { sb, config, config_dir, sock_dir, alerts: Arc::new(Mutex::new(Vec::new())), lock_wait: Duration::from_millis(300), reconcile_every: strata_daemon::RECONCILE_EVERY };
        fx.write_config();
        fx
    }

    pub fn write_config(&self) {
        fs::create_dir_all(&self.config_dir).unwrap();
        fs::write(self.config_dir.join("config.toml"), toml::to_string(&self.config).unwrap()).unwrap();
    }

    pub fn write_config_text(&self, text: &str) {
        fs::create_dir_all(&self.config_dir).unwrap();
        fs::write(self.config_dir.join("config.toml"), text).unwrap();
    }

    /// Before a start with no pins.json: pin the config on disk as if a
    /// human had accepted it (a repo with no pin needs confirmation, so the
    /// daemon itself never adopts it). Never overwrites pins.json: later
    /// config changes go through the daemon's tighten / pending.
    pub fn seed_pins(&self) {
        let p = strata_daemon::pins_path(&self.config_dir);
        if p.exists() {
            return;
        }
        if let Ok(text) = fs::read_to_string(self.config_dir.join("config.toml"))
            && let Ok(c) = Config::parse(&text)
        {
            fs::write(p, pins_json(&c)).unwrap();
        }
    }

    /// One start without seeding pins.json (the "no pin" cases).
    pub fn prepare_unseeded(&self, c: Confirmer) -> strata_daemon::Result<Core> {
        let env = self.sb.git_env();
        startup::prepare(self.options(), c, &move |p: &Path| Git::open_with_env(p, &env))
    }

    pub fn socket(&self) -> PathBuf {
        self.sock_dir.path().join("d.sock")
    }

    pub fn options(&self) -> Options {
        let alerts = self.alerts.clone();
        Options {
            config_dir: self.config_dir.clone(),
            config_file: self.config_dir.join("config.toml"),
            socket: self.socket(),
            lock_wait: self.lock_wait,
            install_path: self.sb.install_path(),
            alert: Arc::new(move |l: &str| alerts.lock().unwrap().push(l.to_string())),
            reconcile_every: self.reconcile_every,
        }
    }

    pub fn alerts(&self) -> Vec<String> {
        self.alerts.lock().unwrap().clone()
    }

    pub fn confirmer(&self, verdicts: Vec<Verdict>) -> (Confirmer, Arc<PromptLog>, FakeClock) {
        let auth = ScriptedAuth::new(true, Ok(()), verdicts);
        let log = auth.log();
        let clock = FakeClock::new();
        let c = Confirmer::with_seams(auth, clock.clone(), Duration::from_secs(2), Duration::from_millis(200));
        (c, log, clock)
    }

    /// A start with a fresh confirmer. The lock probe in `prepare_with` has a
    /// window of its own (the kernel copies the descriptor table into a child
    /// spawned meanwhile before exec closes it), so this retries the whole
    /// start, which takes the lock first and has done nothing else on refusal.
    pub fn prepare(&self) -> strata_daemon::Result<Core> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            match self.prepare_once(self.confirmer(vec![]).0) {
                Err(e) if is_daemon_running(&e) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                r => return r,
            }
        }
    }

    /// A restart right after `close()` can briefly see `daemon_running`: the
    /// lock is `O_CLOEXEC`, but std falls back to fork+exec when a command sets
    /// `PATH` (deps installs do), and a sibling test's child holds a copy of
    /// every descriptor between fork and exec. A real restart is a new process,
    /// so only the in-process tests retry; `prepare_once` is the single attempt.
    pub fn prepare_with(&self, c: Confirmer) -> strata_daemon::Result<Core> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let lock = strata_daemon::lock_path(&self.config_dir);
        // A first start has nothing to wait for (and a probe would only add a
        // window of its own); only a restart probes.
        while lock.exists() && std::time::Instant::now() < deadline {
            match startup::InstanceLock::take(&lock) {
                Err(e) if is_daemon_running(&e) => std::thread::sleep(Duration::from_millis(20)),
                _ => break,
            }
        }
        self.prepare_once(c)
    }

    pub fn prepare_once(&self, c: Confirmer) -> strata_daemon::Result<Core> {
        self.seed_pins();
        let env = self.sb.git_env();
        startup::prepare(self.options(), c, &move |p: &Path| Git::open_with_env(p, &env))
    }

    pub fn main(&self, repo: &str) -> PathBuf {
        self.config.repos[&repo_name(repo)].path.clone()
    }
}

impl Default for Dfx {
    fn default() -> Self {
        Self::new()
    }
}

/// The session's workspace of `repo` (from the store).
pub fn ws(core: &Core, id: &SessionId, repo: &str) -> PathBuf {
    let rec = core.store.session(id).unwrap().expect("session row");
    rec.manifest.repos[&repo_name(repo)].path.clone()
}

/// A manual checkpoint of the workspace against its current base.
pub fn checkpoint(core: &Core, id: &SessionId, repo: &str) -> Checkpoint {
    let rn = repo_name(repo);
    let store = &core.store;
    let rec = store.session(id).unwrap().expect("session row");
    let g = rec.manifest.repos[&rn].base.clone();
    let manifest = store.manifest(&rn, &g.tree).unwrap().expect("base manifest");
    let base = Base { dir: store.base_dir(&g), r#gen: g, manifest: Arc::new(manifest) };
    let root = ws(core, id, repo);
    let wref = WorkspaceRef { session: id, repo: &rn, root: &root, config: &core.config.repos[&rn], git: &core.gits[&rn], store };
    strata_overlay::checkpoint(&wref, &base, None, CheckpointReason::Manual { message: "test".into() })
        .unwrap()
        .checkpoint
}

pub fn batch(session: &SessionId, repo: &str, cp: CheckpointId, paths: &[&str]) -> Batch {
    Batch { session: session.clone(), repo: repo_name(repo), checkpoint: cp, paths: rps(paths) }
}

pub fn group(batches: Vec<Batch>) -> ReleaseGroup {
    ReleaseGroup { id: 0, batches }
}

pub fn reason(e: &strata_types::StrataError) -> Option<&str> {
    e.detail.as_ref().and_then(|d| d.get("reason")).and_then(|v| v.as_str())
}

fn is_daemon_running(e: &strata_types::StrataError) -> bool {
    e.detail.as_ref().and_then(|d| d.get("reason")).and_then(|r| r.as_str()) == Some("daemon_running")
}

/// pins.json for `config` exactly as configured.
pub fn pins_json(config: &Config) -> String {
    let repos: serde_json::Map<String, serde_json::Value> = config
        .repos
        .iter()
        .map(|(n, rc)| {
            (
                n.as_str().to_string(),
                serde_json::json!({ "deploy_on_push": rc.deploy_on_push, "confirm": rc.publish.confirm, "push": rc.publish.push }),
            )
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "repos": repos })).unwrap()
}

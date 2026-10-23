//! The shared world: a git main checkout in a sandbox HOME, a Store, one
//! base generation exported and recorded, and one active session whose `app`
//! workspace is a worktree of a shadow repository.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use strata_gitio::Git;
use strata_overlay::{Base, WorkspaceRef, build_manifest};
use strata_store::{DepsOrigin, Lifecycle, Store};
use strata_types::{
    BaseGen, CommitOid, DepsSet, DepsSetId, RepoConfig, RepoName, RepoPath, SessionId,
    SessionManifest, TreeOid, WorkspaceState,
};

pub fn t(s: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + s)
}

pub fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_owned()).unwrap()
}

/// Writes `rel` under `dir`, creating parents.
pub fn put(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, bytes).unwrap();
}

pub fn put_exec(dir: &Path, rel: &str, bytes: &[u8]) {
    put(dir, rel, bytes);
    fs::set_permissions(dir.join(rel), fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn put_link(dir: &Path, rel: &str, target: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&p);
    symlink(target, &p).unwrap();
}

pub fn blake3_hex(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}

pub fn config_for(main: &Path) -> RepoConfig {
    serde_json::from_value(serde_json::json!({
        "path": main, "default_branch": "main",
        "publish": { "branch": "main", "push": false, "confirm": false }
    }))
    .unwrap()
}

/// The default base tree: a file, an exec file, a symlink, an empty file and nested dirs.
pub fn default_tree(main: &Path) {
    put(main, "README.md", b"hello\n");
    put_exec(main, "bin/run.sh", b"#!/bin/sh\necho run\n");
    put_link(main, "link", "README.md");
    put(main, "empty.txt", b"");
    put(main, "src/lib.rs", b"pub fn a() {}\n");
    put(main, "src/deep/x/y.txt", b"deep\n");
    put(main, ".gitignore", b"*.log\n/node_modules\n/target\n");
}

pub struct World {
    _dir: tempfile::TempDir,
    pub tmp: PathBuf,
    pub home: PathBuf,
    pub xdg: PathBuf,
    pub main: PathBuf,
    pub root: PathBuf,
    pub store: Store,
    pub git: Git,
    pub config: RepoConfig,
    pub repo: RepoName,
    pub sid: SessionId,
    pub deps: DepsSetId,
    pub base: Base,
    pub head: CommitOid,
    pub ws: PathBuf,
    next_base: std::cell::Cell<u64>,
}

impl World {
    pub fn new() -> World {
        World::with(default_tree)
    }

    /// `setup` fills the main checkout before the base commit.
    pub fn with(setup: impl FnOnce(&Path)) -> World {
        let dir = tempfile::Builder::new()
            .prefix("strata-overlay-")
            .tempdir()
            .expect("tempdir");
        let tmp = fs::canonicalize(dir.path()).unwrap();
        let home = tmp.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(&xdg).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Strata Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let main = tmp.join("main");
        fs::create_dir_all(&main).unwrap();
        let root = tmp.join("strata");
        let repo = RepoName::try_from("app".to_owned()).unwrap();
        let sid = SessionId::try_from("s-ovrlayaa".to_owned()).unwrap();
        let deps = DepsSetId::try_from("d".repeat(64)).unwrap();
        let env = |cwd: &Path, args: &[&str]| run_git(&home, &xdg, cwd, args);
        env(&main, &["init", "-q"]);
        setup(&main);
        env(&main, &["add", "-A"]);
        env(&main, &["commit", "-q", "-m", "base"]);
        let git = Git::open_with_env(
            &main,
            &[
                ("HOME", Some(home.as_os_str())),
                ("XDG_CONFIG_HOME", Some(xdg.as_os_str())),
                ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
            ],
        )
        .expect("open git");
        let mut w = World {
            _dir: dir,
            tmp: tmp.clone(),
            home: home.clone(),
            xdg: xdg.clone(),
            main: main.clone(),
            root: root.clone(),
            store: Store::open(&root, &[&main]).expect("open store"),
            git,
            config: config_for(&main),
            repo,
            sid,
            deps,
            base: Base {
                r#gen: BaseGen {
                    repo: RepoName::try_from("app".to_owned()).unwrap(),
                    tree: TreeOid::try_from("0".repeat(40)).unwrap(),
                    deps: DepsSetId::try_from("d".repeat(64)).unwrap(),
                },
                manifest: Arc::new(strata_types::BaseManifest {
                    tree: TreeOid::try_from("0".repeat(40)).unwrap(),
                    entries: BTreeMap::new(),
                }),
                dir: PathBuf::new(),
            },
            head: CommitOid::try_from("0".repeat(40)).unwrap(),
            ws: root.join("sessions/s-ovrlayaa/app"),
            next_base: std::cell::Cell::new(1),
        };
        w.store
            .record_deps_set(
                &DepsSet {
                    id: w.deps.clone(),
                    repo: w.repo.clone(),
                    inputs_hash: "0".repeat(64),
                    platform: "darwin-arm64".into(),
                    toolchain: "none".into(),
                    dirs: vec![],
                },
                DepsOrigin::Empty,
                None,
                t(1),
            )
            .expect("record deps set");
        let (base, head) = w.base_at_head();
        w.base = base;
        w.head = head;
        // The workspace: a shadow repository and an attached worktree.
        fs::create_dir_all(w.tmp.join("shadows")).unwrap();
        let shadow = w
            .git
            .ensure_shadow(&w.tmp.join("shadows/app.git"))
            .expect("shadow");
        let stage = w.root.join("sessions/s-ovrlayaa/.tmp-app");
        fs::create_dir_all(stage.parent().unwrap()).unwrap();
        w.git
            .export_tree(&w.base.r#gen.tree, &stage)
            .expect("export workspace");
        w.git
            .attach_worktree(&shadow, 1, &w.head, &stage, &w.ws)
            .expect("attach");
        fs::rename(&stage, &w.ws).unwrap();
        let mut repos = BTreeMap::new();
        repos.insert(
            w.repo.clone(),
            WorkspaceState {
                base: w.base.r#gen.clone(),
                head: w.head.clone(),
                path: w.ws.clone(),
                tracked: true,
            },
        );
        let m = SessionManifest {
            id: w.sid.clone(),
            ticket: None,
            root: w.root.join("sessions/s-ovrlayaa"),
            repos,
        };
        w.store.create_session(&m, t(3)).expect("create session");
        w.store
            .set_lifecycle(&w.sid, Lifecycle::Creating, Lifecycle::Active, None, t(4))
            .expect("activate");
        w
    }

    pub fn env(&self) -> Vec<(&'static str, Option<&OsStr>)> {
        vec![
            ("HOME", Some(self.home.as_os_str())),
            ("XDG_CONFIG_HOME", Some(self.xdg.as_os_str())),
            ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
        ]
    }

    pub fn open_git(&self) -> Git {
        Git::open_with_env(&self.main, &self.env()).expect("open git")
    }

    pub fn cmd(&self, cwd: &Path) -> Command {
        git_cmd(&self.home, &self.xdg, cwd)
    }

    /// Runs git in `cwd`, asserts success, returns trimmed stdout.
    pub fn git_ok(&self, cwd: &Path, args: &[&str]) -> String {
        run_git(&self.home, &self.xdg, cwd, args)
    }

    /// Exports, builds and records a base generation for the main checkout's HEAD.
    pub fn base_at_head(&self) -> (Base, CommitOid) {
        let head = CommitOid::try_from(self.git_ok(&self.main, &["rev-parse", "HEAD"])).unwrap();
        let tree =
            TreeOid::try_from(self.git_ok(&self.main, &["rev-parse", "HEAD^{tree}"])).unwrap();
        let g = BaseGen {
            repo: self.repo.clone(),
            tree: tree.clone(),
            deps: self.deps.clone(),
        };
        let dir = self.store.base_dir(&g);
        if !dir.exists() {
            fs::create_dir_all(dir.parent().unwrap()).unwrap();
            self.git.export_tree(&tree, &dir).expect("export base");
        }
        let manifest = build_manifest(&dir, &tree, &self.config).expect("build manifest");
        let n = self.next_base.get();
        self.next_base.set(n + 1);
        self.store
            .record_base_gen(&g, &manifest, t(1 + n))
            .expect("record base gen");
        (
            Base {
                r#gen: g,
                manifest: Arc::new(manifest),
                dir,
            },
            head,
        )
    }

    /// Commits `change` on the main checkout and records the new base.
    pub fn next_base(&self, change: impl FnOnce(&Path)) -> (Base, CommitOid) {
        change(&self.main);
        self.git_ok(&self.main, &["add", "-A"]);
        self.git_ok(&self.main, &["commit", "-q", "--allow-empty", "-m", "next"]);
        self.base_at_head()
    }

    pub fn ws_ref(&self) -> WorkspaceRef<'_> {
        WorkspaceRef {
            session: &self.sid,
            repo: &self.repo,
            root: &self.ws,
            config: &self.config,
            git: &self.git,
            store: &self.store,
        }
    }

    pub fn write(&self, rel: &str, bytes: &[u8]) {
        put(&self.ws, rel, bytes);
    }

    pub fn read(&self, rel: &str) -> Vec<u8> {
        fs::read(self.ws.join(rel)).unwrap()
    }

    pub fn remove(&self, rel: &str) {
        let p = self.ws.join(rel);
        if fs::symlink_metadata(&p)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            fs::remove_dir_all(p).unwrap();
        } else {
            fs::remove_file(p).unwrap();
        }
    }

    /// Every file under the store's blob directory (to prove nothing was written).
    pub fn store_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let blobs = self.root.join("store");
        let mut stack = vec![blobs];
        while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_type().unwrap().is_dir() {
                    stack.push(p);
                } else if !p.to_string_lossy().contains("store.db") {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }
}

fn git_cmd(home: &Path, xdg: &Path, cwd: &Path) -> Command {
    let mut c = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
            c.env_remove(&k);
        }
    }
    c.current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    c
}

fn run_git(home: &Path, xdg: &Path, cwd: &Path, args: &[&str]) -> String {
    let o = git_cmd(home, xdg, cwd)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

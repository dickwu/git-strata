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

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::Builder::new().prefix("strata-deps-").tempdir().expect("tempdir");
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
        let sb = Sandbox { _dir: dir, root, home, xdg, bin, state };
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
        Git::open_with_env(root, &self.git_env()).unwrap_or_else(|e| panic!("open {}: {e:?}", root.display()))
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
        let mut tar =
            Command::new("tar").arg("-xf").arg("-").arg("-C").arg(dir).stdin(Stdio::piped()).spawn().unwrap();
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
        InstallEnv { path: self.install_path(), timeout: Duration::from_secs(60) }
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
        fs::read_to_string(self.state.join("count")).map(|s| s.trim().parse().unwrap()).unwrap_or(0)
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
            publish: PublishTarget { branch: "main".into(), push: true, confirm: false },
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

//! Shared helpers: every repository lives in a temp dir; every git the tests
//! start gets the sandbox HOME / XDG_CONFIG_HOME and GIT_CONFIG_NOSYSTEM=1.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use strata_gitio::Git;

pub struct Sandbox {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub xdg: PathBuf,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        let xdg = home.join(".config");
        fs::create_dir_all(&xdg).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Strata Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        Sandbox { _dir: dir, root, home, xdg }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Env for `Git::open_with_env`.
    pub fn env(&self) -> Vec<(&'static str, Option<&OsStr>)> {
        vec![
            ("HOME", Some(self.home.as_os_str())),
            ("XDG_CONFIG_HOME", Some(self.xdg.as_os_str())),
            ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
        ]
    }

    pub fn open(&self, root: &Path) -> Git {
        Git::open_with_env(root, &self.env()).unwrap_or_else(|e| panic!("open {}: {e:?}", root.display()))
    }

    pub fn try_open(&self, root: &Path) -> strata_gitio::Result<Git> {
        Git::open_with_env(root, &self.env())
    }

    pub fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            let u = k.to_string_lossy().to_ascii_uppercase();
            if u.starts_with("GIT_") || u == "STRATA_PUBLISH" {
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

    pub fn git_stdin(&self, cwd: &Path, args: &[&str], input: &[u8]) -> String {
        let mut c = self.cmd(cwd);
        c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim_end().to_string()
    }

    /// `git init` plus one commit with README.md.
    pub fn init_repo(&self, name: &str) -> PathBuf {
        self.init_repo_with(name, &[])
    }

    pub fn init_repo_with(&self, name: &str, init_args: &[&str]) -> PathBuf {
        let p = self.root.join(name);
        fs::create_dir_all(&p).unwrap();
        let mut args = vec!["init", "-q"];
        args.extend_from_slice(init_args);
        self.git(&p, &args);
        self.commit_file(&p, "README.md", b"hello\n", "initial");
        p
    }

    pub fn write(&self, repo: &Path, rel: &str, content: &[u8]) {
        let p = repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Write, add and commit one file; returns the new HEAD.
    pub fn commit_file(&self, repo: &Path, rel: &str, content: &[u8], msg: &str) -> String {
        self.write(repo, rel, content);
        self.git(repo, &["add", "--", rel]);
        self.git(repo, &["commit", "-q", "-m", msg]);
        self.head(repo)
    }

    pub fn head(&self, repo: &Path) -> String {
        self.git(repo, &["rev-parse", "HEAD"])
    }

    pub fn tree_of(&self, repo: &Path, rev: &str) -> String {
        self.git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    /// Extract `treeish` of `repo` into `dir` without any `.git`.
    pub fn archive_tree_into(&self, repo: &Path, treeish: &str, dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let o = self.git_output(repo, &["archive", "--format=tar", treeish]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let mut tar = Command::new("tar").arg("-xf").arg("-").arg("-C").arg(dir).stdin(Stdio::piped()).spawn().unwrap();
        tar.stdin.take().unwrap().write_all(&o.stdout).unwrap();
        assert!(tar.wait().unwrap().success());
    }

    /// A bare remote seeded from `repo`'s main branch.
    pub fn bare_remote(&self, repo: &Path, name: &str) -> PathBuf {
        let p = self.root.join(name);
        self.git(&self.root, &["init", "-q", "--bare", p.to_str().unwrap()]);
        self.git(repo, &["remote", "add", "origin", p.to_str().unwrap()]);
        self.git(repo, &["push", "-q", "origin", "main"]);
        p
    }
}

pub fn env_of(call: &strata_gitio::GitCall, key: &str) -> Option<String> {
    call.env.get(OsStr::new(key)).map(|v| v.to_string_lossy().into_owned())
}

pub fn args_of(call: &strata_gitio::GitCall) -> Vec<String> {
    call.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

/// The first recorded call whose args contain `sub` as a whole argument.
pub fn find_call<'a>(calls: &'a [strata_gitio::GitCall], sub: &str) -> Option<&'a strata_gitio::GitCall> {
    calls.iter().find(|c| c.args.iter().any(|a| a == sub))
}

/// Snapshot of every file under `dir` (path → bytes), for "untouched" checks.
pub fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(base: &Path, d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut ents: Vec<_> = fs::read_dir(d).unwrap().map(|e| e.unwrap()).collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            let m = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if m.file_type().is_symlink() {
                out.push((rel, fs::read_link(&p).unwrap().into_os_string().into_encoded_bytes()));
            } else if m.is_dir() {
                out.push((rel, b"<dir>".to_vec()));
                walk(base, &p, out);
            } else {
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

/// `detail.<key>` of an error as a string.
pub fn detail(e: &strata_types::StrataError, key: &str) -> Option<String> {
    let v = e.detail.as_ref()?.get(key)?;
    Some(match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

pub fn reason(e: &strata_types::StrataError) -> Option<String> {
    detail(e, "reason")
}

/// A hand-built shadow repository of `main` with one workspace whose index
/// holds `tree` (what Task 7 will build for real). Returns (shadow, ws).
pub fn fake_shadow(sb: &Sandbox, main: &Path, tag: &str, tree: &str, owned: bool) -> (PathBuf, PathBuf) {
    let ws = sb.path(&format!("ws-{tag}"));
    fake_shadow_at(sb, main, tag, tree, owned, &ws)
}

pub fn fake_shadow_at(sb: &Sandbox, main: &Path, tag: &str, tree: &str, owned: bool, ws: &Path) -> (PathBuf, PathBuf) {
    let shadow = sb.path(&format!("shadow-{tag}"));
    sb.git(&sb.root, &["init", "-q", "--bare", shadow.to_str().unwrap()]);
    let objects = if owned { main.join(".git/objects") } else { sb.init_repo(&format!("other-{tag}")).join(".git/objects") };
    fs::write(shadow.join("objects/info/alternates"), format!("{}\n", objects.display())).unwrap();
    sb.git(&shadow, &["config", "strata.shadow", "true"]);
    let admin = shadow.join("worktrees/ws1");
    fs::create_dir_all(&admin).unwrap();
    fs::write(admin.join("commondir"), "../..\n").unwrap();
    fs::write(admin.join("HEAD"), format!("{}\n", sb.head(main))).unwrap();
    fs::create_dir_all(ws).unwrap();
    fs::write(ws.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    fs::write(admin.join("gitdir"), format!("{}\n", ws.join(".git").display())).unwrap();
    let o = sb
        .cmd(&shadow)
        .env("GIT_INDEX_FILE", admin.join("index"))
        .args(["--git-dir=.", "read-tree", tree])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    (shadow, ws.to_path_buf())
}

//! Shared helpers for the git-strata tests: run the real binary with a
//! throw-away HOME under /tmp (the socket path must fit sockaddr_un).
#![allow(dead_code)]

pub mod json;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_git-strata");

/// A HOME with a .gitconfig and the git-strata config dir.
pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Home {
        let dir = tempfile::Builder::new().prefix("gh-").tempdir_in("/tmp").expect("home");
        fs::write(
            dir.path().join(".gitconfig"),
            "[user]\n\tname = Strata Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join(".config")).unwrap();
        fs::create_dir_all(Self::cfg_of(dir.path())).unwrap();
        Home { dir }
    }

    fn cfg_of(home: &Path) -> PathBuf {
        home.join("Library/Application Support/git-strata")
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn config_dir(&self) -> PathBuf {
        Self::cfg_of(self.path())
    }

    pub fn socket(&self) -> PathBuf {
        self.config_dir().join("daemon.sock")
    }

    /// The binary with this HOME, a clean git environment, stdin /dev/null.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        for (k, _) in std::env::vars_os() {
            let up = k.to_string_lossy().to_ascii_uppercase();
            if up.starts_with("GIT_") || up.starts_with("STRATA") {
                c.env_remove(&k);
            }
        }
        c.args(args)
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(self.path())
            .stdin(Stdio::null());
        c
    }

    pub fn run(&self, args: &[&str]) -> Run {
        Run::from(self.cmd(args).output().expect("spawn git-strata"))
    }
}

impl Default for Home {
    fn default() -> Self {
        Self::new()
    }
}

/// One finished run: exit code, stdout, stderr, the envelope (serde view).
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

impl Run {
    /// The envelope, checked to be one compact line ending in `\n`.
    pub fn env(&self) -> serde_json::Value {
        assert!(self.stdout.ends_with('\n'), "no trailing newline: {self:?}");
        assert_eq!(self.stdout.matches('\n').count(), 1, "not one line: {self:?}");
        serde_json::from_str(self.stdout.trim_end_matches('\n')).unwrap_or_else(|e| panic!("not JSON ({e}): {self:?}"))
    }

    pub fn ok(&self) -> serde_json::Value {
        let v = self.env();
        assert_eq!(v["ok"], true, "expected success: {self:?}");
        assert_eq!(self.code, 0, "{self:?}");
        v["data"].clone()
    }

    pub fn err_code(&self) -> String {
        let v = self.env();
        assert_eq!(v["ok"], false, "expected failure: {self:?}");
        assert_eq!(self.code, 1, "{self:?}");
        v["error"]["code"].as_str().unwrap_or_default().to_string()
    }

    pub fn reason(&self) -> String {
        self.env()["error"]["detail"]["reason"].as_str().unwrap_or_default().to_string()
    }
}

/// Every name of types' `Code`, read from the enum.
pub fn code_names() -> Vec<String> {
    strata_types::Code::ALL.iter().map(|c| c.as_str().to_string()).collect()
}

/// One `[repo.<name>]` of a test config.
pub struct RepoSpec<'a> {
    pub name: &'a str,
    pub push: bool,
    pub confirm: bool,
    pub deploy_on_push: bool,
}

impl Home {
    /// The canonical HOME (/tmp is /private/tmp on macOS).
    pub fn canon(&self) -> PathBuf {
        fs::canonicalize(self.path()).unwrap()
    }

    /// A repo checkout path `<home>/repos/<name>` (created, empty).
    pub fn repo_dir(&self, name: &str) -> PathBuf {
        let p = self.canon().join("repos").join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// config.toml with `paths.root = <home>/root` and the given repos.
    pub fn write_config(&self, repos: &[RepoSpec]) {
        let root = self.canon().join("root");
        fs::create_dir_all(&root).unwrap();
        let mut t = format!("[paths]\nroot = {:?}\n", root.to_string_lossy());
        for r in repos {
            t.push_str(&format!(
                "\n[repo.{n}]\npath = {p:?}\ndefault_branch = \"main\"\ndeps_dirs = []\ndeploy_on_push = {d}\n\n[repo.{n}.publish]\nbranch = \"main\"\npush = {push}\nconfirm = {c}\n",
                n = r.name,
                p = self.repo_dir(r.name).to_string_lossy(),
                d = r.deploy_on_push,
                push = r.push,
                c = r.confirm,
            ));
        }
        fs::write(self.config_dir().join("config.toml"), t).unwrap();
    }

    /// Run with stdin on a fresh pseudo-terminal (the master end is kept
    /// open until the child exits).
    pub fn run_tty(&self, args: &[&str]) -> Run {
        let (master, slave) = pty();
        let mut c = self.cmd(args);
        c.stdin(Stdio::from(slave));
        let out = c.output().expect("spawn git-strata");
        drop(master);
        Run::from(out)
    }
}

/// A pseudo-terminal pair (master, slave).
pub fn pty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    use std::os::fd::FromRawFd;
    let mut m: libc::c_int = -1;
    let mut s: libc::c_int = -1;
    let rc = unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    unsafe { (std::os::fd::OwnedFd::from_raw_fd(m), std::os::fd::OwnedFd::from_raw_fd(s)) }
}

/// `git` for building fixtures (the binary under test never runs git).
pub fn git(dir: &Path, home: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(&k);
        }
    }
    let o = c
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A fake daemon on the HOME socket: records every request line and
/// answers each with `reply(request)`.
pub struct FakeDaemon {
    pub requests: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    pub connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeDaemon {
    pub fn start(socket: &Path, reply: fn(&serde_json::Value) -> serde_json::Value) -> FakeDaemon {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;
        let l = std::os::unix::net::UnixListener::bind(socket).expect("bind fake socket");
        l.set_nonblocking(true).unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (rq, cn, st) = (requests.clone(), connections.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !st.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((s, _)) => {
                        cn.fetch_add(1, Ordering::SeqCst);
                        s.set_nonblocking(false).unwrap();
                        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                        let mut w = s.try_clone().unwrap();
                        let mut r = BufReader::new(s);
                        let mut line = String::new();
                        while r.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                            let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap_or(serde_json::Value::Null);
                            rq.lock().unwrap().push(v.clone());
                            let mut out = reply(&v).to_string();
                            out.push('\n');
                            let _ = w.write_all(out.as_bytes());
                            line.clear();
                        }
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
        });
        FakeDaemon { requests, connections, stop, thread: Some(thread) }
    }

    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }

    pub fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

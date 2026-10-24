//! An in-process git-stratad for the end-to-end tests: the real startup,
//! handlers and socket protocol, with the confirm seams scripted
//! (strata_daemon::seams). The socket is the cli's own
//! `$HOME/Library/Application Support/git-strata/daemon.sock` under a
//! short /tmp HOME; repositories, bare origins and the store live in a
//! TMPDIR sandbox. No launchd, no network remote.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use strata_daemon::handlers;
use strata_daemon::seams::{Confirmer, FakeClock, PromptLog, ScriptedAuth, Verdict};
use strata_daemon::startup::{self, Core, Daemon, Options};
use strata_gitio::Git;
use strata_types::{RepoName, SessionId};

use super::{Home, Run, git};

pub struct Live {
    pub home: Home,
    sandbox: tempfile::TempDir,
    pub sb: PathBuf,
    pub root: PathBuf,
    pub repos: BTreeMap<String, PathBuf>,
    pub alerts: Arc<Mutex<Vec<String>>>,
    /// The method of every request the in-process daemon received.
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl Drop for Live {
    /// Sealed sets are 0555: make directories writable so the sandbox goes.
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
        writable(&self.sb);
    }
}

impl Live {
    /// Repos `names`, each with `files` committed on main and pushed to a
    /// local bare origin; config.toml written (push and confirm off).
    pub fn new(names: &[&str], files: &[(&str, &[u8])]) -> Live {
        let home = Home::new();
        let sandbox = tempfile::Builder::new().prefix("strata-cli-").tempdir().expect("sandbox");
        let sb = fs::canonicalize(sandbox.path()).unwrap();
        let root = sb.join("strata");
        let mut repos = BTreeMap::new();
        for n in names {
            let p = sb.join(n);
            fs::create_dir_all(&p).unwrap();
            git(&p, home.path(), &["init", "-q"]);
            write(&p, ".gitignore", b".DS_Store\n");
            git(&p, home.path(), &["add", "-f", "--", ".gitignore"]);
            for (rel, content) in files {
                write(&p, rel, content);
                git(&p, home.path(), &["add", "-f", "--", rel]);
            }
            git(&p, home.path(), &["commit", "-q", "-m", "initial"]);
            let origin = sb.join(format!("{n}-origin.git"));
            git(&sb, home.path(), &["init", "-q", "--bare", origin.to_str().unwrap()]);
            git(&p, home.path(), &["remote", "add", "origin", origin.to_str().unwrap()]);
            git(&p, home.path(), &["push", "-q", "origin", "main"]);
            repos.insert(n.to_string(), p);
        }
        let live = Live { home, sandbox, sb, root, repos, alerts: Arc::new(Mutex::new(Vec::new())), requests: Arc::new(Mutex::new(Vec::new())) };
        live.write_config();
        live
    }

    pub fn write_config(&self) {
        self.write_config_guarded(&[]);
    }

    /// config.toml where the repos in `guarded` push, confirm and deploy
    /// on push (the confirm-class fixture); the others do none of these.
    pub fn write_config_guarded(&self, guarded: &[&str]) {
        let mut t = format!("[paths]\nroot = {:?}\n", self.root.to_string_lossy());
        for (n, p) in &self.repos {
            let g = guarded.contains(&n.as_str());
            t.push_str(&format!(
                "\n[repo.{n}]\npath = {p:?}\ndefault_branch = \"main\"\ndeps_dirs = []\ndeploy_on_push = {g}\n\n[repo.{n}.publish]\nbranch = \"main\"\npush = {g}\nconfirm = {g}\n",
                p = p.to_string_lossy(),
            ));
        }
        fs::write(self.home.config_dir().join("config.toml"), t).unwrap();
    }

    fn options(&self) -> Options {
        let alerts = self.alerts.clone();
        let config_dir = self.home.canon().join("Library/Application Support/git-strata");
        Options {
            socket: strata_daemon::socket_path(&config_dir),
            config_file: config_dir.join("config.toml"),
            config_dir,
            lock_wait: Duration::from_millis(500),
            install_path: "/usr/bin:/bin:/usr/sbin:/sbin".into(),
            alert: Arc::new(move |l: &str| alerts.lock().unwrap().push(l.to_string())),
        }
    }

    /// Start the daemon, run `f` with it serving, stop it (also on panic).
    pub fn serve<R>(&self, f: impl FnOnce(&Core) -> R) -> R {
        self.serve_with(vec![], f)
    }

    pub fn serve_with<R>(&self, verdicts: Vec<Verdict>, f: impl FnOnce(&Core) -> R) -> R {
        self.serve_scripted(verdicts, |c, _| f(c))
    }

    /// As `serve_with`, also handing `f` the authenticator's prompt log.
    /// Every request's method is appended to `self.requests`.
    pub fn serve_scripted<R>(&self, verdicts: Vec<Verdict>, f: impl FnOnce(&Core, &PromptLog) -> R) -> R {
        let auth = ScriptedAuth::new(true, Ok(()), verdicts);
        let log = auth.log();
        let c = Confirmer::with_seams(auth, FakeClock::new(), Duration::from_secs(2), Duration::from_millis(200));
        let home = self.home.canon();
        let xdg = home.join(".config");
        let env: Vec<(&str, Option<&OsStr>)> = vec![
            ("HOME", Some(home.as_os_str())),
            ("XDG_CONFIG_HOME", Some(xdg.as_os_str())),
            ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
        ];
        let core = startup::prepare(self.options(), c, &move |p: &Path| Git::open_with_env(p, &env)).expect("prepare");
        let d = Daemon::start(&core).expect("daemon start");
        let listener = startup::bind(&core.opts.socket).expect("bind");
        let ext = AtomicBool::new(false);
        let requests = self.requests.clone();
        std::thread::scope(|s| {
            let h = s.spawn(|| {
                let trackers = handlers::Trackers::new(&core);
                let r = {
                    let server = handlers::Server::new(&d, &trackers);
                    d.serve(&listener, &ext, &|req| {
                        requests.lock().unwrap().push(req.method.name().to_string());
                        server.handle(req)
                    })
                };
                trackers.clear();
                r
            });
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&core, &log)));
            d.request_stop();
            h.join().unwrap().unwrap();
            r.unwrap_or_else(|p| std::panic::resume_unwind(p))
        })
    }

    /// The cli in `dir` with a pseudo-terminal on stdin and extra `env`.
    pub fn run_tty_env(&self, dir: &Path, args: &[&str], env: &[(String, String)]) -> Run {
        let (master, slave) = super::pty();
        let mut c = self.home.cmd(args);
        c.current_dir(dir).stdin(std::process::Stdio::from(slave));
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().expect("spawn git-strata");
        drop(master);
        Run::from(out)
    }

    /// The cli in `dir` (stdin /dev/null).
    pub fn run_in(&self, dir: &Path, args: &[&str]) -> Run {
        Run::from(self.home.cmd(args).current_dir(dir).output().expect("spawn git-strata"))
    }

    /// The cli in `dir` with `input` on a piped stdin.
    pub fn run_piped(&self, dir: &Path, args: &[&str], input: &[u8]) -> Run {
        use std::io::Write;
        let mut c = self.home.cmd(args);
        c.current_dir(dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = c.spawn().expect("spawn git-strata");
        child.stdin.take().unwrap().write_all(input).unwrap();
        Run::from(child.wait_with_output().unwrap())
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.home.run(args)
    }

    /// `git-strata new` over `repos`; the new session id.
    pub fn new_session(&self, ticket: &str, repos: &str) -> SessionId {
        let r = self.run(&["new", "--ticket", ticket, "--repos", repos]);
        let d = r.ok();
        SessionId::try_from(d["manifest"]["id"].as_str().expect("session id").to_string()).unwrap()
    }

    /// The workspace of `repo` in session `s` (read from a reader store).
    pub fn ws(&self, s: &SessionId, repo: &str) -> PathBuf {
        let store = strata_store::Store::open_read_only(&self.root).unwrap();
        let rec = store.session(s).unwrap().expect("session row");
        rec.manifest.repos[&RepoName::try_from(repo.to_string()).unwrap()].path.clone()
    }

    pub fn main(&self, repo: &str) -> PathBuf {
        self.repos[repo].clone()
    }
}

pub fn write(dir: &Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

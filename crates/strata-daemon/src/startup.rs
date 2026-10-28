//! daemon.md §5.1 startup, §5.9 shutdown, and the accept loop.
//!
//! Startup is split in two so the order is fixed and testable:
//! `prepare` (steps 2–7: flock, config, realpaths, pins, `Store::open`,
//! `Git::open`) builds the owned [`Core`]; `Daemon::start` (steps 8–11:
//! `Sessions::open` once, recover, gc, Running → Unknown, quarantine) builds
//! the [`Daemon`] that borrows it; [`bind`] (step 12) comes last. Step 1
//! (`TEST_SEAMS`) is [`refuse_test_seams`], called by `main` first.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error, retry_eintr};
use strata_confirm::Confirmer;
use strata_deps::InstallEnv;
use strata_gitio::Git;
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_session::{BeforeCheckpoint, RecoverReport, Sessions};
use strata_store::{PublishPhase, Store};
use strata_types::{CheckpointReason, Code, Config, RepoId, RepoName, StrataError};

use crate::confirm_flow::{self, Pins};
use crate::locks::Locks;
use crate::rpc::{self, Reply, Request};
use crate::{DaemonWarning, PinnedFlags, GC_KEEP, LOCK_WAIT, MAX_FRAME, Result};

/// Review P1-3: connections served at once; more wait in the backlog.
pub const MAX_CONNS: usize = 128;

/// Where the daemon keeps its files, and the knobs tests turn.
#[derive(Clone)]
pub struct Options {
    /// `~/Library/Application Support/git-strata`: daemon.lock, pins.json.
    pub config_dir: PathBuf,
    /// `<config dir>/config.toml`.
    pub config_file: PathBuf,
    /// `<config dir>/daemon.sock` in production. Tests put it in a short
    /// `/tmp` directory (sockaddr_un holds 104 bytes on macOS).
    pub socket: PathBuf,
    /// [`LOCK_WAIT`] in production.
    pub lock_wait: Duration,
    /// The explicit PATH installs and checks run with
    /// ([`install_path_for_home`](crate::install_path_for_home)).
    pub install_path: String,
    /// One line for the human (stderr in production).
    pub alert: Arc<dyn Fn(&str) + Send + Sync>,
    /// [`RECONCILE_EVERY`](crate::RECONCILE_EVERY) in production (§5.9);
    /// tests shorten it.
    pub reconcile_every: Duration,
}

impl Options {
    /// The production layout under `home`.
    pub fn for_home(home: &Path) -> Options {
        let dir = crate::config_dir_for_home(home);
        Options {
            config_file: dir.join("config.toml"),
            socket: crate::socket_path(&dir),
            config_dir: dir,
            lock_wait: LOCK_WAIT,
            install_path: crate::install_path_for_home(home),
            alert: Arc::new(|line: &str| eprintln!("git-stratad: {line}")),
            reconcile_every: crate::RECONCILE_EVERY,
        }
    }
}

/// Step 1: a binary built with any test seam does not start.
pub fn refuse_test_seams() -> Result<()> {
    if strata_confirm::TEST_SEAMS {
        return Err(StrataError::new(
            Code::InvalidInput,
            "this git-stratad was built with test seams and refuses to start",
        )
        .with_detail(json!({ "reason": "test_seams" })));
    }
    Ok(())
}

/// The single-instance lock: an exclusive `flock` on `daemon.lock`, opened
/// `O_CLOEXEC` so no child inherits it. Released when dropped.
pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    /// Step 2. Another holder → `Exists` (`detail.reason = "daemon_running"`).
    pub fn take(path: &Path) -> Result<InstanceLock> {
        if let Some(dir) = path.parent() {
            make_private_dir(dir)?;
        }
        let mut oo = OpenOptions::new();
        oo.read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_CLOEXEC);
        let file = retry_eintr(|| oo.open(path)).map_err(|e| classify_io_error(&e, IoSide::Write, path))?;
        let locked = retry_eintr(|| {
            // SAFETY: flock on a descriptor this function owns.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
        });
        if let Err(e) = locked {
            if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(StrataError::new(Code::Exists, "another git-stratad is running")
                    .with_detail(json!({ "reason": "daemon_running", "lock": path })));
            }
            return Err(classify_io_error(&e, IoSide::Write, path));
        }
        Ok(InstanceLock { file })
    }

    /// The raw descriptor (tests check `FD_CLOEXEC`).
    pub fn raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}

fn make_private_dir(dir: &Path) -> Result<()> {
    match retry_eintr(|| fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)) {
        Ok(()) => Ok(()),
        Err(e) => Err(classify_io_error(&e, IoSide::Write, dir)),
    }
}

/// The daemon's own warnings: deduplicated, each human alert printed once.
pub struct Warnings {
    list: Mutex<Vec<DaemonWarning>>,
    alert: Arc<dyn Fn(&str) + Send + Sync>,
    /// Keys of the lines `alert_first` already printed.
    seen: Mutex<std::collections::BTreeSet<String>>,
}

impl Warnings {
    fn new(alert: Arc<dyn Fn(&str) + Send + Sync>) -> Warnings {
        Warnings { list: Mutex::new(Vec::new()), alert, seen: Mutex::new(Default::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DaemonWarning>> {
        self.list.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record `w` once. Quarantine and attach alerts also print one line.
    pub fn add(&self, w: DaemonWarning) {
        let mut l = self.lock();
        if l.contains(&w) {
            return;
        }
        let line = match &w {
            DaemonWarning::Quarantine { path } => Some(format!(
                "quarantine: {} is set aside; no git runs in that shadow repository until it is cleared",
                path.display()
            )),
            DaemonWarning::AttachOutside { reason, index } => Some(format!(
                "attach may have written outside its staging ({reason}): check `git status` and `git diff --cached` for {}",
                index.display()
            )),
            _ => None,
        };
        l.push(w);
        drop(l);
        if let Some(line) = line {
            (self.alert)(&line);
        }
    }

    /// Print one line for the human without a warning entry.
    pub fn alert_once(&self, line: &str) {
        (self.alert)(line);
    }

    /// Print `line` the first time `key` is seen; true when printed.
    pub fn alert_first(&self, key: &str, line: &str) -> bool {
        let fresh = self.seen.lock().unwrap_or_else(|p| p.into_inner()).insert(key.to_owned());
        if fresh {
            (self.alert)(line);
        }
        fresh
    }

    pub fn remove(&self, f: impl Fn(&DaemonWarning) -> bool) {
        self.lock().retain(|w| !f(w));
    }

    pub fn snapshot(&self) -> Vec<DaemonWarning> {
        self.lock().clone()
    }
}

/// `BeforeCheckpoint` for `Sessions::open`: called while `move_workspace`
/// already holds the queue and the repo lock, so it takes neither and
/// checkpoints directly (no Tracker before the first scan → `None`).
pub struct DaemonBefore;

impl BeforeCheckpoint for DaemonBefore {
    fn checkpoint(&self, ws: &WorkspaceRef<'_>, base: &Base, reason: CheckpointReason) -> Result<Checkpointed> {
        strata_overlay::checkpoint(ws, base, None, reason)
    }
}

/// Everything the daemon owns for its lifetime (steps 2–7).
pub struct Core {
    pub opts: Options,
    pub config: Config,
    pub store: Store,
    pub gits: BTreeMap<RepoName, Git>,
    pub locks: Locks,
    pub env: InstallEnv,
    pub before: DaemonBefore,
    pub confirmer: Confirmer,
    pub warnings: Warnings,
    /// `pins.json`: the effective safety flags.
    pub pins: Pins,
    instance: InstanceLock,
}

/// Steps 2–7. `open_git` is `Git::open` in production (tests pass
/// `Git::open_with_env` with a sandbox HOME).
pub fn prepare(opts: Options, confirmer: Confirmer, open_git: &dyn Fn(&Path) -> Result<Git>) -> Result<Core> {
    // 2. Single instance before anything else is read.
    let instance = InstanceLock::take(&crate::lock_path(&opts.config_dir))?;
    prepare_locked(opts, confirmer, open_git, &mut Some(instance))
}

/// Steps 3–7 under an instance lock the caller already holds. The lock
/// moves into the [`Core`] only on success; on an error it stays in
/// `instance`, so `main` can retry without letting another daemon in
/// (§5.1a, waiting for the store).
pub fn prepare_locked(
    opts: Options,
    confirmer: Confirmer,
    open_git: &dyn Fn(&Path) -> Result<Git>,
    instance: &mut Option<InstanceLock>,
) -> Result<Core> {
    if instance.is_none() {
        return Err(StrataError::new(Code::InvalidInput, "prepare_locked without the instance lock")
            .with_detail(json!({ "reason": "no_instance_lock" })));
    }
    // 3. Config: parse (TOML + validate in file order), then validate.
    let text = retry_eintr(|| fs::read_to_string(&opts.config_file)).map_err(|e| classify_io_error(&e, IoSide::Read, &opts.config_file))?;
    let mut config = Config::parse(&text)?;
    config.validate()?;
    // 4. Every path exists and is its own realpath.
    for (name, rc) in &config.repos {
        check_realpath(name, &rc.path)?;
    }
    check_realpath_root(&config.paths.root)?;
    // 5. Pins: tighten now, record relaxations as pending; never prompt.
    //    The loaded config carries the effective (pinned) flags.
    let pins = Pins::load(&crate::pins_path(&opts.config_dir), &config)?;
    for (name, rc) in config.repos.iter_mut() {
        if let Some(f) = pins.pinned(name) {
            confirm_flow::apply(rc, f);
        }
    }
    // 6. The store checks its own clean-shutdown marker.
    let checkouts: Vec<&Path> = config.repos.values().map(|r| r.path.as_path()).collect();
    let store = Store::open(&config.paths.root, &checkouts)?;
    // deps-drift §2.4: publish's install-input snapshots of a stopped daemon.
    for p in strata_deps::clean_snapshots(&config.paths) {
        tracing::warn!(path = %p.display(), "an install-input snapshot could not be removed");
    }
    // Dev-port leases (config [dev_ports]); validated with the config.
    store.set_dev_ports_policy(Some(strata_store::DevPortsPolicy::probing(config.dev_ports.blocks()?)));
    // 7. One Git per repo; two names for one repository → NameTaken.
    let mut gits = BTreeMap::new();
    let mut ids: BTreeMap<RepoId, RepoName> = BTreeMap::new();
    for (name, rc) in &config.repos {
        let git = open_git(&rc.path)?;
        let id = git.repo_id();
        if let Some(other) = ids.get(&id) {
            return Err(StrataError::new(
                Code::NameTaken,
                format!("{other} and {name} are the same repository"),
            )
            .with_detail(json!({ "reason": "repo_id", "repos": [other.as_str(), name.as_str()], "repo_id": id })));
        }
        ids.insert(id, name.clone());
        gits.insert(name.clone(), git);
    }
    let locks = Locks::new(config.repos.keys().cloned(), opts.lock_wait);
    let warnings = Warnings::new(opts.alert.clone());
    for repo in pins.pending() {
        warnings.add(DaemonWarning::ConfigRelaxPending { repo });
    }
    let Some(instance) = instance.take() else {
        return Err(StrataError::new(Code::InvalidInput, "prepare_locked without the instance lock")
            .with_detail(json!({ "reason": "no_instance_lock" })));
    };
    Ok(Core {
        pins,
        env: InstallEnv { path: opts.install_path.clone(), timeout: InstallEnv::DEFAULT_TIMEOUT },
        before: DaemonBefore,
        opts,
        config,
        store,
        gits,
        locks,
        confirmer,
        warnings,
        instance,
    })
}

fn check_realpath(name: &RepoName, path: &Path) -> Result<()> {
    let real = retry_eintr(|| fs::canonicalize(path)).map_err(|e| {
        StrataError::new(Code::InvalidInput, format!("repo {name}: {} does not exist", path.display()))
            .with_detail(json!({ "reason": "missing", "repo": name.as_str(), "path": path, "io": e.to_string() }))
    })?;
    if real != path {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!("repo {name}: {} is not its realpath {}", path.display(), real.display()),
        )
        .with_detail(json!({ "reason": "not_realpath", "repo": name.as_str(), "path": path, "realpath": real })));
    }
    Ok(())
}

fn check_realpath_root(root: &Path) -> Result<()> {
    // The root may not exist yet (Store::open creates it); when it does it
    // must be its realpath too.
    if let Ok(real) = retry_eintr(|| fs::canonicalize(root))
        && real != root
    {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!("paths.root {} is not its realpath {}", root.display(), real.display()),
        )
        .with_detail(json!({ "reason": "not_realpath", "path": root, "realpath": real })));
    }
    Ok(())
}

impl Core {
    /// The effective (pinned) flags of `repo`.
    pub fn pinned(&self, repo: &RepoName) -> Result<PinnedFlags> {
        self.pins
            .pinned(repo)
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")))
    }

    /// `config_relax` (§5.5). `between` runs between `require` and
    /// `validate` (test seam; the handler passes `&|| {}`).
    pub fn config_relax_with(&self, repo: &RepoName, between: &dyn Fn()) -> Result<()> {
        let id = match self.gits.get(repo) {
            Some(g) => g.repo_id(),
            None => return Err(StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}"))),
        };
        confirm_flow::config_relax_with(&self.confirmer, &self.pins, repo, id, between)?;
        self.warnings.remove(|w| matches!(w, DaemonWarning::ConfigRelaxPending { repo: r } if r == repo));
        Ok(())
    }

    pub fn config_relax(&self, repo: &RepoName) -> Result<()> {
        self.config_relax_with(repo, &|| {})
    }

    pub fn instance_lock(&self) -> &InstanceLock {
        &self.instance
    }

    /// §5.9 after the daemon is gone: `Store::close` (writes the store's
    /// clean-shutdown marker), unlink the socket, release the flock.
    pub fn close(self) -> Result<()> {
        let Core { store, opts, instance, gits, .. } = self;
        drop(gits);
        let r = store.close();
        let _ = fs::remove_file(&opts.socket);
        drop(instance);
        r
    }
}

/// The running daemon: `Sessions` opened once over the [`Core`].
pub struct Daemon<'a> {
    pub core: &'a Core,
    pub sessions: Sessions<'a>,
    pub recovered: RecoverReport,
    stop: AtomicBool,
    /// Review P1-3: the open connections only (a clone each, for the
    /// stop), removed when the connection ends.
    conns: Mutex<BTreeMap<u64, UnixStream>>,
    next_conn: std::sync::atomic::AtomicU64,
    /// `accept` failures for want of descriptors (EMFILE/ENFILE).
    fd_errors: std::sync::atomic::AtomicU64,
    /// §5.9: the reconcile loop's counters and its last pass (daemon_status).
    reconcile: Mutex<Value>,
    /// §5.12: the du cache and its background pass (stopped on drop).
    pub du: crate::du::DuHandle,
}

impl<'a> Daemon<'a> {
    /// Steps 8–11.
    pub fn start(core: &'a Core) -> Result<Daemon<'a>> {
        // Review P1-5: a ref transaction killed between prepare and commit
        // (SIGKILL, power loss, our own killpg) leaves `refs/strata/*.lock`;
        // every later transaction on that ref would fail. Only locks older
        // than this start go (the instance flock is held).
        let started = SystemTime::now();
        for (repo, git) in core.gits.iter() {
            let _ = core.locks.with_repo(repo, || {
                for path in git.clear_stale_strata_locks(started) {
                    core.warnings.alert_once(&format!("removed stale ref lock {}", path.display()));
                    core.warnings.add(DaemonWarning::StaleRefLock { repo: repo.clone(), path });
                }
                Ok(())
            });
        }
        // 8. Sessions once.
        let sessions = Sessions::open(&core.config, &core.gits, &core.store, &core.locks, &core.env, &core.before)?;
        // 9. recover, then gc per repo under its write lock.
        let recovered = sessions.recover()?;
        // status-perf A1: status reads old-tree verdicts the `old-tree`
        // thread makes (started by `serve`, so after this recover).
        sessions.set_old_tree_mode(strata_session::OldTreeMode::Deferred);
        for repo in core.config.repos.keys() {
            if let Err(e) = core.locks.with_repo(repo, || core.store.gc_repo(repo, GC_KEEP).map(|_| ())) {
                core.warnings.add(DaemonWarning::VerifyFailed { message: format!("gc {repo}: {}", e.message) });
            }
        }
        // 10. Running → Unknown, then report every Unknown. No push, no replay.
        let now = SystemTime::now();
        for rec in core.store.open_publishes()? {
            let id = rec.group.id;
            if rec.phase == PublishPhase::Running {
                core.store.set_publish_phase(id, PublishPhase::Running, PublishPhase::Unknown, None, now)?;
            }
            core.warnings.add(DaemonWarning::UnknownPublish { id });
        }
        // Review M6: kept scenes of uncertain standalone/restore pushes.
        for (repo, commit, scene) in crate::resolve::push_scenes(&core.config.paths.root) {
            core.warnings.add(DaemonWarning::UnknownPush { repo, commit, scene });
        }
        // 11. Quarantine and non-directory worktrees.
        for path in quarantine_entries(&core.config.paths.root) {
            core.warnings.add(DaemonWarning::Quarantine { path });
        }
        if core.store.is_poisoned() {
            core.warnings.add(DaemonWarning::StorePoisoned);
        }
        Ok(Daemon {
            core,
            sessions,
            recovered,
            stop: AtomicBool::new(false),
            conns: Mutex::new(BTreeMap::new()),
            next_conn: std::sync::atomic::AtomicU64::new(0),
            fd_errors: std::sync::atomic::AtomicU64::new(0),
            reconcile: Mutex::new(json!({ "runs": 0, "last": null })),
            du: crate::du::Du::open(&core.config.paths.root),
        })
    }

    /// Connections being served now (review P1-3: a finished connection
    /// leaves no descriptor behind).
    pub fn open_connections(&self) -> usize {
        self.conns.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Ask the accept loop to stop (SIGTERM, or a test).
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Record one finished reconcile pass (§5.9) for `daemon_status`.
    pub(crate) fn reconciled(&self, last: Value) {
        let mut r = self.reconcile.lock().unwrap_or_else(|p| p.into_inner());
        let runs = r["runs"].as_u64().unwrap_or(0) + 1;
        *r = json!({ "runs": runs, "last": last });
    }

    /// Step 12 onwards: serve until [`Daemon::request_stop`] (or `external`
    /// becomes true). Then §5.9: stop accepting, fail queued writers with
    /// `ReadOnly`, let running requests finish, close idle connections.
    pub fn serve(&self, listener: &UnixListener, external: &AtomicBool, handle: &(dyn Fn(Request) -> Reply + Sync)) -> Result<()> {
        listener
            .set_nonblocking(true)
            .map_err(|e| classify_io_error(&e, IoSide::Read, &self.core.opts.socket))?;
        let mut poisoned = false;
        // Incident 2026-10-27: out of descriptors (EMFILE/ENFILE) the loop
        // used to retry every 20 ms in silence (and macOS closes the client
        // whose accept failed, which the CLI saw as `socket_closed`). Now
        // it logs, backs off (20 ms doubling to 1 s), and frees a reserve
        // descriptor so the next accept has a slot. While the reserve
        // cannot be reopened the daemon is still out of descriptors: each
        // client is answered `Busy` (`too_many_open_files`) and closed.
        let mut reserve = File::open("/dev/null").ok();
        let mut backoff = ACCEPT_BACKOFF_MIN;
        let mut last_log: Option<std::time::Instant> = None;
        std::thread::scope(|s| {
            loop {
                if self.stopping() || external.load(Ordering::SeqCst) {
                    break;
                }
                // Review P1-7: a poisoned writer only recovers by reopening
                // the store (WAL recovery + quick_check). Drain like a stop
                // and exit non-zero; launchd's KeepAlive starts us again.
                if self.core.store.is_poisoned() {
                    poisoned = true;
                    break;
                }
                // Review P1-3: at most MAX_CONNS connections are served at
                // once; past that the loop stops accepting (clients wait in
                // the listen backlog) until one ends.
                if self.open_connections() >= MAX_CONNS {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if reserve.is_none() {
                            reserve = File::open("/dev/null").ok();
                            if reserve.is_none() {
                                shed(stream);
                                std::thread::sleep(backoff);
                                backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                                continue;
                            }
                        }
                        backoff = ACCEPT_BACKOFF_MIN;
                        let _ = stream.set_nonblocking(false);
                        let id = self.next_conn.fetch_add(1, Ordering::SeqCst);
                        // Out of descriptors for the clone: answer Busy, not
                        // a silent close.
                        let Ok(c) = stream.try_clone() else {
                            shed(stream);
                            continue;
                        };
                        self.conns.lock().unwrap_or_else(|p| p.into_inner()).insert(id, c);
                        s.spawn(move || {
                            match stream.try_clone() {
                                Ok(w) => {
                                    let mut r = BufReader::new(stream);
                                    let mut w = w;
                                    let mut h = |req: Request| handle(req);
                                    let _ = rpc::serve_connection(&mut r, &mut w, MAX_FRAME, &mut h);
                                }
                                Err(_) => shed(stream),
                            }
                            // The clone kept for the stop goes with the connection.
                            self.conns.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE)) => {
                        let fd_errors = self.fd_errors.fetch_add(1, Ordering::SeqCst) + 1;
                        if last_log.is_none_or(|t| t.elapsed() >= FD_LOG_EVERY) {
                            last_log = Some(std::time::Instant::now());
                            self.core.warnings.alert_once(&format!(
                                "accept: {e} ({fd_errors} times; {} descriptors open, {} connections); shedding new connections with Busy",
                                open_fd_count().map_or("?".to_string(), |n| n.to_string()),
                                self.open_connections()
                            ));
                        }
                        reserve = None;
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                    }
                    Err(e) => {
                        if last_log.is_none_or(|t| t.elapsed() >= FD_LOG_EVERY) {
                            last_log = Some(std::time::Instant::now());
                            self.core.warnings.alert_once(&format!("accept: {e}"));
                        }
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                    }
                }
            }
            // Stop: queued takes fail ReadOnly(shutting_down); the holder
            // finishes. Idle connections see end of input; a connection in
            // the middle of a request still writes its reply.
            drop(reserve.take());
            self.core.locks.shutdown();
            for c in self.conns.lock().unwrap_or_else(|p| p.into_inner()).values() {
                let _ = c.shutdown(std::net::Shutdown::Read);
            }
        });
        if poisoned {
            self.core.warnings.alert_once(
                "the store writer is poisoned after an uncertain commit; git-stratad exits so launchd restarts it and reopens the store",
            );
            return Err(StrataError::new(
                Code::Unknown,
                "the store writer is poisoned; git-stratad exits to reopen the store (launchd restarts it)",
            )
            .with_detail(json!({ "reason": "store_poisoned_restart" })));
        }
        Ok(())
    }

    /// Dispatch one request. Task 3 serves `daemon_status`; the other
    /// methods answer `Failed` until Task 6 wires them.
    pub fn handle(&self, req: Request) -> Reply {
        match &req.params {
            rpc::Params::DaemonStatus(p) => match self.daemon_status(p.verify) {
                Ok(v) => Reply::ok(&req.id, v),
                Err(e) => Reply::error(&req.id, &e),
            },
            _ => Reply::rejected(&req.id, crate::RpcReject::Failed, "not served yet"),
        }
    }

    /// `daemon_status`: holds, queues, the daemon's warnings; never waits.
    pub fn daemon_status(&self, verify: bool) -> Result<Value> {
        let mut warnings = self.core.warnings.snapshot();
        warnings.extend(self.core.locks.long_holds());
        let mut verify_result = Value::Null;
        if verify {
            verify_result = match self.core.store.quick_check() {
                Ok(()) => json!({ "ok": true }),
                Err(e) => {
                    warnings.push(DaemonWarning::VerifyFailed { message: e.message.clone() });
                    json!({ "ok": false, "error": e })
                }
            };
        }
        if self.core.store.is_poisoned() && !warnings.contains(&DaemonWarning::StorePoisoned) {
            warnings.push(DaemonWarning::StorePoisoned);
        }
        Ok(json!({
            "holds": self.core.locks.holds(),
            "repo_waiting": self.core.config.repos.keys()
                .map(|r| (r.as_str().to_string(), self.core.locks.repo_waiting(r)))
                .collect::<BTreeMap<String, usize>>(),
            "warnings": warnings,
            "verify": verify_result,
            "reconcile": self.reconcile.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            "features": crate::FEATURES,
            "fds": {
                "open": open_fd_count(),
                "limit": fd_limit().map(|(soft, _)| soft),
                "accept_errors": self.fd_errors.load(Ordering::SeqCst),
            },
        }))
    }
}

const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(20);
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);
const FD_LOG_EVERY: Duration = Duration::from_secs(10);

/// The soft descriptor limit `raise_fd_limit` asks for (macOS caps
/// `setrlimit` at `OPEN_MAX`, 10240). launchd starts agents with 256; on
/// 2026-10-27 25 live workspaces (then about 8 watcher descriptors each),
/// search-index maps and connections used them up.
pub const FD_LIMIT: u64 = 10240;

/// Raise the soft `RLIMIT_NOFILE` to `min(FD_LIMIT, hard)`. Returns the
/// soft limit before and after. Never lowers it.
pub fn raise_fd_limit() -> io::Result<(u64, u64)> {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: plain syscalls on a local struct.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let before = r.rlim_cur;
    let want = FD_LIMIT.min(r.rlim_max);
    if before >= want {
        return Ok((before, before));
    }
    r.rlim_cur = want;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((before, want))
}

/// (soft, hard) `RLIMIT_NOFILE`.
pub fn fd_limit() -> Option<(u64, u64)> {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) } == 0).then_some((r.rlim_cur, r.rlim_max))
}

/// Descriptors open in this process now (entries of /dev/fd, less the
/// one the listing itself uses).
pub fn open_fd_count() -> Option<usize> {
    let n = fs::read_dir("/dev/fd").ok()?.count();
    Some(n.saturating_sub(1))
}

/// Answer a client the daemon cannot serve now (out of descriptors) with
/// `Busy` and close it, so it does not hang in the backlog.
fn shed(stream: UnixStream) {
    use std::io::Write;
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    let e = StrataError::new(Code::Busy, "git-stratad is out of file descriptors; try again shortly")
        .with_detail(json!({ "reason": "too_many_open_files" }));
    let mut w = &stream;
    let _ = writeln!(w, "{}", rpc::Reply::error(&Value::Null, &e).0);
}

/// Step 12: bind the socket (directory 0700, socket 0600). The caller
/// holds the instance lock, so a socket file left by a crash is stale.
pub fn bind(socket: &Path) -> Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        make_private_dir(dir)?;
    }
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(classify_io_error(&e, IoSide::Write, socket)),
    }
    let l = UnixListener::bind(socket).map_err(|e| classify_io_error(&e, IoSide::Write, socket))?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).map_err(|e| classify_io_error(&e, IoSide::Write, socket))?;
    Ok(l)
}

/// Quarantined entries and non-directory `worktrees` under every shadow
/// repository `<root>/sessions/.repos/<session>/<repo>.git`. Read with
/// `lstat` only: nothing is followed and no git runs there.
pub fn quarantine_entries(root: &Path) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    let repos = root.join("sessions/.repos");
    let Ok(sessions) = fs::read_dir(&repos) else { return Vec::new() };
    for s in sessions.flatten() {
        let Ok(shadows) = fs::read_dir(s.path()) else { continue };
        for sh in shadows.flatten() {
            let shadow = sh.path();
            if !shadow.extension().is_some_and(|e| e == "git") {
                continue;
            }
            if let Ok(q) = fs::read_dir(shadow.join("strata-quarantine")) {
                for e in q.flatten() {
                    out.insert(e.path());
                }
            }
            let wt = shadow.join("worktrees");
            match fs::symlink_metadata(&wt) {
                Ok(m) if !m.is_dir() => {
                    out.insert(wt);
                }
                Ok(_) => {
                    if let Ok(rd) = fs::read_dir(&wt) {
                        for e in rd.flatten() {
                            if fs::symlink_metadata(e.path()).is_ok_and(|m| !m.is_dir()) {
                                out.insert(e.path());
                            }
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
    out.into_iter().collect()
}

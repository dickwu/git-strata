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
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

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
use crate::drain::{self, Drain, ShutdownMode};
use crate::locks::Locks;
use crate::rpc::{self, Reply, Request};
use crate::{DaemonWarning, GC_KEEP, LOCK_WAIT, PinnedFlags, Result};

#[path = "store_state.rs"]
mod store_state;

/// Review P1-3: connections served at once; more wait in the backlog.
pub const MAX_CONNS: usize = 128;

/// Give the seal acknowledgement and outstanding replies time to reach readers.
const SEALED_REPLY_GRACE: Duration = Duration::from_secs(2);

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
    /// [`CATCH_UP_EVERY`](crate::CATCH_UP_EVERY) in production
    /// (rm-followups §1.6 T2); tests lengthen or shorten it.
    pub catch_up_every: Duration,
    pub config_poll_every: Duration,
    /// Test-only scheduler admission; direct worker passes remain callable.
    #[cfg(feature = "test-seams")]
    pub background_main_sync: bool,
    /// Required for every seams-enabled worker, even when constructed in-process.
    #[cfg(feature = "test-seams")]
    pub fixture: Option<crate::fixture::WorkerFixture>,
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
            catch_up_every: crate::CATCH_UP_EVERY,
            config_poll_every: Duration::from_secs(2),
            #[cfg(feature = "test-seams")]
            background_main_sync: true,
            #[cfg(feature = "test-seams")]
            fixture: None,
        }
    }
}

/// Refuse production daemon startup if any linked Strata crate enabled test seams.
pub fn refuse_test_seams() -> Result<()> {
    if strata_types::TEST_SEAMS {
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
        oo.read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC);
        let file = retry_eintr(|| oo.open(path))
            .map_err(|e| classify_io_error(&e, IoSide::Write, path))?;
        let locked = retry_eintr(|| {
            // SAFETY: flock on a descriptor this function owns.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
        if let Err(e) = locked {
            if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(
                    StrataError::new(Code::Exists, "another git-stratad is running")
                        .with_detail(json!({ "reason": "daemon_running", "lock": path })),
                );
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
    match retry_eintr(|| {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }) {
        Ok(()) => Ok(()),
        Err(e) => Err(classify_io_error(&e, IoSide::Write, dir)),
    }
}

/// The daemon's own warnings: deduplicated, each human alert printed once.
pub struct Warnings {
    list: Mutex<Vec<(DaemonWarning, SystemTime)>>,
    first_seen: Mutex<Vec<(DaemonWarning, SystemTime)>>,
    alert: Arc<dyn Fn(&str) + Send + Sync>,
    /// Keys of the lines `alert_first` already printed.
    seen: Mutex<std::collections::BTreeSet<String>>,
}

impl Warnings {
    fn new(alert: Arc<dyn Fn(&str) + Send + Sync>) -> Warnings {
        Warnings {
            list: Mutex::new(Vec::new()),
            first_seen: Mutex::new(Vec::new()),
            alert,
            seen: Mutex::new(Default::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(DaemonWarning, SystemTime)>> {
        self.list.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record `w` once. Quarantine and attach alerts also print one line.
    pub fn add(&self, w: DaemonWarning) {
        let mut l = self.lock();
        if l.iter().any(|(v, _)| v == &w) {
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
        l.push((w, SystemTime::now()));
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
        let fresh = self
            .seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_owned());
        if fresh {
            (self.alert)(line);
        }
        fresh
    }

    pub fn remove(&self, f: impl Fn(&DaemonWarning) -> bool) {
        self.lock().retain(|(w, _)| !f(w));
    }

    pub fn snapshot(&self) -> Vec<DaemonWarning> {
        self.lock().iter().map(|(w, _)| w.clone()).collect()
    }

    pub fn times(&self, warnings: &[DaemonWarning]) -> Vec<Value> {
        let list = self.lock();
        let mut first = self.first_seen.lock().unwrap_or_else(|p| p.into_inner());
        let same = |a: &DaemonWarning, b: &DaemonWarning| match (a, b) {
            (
                DaemonWarning::AutoRebaseStuck {
                    session: a,
                    repo: ar,
                    rev: av,
                    since: at,
                    ..
                },
                DaemonWarning::AutoRebaseStuck {
                    session: b,
                    repo: br,
                    rev: bv,
                    since: bt,
                    ..
                },
            ) => a == b && ar == br && av == bv && at == bt,
            _ => a == b,
        };
        first.retain(|(w, _)| {
            !matches!(w, DaemonWarning::AutoRebaseStuck { .. })
                || warnings.iter().any(|v| same(w, v))
        });
        warnings.iter().map(|w| {
            let at = match w {
                DaemonWarning::LongHold { since, .. } | DaemonWarning::LongWait { since, .. } => *since,
                _ => list.iter().find(|(v, _)| v == w).map(|(_, at)| *at).unwrap_or_else(|| {
                    if let Some((_, at)) = first.iter().find(|(v, _)| same(v, w)) { *at } else {
                        let at = SystemTime::now(); first.push((w.clone(), at)); at
                    }
                }),
            };
            json!({"warning":w, "raised_at":strata_types::serde_rfc3339::format(&at).unwrap_or_default()})
        }).collect()
    }
}

/// `BeforeCheckpoint` for `Sessions::open`: called while `move_workspace`
/// already holds the queue and the repo lock, so it takes neither and
/// checkpoints directly (no Tracker before the first scan → `None`).
pub struct DaemonBefore;

impl BeforeCheckpoint for DaemonBefore {
    fn checkpoint(
        &self,
        ws: &WorkspaceRef<'_>,
        base: &Base,
        reason: CheckpointReason,
    ) -> Result<Checkpointed> {
        strata_overlay::checkpoint(ws, base, None, reason)
    }
}

pub(crate) type GitOpener = dyn Fn(&Path) -> Result<Git> + Send + Sync;

/// Everything the daemon owns for its lifetime (steps 2–7).
pub struct Core {
    pub upstream: crate::upstream::Cache,
    pub opts: Options,
    pub config: Config,
    pub store: Arc<Store>,
    pub catalog: strata_session::catalog::CatalogCell<crate::catalog::Catalog>,
    pub(crate) commit: Mutex<()>,
    pub reload: Arc<crate::reload::ReloadState>,
    pub(crate) open_git: Arc<GitOpener>,
    pub(crate) started: SystemTime,
    pub(crate) drain: Drain,
    epoch: String,
    pub gits: BTreeMap<RepoName, Git>,
    pub locks: Locks,
    pub env: InstallEnv,
    pub before: DaemonBefore,
    pub confirmer: Confirmer,
    pub warnings: Warnings,
    /// `pins.json`: the effective safety flags.
    pub pins: Pins,
    /// `sessions-audit.jsonl`: one line per session auto-close judged.
    pub sessions_audit: crate::audit::AuditFile,
    pub main_sync: crate::main_sync::State,
    /// `daemon_status.busy`: the `Busy` replies requests got.
    pub busy: crate::busy::BusyLog,
    /// `daemon_status.volume`: the data root's volume, sampled on a thread.
    pub volume: crate::volume::VolumeHandle,
    /// Status-bar §3.1: long work in flight (`daemon_status.activity`).
    pub activity: Arc<crate::activity::Activities>,
    instance: InstanceLock,
}

/// Steps 2–7. `open_git` is `Git::open` in production (tests pass
/// `Git::open_with_env` with a sandbox HOME).
pub fn prepare(
    opts: Options,
    confirmer: Confirmer,
    open_git: &(impl Fn(&Path) -> Result<Git> + Clone + Send + Sync + 'static),
) -> Result<Core> {
    let instance = take_instance(&opts)?;
    prepare_locked(opts, confirmer, open_git, &mut Some(instance))
}

/// Both process startup and in-process workers enter through this guard.
pub fn take_instance(opts: &Options) -> Result<InstanceLock> {
    // Test workers must prove their data and socket paths before even creating
    // daemon.lock or answering Busy on the early listener.
    #[cfg(feature = "test-seams")]
    read_config(opts)?;
    InstanceLock::take(&crate::lock_path(&opts.config_dir))
}

/// Dependency children keep the daemon's sanitized environment; a test
/// fixture adds its HOME and Git policy (CR r2 #5).
fn install_child_env(opts: &Options) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    #[cfg(feature = "test-seams")]
    if let Some(fixture) = &opts.fixture {
        return fixture.child_env();
    }
    let _ = opts;
    Vec::new()
}

fn read_config(opts: &Options) -> Result<(String, Config)> {
    #[cfg(feature = "test-seams")]
    let fixture = crate::fixture::WorkerFixture::check_layout(opts)?;
    let text = retry_eintr(|| fs::read_to_string(&opts.config_file))
        .map_err(|e| classify_io_error(&e, IoSide::Read, &opts.config_file))?;
    // Validate the same parsed value that opens the store. The config may have
    // changed since the pre-lock check, including between startup retries.
    #[cfg(feature = "test-seams")]
    let config = fixture.parse_config(&text)?;
    #[cfg(not(feature = "test-seams"))]
    let config = Config::parse(&text)?;
    config.validate()?;
    Ok((text, config))
}

/// Steps 3–7 under an instance lock the caller already holds. The lock
/// moves into the [`Core`] only on success; on an error it stays in
/// `instance`, so `main` can retry without letting another daemon in
/// (§5.1a, waiting for the store).
pub fn prepare_locked(
    opts: Options,
    confirmer: Confirmer,
    open_git: &(impl Fn(&Path) -> Result<Git> + Clone + Send + Sync + 'static),
    instance: &mut Option<InstanceLock>,
) -> Result<Core> {
    if instance.is_none() {
        return Err(StrataError::new(
            Code::InvalidInput,
            "prepare_locked without the instance lock",
        )
        .with_detail(json!({ "reason": "no_instance_lock" })));
    }
    // 3. Config: parse (TOML + validate in file order), then validate.
    let (text, mut config) = read_config(&opts)?;
    let source = config.clone();
    // 4. Every path exists and is its own realpath.
    for (name, rc) in &config.repos {
        check_realpath(name, &rc.path)?;
    }
    check_realpath_root(&config.paths.root)?;
    // 5. Pins: tighten now, record relaxations as pending; never prompt.
    //    The loaded config carries the effective (pinned) flags.
    let pins = Pins::load(&crate::pins_path(&opts.config_dir), &config)?;
    let sessions_audit = crate::audit::AuditFile::sessions(&crate::pins_path(&opts.config_dir));
    for (name, rc) in config.repos.iter_mut() {
        if let Some(f) = pins.pinned(name) {
            confirm_flow::apply(rc, &f);
        }
    }
    // 6. The store checks its own clean-shutdown marker.
    let checkouts: Vec<&Path> = config.repos.values().map(|r| r.path.as_path()).collect();
    let store = Arc::new(store_state::open(
        &opts.config_dir,
        &config.paths.root,
        &checkouts,
    )?);
    // deps-drift §2.4: publish's install-input snapshots of a stopped daemon.
    for p in strata_deps::clean_snapshots(&config.paths) {
        tracing::warn!(path = %p.display(), "an install-input snapshot could not be removed");
    }
    // Main-sync whole-tree exports left by a killed daemon (removed on Drop
    // otherwise). Entries are `<pid>-…`; a live owner's are kept.
    sweep_exports(&config.paths.root);
    // Dev-port leases (config [dev_ports]); validated with the config.
    store.set_dev_ports_policy(Some(strata_store::DevPortsPolicy::probing(
        config.dev_ports.blocks()?,
    )));
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
    locks.set_store(store.clone());
    let reload = Arc::new(crate::reload::ReloadState::default());
    let wake = reload.clone();
    locks.set_close_hook(Arc::new(move || wake.wake()));
    let catalog = strata_session::catalog::CatalogCell::new(crate::catalog::Catalog::build(
        source,
        gits.clone(),
        pins.snapshot(),
        1,
        crate::reload::source_hash(text.as_bytes())?,
    )?);
    locks.set_catalog(catalog.id());
    // new-lock §2.1: every repo's release-point snapshot, computed once now
    // under its write lock and refreshed by each release afterwards.
    for (name, rc) in &config.repos {
        locks.set_ref_source(name, gits[name].clone(), &rc.default_branch);
        let _ = locks.with_repo(name, || Ok(()));
    }
    let warnings = Warnings::new(opts.alert.clone());
    for repo in pins.pending() {
        warnings.add(DaemonWarning::ConfigRelaxPending { repo });
    }
    let Some(instance) = instance.take() else {
        return Err(StrataError::new(
            Code::InvalidInput,
            "prepare_locked without the instance lock",
        )
        .with_detail(json!({ "reason": "no_instance_lock" })));
    };
    let drain = Drain::default();
    let volume = crate::volume::Volume::start_guarded(&config.paths.root, drain.clone());
    Ok(Core {
        busy: Default::default(),
        volume,
        catalog,
        commit: Mutex::new(()),
        reload,
        open_git: Arc::new(open_git.clone()),
        started: SystemTime::now(),
        drain,
        epoch: drain::random_id(),
        main_sync: Default::default(),
        activity: Default::default(),
        upstream: Default::default(),
        pins,
        sessions_audit,
        env: InstallEnv {
            path: opts.install_path.clone(),
            timeout: InstallEnv::DEFAULT_TIMEOUT,
            child_env: install_child_env(&opts),
        },
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

pub(crate) fn check_realpath(name: &RepoName, path: &Path) -> Result<()> {
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

pub(crate) fn check_realpath_root(root: &Path) -> Result<()> {
    // The root may not exist yet (Store::open creates it); when it does it
    // must be its realpath too.
    if let Ok(real) = retry_eintr(|| fs::canonicalize(root))
        && real != root
    {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!(
                "paths.root {} is not its realpath {}",
                root.display(),
                real.display()
            ),
        )
        .with_detail(json!({ "reason": "not_realpath", "path": root, "realpath": real })));
    }
    Ok(())
}

impl Core {
    /// Explicit operation entry; nested domain calls keep their caller's pin.
    pub fn admit(&self) -> Option<strata_session::catalog::Pin<'_>> {
        self.catalog
            .pinned()
            .is_err()
            .then(|| self.catalog.enter(|c| c.generation))
    }
    pub fn cfg_hash(&self, repo: &RepoName) -> Result<String> {
        Ok(strata_session::catalog::content_hash(
            &serde_json::to_vec(&self.snapshot()?.config.repos.get(repo))
                .map_err(|e| StrataError::new(Code::Corrupt, e.to_string()))?,
        ))
    }
    pub fn snapshot(&self) -> Result<&crate::catalog::Catalog> {
        self.catalog.pinned()
    }
    /// The current pinned dependency policy, including a later `config relax`.
    pub fn effective_sync_deps(&self, repo: &RepoName) -> strata_types::SyncDeps {
        self.snapshot()
            .ok()
            .and_then(|c| c.pins.get(repo))
            .and_then(|p| p.sync_deps)
            .unwrap_or(strata_types::SyncDeps::Confirm)
    }

    /// The effective (pinned) flags of `repo`.
    pub fn pinned(&self, repo: &RepoName) -> Result<PinnedFlags> {
        self.snapshot()?
            .pins
            .get(repo)
            .cloned()
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")))
    }

    pub fn config_relax_with(&self, repo: &RepoName, between: &dyn Fn()) -> Result<()> {
        let _admission = self.admit();
        let shown = self
            .snapshot()?
            .file_flags
            .get(repo)
            .cloned()
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, "no repo"))?;
        self.relax_shown(repo, &shown, between)
    }

    pub fn config_relax(&self, repo: &RepoName) -> Result<()> {
        self.config_relax_with(repo, &|| {})
    }

    pub fn instance_lock(&self) -> &InstanceLock {
        &self.instance
    }

    /// Finalize shutdown for the daemon executable. An accepted restart
    /// exits with EX_TEMPFAIL after the store, socket and flock are closed.
    /// In-process callers can take the code with [Self::close_with_exit_code].
    pub fn close(self) -> Result<()> {
        let code = self.close_with_exit_code()?;
        if code != 0 {
            std::process::exit(i32::from(code));
        }
        Ok(())
    }

    /// §5.9: close the store, unlink the socket, release the flock, and
    /// return the chosen exit code. A close failure during orderly
    /// shutdown is recorded without changing that code.
    pub fn close_with_exit_code(self) -> Result<u8> {
        let mode = self.drain.shutdown_mode();
        let Core {
            store,
            opts,
            instance,
            locks,
            epoch,
            ..
        } = self;
        drop(locks);
        let r = Arc::try_unwrap(store)
            .map_err(|_| StrataError::new(Code::Busy, "store is still borrowed"))
            .and_then(Store::close);
        let _ = fs::remove_file(&opts.socket);
        if let Some(mode) = mode {
            let error = r.as_ref().err();
            if let Some(e) = error {
                (opts.alert)(&format!("daemon shutdown: {}", e.message));
            }
            if let Err(e) = record_shutdown(&opts.config_dir, mode, &epoch, error) {
                (opts.alert)(&format!("could not record daemon shutdown: {e}"));
            }
            return Ok(mode.exit_code());
        }
        drop(instance);
        r.map(|()| 0)
    }
}

fn record_shutdown(
    dir: &Path,
    mode: ShutdownMode,
    epoch: &str,
    close_error: Option<&StrataError>,
) -> io::Result<()> {
    let tmp = dir.join(format!(".last_shutdown.{epoch}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        let data = json!({
            "mode": mode,
            "epoch": epoch,
            "close_error": close_error,
            "at": strata_types::serde_rfc3339::format(&SystemTime::now()).ok(),
        });
        serde_json::to_writer(&mut file, &data)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join("last_shutdown.json"))?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The running daemon: `Sessions` opened once over the [`Core`].
pub struct Daemon<'a> {
    pub core: &'a Core,
    pub sessions: Sessions<'a>,
    pub recovered: RecoverReport,
    stop: AtomicBool,
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
        let _admission = core.admit();
        // Review P1-5: a ref transaction killed between prepare and commit
        // (SIGKILL, power loss, our own killpg) leaves `refs/strata/*.lock`;
        // every later transaction on that ref would fail. Only locks older
        // than this start go (the instance flock is held).
        let started = SystemTime::now();
        for (repo, git) in core.gits.iter() {
            let _ = core.locks.with_repo(repo, || {
                for path in git.clear_stale_strata_locks(started) {
                    core.warnings
                        .alert_once(&format!("removed stale ref lock {}", path.display()));
                    core.warnings.add(DaemonWarning::StaleRefLock {
                        repo: repo.clone(),
                        path,
                    });
                }
                Ok(())
            });
        }
        // 8. Sessions once.
        let sessions = Sessions::open(
            &core.config,
            &core.gits,
            &core.store,
            &core.locks,
            &core.env,
            &core.before,
        )?
        .with_catalog(core);
        // 9. recover, then gc per repo under its write lock.
        let recovered = sessions.recover()?;
        for p in &recovered.orphans_retired {
            tracing::warn!(path = %p.display(), "recover retired an orphan session directory");
        }
        // status-perf A1: status reads old-tree verdicts the `old-tree`
        // thread makes (started by `serve`, so after this recover).
        sessions.set_old_tree_mode(strata_session::OldTreeMode::Deferred);
        for repo in core.config.repos.keys() {
            if let Err(e) = core
                .locks
                .with_repo(repo, || core.store.gc_repo(repo, GC_KEEP).map(|_| ()))
            {
                core.warnings.add(DaemonWarning::VerifyFailed {
                    message: format!("gc {repo}: {}", e.message),
                });
            }
        }
        // 10. Running → Unknown, then report every Unknown. No push, no replay.
        let now = SystemTime::now();
        for rec in core.store.open_publishes()? {
            let id = rec.group.id;
            if rec.phase == PublishPhase::Running {
                core.store.set_publish_phase(
                    id,
                    PublishPhase::Running,
                    PublishPhase::Unknown,
                    None,
                    now,
                )?;
            }
            core.warnings.add(DaemonWarning::UnknownPublish { id });
        }
        // rm-followups §1.6 rule 4: each repo's rebase_pending rows are
        // reconstructed under its write lock (no publish runs yet).
        for (repo, git) in core.gits.iter() {
            let Some(rc) = core.config.repos.get(repo) else {
                continue;
            };
            let sc = strata_publish::pending::Scope {
                store: &core.store,
                git,
                repo,
                branch: &rc.publish.branch,
            };
            if let Err(e) = core
                .locks
                .with_repo(repo, || strata_publish::pending::reconstruct(sc))
            {
                tracing::warn!(repo = %repo, error = %e.message, "rebase_pending reconstruction failed");
            }
        }
        // Review M6: kept scenes of uncertain standalone/restore pushes.
        for (repo, commit, scene) in crate::resolve::push_scenes(&core.config.paths.root) {
            core.warnings.add(DaemonWarning::UnknownPush {
                repo,
                commit,
                scene,
            });
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
            next_conn: std::sync::atomic::AtomicU64::new(0),
            fd_errors: std::sync::atomic::AtomicU64::new(0),
            reconcile: Mutex::new(json!({ "runs": 0, "last": null })),
            du: crate::du::Du::open_guarded(&core.config.paths.root, core.drain.clone()),
        })
    }

    /// `config_relax`, then the newly pinned `rm.regenerable` goes to
    /// `Sessions` at once (rm-followups §1.8: no restart needed).
    pub fn config_relax_with(&self, repo: &RepoName, between: &dyn Fn()) -> Result<()> {
        self.core.config_relax_with(repo, between)?;
        Ok(())
    }

    pub fn config_relax(&self, repo: &RepoName) -> Result<()> {
        self.config_relax_with(repo, &|| {})
    }

    /// Connections being served now (review P1-3: a finished connection
    /// leaves no descriptor behind).
    pub fn open_connections(&self) -> usize {
        self.core.drain.open_connections()
    }

    /// Worker control entry point for H2. The service socket cannot start a handover.
    pub fn begin_handover_parking(&self, id: &str, to_pid: u32) -> bool {
        self.core.drain.begin_parking(id, to_pid)
    }

    /// Resume this worker's parked requests in dispatch sequence order. Each connection
    /// continues serving its saved read-ahead after its first reply.
    pub fn abort_handover_parking(&self, id: &str) -> bool {
        self.core.drain.abort_parking(id)
    }

    fn finish_connections(&self, mode: Option<ShutdownMode>) {
        self.core
            .drain
            .shutdown_connections(std::net::Shutdown::Read);
        // A sealed daemon has already waited for writes to finish. So has an
        // H2 stop without a sealed lease (mode Sigterm): finish_serving waited
        // for admitted work and every terminal refusal (CR r2 #2). Allow final
        // replies, including the small seal acknowledgement, before unblocking
        // socket writes to any client that has stopped reading.
        let settled = match mode {
            Some(ShutdownMode::Stop | ShutdownMode::Restart | ShutdownMode::Hold) => true,
            Some(_) => self.core.drain.stopped_handover(),
            None => false,
        };
        if settled {
            let deadline = Instant::now() + SEALED_REPLY_GRACE;
            while self.open_connections() != 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.core
                .drain
                .shutdown_connections(std::net::Shutdown::Write);
        }
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
    pub fn serve(
        &self,
        listener: &UnixListener,
        external: &AtomicBool,
        handle: &(dyn Fn(Request) -> Reply + Sync),
    ) -> Result<()> {
        self.serve_admitted(listener, external, &|req, _write| handle(req))
    }

    pub(crate) fn serve_admitted(
        &self,
        listener: &UnixListener,
        external: &AtomicBool,
        handle: &(dyn Fn(Request, Option<drain::Write>) -> Reply + Sync),
    ) -> Result<()> {
        listener
            .set_nonblocking(true)
            .map_err(|e| classify_io_error(&e, IoSide::Read, &self.core.opts.socket))?;
        let mut poisoned = false;
        let mut orderly = false;
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
                if self.core.drain.held_connections() >= MAX_CONNS {
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
                        self.core.drain.register(id, c);
                        s.spawn(move || {
                            let _ = self.core.drain.serve_stream(
                                id,
                                stream,
                                &self.core.busy,
                                &mut io::stderr(),
                                handle,
                            );
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e)
                        if matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE)) =>
                    {
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
            let mode = self.core.drain.finish_serving(poisoned);
            orderly = mode.is_some();
            drop(reserve.take());
            self.core.locks.shutdown();
            self.finish_connections(mode);
        });
        if poisoned && !orderly {
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
        let _catalog = self.core.admit();
        let (draining, inflight, handover) = self.core.drain.status();
        // Status-bar §12.3: the activity registry is copied (and released)
        // before any lock FIFO or store read.
        let activity = self.core.activity.snapshot();
        let catalog = self.core.snapshot()?;
        let mut warnings = self.core.warnings.snapshot();
        warnings.extend(
            self.core
                .locks
                .long_holds_with(&|repo, request| activity.exempt_step(repo, request)),
        );
        // Read with the activity snapshot, before the store, so a hold and
        // the work behind it are shown as of nearly the same moment.
        let holds: Vec<_> = self
            .core
            .locks
            .holds()
            .into_iter()
            .filter(|h| catalog.config.repos.contains_key(&h.repo))
            .collect();
        warnings.extend(self.core.locks.long_waits());
        let pending = self.core.store.rebase_pending(None).unwrap_or_default();
        for p in pending
            .iter()
            .filter(|p| strata_session::rebase_stuck(p, SystemTime::now()))
        {
            warnings.push(DaemonWarning::AutoRebaseStuck {
                session: p.session.clone(),
                repo: p.repo.clone(),
                reason: p.reason.clone(),
                attempts: p.attempts,
                rev: p.rev,
                since: p.first_at,
            });
        }
        let mut verify_result = Value::Null;
        if verify {
            verify_result = match self.core.store.quick_check() {
                Ok(()) => json!({ "ok": true }),
                Err(e) => {
                    warnings.push(DaemonWarning::VerifyFailed {
                        message: e.message.clone(),
                    });
                    json!({ "ok": false, "error": e })
                }
            };
        }
        if self.core.store.is_poisoned() && !warnings.contains(&DaemonWarning::StorePoisoned) {
            warnings.push(DaemonWarning::StorePoisoned);
        }
        let last_attempt = self.core.reload.status();
        let restart_required = last_attempt["restart_required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        warnings.retain(|w| !matches!(w, DaemonWarning::ConfigRelaxPending { .. }));
        warnings.extend(
            catalog
                .pending_relax
                .iter()
                .cloned()
                .map(|repo| DaemonWarning::ConfigRelaxPending { repo }),
        );
        let mut warning_times = self.core.warnings.times(&warnings);
        for (w, time) in warnings.iter().zip(&mut warning_times) {
            if let DaemonWarning::AutoRebaseStuck { session, repo, .. } = w
                && let Some(p) = pending
                    .iter()
                    .find(|p| &p.session == session && &p.repo == repo)
            {
                time["since"] =
                    json!(strata_types::serde_rfc3339::format(&p.first_at).unwrap_or_default());
                time["holders"] = json!(
                    p.detail
                        .as_ref()
                        .and_then(|d| d["holders"].as_array())
                        .map(|h| h.iter().take(5).cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                );
            }
        }
        Ok(json!({
            "holds": holds,
            "activity": activity.json(),
            "queues": self.core.locks.queue_snapshot(),
            "repo_waiting": catalog.config.repos.keys()
                .map(|r| (r.as_str().to_string(), self.core.locks.repo_waiting(r)))
                .collect::<BTreeMap<String, usize>>(),
            "warning_times": warning_times,
            "pins_meta": self.core.pins.metadata(),
            "sessions_audit_error": self.core.sessions_audit.last_error(),
            "warnings": warnings,
            "verify": verify_result,
            "reconcile": self.reconcile.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            "features": crate::FEATURES,
            "draining": draining,
            "handover": handover,
            "inflight": inflight,
            "store_schema": strata_types::STORE_SCHEMA,
            "config": { "generation": catalog.generation, "hash": catalog.hash, "loaded_at": catalog.loaded_at, "last_attempt": last_attempt, "restart_required": restart_required },
            "fds": {
                "open": open_fd_count(),
                "limit": fd_limit().map(|(soft, _)| soft),
                "accept_errors": self.fd_errors.load(Ordering::SeqCst),
            },
            // Status bar (FEATURES `daemon_status.activity`): who answers,
            // the Busy replies, the data volume and the size scan. Each
            // reads a cache or counters, never the file system.
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "started": strata_types::serde_rfc3339::format(&self.core.started).ok(),
            "busy": self.core.busy.json(),
            "volume": self.core.volume.json(),
            "scan": self.du.counts(),
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
    let mut r = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
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
    let mut r = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) } == 0)
        .then_some((r.rlim_cur, r.rlim_max))
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
    let e = StrataError::new(
        Code::Busy,
        "git-stratad is out of file descriptors; try again shortly",
    )
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
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|e| classify_io_error(&e, IoSide::Write, socket))?;
    Ok(l)
}

/// Quarantined entries and non-directory `worktrees` under every shadow
/// repository `<root>/sessions/.repos/<session>/<repo>.git`. Read with
/// `lstat` only: nothing is followed and no git runs there.
pub fn quarantine_entries(root: &Path) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    let repos = root.join("sessions/.repos");
    // Check every traversed directory with lstat; a link is never a directory
    // for this scan, including a dangling link at the quarantine location.
    let real_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    if !real_dir(&root.join("sessions")) || !real_dir(&repos) {
        return Vec::new();
    }
    let Ok(sessions) = fs::read_dir(&repos) else {
        return Vec::new();
    };
    for s in sessions.flatten() {
        if !real_dir(&s.path()) {
            continue;
        }
        let Ok(shadows) = fs::read_dir(s.path()) else {
            continue;
        };
        for sh in shadows.flatten() {
            let shadow = sh.path();
            if !shadow.extension().is_some_and(|e| e == "git") {
                continue;
            }
            if !real_dir(&shadow) {
                out.insert(shadow);
                continue;
            }
            let quarantine = shadow.join("strata-quarantine");
            match fs::symlink_metadata(&quarantine) {
                Ok(m) if !m.is_dir() => {
                    out.insert(quarantine);
                }
                Ok(_) => {
                    if let Ok(q) = fs::read_dir(&quarantine) {
                        for e in q.flatten() {
                            if fs::symlink_metadata(e.path()).is_ok() {
                                out.insert(e.path());
                            }
                        }
                    }
                }
                Err(_) => {}
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

fn sweep_exports(root: &Path) {
    for sub in ["daemon/main-follow", "daemon/sync"] {
        let dir =
            strata_types::RepoPath::try_from(sub.to_owned()).expect("static export directory");
        for (entry, attrs) in strata_apfs::list_dir(root, Some(&dir)).flatten() {
            if !matches!(
                attrs.kind,
                strata_apfs::FileKind::Dir | strata_apfs::FileKind::Symlink { .. }
            ) {
                continue;
            }
            let Some(pid) = entry
                .as_str()
                .rsplit('/')
                .next()
                .and_then(|n| n.split('-').next())
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            // SAFETY: kill with signal 0 only checks that the process exists.
            let alive = pid == std::process::id() as i32
                || unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            if !alive {
                match strata_apfs::remove_tree_rooted(root, &entry) {
                    Ok(report) if report.is_complete() => {}
                    result => {
                        tracing::warn!(path = %entry, ?result, "a stale main-sync export could not be removed")
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod r2_warning_tests {
    use super::*;
    #[test]
    fn timestamp_siblings_preserve_warning_shape_and_first_sighting() {
        let w = Warnings::new(Arc::new(|_| {}));
        w.add(DaemonWarning::StorePoisoned);
        let first = w.times(&w.snapshot());
        w.add(DaemonWarning::StorePoisoned);
        assert_eq!(
            serde_json::to_value(w.snapshot()).unwrap(),
            json!(["StorePoisoned"])
        );
        assert_eq!(w.times(&w.snapshot()), first);
        let warning = DaemonWarning::VerifyFailed {
            message: "check".into(),
        };
        let first = w.times(std::slice::from_ref(&warning));
        assert_eq!(w.times(&[warning]), first);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let held = DaemonWarning::LongHold {
            repo: RepoName::try_from("app".to_string()).unwrap(),
            method: "sync".into(),
            since: at,
            origin: Some(crate::locks::Origin::Request),
            session: None,
        };
        assert_eq!(w.times(&[held])[0]["raised_at"], "1970-01-01T00:00:01Z");
    }

    /// Status-bar §3.1: a `LongHold` (and a held lock) an older daemon sent,
    /// without `origin` and `session`, still reads; a new one carries both.
    #[test]
    fn long_hold_and_lock_hold_read_old_payloads() {
        let old = json!({"LongHold":{"repo":"app","method":"catch_up","since":{"secs_since_epoch":1,"nanos_since_epoch":0}}});
        let w: DaemonWarning = serde_json::from_value(old).unwrap();
        assert!(
            matches!(&w, DaemonWarning::LongHold { origin: None, session: None, method, .. } if method == "catch_up")
        );
        let new = DaemonWarning::LongHold {
            repo: RepoName::try_from("app".to_string()).unwrap(),
            method: "catch_up".into(),
            since: SystemTime::UNIX_EPOCH,
            origin: Some(crate::locks::Origin::Background),
            session: Some(strata_types::SessionId::try_from("s-aaaaaaaa".to_string()).unwrap()),
        };
        let v = serde_json::to_value(&new).unwrap();
        assert_eq!(
            (&v["LongHold"]["origin"], &v["LongHold"]["session"]),
            (&json!("background"), &json!("s-aaaaaaaa"))
        );
        assert_eq!(serde_json::from_value::<DaemonWarning>(v).unwrap(), new);
        let hold: crate::LockHold = serde_json::from_value(
            json!({"repo":"app","method":"sync","session":null,"since":{"secs_since_epoch":1,"nanos_since_epoch":0}}),
        )
        .unwrap();
        assert_eq!((hold.origin, hold.elapsed_ms), (None, None));
    }

    #[test]
    fn long_wait_timestamps_use_the_attempt_start_for_live_and_recorded_waits() {
        let warnings = Warnings::new(Arc::new(|_| {}));
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(2);
        let wait = DaemonWarning::LongWait {
            repo: Some(RepoName::try_from("app".to_string()).unwrap()),
            session: None,
            method: "sync".into(),
            since: at,
            attempt_since: at,
        };
        let live = warnings.times(std::slice::from_ref(&wait));
        assert_eq!(live[0]["warning"], serde_json::to_value(&wait).unwrap());
        assert_eq!(live[0]["raised_at"], "1970-01-01T00:00:02Z");
        warnings.add(wait);
        assert_eq!(warnings.times(&warnings.snapshot()), live);
    }
}

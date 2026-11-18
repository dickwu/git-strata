//! Update installation and crash recovery (design §4.2, steps 4–12).

use std::cmp::Ordering;
use std::fs::{self, OpenOptions, TryLockError};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(test)]
use std::time::Instant;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_types::{Code, STORE_SCHEMA};

use super::bundle::{BundleInfo, BundleOps, ExchangeError, ExchangeEvidence};
use super::source::VerifiedUpdate;
use super::state::{RollbackPhase, TxnPhase as Phase, UpdateState};
use super::txn::{
    DaemonIdentity, InstallDetails, RollbackAction, RollbackAttempt, RollbackScope,
    RollbackSubject, TargetRecovery, Txn, TxnStore, not_cancellable, stale_rollback,
};
use super::{Clock, UpdateFuture};
use crate::error::GuiError;
use crate::service::{AGENT_LABEL, ServiceManager, scope};

const POLL: Duration = Duration::from_secs(1);
const NO_PID_GAP: Duration = Duration::from_secs(5);
const CRASH_LOOP_GAP: Duration = Duration::from_secs(10);
const STOP_BUDGET: Duration = Duration::from_secs(120);
const START_BUDGET: Duration = Duration::from_secs(150);
const LEASE_TTL_S: u32 = 120;
const RENEW_INTERVAL: Duration = Duration::from_secs(30);

pub type Progress<'a> = dyn FnMut(UpdateState) + Send + 'a;

pub enum DrainAction<'a> {
    Acquire {
        ttl_s: u32,
        expected: &'a DaemonIdentity,
    },
    Renew {
        lease_id: &'a str,
        ttl_s: u32,
    },
    Release {
        lease_id: &'a str,
    },
    SealStop {
        lease_id: &'a str,
    },
}

/// Fresh, bounded RPCs to the daemon at this login home's socket. The drain
/// adapter uses the same feature-gated client as the restart command.
pub trait InstallDaemon: Send + Sync {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>>;
    /// Acquire and renew return the daemon's lease ID.
    fn drain<'a>(
        &'a self,
        action: DrainAction<'a>,
    ) -> UpdateFuture<'a, Result<Option<String>, GuiError>>;
    /// True only for an absent/refusing socket and a takeable daemon.lock.
    /// A local adapter can use [`socket_refusing_and_lock_free`].
    fn quiescent(&self) -> Result<bool, GuiError>;
    /// The close_error of the stop just observed, from last_shutdown.json.
    fn close_error(&self, after: SystemTime) -> Option<Value>;
    /// The last verified schema for the current config's store root, if known.
    fn retained_store_schema(&self) -> Option<u32>;
}

/// Inspect the two files in the daemon's config directory. A stale Unix
/// socket is fine. Access errors and a pending connect never prove a stop.
pub fn socket_refusing_and_lock_free(socket: &Path, lock: &Path) -> Result<bool, GuiError> {
    let probe = || -> io::Result<bool> {
        if !socket_refusing(socket)? {
            return Ok(false);
        }
        let lock = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(lock)
        {
            Ok(lock) => lock,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(e),
        };
        if !lock.metadata()?.is_file() {
            return Ok(false);
        }
        match lock.try_lock() {
            Ok(()) => Ok(true),
            Err(TryLockError::WouldBlock) => Ok(false),
            Err(TryLockError::Error(e)) => Err(e),
        }
    };
    probe().map_err(|e| {
        GuiError::new(
            Code::Unknown,
            format!("Cannot verify the daemon is stopped: {e}"),
            json!({"reason": "update_daemon_probe", "errno": e.raw_os_error()}),
        )
    })
}

fn socket_refusing(path: &Path) -> io::Result<bool> {
    // Use a nonblocking connect so a full backlog cannot stall the stop poll.
    // This probe sends no request and accepts only definite refusal or absence.
    // SAFETY: All-zero is valid before the address fields are populated.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid daemon socket path",
        ));
    }
    addr.sun_family = libc::AF_UNIX as _;
    for (to, from) in addr.sun_path.iter_mut().zip(bytes) {
        *to = *from as _;
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        addr.sun_len = len as _;
    }

    // SAFETY: socket takes no pointers and returns an owned descriptor.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ownership transfers from the newly opened descriptor.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    // SAFETY: fcntl operates on the descriptor owned by stream.
    if unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    stream.set_nonblocking(true)?;
    // SAFETY: addr is initialized and len covers the populated address.
    if unsafe {
        libc::connect(
            stream.as_raw_fd(),
            (&addr as *const libc::sockaddr_un).cast(),
            len,
        )
    } == 0
    {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    match error.kind() {
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => {
            match fs::symlink_metadata(path) {
                Ok(meta) => Ok(meta.file_type().is_socket()
                    && error.kind() == io::ErrorKind::ConnectionRefused),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(e) => Err(e),
            }
        }
        io::ErrorKind::WouldBlock => Ok(false),
        _ if error.raw_os_error() == Some(libc::EINPROGRESS) => Ok(false),
        _ => Err(error),
    }
}

pub struct PreparedUpdate {
    pub update: VerifiedUpdate,
    pub info: BundleInfo,
}

/// Integration with the installed App and the steps before the transaction.
/// Kept separate so the state machine never downloads again during recovery.
pub trait InstallHost: Send + Sync {
    /// Validate the release bundle location, canonical login home and agent
    /// plist. The plist must target program with conditional KeepAlive.
    fn validate(&self, bundle: &Path, plist: &Path, program: &Path) -> Result<(), GuiError>;
    /// Steps 1–3: verified download, signed metadata/schema guard and backup.
    fn prepare<'a>(
        &'a self,
        target: &'a str,
        schema_acknowledgement: Option<u32>,
        live_schema: Option<u32>,
        rollback_zip: &'a Path,
        progress: &'a mut Progress<'_>,
    ) -> UpdateFuture<'a, Result<PreparedUpdate, GuiError>>;
    fn daemon_version(&self) -> Result<String, GuiError>;
    fn transaction_id(&self) -> Option<String> {
        None
    }
    fn rollback_scope(&self) -> Result<(PathBuf, PathBuf), GuiError> {
        Err(unexpected())
    }
    fn verify_loaded_policy(&self, _dump: &str) -> Result<(), GuiError> {
        Ok(())
    }
    fn check_preconditions(&self, _running: Option<&str>, _bundled: &str) -> Result<(), GuiError> {
        Ok(())
    }
    fn confirm_schema<'a>(
        &'a self,
        live_schema: Option<u32>,
        target: &'a BundleInfo,
    ) -> UpdateFuture<'a, Result<bool, GuiError>>;
    /// Restore the already codesign- and version-verified bundle returned
    /// by the plugin, without overwriting an unrelated installed bundle.
    fn restore_kept(&self, kept: &Path, bundle: &Path, txn: &Txn) -> Result<(), GuiError>;
}

pub struct InstallPaths {
    pub bundle: PathBuf,
    pub plist: PathBuf,
    pub app_support: PathBuf,
    pub app_version: String,
}

pub struct InstallAdapters {
    pub service: Arc<dyn ServiceManager>,
    pub daemon: Arc<dyn InstallDaemon>,
    pub bundles: Arc<dyn BundleOps>,
    pub host: Arc<dyn InstallHost>,
    pub clock: Arc<dyn Clock>,
}

/// Managed once in Tauri. The lifecycle lock can also be shared by the
/// Move, Restart, Set up and Roll back coordinators when wiring the App.
pub struct InstallCoordinator {
    paths: InstallPaths,
    program: PathBuf,
    adapters: InstallAdapters,
    store: TxnStore,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    cancel: Mutex<Cancel>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CancelMode {
    #[default]
    Closed,
    Drain,
    SealedStop,
    NotServing,
}

#[derive(Default)]
struct Cancel {
    mode: CancelMode,
    requested: bool,
}

struct CancelScope<'a>(&'a InstallCoordinator);

impl Drop for CancelScope<'_> {
    fn drop(&mut self) {
        *self.0.cancel_state() = Cancel::default();
    }
}

#[derive(Clone, Debug)]
struct Job {
    pid: Option<u32>,
    last_exit: Option<LastExit>,
    exit_timeout: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastExit {
    Clean,
    Failed,
}

enum Eligible {
    Serving(DaemonIdentity, Option<u32>),
    NotServing,
}

#[derive(PartialEq, Eq)]
enum Seal {
    Accepted,
    Uncertain,
    Cancelled,
}

enum Stopped {
    Steady {
        job: Job,
        cancelled: bool,
        sealed: bool,
    },
    Respawned(DaemonIdentity, bool),
    CrashLoop(bool),
}

struct LiveLease {
    id: String,
    peer: DaemonIdentity,
    renew_at: std::time::Instant,
}

#[derive(Default)]
struct RollbackDrain {
    lease: Option<LiveLease>,
    orphan_seen_at: Option<std::time::Instant>,
}

#[derive(PartialEq, Eq)]
enum DrainObservation<'a> {
    Absent,
    Unsealed(&'a str),
    Sealed,
}

#[derive(Clone, Copy)]
enum DrainStep {
    Acquire,
    Renew,
    Seal,
    Wait,
}

impl DrainStep {
    fn action<'a>(
        self,
        peer: &'a DaemonIdentity,
        acknowledged: Option<&'a str>,
    ) -> Result<Option<DrainAction<'a>>, GuiError> {
        let action = match self {
            Self::Acquire => DrainAction::Acquire {
                ttl_s: LEASE_TTL_S,
                expected: peer,
            },
            Self::Renew => DrainAction::Renew {
                lease_id: acknowledged.ok_or_else(unexpected)?,
                ttl_s: LEASE_TTL_S,
            },
            Self::Seal => DrainAction::SealStop {
                lease_id: acknowledged.ok_or_else(unexpected)?,
            },
            Self::Wait => return Ok(None),
        };
        Ok(Some(action))
    }
}

struct StopRequest<'a> {
    predecessor: Option<&'a DaemonIdentity>,
    requested_ms: Option<u64>,
    seal_accepted: bool,
    allow_original_absent: bool,
    allow_unsealed_original: bool,
    expected_version: Option<&'a str>,
}

enum StopObservation {
    Steady(Job, bool),
    Unsealed(DaemonIdentity),
    CrashLoop,
}

enum Fence {
    Unloaded,
    DrainAgain,
    Ended(InstallOutcome),
}

enum Unload {
    Done,
    Failed,
    Uncertain,
}

#[derive(PartialEq, Eq)]
enum Installed {
    Target,
    Previous,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Relaunch,
    Updated,
    Cancelled,
    Interrupted { from: String },
    NotStarting,
    Unchanged,
    Restored,
    RestoreFailed,
    CancelBlocked,
    BootoutFailed,
    FenceUncertain,
}

impl InstallCoordinator {
    pub fn new(
        paths: InstallPaths,
        adapters: InstallAdapters,
        lifecycle: Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        let store = TxnStore::new(paths.app_support.join("update.json"));
        let program = paths.bundle.join("Contents/MacOS/git-stratad");
        Self {
            paths,
            program,
            adapters,
            store,
            lifecycle,
            cancel: Mutex::new(Cancel::default()),
        }
    }

    pub(crate) fn pending(&self) -> Result<Option<Txn>, GuiError> {
        self.store.load()
    }

    pub(crate) fn require_idle(&self) -> Result<(), GuiError> {
        self.store.require_idle()
    }

    pub(crate) fn rollback_relaunch_pending(&self) -> Result<bool, GuiError> {
        let Some(txn) = self.store.load()? else {
            return Ok(false);
        };
        Ok(self.paths.app_version == txn.to
            && self
                .store
                .rollback(&txn)?
                .is_some_and(|attempt| attempt.phase == RollbackPhase::Relaunching))
    }

    pub(crate) async fn eligibility(&self) -> Result<(), GuiError> {
        self.store.require_idle()?;
        self.validate().await?;
        self.eligible().await.map(|_| ())
    }

    pub fn cancel(&self) -> Result<(), GuiError> {
        if self.store.withdraw_rollback()? {
            return Ok(());
        }
        let mut cancel = self.cancel_state();
        match cancel.mode {
            CancelMode::Closed => return Err(not_cancellable()),
            CancelMode::SealedStop => {
                // The RPC can complete while the App is dying. Persist the
                // cancellation as soon as it is requested, including then.
                let mut txn = self.store.load()?.ok_or_else(unexpected)?;
                self.phase(&mut txn, Phase::CancelAfterStop)?;
            }
            CancelMode::NotServing => {
                let txn = self.store.load()?.ok_or_else(unexpected)?;
                self.store.edit_install(&txn, None, |details| {
                    details.cancel_not_serving = true;
                })?;
            }
            CancelMode::Drain => {}
        }
        cancel.requested = true;
        Ok(())
    }

    pub async fn install(
        &self,
        target: &str,
        schema_acknowledgement: Option<u32>,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        let _guard = self.lifecycle.try_lock().map_err(|_| lifecycle_busy())?;
        let _cancel = CancelScope(self);
        self.store.require_idle()?;
        self.validate().await?;
        let from = &self.paths.app_version;
        if release_cmp(target, from) != Some(Ordering::Greater) {
            return Err(problem(
                Code::InvalidInput,
                "update_version",
                "The update must be a newer release",
            ));
        }
        if self.bundle_info_at(&self.paths.bundle).await?.version != *from {
            return Err(unexpected());
        }

        let mut schema = self.observed_schema(&self.eligible().await?);
        let rollback_zip = self.rollback_zip(from);
        let prepared = self
            .adapters
            .host
            .prepare(
                target,
                schema_acknowledgement,
                schema,
                &rollback_zip,
                progress,
            )
            .await?;
        if prepared.info.version != target {
            return Err(unexpected());
        }

        // Download and user prompts can outlast the original process or schema observation.
        self.validate().await?;
        let mut eligible = self
            .eligible_for_target(&prepared.info, &mut schema)
            .await?;
        let mut txn = Txn {
            phase: Phase::Draining.name().into(),
            from: from.clone(),
            to: target.into(),
            from_schema: STORE_SCHEMA,
            to_schema: prepared.info.store_schema,
            to_floor: prepared.info.downgrade_floor.clone(),
            from_daemon: None,
            program: self.program.clone(),
            rollback_zip,
            was_enabled: None,
        };
        if let Eligible::Serving(identity, _) = &eligible {
            txn.from_daemon = Some(identity.clone());
        }
        self.store
            .begin_with_id(&txn, self.adapters.host.transaction_id())?;

        loop {
            let (job, sealed) = match eligible {
                Eligible::Serving(identity, _) => {
                    txn.from_daemon = Some(identity);
                    let seal = self.drain_and_seal(&mut txn, progress).await?;
                    if seal == Seal::Cancelled {
                        return self.finish(&txn, InstallOutcome::Cancelled, progress);
                    }
                    match self
                        .wait_stopped(&txn, seal == Seal::Uncertain, false, progress)
                        .await?
                    {
                        Stopped::Steady {
                            cancelled: true, ..
                        } => {
                            return self.cancel_stopped(&txn, progress).await;
                        }
                        Stopped::Steady { job, sealed, .. } => (job, sealed),
                        Stopped::Respawned(daemon, true) => {
                            return self.finish_respawn(&txn, &daemon, true, progress).await;
                        }
                        Stopped::Respawned(_, false) => {
                            eligible = self
                                .eligible_for_target(&prepared.info, &mut schema)
                                .await?;
                            continue;
                        }
                        Stopped::CrashLoop(true) => return self.not_starting(&txn, progress).await,
                        Stopped::CrashLoop(false) => {
                            self.confirm_observed_schema(
                                &prepared.info,
                                &mut schema,
                                self.adapters.daemon.retained_store_schema(),
                            )
                            .await?;
                            txn.from_daemon = None;
                            (self.loaded().await?, false)
                        }
                    }
                }
                Eligible::NotServing => {
                    txn.from_daemon = None;
                    (self.loaded().await?, false)
                }
            };

            match self.fence(&mut txn, &job, sealed, progress).await? {
                Fence::Unloaded => break,
                Fence::DrainAgain => {
                    eligible = self
                        .eligible_for_target(&prepared.info, &mut schema)
                        .await?;
                }
                Fence::Ended(outcome) => return Ok(outcome),
            }
        }
        self.swap(&mut txn, prepared.update, progress).await
    }

    /// Recovery controls use fresh identity and schema evidence. A stale
    /// timeout hint alone does not permit a launch or bundle replacement.
    pub(crate) async fn recovery(
        &self,
        txn: &Txn,
        config: &Path,
        root: &Path,
    ) -> Result<Value, GuiError> {
        if let Some(attempt) = self.store.rollback(txn)? {
            self.validate().await?;
            if attempt.scope != self.rollback_scope(config, root)
                || txn.program != self.program
                || ![&txn.from, &txn.to].contains(&&self.paths.app_version)
            {
                return Err(unexpected());
            }
            // The same recovery command resumes the saved attempt, including
            // a pending stop or withdrawn lease cleanup.
            return Ok(json!({"start":false, "rollback_version": &txn.from}));
        }
        if !matches!(
            Phase::parse(&txn.phase),
            Some(
                Phase::StartingDaemon
                    | Phase::Relaunching
                    | Phase::WaitingDaemon
                    | Phase::WaitingPermission
            )
        ) || self.paths.app_version != txn.to
        {
            return Err(unexpected());
        }
        self.validate().await?;
        if self.installed(txn).await? != Installed::Target {
            return Err(unexpected());
        }
        let eligible = self.eligible().await?;
        let (start, evidence) = match eligible {
            Eligible::Serving(_, schema) => (false, super::rollback::StoreEvidence::Live(schema)),
            Eligible::NotServing => (
                true,
                super::rollback::StoreEvidence::Down {
                    config_dir: config,
                    data_root: root,
                },
            ),
        };
        let rollback = super::rollback::check(txn, evidence).is_ok()
            && fs::symlink_metadata(&txn.rollback_zip).is_ok_and(|meta| meta.is_file());
        Ok(json!({"start":start, "rollback_version":rollback.then_some(&txn.from)}))
    }

    pub(crate) async fn start_recovery(
        &self,
        config: &Path,
        root: &Path,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let _guard = self.lifecycle.try_lock().map_err(|_| lifecycle_busy())?;
        let mut txn = self.store.load()?.ok_or_else(unexpected)?;
        if self.recovery(&txn, config, root).await?["start"] != true
            || !self.can_offer_not_serving().await
        {
            return Err(not_quiescent());
        }
        self.require_enabled().await?;
        self.install_phase(&mut txn, Phase::WaitingDaemon, |details| {
            details.start_requested_ms = Some(unix_millis(self.adapters.clock.now()));
            details.permission_wait_elapsed_ms = None;
        })?;
        self.kickstart().await?;
        self.start_new(&mut txn, false, progress).await?;
        Ok(())
    }

    pub(crate) async fn rollback_to_previous(
        &self,
        config: &Path,
        root: &Path,
        progress: &mut Progress<'_>,
    ) -> Result<bool, GuiError> {
        let _guard = self.lifecycle.try_lock().map_err(|_| lifecycle_busy())?;
        let txn = self.store.load()?.ok_or_else(unexpected)?;
        let scope = self.rollback_scope(config, root);
        if self.store.rollback(&txn)?.is_none() {
            let parent_id = self.store.id(&txn)?;
            self.verify_rollback_scope(&scope).await?;
            if !self.recovery(&txn, config, root).await?["rollback_version"].is_string() {
                return Err(problem(
                    Code::Unsupported,
                    "rollback_unavailable",
                    "Cannot verify a safe rollback. See the release notes and store backups.",
                ));
            }
            let bundles = self.adapters.bundles.clone();
            let bundle = self.paths.bundle.clone();
            let saved = txn.clone();
            blocking(move || bundles.verify_previous_archive(&bundle, &saved)).await?;
            let subject = match self.eligible().await? {
                Eligible::Serving(daemon, schema) => {
                    super::rollback::check(&txn, super::rollback::StoreEvidence::Live(schema))?;
                    RollbackSubject::Serving(daemon)
                }
                Eligible::NotServing => {
                    self.check_rollback_snapshot(&txn, &scope)?;
                    RollbackSubject::NoPid
                }
            };
            self.require_rollback_target(&txn).await?;
            self.store
                .begin_rollback(&txn, &parent_id, subject, scope.clone())?;
        }
        let outcome = self.resume_rollback(&txn, &scope, progress).await?;
        Ok(outcome == InstallOutcome::Restored && self.paths.app_version != txn.from)
    }

    fn rollback_scope(&self, config: &Path, root: &Path) -> RollbackScope {
        RollbackScope {
            plist: self.paths.plist.clone(),
            config_dir: config.to_owned(),
            data_root: root.to_owned(),
        }
    }

    async fn verify_rollback_scope(&self, scope: &RollbackScope) -> Result<(), GuiError> {
        let host = self.adapters.host.clone();
        let (config, root) = blocking(move || host.rollback_scope()).await?;
        if *scope != self.rollback_scope(&config, &root) {
            return Err(unexpected());
        }
        Ok(())
    }

    fn check_rollback_snapshot(&self, txn: &Txn, scope: &RollbackScope) -> Result<(), GuiError> {
        super::rollback::check(
            txn,
            super::rollback::StoreEvidence::Down {
                config_dir: &scope.config_dir,
                data_root: &scope.data_root,
            },
        )
    }

    async fn resume_rollback(
        &self,
        txn: &Txn,
        scope: &RollbackScope,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.validate().await?;
        if txn.program != self.program
            || txn.rollback_zip != self.rollback_zip(&txn.from)
            || release_cmp(&txn.to, &txn.from) != Some(Ordering::Greater)
            || ![&txn.from, &txn.to].contains(&&self.paths.app_version)
        {
            return Err(unexpected());
        }
        self.verify_rollback_scope(scope).await?;
        let id = self.store.rollback(txn)?.ok_or_else(unexpected)?.id;
        let mut drain = RollbackDrain::default();
        loop {
            let attempt = self.rollback_attempt(txn, &id)?;
            if attempt.scope != *scope {
                return Err(unexpected());
            }
            match attempt.phase {
                RollbackPhase::Draining => {
                    self.rollback_drain(txn, &attempt, &mut drain, progress)
                        .await?;
                }
                RollbackPhase::Stopping | RollbackPhase::CancelAfterStop => {
                    self.rollback_stop(txn, &attempt, progress)
                        .await
                        .inspect_err(|error| {
                            progress(UpdateState::DaemonFailed {
                                reason: error.reason().unwrap_or("daemon_identity").into(),
                            });
                        })?;
                }
                RollbackPhase::Fencing => {
                    self.rollback_disable(txn, &attempt, progress).await?;
                }
                RollbackPhase::Disabling => {
                    self.rollback_observe_disable(txn, &attempt).await?;
                }
                RollbackPhase::Fenced => {
                    self.rollback_unload(txn, &attempt, progress).await?;
                }
                RollbackPhase::RollbackEnabling => {
                    self.require_rollback_target(txn).await?;
                    if self.rollback_enable(txn, &attempt).await? {
                        self.store.advance_rollback(txn, &attempt, |next| {
                            next.owns_fence = false;
                            if next.withdrawn() {
                                retain_target(next, TargetRecovery::Cancelled);
                            } else {
                                next.phase = RollbackPhase::Draining;
                            }
                        })?;
                    }
                }
                RollbackPhase::Bootout => {
                    // A relaunched coordinator can observe explicit unload. It
                    // has no launchctl exit result and cannot replay bootout.
                    let timeout = self
                        .job()
                        .await
                        .ok()
                        .flatten()
                        .and_then(|job| job.exit_timeout);
                    if matches!(self.wait_unloaded(timeout, false).await, Unload::Done) {
                        self.rollback_unloaded(txn, &attempt).await?;
                    } else {
                        return Ok(fence_uncertain(progress));
                    }
                }
                RollbackPhase::Restoring => {
                    self.rollback_exchange(txn, &attempt, progress).await?;
                }
                RollbackPhase::StartingTarget => {
                    if let Some(outcome) =
                        self.rollback_start_target(txn, &attempt, progress).await?
                    {
                        return Ok(outcome);
                    }
                }
                RollbackPhase::StartingPrevious | RollbackPhase::Relaunching => {
                    if let Some(outcome) = self
                        .rollback_start_previous(txn, &attempt, progress)
                        .await?
                    {
                        return Ok(outcome);
                    }
                }
            }
        }
    }

    fn rollback_attempt(&self, txn: &Txn, id: &str) -> Result<RollbackAttempt, GuiError> {
        self.store
            .rollback(txn)?
            .filter(|attempt| attempt.id == id)
            .ok_or_else(stale_rollback)
    }

    /// A reply can settle its own admission after withdrawal. It cannot
    /// replace the withdrawn continuation, or complete a later admission.
    fn settle_rollback(
        &self,
        txn: &Txn,
        admitted: &RollbackAttempt,
        settle: impl Fn(&mut RollbackAttempt),
    ) -> Result<RollbackAttempt, GuiError> {
        loop {
            let current = self.rollback_attempt(txn, &admitted.id)?;
            let same_phase = current.phase == admitted.phase
                || (admitted.phase == RollbackPhase::Stopping
                    && current.phase == RollbackPhase::CancelAfterStop
                    && current.withdrawn());
            if !same_phase
                || current.pending != admitted.pending
                || (admitted.withdrawn() && !current.withdrawn())
                || admitted.pending.is_none()
            {
                return Err(stale_rollback());
            }
            if let Some(next) = self.store.advance_rollback(txn, &current, |next| {
                next.pending = None;
                settle(next);
            })? {
                return Ok(next);
            }
        }
    }

    async fn rollback_drain(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        drain: &mut RollbackDrain,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        self.require_rollback_target(txn).await?;
        self.require_enabled().await?;
        let job = self.loaded().await?;
        if job.pid.is_none() {
            if !self.can_offer_not_serving().await {
                return Err(not_quiescent());
            }
            if !attempt.withdrawn() {
                self.check_rollback_snapshot(txn, &attempt.scope)?;
            }
            *drain = RollbackDrain::default();
            self.store.advance_rollback(txn, attempt, |next| {
                next.subject = RollbackSubject::NoPid;
                next.pending = None;
                next.seal_accepted = false;
                next.stopped = true;
                if next.withdrawn() {
                    retain_target(next, TargetRecovery::Cancelled);
                } else {
                    next.phase = RollbackPhase::Fencing;
                }
            })?;
            return Ok(());
        }
        let (peer, status) = self.verified_drain_status().await?;
        if job.pid != Some(peer.pid) || peer.version != txn.to {
            return Err(daemon_identity());
        }
        let observation = drain_observation(&status).ok_or_else(invalid_drain_status)?;
        if !attempt.withdrawn() {
            super::rollback::check(
                txn,
                super::rollback::StoreEvidence::Live(store_schema(&status)),
            )?;
        }

        if attempt.subject.daemon() != Some(&peer) {
            if attempt
                .subject
                .daemon()
                .is_some_and(|old| !replacement(old, &peer))
            {
                return Err(daemon_identity());
            }
            // A different verified process cannot execute the saved RPC.
            *drain = RollbackDrain::default();
            self.store.advance_rollback(txn, attempt, |next| {
                next.subject = RollbackSubject::Serving(peer.clone());
                next.pending = None;
                next.seal_accepted = false;
                next.stopped = false;
            })?;
            return Ok(());
        }

        let idle = report_draining(&status, progress);
        if let Some(action) = attempt.action() {
            match action {
                // A status on a second connection can overtake a fully sent
                // acquire. Null and a local deadline cannot settle it.
                RollbackAction::Acquire => return Err(cleanup_uncertain()),
                RollbackAction::Renew | RollbackAction::Release
                    if observation == DrainObservation::Absent =>
                {
                    *drain = RollbackDrain::default();
                    self.store
                        .advance_rollback(txn, attempt, |next| next.pending = None)?;
                    return Ok(());
                }
                RollbackAction::Renew | RollbackAction::Release if attempt.withdrawn() => {
                    return self.observe_orphan_lease(drain, observation).await;
                }
                RollbackAction::Renew | RollbackAction::Release => return Err(cleanup_uncertain()),
                _ => return Err(unexpected()),
            }
        }

        if observation == DrainObservation::Absent {
            *drain = RollbackDrain::default();
        }
        let acknowledged = drain.lease.as_ref().filter(|live| live.peer == peer);
        let id = acknowledged.map(|live| live.id.clone());
        let action = if attempt.withdrawn() {
            if observation == DrainObservation::Absent {
                self.store.advance_rollback(txn, attempt, |next| {
                    retain_target(next, TargetRecovery::Cancelled)
                })?;
                return Ok(());
            }
            let Some(id) = id
                .as_deref()
                .filter(|id| observation == DrainObservation::Unsealed(id))
            else {
                return self.observe_orphan_lease(drain, observation).await;
            };
            DrainAction::Release { lease_id: id }
        } else {
            let step = next_drain_step(
                observation,
                acknowledged,
                idle,
                self.adapters.clock.monotonic(),
            )?;
            let Some(action) = step.action(&peer, id.as_deref())? else {
                self.adapters.clock.sleep(POLL).await;
                return Ok(());
            };
            action
        };
        self.verify_rollback_scope(&attempt.scope).await?;
        self.rollback_drain_rpc(txn, attempt, action, &mut drain.lease)
            .await
    }

    async fn observe_orphan_lease(
        &self,
        drain: &mut RollbackDrain,
        observation: DrainObservation<'_>,
    ) -> Result<(), GuiError> {
        if matches!(observation, DrainObservation::Unsealed(_)) {
            let clock = &self.adapters.clock;
            let now = clock.monotonic();
            let seen_at = *drain.orphan_seen_at.get_or_insert(now);
            if now.duration_since(seen_at) < Duration::from_secs(LEASE_TTL_S.into()) {
                clock.sleep(POLL).await;
                return Ok(());
            }
        }
        Err(cleanup_uncertain())
    }

    async fn verified_drain_status(&self) -> Result<(DaemonIdentity, Value), GuiError> {
        let status = self.adapters.daemon.status().await?;
        let peer = identity(&status)?;
        if self.loaded().await?.pid != Some(peer.pid) {
            return Err(daemon_identity());
        }
        require_drain(&status)?;
        Ok((peer, status))
    }

    async fn rollback_drain_rpc(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        action: DrainAction<'_>,
        lease: &mut Option<LiveLease>,
    ) -> Result<(), GuiError> {
        let (kind, renew_id) = match action {
            DrainAction::Acquire { .. } => (RollbackAction::Acquire, None),
            DrainAction::Renew { lease_id, .. } => (RollbackAction::Renew, Some(lease_id)),
            DrainAction::Release { .. } => (RollbackAction::Release, None),
            DrainAction::SealStop { .. } => (RollbackAction::Seal, None),
        };
        let Some(admitted) = self.store.advance_rollback(txn, attempt, |next| {
            if kind == RollbackAction::Seal {
                next.phase = RollbackPhase::Stopping;
                next.stop_requested_ms = Some(unix_millis(self.adapters.clock.now()));
                next.seal_accepted = false;
            }
            next.admit(kind);
        })?
        else {
            return Ok(());
        };
        let result = self.adapters.daemon.drain(action).await;
        let uncertain = matches!(&result, Err(error) if error.code == Code::Unknown.as_str())
            || (matches!(kind, RollbackAction::Acquire | RollbackAction::Renew)
                && matches!(&result, Ok(id) if !valid_lease_grant(id.as_deref(), renew_id)));
        if uncertain {
            return if kind == RollbackAction::Seal {
                Ok(())
            } else {
                Err(cleanup_uncertain())
            };
        }

        let current = self.settle_rollback(txn, &admitted, |next| {
            if kind == RollbackAction::Seal {
                next.seal_accepted = result.is_ok();
                if result.is_err() && !next.withdrawn() {
                    next.phase = RollbackPhase::Draining;
                }
            }
        })?;
        match result {
            Ok(Some(id)) if matches!(kind, RollbackAction::Acquire | RollbackAction::Renew) => {
                *lease = Some(LiveLease {
                    id,
                    peer: attempt.subject.daemon().ok_or_else(unexpected)?.clone(),
                    renew_at: self.adapters.clock.monotonic() + RENEW_INTERVAL,
                });
            }
            Ok(_) if kind == RollbackAction::Release => *lease = None,
            Err(error) if error.reason() == Some("lease_lost") => *lease = None,
            Err(error) if !current.withdrawn() && error.reason() != Some("not_idle") => {
                return Err(error);
            }
            _ => {}
        }
        if kind == RollbackAction::Seal && current.phase == RollbackPhase::Draining {
            self.adapters.clock.sleep(POLL).await;
        }
        Ok(())
    }

    async fn rollback_stop(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        self.require_rollback_target(txn).await?;
        let expected = attempt.subject.daemon().ok_or_else(unexpected)?;
        let uncertain = attempt.action() == Some(RollbackAction::Seal);
        let sample = self
            .observe_stop(
                StopRequest {
                    predecessor: Some(expected),
                    requested_ms: attempt.stop_requested_ms,
                    seal_accepted: attempt.seal_accepted,
                    allow_original_absent: uncertain,
                    allow_unsealed_original: !uncertain && !attempt.seal_accepted,
                    expected_version: Some(&txn.to),
                },
                || Ok(self.rollback_attempt(txn, &attempt.id)?.withdrawn()),
                progress,
            )
            .await?;
        let current = self.rollback_attempt(txn, &attempt.id)?;
        if current.pending != attempt.pending
            || !matches!(
                current.phase,
                RollbackPhase::Stopping | RollbackPhase::CancelAfterStop
            )
        {
            return Err(stale_rollback());
        }
        self.store.advance_rollback(txn, &current, |next| {
            next.pending = None;
            match &sample {
                StopObservation::Unsealed(peer) => {
                    next.phase = RollbackPhase::Draining;
                    next.subject = RollbackSubject::Serving(peer.clone());
                    next.seal_accepted = false;
                    next.stopped = false;
                }
                StopObservation::Steady(_, sealed) => {
                    next.stopped = true;
                    if next.withdrawn() {
                        retain_target(next, TargetRecovery::Cancelled);
                    } else {
                        next.phase = RollbackPhase::Fencing;
                        next.seal_accepted = *sealed;
                    }
                }
                StopObservation::CrashLoop => {
                    if next.withdrawn() {
                        retain_target(next, TargetRecovery::NotStarting);
                    } else {
                        next.phase = RollbackPhase::Fencing;
                        next.seal_accepted = false;
                    }
                }
            }
        })?;
        Ok(())
    }

    async fn rollback_disable(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        self.require_rollback_target(txn).await?;
        self.require_enabled().await?;
        let job = self.loaded().await?;
        if attempt.withdrawn() || job.pid.is_some() {
            self.store.advance_rollback(txn, attempt, |next| {
                if next.withdrawn() {
                    retain_target(next, TargetRecovery::Cancelled);
                } else {
                    next.phase = RollbackPhase::Draining;
                }
            })?;
            return Ok(());
        }
        if !self.adapters.daemon.quiescent()?
            || attempt.seal_accepted && job.last_exit != Some(LastExit::Clean)
        {
            return Err(not_quiescent());
        }
        self.check_rollback_snapshot(txn, &attempt.scope)?;
        self.verify_rollback_scope(&attempt.scope).await?;
        self.require_enabled().await?;
        progress(UpdateState::Stopping);
        let Some(admitted) = self.store.advance_rollback(txn, attempt, |next| {
            next.was_enabled = true;
            next.phase = RollbackPhase::Disabling;
            next.admit(RollbackAction::Disable);
        })?
        else {
            return Ok(());
        };
        match self.service(|service| service.disable(AGENT_LABEL)).await {
            Ok(()) => self.rollback_observe_disable(txn, &admitted).await,
            Err(error) if disable_settled(&error) => {
                self.settle_rollback(txn, &admitted, |next| {
                    // A pre-dispatch refusal cannot own a later disabled label.
                    next.owns_fence = !crate::service::pre_dispatch_error(&error);
                    retain_target(
                        next,
                        if next.withdrawn() {
                            TargetRecovery::Cancelled
                        } else {
                            TargetRecovery::DisableRefused
                        },
                    );
                })?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    async fn rollback_observe_disable(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
    ) -> Result<(), GuiError> {
        if !attempt.was_enabled || attempt.action() != Some(RollbackAction::Disable) {
            return Err(unexpected());
        }
        // Disable cannot unload a job. An unloaded or still-enabled sample
        // cannot settle this admission or grant authority to restore.
        self.loaded().await?;
        if !self.read_fence_state().await? {
            return Err(not_quiescent());
        }
        self.settle_rollback(txn, attempt, |next| {
            next.owns_fence = true;
            next.phase = RollbackPhase::Fenced;
        })?;
        Ok(())
    }

    async fn rollback_unload(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        if !attempt.owns_fence || !attempt.was_enabled || attempt.pending.is_some() {
            return Err(unexpected());
        }
        if attempt.withdrawn() {
            self.store.advance_rollback(txn, attempt, |next| {
                retain_target(next, TargetRecovery::Cancelled)
            })?;
            return Ok(());
        }
        self.require_rollback_target(txn).await?;
        self.verify_rollback_scope(&attempt.scope).await?;
        let checked = self.recheck_fence(attempt.seal_accepted).await;
        let stopped = match checked {
            Ok(Some(stopped)) => stopped,
            Ok(None) => {
                self.store.advance_rollback(txn, attempt, |next| {
                    next.phase = RollbackPhase::RollbackEnabling
                })?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let Some(admitted) = self.store.advance_rollback(txn, attempt, |next| {
            next.phase = RollbackPhase::Bootout;
            next.bootout_admitted = true;
            next.admit(RollbackAction::Bootout);
        })?
        else {
            return Ok(());
        };
        match self.unload(&stopped).await {
            Unload::Done => self.rollback_unloaded(txn, &admitted).await,
            Unload::Failed => {
                self.settle_rollback(txn, &admitted, |next| {
                    retain_target(next, TargetRecovery::BootoutFailed)
                })?;
                Ok(())
            }
            Unload::Uncertain => {
                fence_uncertain(progress);
                Err(not_quiescent())
            }
        }
    }

    async fn rollback_unloaded(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
    ) -> Result<(), GuiError> {
        if attempt.action() != Some(RollbackAction::Bootout)
            || !attempt.was_enabled
            || !attempt.owns_fence
            || !self.fenced_and_unloaded().await?
        {
            return Err(not_quiescent());
        }
        self.settle_rollback(txn, attempt, |next| {
            next.unloaded = true;
            next.phase = RollbackPhase::Restoring;
        })?;
        Ok(())
    }

    async fn rollback_exchange(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        if !attempt.unloaded
            || !attempt.owns_fence
            || !attempt.bootout_admitted
            || attempt.withdrawn()
            || !self.fenced_and_unloaded().await?
        {
            return Err(not_quiescent());
        }
        let bundle = self.paths.bundle.clone();
        let adapters = &self.adapters;
        let ops = adapters.bundles.clone();
        let service = adapters.service.clone();
        let daemon = adapters.daemon.clone();
        let host = adapters.host.clone();
        let saved = txn.clone();
        let attempt = attempt.clone();
        let store = TxnStore::new(self.paths.app_support.join("update.json"));
        progress(UpdateState::Installing);
        let result = blocking(move || {
            let check_fence = || {
                let (config, root) = host.rollback_scope()?;
                if config != attempt.scope.config_dir
                    || root != attempt.scope.data_root
                    || !service.print_disabled(AGENT_LABEL)?
                    || service.print(AGENT_LABEL)?.is_some()
                    || !daemon.quiescent()?
                {
                    return Err(not_quiescent());
                }
                Ok(())
            };
            check_fence()?;
            if let Some(evidence) = &attempt.exchange {
                if attempt.action() != Some(RollbackAction::Exchange) {
                    return Err(unexpected());
                }
                super::rollback::check(
                    &saved,
                    super::rollback::StoreEvidence::Down {
                        config_dir: &attempt.scope.config_dir,
                        data_root: &attempt.scope.data_root,
                    },
                )?;
                return super::rollback::verify_exchange(ops.as_ref(), &bundle, &saved, evidence)
                    .map(|()| None);
            }
            if attempt.pending.is_some() {
                return Err(unexpected());
            }
            let admitted = |evidence: &ExchangeEvidence| {
                store
                    .advance_rollback(&saved, &attempt, |next| {
                        next.exchange = Some(evidence.clone());
                        next.admit(RollbackAction::Exchange);
                    })?
                    .ok_or_else(stale_rollback)?;
                Ok(())
            };
            match super::rollback::exchange_fenced(
                ops.as_ref(),
                &bundle,
                &saved,
                &attempt.scope.config_dir,
                &attempt.scope.data_root,
                &check_fence,
                &admitted,
            ) {
                Ok(()) => {
                    let current = store.rollback(&saved)?.ok_or_else(stale_rollback)?;
                    let evidence = current.exchange.as_ref().ok_or_else(unexpected)?;
                    super::rollback::verify_exchange(ops.as_ref(), &bundle, &saved, evidence)?;
                    Ok(None)
                }
                Err(ExchangeError::NotActivated(error)) => {
                    // Compensate only when the retained signed target still
                    // matches the one selected for exchange.
                    let target = super::rollback::verified_target(ops.as_ref(), &bundle, &saved)?;
                    let current = store.rollback(&saved)?.ok_or_else(stale_rollback)?;
                    if current.id != attempt.id
                        || current
                            .exchange
                            .as_ref()
                            .is_some_and(|evidence| evidence.target != target)
                    {
                        return Err(error);
                    }
                    Ok(Some(error))
                }
                Err(ExchangeError::Uncertain(error)) => Err(error),
            }
        })
        .await;
        let refusal = result.inspect_err(|_| progress(UpdateState::RestoreFailed))?;
        let current = self.store.rollback(txn)?.ok_or_else(stale_rollback)?;
        if current.phase != RollbackPhase::Restoring || current.withdrawn() {
            return Err(stale_rollback());
        }
        if refusal.is_none()
            && (current.action() != Some(RollbackAction::Exchange) || current.exchange.is_none())
        {
            return Err(unexpected());
        }
        let changed = self
            .store
            .advance_rollback(txn, &current, |next| {
                next.pending = None;
                if refusal.is_some() {
                    retain_target(next, TargetRecovery::ExchangeRefused);
                } else {
                    next.phase = RollbackPhase::StartingPrevious;
                }
            })?
            .ok_or_else(stale_rollback)?;
        if let Some(evidence) = changed.exchange {
            let ops = self.adapters.bundles.clone();
            let bundle = self.paths.bundle.clone();
            blocking(move || {
                ops.discard_exchange(&bundle, &evidence, refusal.is_none());
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    /// Enable only a fence established by this attempt. An unloaded job is
    /// eligible only after this attempt verified its bootout.
    async fn rollback_enable(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
    ) -> Result<bool, GuiError> {
        self.verify_rollback_scope(&attempt.scope).await?;
        self.verify_start_job(attempt).await?;
        if !self.disabled().await? {
            if attempt.action() == Some(RollbackAction::Enable) {
                self.settle_rollback(txn, attempt, |_| {})?;
                return Ok(false);
            }
            return Ok(true);
        }
        if !attempt.was_enabled
            || !attempt.owns_fence
            || !matches!(attempt.action(), None | Some(RollbackAction::Enable))
        {
            return Err(agent_disabled());
        }
        let Some(admitted) = self
            .store
            .advance_rollback(txn, attempt, |next| next.admit(RollbackAction::Enable))?
        else {
            return Ok(false);
        };
        self.verify_start_job(&admitted).await?;
        self.service(|service| service.enable(AGENT_LABEL)).await?;
        self.require_enabled().await?;
        self.settle_rollback(txn, &admitted, |_| {})?;
        Ok(false)
    }

    async fn verify_start_job(&self, attempt: &RollbackAttempt) -> Result<Option<Job>, GuiError> {
        let job = self.job().await?;
        if job.is_none() && (!attempt.unloaded || !self.adapters.daemon.quiescent()?) {
            return Err(not_quiescent());
        }
        Ok(job)
    }

    async fn verify_exchanged_bundle(
        &self,
        expected: &super::bundle::VerifiedBundle,
    ) -> Result<(), GuiError> {
        let ops = self.adapters.bundles.clone();
        let bundle = self.paths.bundle.clone();
        let expected = expected.clone();
        blocking(move || {
            let installed = super::bundle::probe_bundle(ops.as_ref(), &bundle)
                .map_err(super::bundle::ProbeError::into_error)?;
            if installed == expected {
                Ok(())
            } else {
                Err(unexpected())
            }
        })
        .await
    }

    async fn rollback_start_target(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        progress: &mut Progress<'_>,
    ) -> Result<Option<InstallOutcome>, GuiError> {
        self.require_rollback_target(txn).await?;
        if self.daemon_version().await? != txn.to {
            return Err(unexpected());
        }
        if let Some(evidence) = &attempt.exchange {
            self.verify_exchanged_bundle(&evidence.target).await?;
        }
        let reason = attempt.target_recovery.ok_or_else(unexpected)?;
        let (outcome, state) = match reason {
            TargetRecovery::Cancelled => (InstallOutcome::Cancelled, UpdateState::Cancelled),
            TargetRecovery::NotStarting => (InstallOutcome::NotStarting, UpdateState::NotStarting),
            TargetRecovery::DisableRefused | TargetRecovery::ExchangeRefused => (
                InstallOutcome::Unchanged,
                UpdateState::InstallFailedUnchanged,
            ),
            TargetRecovery::BootoutFailed => {
                (InstallOutcome::BootoutFailed, UpdateState::BootoutFailed)
            }
        };
        if reason == TargetRecovery::NotStarting {
            self.require_enabled().await?;
        } else {
            if !self
                .rollback_start_job(txn, attempt, &txn.to, progress)
                .await?
            {
                return Ok(None);
            }
            self.require_rollback_target(txn).await?;
            if let Some(evidence) = &attempt.exchange {
                self.verify_exchanged_bundle(&evidence.target).await?;
            }
        }
        if !self.store.finish_rollback(txn, attempt, false)? {
            return Ok(None);
        }
        progress(state);
        Ok(Some(outcome))
    }

    async fn rollback_start_previous(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        progress: &mut Progress<'_>,
    ) -> Result<Option<InstallOutcome>, GuiError> {
        let evidence = attempt.exchange.as_ref().ok_or_else(unexpected)?;
        self.require_previous(txn).await?;
        if self.daemon_version().await? != txn.from {
            return Err(unexpected());
        }
        self.verify_exchanged_bundle(&evidence.previous).await?;
        if attempt.phase == RollbackPhase::StartingPrevious {
            if self.job().await?.is_none() {
                self.check_rollback_snapshot(txn, &attempt.scope)?;
            }
            if !self
                .rollback_start_job(txn, attempt, &txn.from, progress)
                .await?
            {
                return Ok(None);
            }
            self.verify_exchanged_bundle(&evidence.previous).await?;
            self.store
                .advance_rollback(txn, attempt, |next| next.phase = RollbackPhase::Relaunching)?;
            return Ok(None);
        }

        // The target App leaves the verified handoff in place even if the
        // relaunch dispatch fails or the process exits before dispatch.
        if self.paths.app_version != txn.from {
            self.require_app_version(&txn.to)?;
            progress(UpdateState::Relaunching);
        } else {
            self.require_enabled().await?;
            let predecessor = attempt.subject.daemon().or(txn.from_daemon.as_ref());
            self.wait_ready(&txn.from, predecessor, None, true, progress)
                .await?;
            self.verify_exchanged_bundle(&evidence.previous).await?;
            if !self.store.finish_rollback(txn, attempt, true)? {
                return Ok(None);
            }
            progress(UpdateState::Restored {
                from: txn.from.clone(),
            });
        }
        Ok(Some(InstallOutcome::Restored))
    }

    /// Select the same plain kickstart or bootstrap used by forward recovery.
    /// Keep the admission until there is verified readiness on the saved bundle.
    async fn rollback_start_job(
        &self,
        txn: &Txn,
        attempt: &RollbackAttempt,
        version: &str,
        progress: &mut Progress<'_>,
    ) -> Result<bool, GuiError> {
        if !self.rollback_enable(txn, attempt).await? {
            return Ok(false);
        }
        self.require_enabled().await?;
        let job = self.verify_start_job(attempt).await?;
        let prior_launch = attempt.action();
        let action = match &job {
            Some(job) if job.pid.is_some() => None,
            // The prior bootstrap registered the job. Wait for the daemon
            // without submitting a second start against that admission.
            Some(_) if prior_launch == Some(RollbackAction::Bootstrap) => None,
            Some(_) => Some(RollbackAction::Kickstart),
            None => Some(RollbackAction::Bootstrap),
        };
        if prior_launch.is_some()
            && !matches!(
                prior_launch,
                Some(RollbackAction::Bootstrap | RollbackAction::Kickstart)
            )
        {
            return Err(unexpected());
        }
        let admitted = if let Some(action) = action {
            if prior_launch.is_some_and(|pending| pending != action)
                || !self.adapters.daemon.quiescent()?
            {
                return Err(not_quiescent());
            }
            progress(UpdateState::StartingDaemon);
            let Some(admitted) = self.store.advance_rollback(txn, attempt, |next| {
                next.start_requested_ms = Some(unix_millis(self.adapters.clock.now()));
                next.admit(action);
            })?
            else {
                return Ok(false);
            };
            self.require_enabled().await?;
            let job = self.verify_start_job(&admitted).await?;
            match (action, job) {
                (_, Some(job)) if job.pid.is_some() => {}
                (RollbackAction::Kickstart, Some(_)) if self.adapters.daemon.quiescent()? => {
                    self.kickstart().await?
                }
                (RollbackAction::Bootstrap, None) if self.adapters.daemon.quiescent()? => {
                    self.bootstrap().await?
                }
                _ => return Err(not_quiescent()),
            }
            Some(admitted)
        } else {
            prior_launch.map(|_| attempt.clone())
        };

        let previous_bundle = version == txn.from;
        let predecessor = if previous_bundle {
            attempt.subject.daemon().or(txn.from_daemon.as_ref())
        } else if attempt.stopped || admitted.is_some() {
            attempt.subject.daemon()
        } else {
            None
        };
        self.wait_ready(version, predecessor, None, true, progress)
            .await?;
        if let Some(admitted) = admitted {
            self.settle_rollback(txn, &admitted, |_| {})?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Run at startup before checking for update eligibility.
    pub async fn reconcile(
        &self,
        progress: &mut Progress<'_>,
    ) -> Result<Option<InstallOutcome>, GuiError> {
        let _guard = self.lifecycle.try_lock().map_err(|_| lifecycle_busy())?;
        let _cancel = CancelScope(self);
        let Some(mut txn) = self.store.load()? else {
            return Ok(None);
        };
        if self.store.rollback(&txn)?.is_some() {
            let host = self.adapters.host.clone();
            let (config, root) = blocking(move || host.rollback_scope()).await?;
            return self
                .resume_rollback(&txn, &self.rollback_scope(&config, &root), progress)
                .await
                .map(Some);
        }
        let phase = Phase::parse(&txn.phase).ok_or_else(|| {
            problem(
                Code::InvalidInput,
                "update_phase_unknown",
                format!("Unrecognized update transaction phase: {}", txn.phase),
            )
        })?;
        if txn.program != self.program
            || release_cmp(&txn.to, &txn.from) != Some(Ordering::Greater)
            || txn.rollback_zip != self.rollback_zip(&txn.from)
        {
            return Err(unexpected());
        }
        self.validate().await?;
        progress(UpdateState::Reconciling);

        let outcome = match phase {
            Phase::Draining => {
                self.resume_old(
                    &txn,
                    InstallOutcome::Interrupted {
                        from: txn.from.clone(),
                    },
                    progress,
                )
                .await?
            }
            Phase::Stopping | Phase::CancelAfterStop => {
                let cancelled = phase == Phase::CancelAfterStop;
                self.set_cancel(CancelMode::SealedStop, cancelled);
                match self.wait_stopped(&txn, true, cancelled, progress).await? {
                    Stopped::Steady {
                        cancelled: true, ..
                    } => self.cancel_stopped(&txn, progress).await?,
                    Stopped::Steady { .. } | Stopped::CrashLoop(false) => {
                        self.resume_old(
                            &txn,
                            InstallOutcome::Interrupted {
                                from: txn.from.clone(),
                            },
                            progress,
                        )
                        .await?
                    }
                    Stopped::Respawned(daemon, cancelled) => {
                        self.finish_respawn(&txn, &daemon, cancelled, progress)
                            .await?
                    }
                    Stopped::CrashLoop(true) => self.not_starting(&txn, progress).await?,
                }
            }
            Phase::Disabling | Phase::Fencing => {
                self.resume_fencing(&mut txn, phase, progress).await?
            }
            Phase::Bootout => {
                // The old process cannot attest to the launchctl exit status.
                // Resume by waiting for explicit unload, never replay bootout.
                let timeout = self
                    .job()
                    .await
                    .ok()
                    .flatten()
                    .and_then(|job| job.exit_timeout);
                match self.wait_unloaded(timeout, false).await {
                    Unload::Done => self.unwind(&mut txn, progress).await?,
                    Unload::Failed | Unload::Uncertain => fence_uncertain(progress),
                }
            }
            Phase::Installing => match self.inspect_for_recovery(&txn, progress).await? {
                Installed::Target => self.start_new(&mut txn, false, progress).await?,
                Installed::Previous => self.unwind(&mut txn, progress).await?,
                Installed::Invalid => self.restore(&mut txn, progress).await?,
            },
            Phase::Restoring => self.restore(&mut txn, progress).await?,
            Phase::StartingDaemon
            | Phase::Relaunching
            | Phase::WaitingDaemon
            | Phase::WaitingPermission => self.start_new(&mut txn, false, progress).await?,
            Phase::Interrupted => {
                self.finish_old(
                    &txn,
                    InstallOutcome::Interrupted {
                        from: txn.from.clone(),
                    },
                    progress,
                )
                .await?
            }
            Phase::Unchanged => {
                self.finish_old(&txn, InstallOutcome::Unchanged, progress)
                    .await?
            }
            Phase::Restored => {
                self.finish_old(&txn, InstallOutcome::Restored, progress)
                    .await?
            }
            Phase::BootoutFailed => self.finish_bootout_failure(&txn, progress).await?,
        };
        Ok(Some(outcome))
    }

    fn cancel_state(&self) -> MutexGuard<'_, Cancel> {
        self.cancel.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_cancel(&self, mode: CancelMode, requested: bool) {
        *self.cancel_state() = Cancel { mode, requested };
    }

    fn close_cancel(&self) -> bool {
        let mut cancel = self.cancel_state();
        let requested = cancel.requested;
        *cancel = Cancel::default();
        requested
    }

    fn phase(&self, txn: &mut Txn, phase: Phase) -> Result<(), GuiError> {
        txn.phase = phase.name().into();
        self.store.save(txn)
    }

    fn install_phase(
        &self,
        txn: &mut Txn,
        phase: Phase,
        edit: impl FnOnce(&mut InstallDetails),
    ) -> Result<(), GuiError> {
        self.store.edit_install(txn, Some(phase.name()), edit)?;
        txn.phase = phase.name().into();
        Ok(())
    }

    fn rollback_zip(&self, version: &str) -> PathBuf {
        self.paths
            .app_support
            .join("previous")
            .join(format!("GIT-STRATA-{version}.zip"))
    }

    async fn validate(&self) -> Result<(), GuiError> {
        let host = self.adapters.host.clone();
        let bundle = self.paths.bundle.clone();
        let plist = self.paths.plist.clone();
        let program = self.program.clone();
        blocking(move || host.validate(&bundle, &plist, &program)).await
    }

    async fn service<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&dyn ServiceManager) -> Result<T, GuiError> + Send + 'static,
    ) -> Result<T, GuiError> {
        let service = self.adapters.service.clone();
        blocking(move || operation(service.as_ref())).await
    }

    async fn job(&self) -> Result<Option<Job>, GuiError> {
        self.service(|service| service.print(AGENT_LABEL))
            .await?
            .map(|dump| {
                let job = Job::parse(&dump, &self.paths.plist, &self.program)?;
                self.adapters.host.verify_loaded_policy(&dump)?;
                Ok(job)
            })
            .transpose()
    }

    async fn loaded(&self) -> Result<Job, GuiError> {
        self.job().await?.ok_or_else(|| {
            problem(
                Code::InvalidInput,
                "update_agent_unloaded",
                "The bundled daemon is no longer loaded",
            )
        })
    }

    async fn disabled(&self) -> Result<bool, GuiError> {
        self.service(|service| service.print_disabled(AGENT_LABEL))
            .await
    }

    async fn daemon_version(&self) -> Result<String, GuiError> {
        let host = self.adapters.host.clone();
        blocking(move || host.daemon_version()).await
    }

    async fn eligible(&self) -> Result<Eligible, GuiError> {
        if self.disabled().await? {
            return Err(agent_disabled());
        }
        let mut job = self.loaded().await?;
        if job.pid.is_none() {
            if !self.adapters.daemon.quiescent()? {
                return Err(not_quiescent());
            }
            self.adapters.clock.sleep(NO_PID_GAP).await;
            job = self.loaded().await?;
            if job.pid.is_none() {
                if !self.adapters.daemon.quiescent()? {
                    return Err(not_quiescent());
                }
                // A stopped bundle must still be safe to bootstrap on failure.
                let bundled = self.daemon_version().await?;
                self.check_preconditions(None, &bundled).await?;
                if bundled != self.paths.app_version {
                    return Err(unexpected());
                }
                return Ok(Eligible::NotServing);
            }
        }
        let status = self.adapters.daemon.status().await?;
        let daemon = identity(&status)?;
        if job.pid != Some(daemon.pid) {
            return Err(daemon_identity());
        }
        require_drain(&status)?;
        let bundled = self.daemon_version().await?;
        self.check_preconditions(Some(&daemon.version), &bundled)
            .await?;
        match release_cmp(&bundled, &daemon.version) {
            Some(Ordering::Equal) if daemon.version == self.paths.app_version => {}
            Some(Ordering::Greater) => {
                return Err(problem(
                    Code::InvalidInput,
                    "daemon_restart_required",
                    "Restart the daemon first",
                ));
            }
            Some(Ordering::Equal | Ordering::Less) | None => {
                return Err(problem(
                    Code::InvalidInput,
                    "daemon_bundle_version",
                    "Install a matching or newer GIT STRATA first",
                ));
            }
        }
        if self.disabled().await? {
            return Err(agent_disabled());
        }
        let schema = store_schema(&status);
        Ok(Eligible::Serving(daemon, schema))
    }

    async fn check_preconditions(
        &self,
        running: Option<&str>,
        bundled: &str,
    ) -> Result<(), GuiError> {
        let host = self.adapters.host.clone();
        let running = running.map(str::to_owned);
        let bundled = bundled.to_owned();
        blocking(move || host.check_preconditions(running.as_deref(), &bundled)).await
    }

    fn observed_schema(&self, eligible: &Eligible) -> Option<u32> {
        match eligible {
            Eligible::Serving(_, schema) => *schema,
            Eligible::NotServing => self.adapters.daemon.retained_store_schema(),
        }
    }

    async fn eligible_for_target(
        &self,
        target: &BundleInfo,
        acknowledged: &mut Option<u32>,
    ) -> Result<Eligible, GuiError> {
        loop {
            let eligible = self.eligible().await?;
            let schema = self.observed_schema(&eligible);
            if !self
                .confirm_observed_schema(target, acknowledged, schema)
                .await?
            {
                return Ok(eligible);
            }
            // Confirming can take long enough for the observed daemon to change again.
        }
    }

    /// True when we had to wait for a fresh acknowledgement.
    async fn confirm_observed_schema(
        &self,
        target: &BundleInfo,
        acknowledged: &mut Option<u32>,
        schema: Option<u32>,
    ) -> Result<bool, GuiError> {
        if schema.is_some_and(|live| live > target.store_schema) {
            return Err(schema_older());
        }
        if schema == *acknowledged || schema.is_some_and(|current| current >= target.store_schema) {
            return Ok(false);
        }
        if !self.adapters.host.confirm_schema(schema, target).await? {
            return Err(problem(
                Code::NotConfirmed,
                "update_schema_not_confirmed",
                "The current store schema was not confirmed for this update",
            ));
        }
        *acknowledged = schema;
        Ok(true)
    }

    /// A lost seal reply is handled by inspecting the job and lease before
    /// draining again, since a second seal could otherwise target a respawn.
    async fn drain_and_seal(
        &self,
        txn: &mut Txn,
        progress: &mut Progress<'_>,
    ) -> Result<Seal, GuiError> {
        let daemon = &self.adapters.daemon;
        let clock = &self.adapters.clock;
        let predecessor = txn.from_daemon.as_ref().ok_or_else(unexpected)?.clone();
        self.phase(txn, Phase::Draining)?;
        self.set_cancel(CancelMode::Drain, false);
        let mut lease: Option<LiveLease> = None;
        loop {
            if self.cancel_state().requested {
                self.close_cancel();
                if let Some(live) = lease.as_ref()
                    && let Err(error) = daemon
                        .drain(DrainAction::Release { lease_id: &live.id })
                        .await
                    && error.reason() != Some("lease_lost")
                {
                    return Err(error);
                }
                self.require_previous(txn).await?;
                return Ok(Seal::Cancelled);
            }

            let (peer, status) = self.verified_drain_status().await?;
            if peer != predecessor {
                return Err(daemon_identity());
            }
            let observation = drain_observation(&status).ok_or_else(invalid_drain_status)?;
            if observation == DrainObservation::Absent {
                lease = None;
            }
            let idle = lease.is_some() && report_draining(&status, progress);
            let step = next_drain_step(observation, lease.as_ref(), idle, clock.monotonic())?;
            let id = lease.as_ref().map(|live| live.id.as_str());
            let Some(action) = step.action(&predecessor, id)? else {
                clock.sleep(POLL).await;
                continue;
            };

            if matches!(step, DrainStep::Seal) {
                {
                    let mut cancel = self.cancel_state();
                    if cancel.requested {
                        continue;
                    }
                    self.install_phase(txn, Phase::Stopping, |details| {
                        details.stop_requested_ms = Some(unix_millis(clock.now()));
                    })?;
                    cancel.mode = CancelMode::SealedStop;
                }
                match daemon.drain(action).await {
                    Ok(_) => return Ok(Seal::Accepted),
                    Err(error) if matches!(error.reason(), Some("not_idle" | "lease_lost")) => {
                        let mut cancel = self.cancel_state();
                        if error.reason() == Some("lease_lost") {
                            lease = None;
                        } else if let Some(live) = &mut lease {
                            live.renew_at = clock.monotonic();
                        }
                        if !cancel.requested {
                            self.phase(txn, Phase::Draining)?;
                            cancel.mode = CancelMode::Drain;
                        }
                    }
                    Err(error) if error.code == Code::Unknown.as_str() => {
                        return Ok(Seal::Uncertain);
                    }
                    Err(error) => return Err(error),
                }
                clock.sleep(POLL).await;
                continue;
            }

            if self.cancel_state().requested {
                continue;
            }
            let renew = if matches!(step, DrainStep::Renew) {
                id
            } else {
                None
            };
            match daemon.drain(action).await {
                Ok(Some(id)) if valid_lease_grant(Some(&id), renew) => {
                    lease = Some(LiveLease {
                        id,
                        peer: peer.clone(),
                        renew_at: clock.monotonic() + RENEW_INTERVAL,
                    });
                }
                Err(error) if error.reason() == Some("lease_lost") => {
                    lease = None;
                    clock.sleep(POLL).await;
                }
                Err(error) => return Err(error),
                _ => {
                    return Err(problem(
                        Code::InvalidInput,
                        "update_drain_reply",
                        "The drain reply has no matching lease ID",
                    ));
                }
            }
        }
    }

    async fn wait_stopped(
        &self,
        txn: &Txn,
        seal_uncertain: bool,
        already_cancelled: bool,
        progress: &mut Progress<'_>,
    ) -> Result<Stopped, GuiError> {
        let observed = self
            .observe_stop(
                StopRequest {
                    predecessor: txn.from_daemon.as_ref(),
                    requested_ms: self.store.install_details(txn)?.stop_requested_ms,
                    seal_accepted: !seal_uncertain,
                    allow_original_absent: seal_uncertain,
                    allow_unsealed_original: false,
                    expected_version: None,
                },
                || Ok(already_cancelled || self.cancel_state().requested),
                progress,
            )
            .await?;
        let cancelled = self.close_cancel() || already_cancelled;
        Ok(match observed {
            StopObservation::Steady(job, sealed) => Stopped::Steady {
                job,
                cancelled,
                sealed,
            },
            StopObservation::Unsealed(peer) => Stopped::Respawned(peer, cancelled),
            StopObservation::CrashLoop => Stopped::CrashLoop(cancelled),
        })
    }

    /// Observes step 7 for both bundle directions. A daemon answering on the
    /// owned job must report a valid drain state before it can be redrained.
    async fn observe_stop(
        &self,
        request: StopRequest<'_>,
        is_cancelled: impl Fn() -> Result<bool, GuiError>,
        progress: &mut Progress<'_>,
    ) -> Result<StopObservation, GuiError> {
        let clock = &self.adapters.clock;
        let began = clock.monotonic();
        let stop_after = request
            .requested_ms
            .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms))
            .or_else(|| request.predecessor.map(|daemon| daemon.started))
            .unwrap_or_else(|| clock.now());
        let mut warned = false;
        let mut last_failure = None;
        let mut sealed_exit = request.seal_accepted;
        let predecessor_pid = request.predecessor.map(|daemon| daemon.pid);
        progress(UpdateState::Stopping);
        loop {
            match self.stop_sample().await {
                Ok(Some((job, _))) if job.pid.is_some() => {
                    if job.pid != predecessor_pid {
                        sealed_exit = false;
                    }
                    if let Ok(status) = self.adapters.daemon.status().await
                        && let Ok(peer) = identity(&status)
                        && job.pid == Some(peer.pid)
                        && request
                            .expected_version
                            .is_none_or(|version| peer.version == version)
                        && let Some(
                            observation
                            @ (DrainObservation::Absent | DrainObservation::Unsealed(_)),
                        ) = drain_observation(&status)
                    {
                        let respawned = request
                            .predecessor
                            .is_some_and(|old| replacement(old, &peer));
                        let original_settled = request.predecessor == Some(&peer)
                            && (request.allow_unsealed_original
                                || (request.allow_original_absent
                                    && observation == DrainObservation::Absent));
                        if respawned || original_settled {
                            return Ok(StopObservation::Unsealed(peer));
                        }
                    }
                }
                Ok(Some((job, true))) if job.last_exit == Some(LastExit::Clean) => {
                    if let Some(close_error) = self.adapters.daemon.close_error(stop_after) {
                        progress(UpdateState::StopDetail {
                            slow: warned,
                            close_error: Some(close_error),
                        });
                    }
                    return Ok(StopObservation::Steady(job, sealed_exit));
                }
                Ok(Some((job, true))) if job.last_exit == Some(LastExit::Failed) => {
                    sealed_exit = false;
                    let now = clock.monotonic();
                    // A process seen between these samples can itself be a failed respawn.
                    let first = *last_failure.get_or_insert(now);
                    if now.duration_since(first) >= CRASH_LOOP_GAP
                        && (now.duration_since(began) >= STOP_BUDGET || is_cancelled()?)
                    {
                        return Ok(StopObservation::CrashLoop);
                    }
                }
                Ok(_) => {}
                Err(e) => return Err(e),
            }

            if !warned && clock.monotonic().duration_since(began) >= STOP_BUDGET {
                warned = true;
                progress(UpdateState::StopDetail {
                    slow: true,
                    close_error: None,
                });
            }
            clock.sleep(POLL).await;
        }
    }

    /// A transient probe error supplies no evidence for either stop path.
    async fn stop_sample(&self) -> Result<Option<(Job, bool)>, GuiError> {
        let sample = async {
            let job = self.loaded().await?;
            let quiet = job.pid.is_none() && self.adapters.daemon.quiescent()?;
            Ok((job, quiet))
        }
        .await;
        match sample {
            Ok(sample) => Ok(Some(sample)),
            Err(e) if transient_stop_probe(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn fence(
        &self,
        txn: &mut Txn,
        job: &Job,
        sealed_stop: bool,
        progress: &mut Progress<'_>,
    ) -> Result<Fence, GuiError> {
        if job.pid.is_some() {
            return Ok(Fence::DrainAgain);
        }
        if self.read_fence_state().await? {
            return Err(agent_disabled());
        }

        txn.was_enabled = Some(true);
        self.phase(txn, Phase::Fencing)?;
        if !sealed_stop {
            self.set_cancel(CancelMode::NotServing, false);
            progress(UpdateState::Stopping);
        }
        let already_cancelled = {
            let mut cancel = self.cancel_state();
            if cancel.requested {
                *cancel = Cancel::default();
                true
            } else {
                // No acknowledgement on this intent means a launchctl child may
                // still disable the label, including after the App has exited.
                self.phase(txn, Phase::Disabling)?;
                false
            }
        };
        if already_cancelled {
            return self
                .finish_not_serving_cancel(txn, progress)
                .await
                .map(Fence::Ended);
        }
        if let Err(e) = self.service(|service| service.disable(AGENT_LABEL)).await {
            let phase = if disable_settled(&e) {
                self.record_disable_settled(txn).await?;
                Phase::Fencing
            } else {
                Phase::Disabling
            };
            return self
                .resume_fencing(txn, phase, progress)
                .await
                .map(Fence::Ended);
        }
        self.record_disable_settled(txn).await?;

        let checked = self.recheck_fence(sealed_stop).await;
        let cancelled = self.close_cancel();
        if cancelled {
            return self
                .finish_not_serving_cancel(txn, progress)
                .await
                .map(Fence::Ended);
        }
        match checked {
            Ok(Some(stopped)) => {
                self.save_or_restore_enabled(txn, Phase::Bootout).await?;
                match self.unload(&stopped).await {
                    Unload::Done => Ok(Fence::Unloaded),
                    Unload::Failed => self.bootout_failed(txn, progress).await.map(Fence::Ended),
                    Unload::Uncertain => Ok(Fence::Ended(fence_uncertain(progress))),
                }
            }
            Ok(None) => {
                self.restore_enabled(txn).await?;
                Ok(Fence::DrainAgain)
            }
            Err(e) => {
                self.restore_enabled(txn).await?;
                Err(e)
            }
        }
    }

    async fn recheck_fence(&self, sealed_stop: bool) -> Result<Option<Job>, GuiError> {
        let Some(first) = self.fence_sample().await? else {
            return Ok(None);
        };
        if sealed_stop {
            return if first.last_exit == Some(LastExit::Clean) {
                Ok(Some(first))
            } else {
                Err(not_quiescent())
            };
        }

        let began = self.adapters.clock.monotonic();
        while self.adapters.clock.monotonic().duration_since(began) < NO_PID_GAP
            && !self.cancel_state().requested
        {
            self.adapters.clock.sleep(POLL).await;
        }
        self.fence_sample().await
    }

    async fn read_fence_state(&self) -> Result<bool, GuiError> {
        match self.disabled().await {
            Err(e) if transient_stop_probe(&e) => {
                self.adapters.clock.sleep(POLL).await;
                self.disabled().await
            }
            result => result,
        }
    }

    async fn fence_sample(&self) -> Result<Option<Job>, GuiError> {
        let clock = &self.adapters.clock;
        let began = clock.monotonic();
        while !self.cancel_state().requested {
            match self.disabled().await {
                Ok(true) => match self.stop_sample().await? {
                    Some((job, _)) if job.pid.is_some() => return Ok(None),
                    Some((job, true)) => return Ok(Some(job)),
                    Some((_, false)) => return Err(not_quiescent()),
                    None => {}
                },
                Ok(false) => {
                    return Err(problem(
                        Code::Unknown,
                        "update_fence_missing",
                        "The launch agent is still enabled",
                    ));
                }
                Err(e) if transient_stop_probe(&e) => {}
                Err(e) => return Err(e),
            }
            if clock.monotonic().duration_since(began) >= NO_PID_GAP {
                return Err(not_quiescent());
            }
            clock.sleep(POLL).await;
        }
        Ok(None)
    }

    async fn unload(&self, job: &Job) -> Unload {
        let error = self
            .service(|service| {
                service.bootout_targeted(
                    AGENT_LABEL,
                    crate::service::launchctl::BootoutTarget::Stopped,
                )
            })
            .await
            .err();
        if error.as_ref().is_some_and(bootout_preflight_failed) {
            return Unload::Failed;
        }
        let nonzero = error.is_some_and(|e| launchctl_exited_nonzero(&e, "bootout"));
        self.wait_unloaded(job.exit_timeout, nonzero).await
    }

    async fn wait_unloaded(&self, exit_timeout: Option<Duration>, nonzero: bool) -> Unload {
        let clock = &self.adapters.clock;
        let began = clock.monotonic();
        // Without a documented exit timeout, a loaded job is never enough
        // evidence to classify the operation as a definite failure.
        let budget = exit_timeout.unwrap_or(STOP_BUDGET) + NO_PID_GAP + POLL;
        let mut still_loaded = None;
        loop {
            let observed = self.job().await;
            let now = clock.monotonic();
            match observed {
                Ok(None) => return Unload::Done,
                Ok(Some(job))
                    if nonzero && exit_timeout.is_some_and(|d| now.duration_since(began) >= d) =>
                {
                    match still_loaded {
                        Some((pid, at))
                            if pid == job.pid && now.duration_since(at) >= NO_PID_GAP =>
                        {
                            return Unload::Failed;
                        }
                        Some((pid, _)) if pid == job.pid => {}
                        _ => still_loaded = Some((job.pid, now)),
                    }
                }
                _ => still_loaded = None,
            }
            if now.duration_since(began) >= budget {
                return Unload::Uncertain;
            }
            clock.sleep(POLL).await;
        }
    }

    async fn restore_enabled(&self, txn: &Txn) -> Result<(), GuiError> {
        if txn.was_enabled == Some(true) && !matches!(self.disabled().await, Ok(false)) {
            // When the read itself fails, still try to put back the state
            // that was saved before disable. Keep the txn if enable fails.
            self.service(|service| service.enable(AGENT_LABEL)).await?;
            self.require_enabled().await?;
        }
        Ok(())
    }

    async fn record_disable_settled(&self, txn: &mut Txn) -> Result<(), GuiError> {
        if let Err(error) = self.phase(txn, Phase::Fencing) {
            // Bootout has not begun, so restore the label even if the journal
            // is unavailable. Save the settled phase once more so startup can
            // unwind an enabled job without waiting for another disable.
            let restored = self.restore_enabled(txn).await;
            let recorded = self.phase(txn, Phase::Fencing);
            restored?;
            recorded?;
            return Err(error);
        }
        Ok(())
    }

    /// For a verified bundle and no bootout in flight, an I/O failure must
    /// still put back the enabled state that was recorded before disable.
    async fn save_or_restore_enabled(&self, txn: &mut Txn, phase: Phase) -> Result<(), GuiError> {
        if let Err(e) = self.phase(txn, phase) {
            self.restore_enabled(txn).await?;
            return Err(e);
        }
        Ok(())
    }

    async fn bundle_info_at(&self, path: &Path) -> Result<BundleInfo, GuiError> {
        let path = path.to_owned();
        let bundles = self.adapters.bundles.clone();
        blocking(move || {
            bundles.codesign_verify(&path)?;
            bundles.read_info_plist(&path)
        })
        .await
    }

    async fn installed(&self, txn: &Txn) -> Result<Installed, GuiError> {
        let path = self.paths.bundle.clone();
        let bundles = self.adapters.bundles.clone();
        let Some(info) = blocking(move || inspect_bundle(bundles.as_ref(), &path)).await? else {
            return Ok(Installed::Invalid);
        };
        if info.version == txn.from {
            super::bundle::verify_predecessor_schema(&info, txn)?;
            return Ok(Installed::Previous);
        }
        if info.version == txn.to
            && info.store_schema == txn.to_schema
            && info.downgrade_floor == txn.to_floor
        {
            return Ok(Installed::Target);
        }
        Err(unexpected())
    }

    async fn require_previous(&self, txn: &Txn) -> Result<(), GuiError> {
        match self.installed(txn).await? {
            Installed::Previous => Ok(()),
            _ => Err(unexpected()),
        }
    }

    async fn require_rollback_target(&self, txn: &Txn) -> Result<(), GuiError> {
        if self.paths.app_version != txn.to || self.installed(txn).await? != Installed::Target {
            return Err(unexpected());
        }
        Ok(())
    }

    fn require_app_version(&self, expected: &str) -> Result<(), GuiError> {
        if self.paths.app_version == expected {
            return Ok(());
        }
        Err(GuiError::new(
            Code::InvalidInput,
            format!("Open the installed GIT STRATA {expected} to finish the update"),
            json!({"reason": "update_app_version", "expected": expected, "running": self.paths.app_version}),
        ))
    }

    async fn require_previous_app(&self, txn: &Txn) -> Result<(), GuiError> {
        self.require_app_version(&txn.from)?;
        self.require_previous(txn).await
    }

    async fn inspect_for_recovery(
        &self,
        txn: &Txn,
        progress: &mut Progress<'_>,
    ) -> Result<Installed, GuiError> {
        self.installed(txn)
            .await
            .inspect_err(|_| progress(UpdateState::RestoreFailed))
    }

    async fn swap(
        &self,
        txn: &mut Txn,
        update: VerifiedUpdate,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        if self.inspect_for_recovery(txn, progress).await? != Installed::Previous {
            progress(UpdateState::RestoreFailed);
            return Err(unexpected());
        }
        if !self.fenced_and_unloaded().await? {
            return Ok(fence_uncertain(progress));
        }
        let vacant = self
            .named_kept_paths()
            .into_iter()
            .filter(|path| {
                fs::symlink_metadata(path).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
            })
            .collect();
        if let Err(e) = self.install_phase(txn, Phase::Installing, |details| {
            details.kept_candidates = vacant;
        }) {
            self.restore_enabled(txn).await?;
            return Err(e);
        }

        progress(UpdateState::Installing);
        let bundles = self.adapters.bundles.clone();
        let bundle = self.paths.bundle.clone();
        let saved = txn.clone();
        let kept = blocking(move || {
            let result =
                super::bundle::install_verified(bundles.as_ref(), update, &bundle, &saved)?;
            Ok(match result {
                Err(tauri_plugin_updater::Error::PreviousAppNotRestored(path)) => Some(path),
                _ => None,
            })
        })
        .await?;
        if let Some(path) = kept {
            self.store
                .edit_install(txn, None, |details| details.kept_bundle = Some(path))?;
        }
        match self.inspect_for_recovery(txn, progress).await? {
            Installed::Target => self.start_new(txn, true, progress).await,
            Installed::Previous => {
                self.save_or_restore_enabled(txn, Phase::Unchanged).await?;
                self.finish_old(txn, InstallOutcome::Unchanged, progress)
                    .await
            }
            Installed::Invalid => self.restore(txn, progress).await,
        }
    }

    async fn fenced_and_unloaded(&self) -> Result<bool, GuiError> {
        Ok(self.disabled().await?
            && self.job().await?.is_none()
            && self.adapters.daemon.quiescent()?)
    }

    fn named_kept_paths(&self) -> Vec<PathBuf> {
        let Some(stem) = self.paths.bundle.file_stem() else {
            return Vec::new();
        };
        let mut name = stem.to_os_string();
        name.push(" (previous version).app");
        let mut paths = vec![self.paths.bundle.with_file_name(&name)];
        // validate() binds this plist to the login home. Match the updater's
        // fallback directory without looking at a different process's HOME.
        if let Some(agents) = self
            .paths
            .plist
            .parent()
            .filter(|p| p.ends_with("Library/LaunchAgents"))
            && let Some(home) = agents.parent().and_then(Path::parent)
        {
            paths.push(home.join(name));
        }
        paths
    }

    fn is_kept_bundle(&self, path: &Path) -> bool {
        if !fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
            return false;
        }
        if self.named_kept_paths().iter().any(|named| named == path) {
            return true;
        }
        // The plugin can retain the exact random backup directory when neither
        // named move worked. Accept that shape from its saved reply only.
        let Some(parent) = path.parent() else {
            return false;
        };
        path.ends_with("current_app")
            && parent
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"tauri_current_app"))
            && fs::symlink_metadata(parent).is_ok_and(|meta| meta.is_dir())
            && parent.parent().is_some_and(|root| {
                Some(root) == self.paths.bundle.parent() || root == std::env::temp_dir()
            })
    }

    async fn restore(
        &self,
        txn: &mut Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        let mut installed = self.inspect_for_recovery(txn, progress).await?;
        match installed {
            Installed::Invalid => {
                if !self.fenced_and_unloaded().await? {
                    return Ok(fence_uncertain(progress));
                }
                let details = self.store.install_details(txn)?;
                self.phase(txn, Phase::Restoring)?;
                let named = self.named_kept_paths();
                let candidates = details.kept_candidates.iter().filter(|path| {
                    named.contains(path) && Some(*path) != details.kept_bundle.as_ref()
                });
                for path in details.kept_bundle.iter().chain(candidates) {
                    if !self.is_kept_bundle(path)
                        || !self.bundle_info_at(path).await.is_ok_and(|info| {
                            info.version == txn.from && info.store_schema == txn.from_schema
                        })
                    {
                        continue;
                    }
                    installed = self.inspect_for_recovery(txn, progress).await?;
                    if installed != Installed::Invalid {
                        break;
                    }
                    if !self.fenced_and_unloaded().await? {
                        return Ok(fence_uncertain(progress));
                    }
                    let host = self.adapters.host.clone();
                    let bundle = self.paths.bundle.clone();
                    let path = path.clone();
                    let saved = txn.clone();
                    if let Err(error) =
                        blocking(move || host.restore_kept(&path, &bundle, &saved)).await
                        && matches!(
                            error.reason(),
                            Some(
                                "bundle_verification_uncertain"
                                    | "unexpected_bundle_version"
                                    | "unexpected_bundle_metadata"
                                    | "restore_verification_failed"
                            )
                        )
                    {
                        progress(UpdateState::RestoreFailed);
                        return Err(error);
                    }
                    installed = self.inspect_for_recovery(txn, progress).await?;
                    if installed != Installed::Invalid {
                        break;
                    }
                }

                if installed == Installed::Invalid
                    && self.inspect_for_recovery(txn, progress).await? == Installed::Invalid
                {
                    if !self.fenced_and_unloaded().await? {
                        return Ok(fence_uncertain(progress));
                    }
                    let bundle = self.paths.bundle.clone();
                    let saved = txn.clone();
                    let bundles = self.adapters.bundles.clone();
                    if let Err(error) = blocking(move || {
                        super::bundle::restore_interrupted_zip(bundles.as_ref(), &bundle, &saved)
                    })
                    .await
                    {
                        progress(UpdateState::RestoreFailed);
                        return Err(error);
                    }
                }
            }
            Installed::Previous => {}
            Installed::Target => {
                progress(UpdateState::RestoreFailed);
                return Err(unexpected());
            }
        }
        if self.inspect_for_recovery(txn, progress).await? != Installed::Previous {
            progress(UpdateState::RestoreFailed);
            return Ok(InstallOutcome::RestoreFailed);
        }
        self.save_or_restore_enabled(txn, Phase::Restored).await?;
        self.finish_old(txn, InstallOutcome::Restored, progress)
            .await
    }

    async fn start_new(
        &self,
        txn: &mut Txn,
        relaunch: bool,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        // The signed bundle's nested executable is verified with the app.
        // Leave the first launch of that executable to launchd's respawn policy.
        self.require_app_version(if relaunch { &txn.from } else { &txn.to })?;
        if self.inspect_for_recovery(txn, progress).await? != Installed::Target {
            return Err(unexpected());
        }
        let details = self.store.install_details(txn)?;
        progress(UpdateState::StartingDaemon);
        let bootstrap = self.needs_bootstrap(txn).await?;
        if details.start_requested_ms.is_none()
            || (bootstrap && txn.phase == Phase::StartingDaemon.name())
        {
            // Place the anchor at the bootstrap attempt. If the App died
            // before dispatch, an unloaded job in StartingDaemon needs a fresh anchor.
            let now = unix_millis(self.adapters.clock.now());
            self.install_phase(txn, Phase::StartingDaemon, |details| {
                details.start_requested_ms = Some(now);
            })?;
        }
        if bootstrap {
            self.bootstrap().await?;
        }
        if relaunch {
            self.phase(txn, Phase::Relaunching)?;
            progress(UpdateState::Relaunching);
            return Ok(InstallOutcome::Relaunch);
        }

        if details.permission_wait_elapsed_ms.is_none() {
            self.phase(txn, Phase::WaitingDaemon)?;
        }
        self.wait_ready(
            &self.paths.app_version,
            txn.from_daemon.as_ref(),
            Some(txn.clone()),
            false,
            progress,
        )
        .await?;
        if self.inspect_for_recovery(txn, progress).await? != Installed::Target {
            return Err(unexpected());
        }
        self.finish(txn, InstallOutcome::Updated, progress)
    }

    async fn needs_bootstrap(&self, txn: &Txn) -> Result<bool, GuiError> {
        if txn.was_enabled == Some(false) {
            return Err(agent_disabled());
        }
        let interrupted = txn.phase == Phase::Interrupted.name();
        // An earlier App may have crashed after recording Interrupted. Its
        // earlier lock and socket probes cannot authorize this enable.
        if self.job().await?.is_none()
            && interrupted
            && !matches!(self.adapters.daemon.quiescent(), Ok(true))
        {
            return Err(not_quiescent());
        }
        self.restore_enabled(txn).await?;
        self.require_enabled().await?;
        let unloaded = self.job().await?.is_none();
        if unloaded && txn.was_enabled != Some(true) {
            return Err(unexpected());
        }
        Ok(unloaded)
    }

    async fn bootstrap(&self) -> Result<(), GuiError> {
        let plist = self.paths.plist.clone();
        self.service(move |service| service.bootstrap(&plist)).await
    }

    async fn kickstart(&self) -> Result<(), GuiError> {
        self.service(|service| service.kickstart(AGENT_LABEL)).await
    }

    async fn require_enabled(&self) -> Result<(), GuiError> {
        if self.disabled().await? {
            Err(agent_disabled())
        } else {
            Ok(())
        }
    }

    async fn unwind(
        &self,
        txn: &mut Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.require_previous_app(txn).await?;
        if !matches!(self.adapters.daemon.quiescent(), Ok(true)) {
            return Ok(fence_uncertain(progress));
        }
        self.save_or_restore_enabled(txn, Phase::Interrupted)
            .await?;
        self.finish_old(
            txn,
            InstallOutcome::Interrupted {
                from: txn.from.clone(),
            },
            progress,
        )
        .await
    }

    async fn resume_fencing(
        &self,
        txn: &mut Txn,
        phase: Phase,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        if phase == Phase::Disabling {
            // An enabled read cannot distinguish a failed disable from one that
            // has not reached launchd yet. Wait for its effect before undoing it.
            if txn.was_enabled != Some(true) || !self.wait_for_disable().await {
                return Ok(fence_uncertain(progress));
            }
            self.record_disable_settled(txn).await?;
        }
        self.close_cancel();
        if self.store.install_details(txn)?.cancel_not_serving {
            return self.finish_not_serving_cancel(txn, progress).await;
        }
        self.require_previous_app(txn)
            .await
            .inspect_err(|_| progress(UpdateState::RestoreFailed))?;
        if txn.was_enabled == Some(true) && self.read_fence_state().await? {
            match self.job().await {
                Ok(None) => return self.unwind(txn, progress).await,
                Ok(Some(_)) => {}
                Err(e) => {
                    self.restore_enabled(txn).await?;
                    return Err(e);
                }
            }
            // No bootout was issued in this phase. A proved stopped job can
            // be unloaded for the step-10 restore-enabled/bootstrap sequence.
            match self.recheck_fence(false).await {
                Ok(Some(stopped)) => {
                    self.save_or_restore_enabled(txn, Phase::Bootout).await?;
                    return match self.unload(&stopped).await {
                        Unload::Done => self.unwind(txn, progress).await,
                        Unload::Failed => self.bootout_failed(txn, progress).await,
                        Unload::Uncertain => Ok(fence_uncertain(progress)),
                    };
                }
                Ok(None) => self.restore_enabled(txn).await?,
                Err(e) => {
                    self.restore_enabled(txn).await?;
                    return Err(e);
                }
            }
        }
        self.resume_old(
            txn,
            InstallOutcome::Interrupted {
                from: txn.from.clone(),
            },
            progress,
        )
        .await
    }

    async fn wait_for_disable(&self) -> bool {
        let clock = &self.adapters.clock;
        let began = clock.monotonic();
        loop {
            if matches!(self.disabled().await, Ok(true)) {
                return true;
            }
            if clock.monotonic().duration_since(began) >= STOP_BUDGET {
                return false;
            }
            clock.sleep(POLL).await;
        }
    }

    async fn finish_not_serving_cancel(
        &self,
        txn: &Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.restore_enabled(txn).await?;
        self.require_previous_app(txn).await?;
        self.finish(txn, InstallOutcome::Cancelled, progress)
    }

    async fn finish_respawn(
        &self,
        txn: &Txn,
        daemon: &DaemonIdentity,
        cancelled: bool,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.require_app_version(&txn.from)?;
        let bundle = self.bundle_info_at(&self.paths.bundle).await?;
        if bundle.version != daemon.version
            || (!cancelled && bundle.version != txn.from)
            || !matches!(
                release_cmp(&bundle.version, &txn.from),
                Some(Ordering::Equal | Ordering::Greater)
            )
        {
            return Err(unexpected());
        }
        let outcome = if cancelled {
            InstallOutcome::Cancelled
        } else {
            InstallOutcome::Interrupted {
                from: txn.from.clone(),
            }
        };
        self.finish(txn, outcome, progress)
    }

    async fn resume_old(
        &self,
        txn: &Txn,
        outcome: InstallOutcome,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.require_previous_app(txn).await?;
        if self.daemon_version().await? != txn.from {
            return Err(unexpected());
        }
        let starts = self.start_without_fence().await?;
        let predecessor = starts.then_some(txn.from_daemon.as_ref()).flatten();
        self.wait_ready(&txn.from, predecessor, None, true, progress)
            .await?;
        self.require_previous(txn).await?;
        self.finish(txn, outcome, progress)
    }

    /// This path had no successful bootout. A loaded job needs kickstart;
    /// an already serving job stays in place.
    async fn start_without_fence(&self) -> Result<bool, GuiError> {
        self.require_enabled().await?;
        match self.job().await? {
            Some(job) if job.pid.is_some() => Ok(false),
            Some(_) if self.adapters.daemon.quiescent()? => {
                self.kickstart().await?;
                Ok(true)
            }
            Some(_) => Err(not_quiescent()),
            None if self.adapters.daemon.quiescent()? => {
                self.bootstrap().await?;
                Ok(true)
            }
            None => Err(not_quiescent()),
        }
    }

    async fn finish_old(
        &self,
        txn: &Txn,
        outcome: InstallOutcome,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.require_previous_app(txn).await?;
        if self.daemon_version().await? != txn.from {
            return Err(unexpected());
        }
        match self.needs_bootstrap(txn).await {
            Ok(true) => {
                if txn.phase == Phase::Interrupted.name()
                    && !matches!(self.adapters.daemon.quiescent(), Ok(true))
                {
                    return Ok(fence_uncertain(progress));
                }
                self.bootstrap().await?;
            }
            Ok(false) => {}
            Err(error) if error.reason() == Some("update_not_quiescent") => {
                return Ok(fence_uncertain(progress));
            }
            Err(error) => return Err(error),
        }
        if outcome == InstallOutcome::Unchanged && txn.from_daemon.is_none() {
            // There was no healthy predecessor. The verified old bundle
            // and its registered job are enough for step 8(b).
            self.loaded().await?;
        } else {
            self.wait_ready(&txn.from, txn.from_daemon.as_ref(), None, false, progress)
                .await?;
        }
        self.require_previous(txn).await?;
        self.finish(txn, outcome, progress)
    }

    async fn bootout_failed(
        &self,
        txn: &mut Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.save_or_restore_enabled(txn, Phase::BootoutFailed)
            .await?;
        self.finish_bootout_failure(txn, progress).await
    }

    async fn finish_bootout_failure(
        &self,
        txn: &Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.restore_enabled(txn).await?;
        self.resume_cancel(txn, InstallOutcome::BootoutFailed, progress)
            .await
    }

    async fn cancel_stopped(
        &self,
        txn: &Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.resume_cancel(txn, InstallOutcome::Cancelled, progress)
            .await
    }

    async fn resume_cancel(
        &self,
        txn: &Txn,
        outcome: InstallOutcome,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        let Some(version) = self.cancel_version(txn).await? else {
            progress(UpdateState::CancelBlocked);
            return Ok(InstallOutcome::CancelBlocked);
        };
        let starts = self.start_without_fence().await?;
        let predecessor = starts.then_some(txn.from_daemon.as_ref()).flatten();
        self.wait_ready(&version, predecessor, None, true, progress)
            .await?;
        if self.bundle_info_at(&self.paths.bundle).await?.version != version {
            return Err(unexpected());
        }
        self.finish(txn, outcome, progress)
    }

    async fn cancel_version(&self, txn: &Txn) -> Result<Option<String>, GuiError> {
        let original = txn
            .from_daemon
            .as_ref()
            .map(|d| d.version.as_str())
            .unwrap_or(&txn.from);
        let Some(info) = self.cancel_bundle().await else {
            return Ok(None);
        };
        match release_cmp(&info.version, original) {
            Some(Ordering::Equal) => Ok(Some(info.version)),
            Some(Ordering::Greater) => {
                let live = self.adapters.daemon.retained_store_schema();
                // The installed bundle can change while the consent dialog is open.
                if live.is_none_or(|schema| info.store_schema > schema)
                    && (!self.adapters.host.confirm_schema(live, &info).await?
                        || self.cancel_bundle().await.as_ref() != Some(&info))
                {
                    return Ok(None);
                }
                Ok(Some(info.version))
            }
            _ => Ok(None),
        }
    }

    async fn cancel_bundle(&self) -> Option<BundleInfo> {
        let version = self.daemon_version().await.ok()?;
        let info = self.bundle_info_at(&self.paths.bundle).await.ok()?;
        (info.version == version).then_some(info)
    }

    async fn not_starting(
        &self,
        txn: &Txn,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        self.restore_enabled(txn).await?;
        self.require_previous_app(txn).await?;
        self.finish(txn, InstallOutcome::NotStarting, progress)
    }

    async fn wait_ready(
        &self,
        version: &str,
        predecessor: Option<&DaemonIdentity>,
        mut journal: Option<Txn>,
        require_unsealed: bool,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let clock = &self.adapters.clock;
        let details = journal
            .as_ref()
            .map(|txn| self.store.install_details(txn))
            .transpose()?
            .unwrap_or_default();
        let since = details
            .start_requested_ms
            .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms))
            .unwrap_or_else(|| clock.now());
        let paused_at = details.permission_wait_elapsed_ms;
        let mut elapsed = paused_at
            .map(Duration::from_millis)
            .unwrap_or_else(|| clock.now().duration_since(since).unwrap_or_default());
        let mut mark = clock.monotonic();
        let mut paused = paused_at.is_some();
        let mut reason = "start_timeout";
        progress(if paused {
            UpdateState::WaitingPermission
        } else {
            UpdateState::WaitingDaemon { since }
        });
        loop {
            let job = self.job().await;
            let status = self.adapters.daemon.status().await;
            let now = clock.monotonic();
            if !paused {
                elapsed = elapsed.saturating_add(now.duration_since(mark));
            }
            mark = now;

            let looks_not_serving =
                matches!(&job, Ok(Some(job)) if job.pid.is_none()) && status.is_err();
            let mut rollback_path = None;
            match (job, status) {
                (Ok(Some(job)), Ok(status)) => {
                    let reply_matches = job.pid.is_some() && job.pid == status_pid(&status);
                    let new_process = predecessor.is_none_or(|old| {
                        identity(&status)
                            .is_ok_and(|new| new.pid != old.pid && new.started > old.started)
                    });
                    let drain = drain_observation(&status);
                    if reply_matches
                        && status["version"] == version
                        && new_process
                        && (!require_unsealed
                            || (drain == Some(DrainObservation::Absent)
                                && identity(&status).is_ok()))
                    {
                        return Ok(());
                    }
                    if journal.is_some()
                        && reply_matches
                        && status["version"] == version
                        && matches!(
                            drain,
                            Some(DrainObservation::Absent | DrainObservation::Unsealed(_))
                        )
                        && require_drain(&status).is_ok()
                    {
                        rollback_path = Some("sealed");
                    }
                    reason = if !reply_matches || !new_process {
                        "daemon_identity"
                    } else if status["version"] != version {
                        "daemon_version"
                    } else {
                        "draining"
                    };
                }
                (Ok(Some(job)), Err(e)) if job.pid.is_some() && waiting_permission(&e) => {
                    if !paused {
                        if let Some(txn) = &mut journal {
                            self.install_phase(txn, Phase::WaitingPermission, |details| {
                                details.permission_wait_elapsed_ms = Some(duration_millis(elapsed));
                            })?;
                        }
                        progress(UpdateState::WaitingPermission);
                    }
                    paused = true;
                    clock.sleep(POLL).await;
                    continue;
                }
                (_, Err(e)) if e.reason() == Some("store_unavailable") => {
                    reason = "store_unavailable";
                }
                (Err(e), _) if e.reason() == Some("update_agent_identity") => {
                    progress(UpdateState::DaemonFailed {
                        reason: "update_agent_identity".into(),
                    });
                    return Err(e);
                }
                _ => {}
            }
            if paused {
                paused = false;
                if let Some(txn) = &mut journal {
                    self.install_phase(txn, Phase::WaitingDaemon, |details| {
                        details.permission_wait_elapsed_ms = None;
                        details.start_requested_ms =
                            Some(unix_millis(clock.now()).saturating_sub(duration_millis(elapsed)));
                    })?;
                }
                progress(UpdateState::WaitingDaemon { since });
            }
            if elapsed >= START_BUDGET {
                progress(UpdateState::DaemonFailed {
                    reason: reason.into(),
                });
                // The rollback probe has its own five-second window, even when
                // the App was relaunched after the entire startup budget elapsed.
                if journal.is_some() && looks_not_serving && self.can_offer_not_serving().await {
                    rollback_path = Some("not_serving");
                }
                return Err(GuiError::new(
                    Code::Busy,
                    "The daemon didn't come back after the update",
                    json!({ "reason": reason, "rollback_path": rollback_path }),
                ));
            }
            clock.sleep(POLL.min(START_BUDGET - elapsed)).await;
        }
    }

    async fn can_offer_not_serving(&self) -> bool {
        for sample in 0..2 {
            if sample != 0 {
                self.adapters.clock.sleep(NO_PID_GAP).await;
            }
            if !matches!(self.stop_sample().await, Ok(Some((job, true))) if job.pid.is_none()) {
                return false;
            }
        }
        true
    }

    fn finish(
        &self,
        txn: &Txn,
        outcome: InstallOutcome,
        progress: &mut Progress<'_>,
    ) -> Result<InstallOutcome, GuiError> {
        let state = match outcome {
            InstallOutcome::Updated => UpdateState::Done,
            InstallOutcome::Cancelled | InstallOutcome::Interrupted { .. } => {
                UpdateState::Cancelled
            }
            InstallOutcome::NotStarting => UpdateState::NotStarting,
            InstallOutcome::Unchanged => UpdateState::InstallFailedUnchanged,
            InstallOutcome::Restored => UpdateState::Restored {
                from: txn.from.clone(),
            },
            InstallOutcome::BootoutFailed => UpdateState::BootoutFailed,
            _ => return Err(unexpected()),
        };
        self.store.clear(txn)?;
        progress(state);
        Ok(outcome)
    }
}

impl Job {
    fn parse(dump: &str, plist: &Path, program: &Path) -> Result<Self, GuiError> {
        let parse_error = || {
            problem(
                Code::InvalidInput,
                "update_agent_identity",
                "Cannot verify the loaded launch agent's plist, program or process",
            )
        };
        let path = scope::loaded_path(dump).ok_or_else(parse_error)?;
        let mut fields = std::collections::BTreeMap::new();
        let mut depth = 0i32;
        for line in dump.lines().map(str::trim) {
            if line.starts_with('}') {
                depth -= 1;
                if depth < 0 {
                    return Err(parse_error());
                }
                continue;
            }
            if depth == 1
                && let Some((key, value)) = line.split_once(" = ")
            {
                fields.insert(key.trim(), value.trim());
            }
            if line.ends_with('{') {
                depth += 1;
            }
        }
        let job_program = fields.get("program").ok_or_else(parse_error)?;
        if depth != 0
            || scope::anchored(Path::new(&path)) != scope::anchored(plist)
            || scope::anchored(Path::new(job_program)) != scope::anchored(program)
        {
            return Err(parse_error());
        }
        let pid = fields
            .get("pid")
            .map(|n| {
                n.parse::<u32>()
                    .ok()
                    .filter(|&pid| pid != 0)
                    .ok_or_else(parse_error)
            })
            .transpose()?;
        if pid.is_none() && fields.get("state") == Some(&"running") {
            return Err(parse_error());
        }
        let signal = fields
            .get("last terminating signal")
            .and_then(|value| value.rsplit(':').next()?.trim().parse::<u32>().ok());
        let last_exit = if signal.is_some_and(|signal| signal > 0) {
            Some(LastExit::Failed)
        } else {
            fields.get("last exit code").and_then(|value| {
                let code = value.split(':').next()?.trim().parse::<i32>().ok()?;
                Some(if code == 0 {
                    LastExit::Clean
                } else {
                    LastExit::Failed
                })
            })
        };
        let exit_timeout = fields
            .get("exit timeout")
            .and_then(|n| n.parse::<u32>().ok())
            .map(|s| Duration::from_secs(u64::from(s)));
        Ok(Self {
            pid,
            last_exit,
            exit_timeout,
        })
    }
}

/// Absence and a verifier-confirmed invalid signature permit restoration.
/// A permission failure, verifier failure or changing path proves neither.
fn inspect_bundle(bundles: &dyn BundleOps, path: &Path) -> Result<Option<BundleInfo>, GuiError> {
    let access_error = |e| {
        problem(
            Code::Unknown,
            "update_bundle_inspection",
            format!("Cannot inspect the installed app: {e}"),
        )
    };
    let before = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => meta,
        Ok(_) => return Err(unexpected()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(access_error(e)),
    };
    let result = match bundles.codesign_verify(path) {
        Ok(()) => bundles.read_info_plist(path).map(Some),
        Err(e) if e.code == Code::CheckFailed.as_str() && e.reason() == Some("bad_codesign") => {
            Ok(None)
        }
        Err(e) => Err(e),
    };
    let after = fs::symlink_metadata(path).map_err(access_error)?;
    if !after.is_dir()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.modified().map_err(access_error)? != after.modified().map_err(access_error)?
    {
        return Err(problem(
            Code::Busy,
            "update_bundle_inspection",
            "The installed app changed during verification",
        ));
    }
    result
}

fn launchctl_exited_nonzero(error: &GuiError, step: &str) -> bool {
    error.reason() == Some("launchctl_failed")
        && error.detail["step"] == step
        && error.detail["status"].as_i64().is_some_and(|n| n != 0)
}

fn bootout_preflight_failed(error: &GuiError) -> bool {
    crate::service::pre_dispatch_error(error)
}

fn disable_settled(error: &GuiError) -> bool {
    launchctl_exited_nonzero(error, "disable") || crate::service::pre_dispatch_error(error)
}

fn transient_stop_probe(error: &GuiError) -> bool {
    matches!(
        error.reason(),
        Some("launchctl_failed" | "update_daemon_probe")
    )
}

/// The updater targets stable releases only.
fn release_cmp(left: &str, right: &str) -> Option<Ordering> {
    fn release(text: &str) -> Option<[u64; 3]> {
        let mut parts = text.split('.');
        let mut version = [0; 3];
        for component in &mut version {
            let s = parts.next()?;
            if !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
                return None;
            }
            *component = s.parse().ok()?;
        }
        parts.next().is_none().then_some(version)
    }
    Some(release(left)?.cmp(&release(right)?))
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, GuiError> + Send + 'static,
) -> Result<T, GuiError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|e| GuiError::internal(format!("Update operation failed: {e}")))?
}

fn status_pid(status: &Value) -> Option<u32> {
    u32::try_from(status["pid"].as_u64()?)
        .ok()
        .filter(|&pid| pid != 0)
}

fn identity(status: &Value) -> Result<DaemonIdentity, GuiError> {
    let pid = status_pid(status).ok_or_else(daemon_identity)?;
    let started = status["started"]
        .as_str()
        .and_then(|s| strata_types::serde_rfc3339::parse(s).ok())
        .ok_or_else(daemon_identity)?;
    let version = status["version"]
        .as_str()
        .ok_or_else(daemon_identity)?
        .into();
    Ok(DaemonIdentity {
        pid,
        started,
        version,
    })
}

fn require_drain(status: &Value) -> Result<(), GuiError> {
    if status["features"]
        .as_array()
        .is_some_and(|features| features.iter().any(|f| f == "daemon.drain"))
    {
        Ok(())
    } else {
        Err(GuiError::new(
            Code::Unsupported,
            "This daemon can't be updated safely from the app",
            json!({"reason": "daemon_too_old", "feature": "daemon.drain"}),
        ))
    }
}

fn drain_observation(status: &Value) -> Option<DrainObservation<'_>> {
    let value = status.get("draining")?;
    if value.is_null() {
        return Some(DrainObservation::Absent);
    }
    let id = value
        .get("lease_id")?
        .as_str()
        .filter(|id| !id.is_empty())?;
    match value.get("sealed")?.as_bool()? {
        false
            if value.get("then")?.is_null()
                && strata_types::serde_rfc3339::parse(value.get("expires_at")?.as_str()?)
                    .is_ok() =>
        {
            Some(DrainObservation::Unsealed(id))
        }
        true if value.get("expires_at")?.is_null()
            && matches!(
                value.get("then")?.as_str(),
                Some("stop" | "restart" | "hold")
            ) =>
        {
            Some(DrainObservation::Sealed)
        }
        _ => None,
    }
}

fn next_drain_step(
    observed: DrainObservation<'_>,
    lease: Option<&LiveLease>,
    idle: bool,
    now: std::time::Instant,
) -> Result<DrainStep, GuiError> {
    match observed {
        DrainObservation::Absent => Ok(DrainStep::Acquire),
        DrainObservation::Unsealed(id) if lease.is_some_and(|live| live.id == id) => {
            if lease.is_some_and(|live| now >= live.renew_at) {
                Ok(DrainStep::Renew)
            } else if idle {
                Ok(DrainStep::Seal)
            } else {
                Ok(DrainStep::Wait)
            }
        }
        DrainObservation::Unsealed(_) => Err(problem(
            Code::Busy,
            "draining",
            "The daemon already has a drain lease",
        )),
        DrainObservation::Sealed => Err(problem(
            Code::Busy,
            "daemon_already_sealed",
            "The daemon is already stopping",
        )),
    }
}

fn valid_lease_grant(id: Option<&str>, renew: Option<&str>) -> bool {
    id.is_some_and(|id| !id.is_empty() && renew.is_none_or(|old| id == old))
}

fn invalid_drain_status() -> GuiError {
    problem(
        Code::InvalidInput,
        "invalid_drain_status",
        "The daemon did not report a valid drain lease status",
    )
}

fn cleanup_uncertain() -> GuiError {
    problem(
        Code::Unknown,
        "drain_release_unconfirmed",
        "Drain lease cleanup is uncertain",
    )
}

fn replacement(old: &DaemonIdentity, new: &DaemonIdentity) -> bool {
    new.started > old.started || (new.pid != old.pid && new.started == old.started)
}

fn store_schema(status: &Value) -> Option<u32> {
    status["store_schema"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
}

fn retain_target(attempt: &mut RollbackAttempt, reason: TargetRecovery) {
    attempt.phase = RollbackPhase::StartingTarget;
    attempt.target_recovery = Some(reason);
}

fn report_draining(status: &Value, progress: &mut Progress<'_>) -> bool {
    let inflight = list(status, "inflight");
    let background: Vec<_> = list(status, "holds")
        .into_iter()
        .filter(|hold| hold["origin"] == "background")
        .collect();
    let idle = inflight.is_empty() && background.is_empty();
    progress(UpdateState::Draining {
        inflight,
        background,
    });
    idle
}

fn list(status: &Value, field: &str) -> Vec<Value> {
    status[field].as_array().cloned().unwrap_or_default()
}

fn waiting_permission(error: &GuiError) -> bool {
    // Same TCC classification as service::classify.
    error.reason() == Some("store_unavailable")
        && matches!(
            error.detail["last_error"]["detail"]["errno"].as_i64(),
            Some(1 | 4 | 13)
        )
}

fn unix_millis(time: SystemTime) -> u64 {
    duration_millis(
        time.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default(),
    )
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn fence_uncertain(progress: &mut Progress<'_>) -> InstallOutcome {
    progress(UpdateState::FenceUncertain);
    InstallOutcome::FenceUncertain
}

fn problem(code: Code, reason: &str, message: impl Into<String>) -> GuiError {
    GuiError::new(code, message, json!({ "reason": reason }))
}

fn schema_older() -> GuiError {
    problem(
        Code::Unsupported,
        "update_schema_older",
        "The update cannot open the current store schema",
    )
}

pub(super) fn require_schema_ack(
    live: Option<u32>,
    target: &BundleInfo,
    acknowledged: Option<u32>,
) -> Result<(), GuiError> {
    if live.is_some_and(|live| live > target.store_schema) {
        return Err(schema_older());
    }
    if live.is_none_or(|live| live < target.store_schema)
        && acknowledged != Some(target.store_schema)
    {
        return Err(GuiError::new(
            Code::NotConfirmed,
            "Confirm the verified store schema change",
            json!({
                "reason":"schema_acknowledgement_required", "from_schema":live, "to_schema":target.store_schema
            }),
        ));
    }
    Ok(())
}

fn lifecycle_busy() -> GuiError {
    problem(
        Code::Busy,
        "update_in_flight",
        "An update or daemon operation is already in progress",
    )
}

fn unexpected() -> GuiError {
    problem(
        Code::InvalidInput,
        "update_txn_unexpected",
        "The installed app or daemon differs from the update transaction",
    )
}

fn agent_disabled() -> GuiError {
    problem(
        Code::Unsupported,
        "agent_disabled",
        "The daemon's launch agent is disabled",
    )
}

fn daemon_identity() -> GuiError {
    problem(
        Code::InvalidInput,
        "daemon_identity",
        "The answering daemon does not match the bundled launch agent",
    )
}

fn not_quiescent() -> GuiError {
    problem(
        Code::Busy,
        "update_not_quiescent",
        "Cannot verify that the bundled daemon is stopped",
    )
}

#[cfg(test)]
pub(in crate::update) mod tests;

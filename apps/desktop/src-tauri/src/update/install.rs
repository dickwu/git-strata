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
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_types::{Code, STORE_SCHEMA};

use super::bundle::{BundleInfo, BundleOps};
use super::source::VerifiedUpdate;
use super::state::{TxnPhase as Phase, UpdateState};
use super::txn::{DaemonIdentity, InstallDetails, Txn, TxnStore};
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
    Acquire { ttl_s: u32 },
    Renew { lease_id: &'a str, ttl_s: u32 },
    Release { lease_id: &'a str },
    SealStop { lease_id: &'a str },
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
    fn confirm_schema<'a>(
        &'a self,
        live_schema: Option<u32>,
        target: &'a BundleInfo,
    ) -> UpdateFuture<'a, Result<bool, GuiError>>;
    /// Restore the already codesign- and version-verified bundle returned
    /// by the plugin, without overwriting an unrelated installed bundle.
    fn restore_kept(&self, kept: &Path, bundle: &Path) -> Result<(), GuiError>;
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

    pub fn cancel(&self) -> Result<(), GuiError> {
        let mut cancel = self.cancel_state();
        match cancel.mode {
            CancelMode::Closed => {
                return Err(problem(
                    Code::Busy,
                    "update_not_cancellable",
                    "The update is past its cancellation point",
                ));
            }
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
        self.store.begin(&txn)?;

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
            .map(|dump| Job::parse(&dump, &self.paths.plist, &self.program))
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
                if self.daemon_version().await? != self.paths.app_version {
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
        match release_cmp(&self.daemon_version().await?, &daemon.version) {
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
        let schema = status["store_schema"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok());
        Ok(Eligible::Serving(daemon, schema))
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
        let mut lease: Option<String> = None;
        let mut renew_at = clock.monotonic();
        loop {
            if self.cancel_state().requested {
                self.close_cancel();
                if let Some(id) = lease.as_deref()
                    && let Err(error) = daemon.drain(DrainAction::Release { lease_id: id }).await
                    && error.reason() != Some("lease_lost")
                {
                    return Err(error);
                }
                self.require_previous(txn).await?;
                return Ok(Seal::Cancelled);
            }

            let status = daemon.status().await?;
            if self.loaded().await?.pid != Some(predecessor.pid)
                || identity(&status)? != predecessor
            {
                return Err(daemon_identity());
            }
            require_drain(&status)?;
            if sealed(&status) {
                return Err(problem(
                    Code::Busy,
                    "daemon_already_sealed",
                    "The daemon is already stopping",
                ));
            }

            if clock.monotonic() >= renew_at {
                let action = match lease.as_deref() {
                    Some(id) => DrainAction::Renew {
                        lease_id: id,
                        ttl_s: LEASE_TTL_S,
                    },
                    None => DrainAction::Acquire { ttl_s: LEASE_TTL_S },
                };
                match daemon.drain(action).await {
                    Ok(Some(id)) if !id.is_empty() => lease = Some(id),
                    Err(e) if e.reason() == Some("lease_lost") => {
                        lease = None;
                        clock.sleep(POLL).await;
                        continue;
                    }
                    Err(e) => return Err(e),
                    _ => {
                        return Err(problem(
                            Code::InvalidInput,
                            "update_drain_reply",
                            "The drain reply has no lease ID",
                        ));
                    }
                }
                renew_at = clock.monotonic() + RENEW_INTERVAL;
            }

            let inflight = list(&status, "inflight");
            let background: Vec<_> = list(&status, "holds")
                .into_iter()
                .filter(|hold| hold["origin"] == "background")
                .collect();
            let idle = inflight.is_empty() && background.is_empty();
            progress(UpdateState::Draining {
                inflight,
                background,
            });
            if idle {
                // A Cancel during this RPC must be durable, even if the App
                // never receives the response. The daemon arbitrates idleness.
                {
                    let mut cancel = self.cancel_state();
                    if cancel.requested {
                        continue;
                    }
                    self.phase(txn, Phase::Stopping)?;
                    cancel.mode = CancelMode::SealedStop;
                }
                let id = lease.as_deref().ok_or_else(unexpected)?;
                match daemon.drain(DrainAction::SealStop { lease_id: id }).await {
                    Ok(_) => return Ok(Seal::Accepted),
                    Err(e) if matches!(e.reason(), Some("not_idle" | "lease_lost")) => {
                        let mut cancel = self.cancel_state();
                        if e.reason() == Some("lease_lost") {
                            lease = None;
                        }
                        renew_at = clock.monotonic();
                        self.phase(txn, Phase::Draining)?;
                        cancel.mode = CancelMode::Drain;
                    }
                    Err(e) if e.code == Code::Unknown.as_str() => return Ok(Seal::Uncertain),
                    Err(e) => return Err(e),
                }
            }
            clock.sleep(POLL).await;
        }
    }

    async fn wait_stopped(
        &self,
        txn: &Txn,
        seal_uncertain: bool,
        already_cancelled: bool,
        progress: &mut Progress<'_>,
    ) -> Result<Stopped, GuiError> {
        let clock = &self.adapters.clock;
        let began = clock.monotonic();
        let stop_after = txn
            .from_daemon
            .as_ref()
            .map(|d| d.started)
            .unwrap_or_else(|| clock.now());
        let mut warned = false;
        let mut last_failure = None;
        let mut sealed_exit = !seal_uncertain;
        let predecessor_pid = txn.from_daemon.as_ref().map(|daemon| daemon.pid);
        progress(UpdateState::Stopping);
        loop {
            match self.stop_sample().await {
                Ok(Some((job, _))) if job.pid.is_some() => {
                    // A replacement's exit can be clean even if it never served.
                    if job.pid != predecessor_pid {
                        sealed_exit = false;
                    }
                    if let Ok(status) = self.adapters.daemon.status().await
                        && let Ok(now) = identity(&status)
                        && job.pid == Some(now.pid)
                        && !sealed(&status)
                    {
                        let respawned = txn
                            .from_daemon
                            .as_ref()
                            .is_some_and(|old| old.pid != now.pid || now.started > old.started);
                        // A missing lease on the original process after a lost reply
                        // proves that the old lease can no longer seal.
                        if respawned || (seal_uncertain && status["draining"].is_null()) {
                            return Ok(Stopped::Respawned(
                                now,
                                self.close_cancel() || already_cancelled,
                            ));
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
                    return Ok(Stopped::Steady {
                        job,
                        cancelled: self.close_cancel() || already_cancelled,
                        sealed: sealed_exit,
                    });
                }
                Ok(Some((job, true))) if job.last_exit == Some(LastExit::Failed) => {
                    sealed_exit = false;
                    let now = clock.monotonic();
                    // A process seen between the two samples can itself be a failed
                    // respawn. Only a matching, answering process takes the branch above.
                    let first = *last_failure.get_or_insert(now);
                    if now.duration_since(first) >= CRASH_LOOP_GAP
                        && (now.duration_since(began) >= STOP_BUDGET
                            || already_cancelled
                            || self.cancel_state().requested)
                    {
                        return Ok(Stopped::CrashLoop(self.close_cancel() || already_cancelled));
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
            .service(|service| service.bootout(AGENT_LABEL))
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
        let kept = blocking(move || {
            let result = bundles.install(update);
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
                        || !self
                            .bundle_info_at(path)
                            .await
                            .is_ok_and(|info| info.version == txn.from)
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
                    let _ = blocking(move || host.restore_kept(&path, &bundle)).await;
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
                    let zip = txn.rollback_zip.clone();
                    let bundles = self.adapters.bundles.clone();
                    // Verification below decides; the restore may have edited
                    // the destination before reporting an error.
                    let _ = blocking(move || bundles.restore_zip(&zip, &bundle)).await;
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

    async fn start_fenced(&self, txn: &Txn) -> Result<(), GuiError> {
        if self.needs_bootstrap(txn).await? {
            self.bootstrap().await?;
        }
        Ok(())
    }

    async fn needs_bootstrap(&self, txn: &Txn) -> Result<bool, GuiError> {
        if txn.was_enabled == Some(false) {
            return Err(agent_disabled());
        }
        self.job().await?; // Refuse a foreign plist or program before enabling.
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
                self.service(|service| service.kickstart(AGENT_LABEL))
                    .await?;
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
        self.start_fenced(txn).await?;
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
                    if reply_matches
                        && status["version"] == version
                        && new_process
                        && (!require_unsealed || status["draining"].is_null())
                    {
                        return Ok(());
                    }
                    if journal.is_some()
                        && reply_matches
                        && status["version"] == version
                        && !sealed(&status)
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
    // The adapter checks the login home, job ownership and target before dispatch.
    match error.reason() {
        Some("launchctl_failed") => error.detail["step"] == "print",
        Some(
            "no_home"
            | "foreign_home"
            | "foreign_agent"
            | "bootout_target_required"
            | "bootout_target_changed",
        ) => true,
        _ => false,
    }
}

fn disable_settled(error: &GuiError) -> bool {
    // The service adapter checks the loaded job with print before dispatch.
    // Failure of that read cannot leave a disable running in the background.
    launchctl_exited_nonzero(error, "disable")
        || (error.reason() == Some("launchctl_failed") && error.detail["step"] == "print")
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

fn sealed(status: &Value) -> bool {
    status["draining"]["sealed"].as_bool() == Some(true)
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
mod tests;

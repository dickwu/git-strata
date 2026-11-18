//! Drained restart of the bundled daemon.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};
use strata_types::{Code, StrataError};
use tauri::{Manager, WebviewWindow, ipc::Channel};

use crate::error::GuiError;
use crate::service::{AGENT_LABEL, ServiceManager, launchctl::Launchctl, plist_for_home, scope};
use crate::state::AppState;

use super::exec::Operation;
use super::state::UpdateState;
use super::txn::DaemonIdentity;
use super::{Clock, UpdateFuture};

const DRAIN_FEATURE: &str = "daemon.drain";
const RPC_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RENEW_INTERVAL: Duration = Duration::from_secs(30);
const RESTART_TIMEOUT: Duration = Duration::from_secs(150);

pub struct RestartBundle {
    /// stdout from `Operation::DaemonVersion` in the pinned updater runner.
    pub daemon_version_output: String,
    /// GitStrataStoreSchema from the installed bundle's Info.plist.
    pub store_schema: u32,
}

type InspectBundle = dyn Fn(&Path) -> Result<RestartBundle, GuiError> + Send + Sync;
type Progress<'a> = dyn FnMut(UpdateState) + Send + 'a;

/// The App manages one controller and routes the shared Cancel command here while draining.
pub struct RestartController {
    inspect_bundle: Arc<InspectBundle>,
    phase: Mutex<Option<Phase>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Draining,
    Cancelled,
    Sealing,
    Finishing,
}

enum DrainLease {
    Absent,
    Held(String),
    AcquisitionUncertain,
}

impl DrainLease {
    fn id(&self) -> Option<&str> {
        match self {
            Self::Held(id) => Some(id),
            Self::Absent | Self::AcquisitionUncertain => None,
        }
    }
}

struct RestartObservation {
    previous: DaemonIdentity,
    expected_version: String,
    seal_error: Option<GuiError>,
    deadline: Instant,
}

struct RestartFlight<'a>(&'a Mutex<Option<Phase>>);

impl RestartFlight<'_> {
    fn cancelled(&self) -> bool {
        *self.0.lock().unwrap() == Some(Phase::Cancelled)
    }

    fn begin_seal(&self) -> bool {
        let mut phase = self.0.lock().unwrap();
        if *phase == Some(Phase::Cancelled) {
            return false;
        }
        *phase = Some(Phase::Sealing);
        true
    }

    fn retry_drain(&self) {
        *self.0.lock().unwrap() = Some(Phase::Draining);
    }

    fn finish_drain(&self) -> bool {
        let mut phase = self.0.lock().unwrap();
        let cancelled = *phase == Some(Phase::Cancelled);
        *phase = Some(Phase::Finishing);
        cancelled
    }
}

impl Drop for RestartFlight<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

impl RestartController {
    /// `inspect_bundle` receives only the verified login home. The inspector runs
    /// the fixed `--version` operation and reads metadata for the same bundle.
    pub fn new(
        inspect_bundle: impl Fn(&Path) -> Result<RestartBundle, GuiError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inspect_bundle: Arc::new(inspect_bundle),
            phase: Mutex::new(None),
        }
    }

    /// Returns true when this restart accepted Cancel. A seal in flight or a
    /// sealed restart must be observed through its terminal progress event.
    pub fn request_cancel(&self) -> bool {
        let mut phase = self.phase.lock().unwrap();
        if matches!(*phase, Some(Phase::Draining | Phase::Cancelled)) {
            *phase = Some(Phase::Cancelled);
            return true;
        }
        false
    }

    fn begin(&self) -> Result<RestartFlight<'_>, GuiError> {
        let mut phase = self.phase.lock().unwrap();
        if phase.is_some() {
            return Err(failure(
                Code::Busy,
                "restart_in_flight",
                "A daemon restart is already in progress",
            ));
        }
        *phase = Some(Phase::Draining);
        Ok(RestartFlight(&self.phase))
    }

    async fn run(
        &self,
        target: &RestartTarget,
        acknowledgement: Option<u32>,
        clock: &dyn Clock,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let flight = self.begin()?;
        let mut lease = DrainLease::Absent;
        let mut last_progress = None;
        let seal = self
            .drain_and_seal(
                target,
                acknowledgement,
                clock,
                &flight,
                &mut |state| {
                    last_progress = Some(state.clone());
                    progress(state);
                },
                &mut lease,
            )
            .await;
        let observation = match seal {
            Ok(Some(observation)) => observation,
            unsealed => {
                let cancelled = flight.finish_drain();
                let drain_error = unsealed.err();
                let released = match lease {
                    DrainLease::AcquisitionUncertain => {
                        // A lost acquisition reply leaves no lease ID to release.
                        if !matches!(last_progress, Some(UpdateState::Draining { .. })) {
                            progress(UpdateState::Draining {
                                inflight: Vec::new(),
                                background: Vec::new(),
                            });
                        }
                        return Err(GuiError::new(
                            Code::Unknown,
                            "Drain cleanup is uncertain. The daemon may still refuse writes until the lease expires (up to 120 seconds).",
                            json!({"reason":"drain_release_unconfirmed", "acquire_error": drain_error}),
                        ));
                    }
                    DrainLease::Held(id) => {
                        let released = target.release(id.clone()).await;
                        if !cancelled
                            && released
                                .as_ref()
                                .is_err_and(|error| error.code == Code::Unknown.as_str())
                        {
                            // A same-ID release can only remove that lease. A
                            // lease_lost reply confirms it is already gone.
                            target.release(id).await
                        } else {
                            released
                        }
                    }
                    DrainLease::Absent => Ok(()),
                };
                let refusal = drain_error
                    .filter(|error| !cancelled || error.reason() == Some("unexpected_lease"));
                if let Err(release_error) = released {
                    // Keep the operation in Draining so the UI shows the
                    // cleanup error and ends any expected restart window.
                    if !matches!(last_progress, Some(UpdateState::Draining { .. })) {
                        progress(UpdateState::Draining {
                            inflight: Vec::new(),
                            background: Vec::new(),
                        });
                    }
                    let message = "Could not confirm that the drain lease was released. The daemon may still refuse writes.";
                    let message = match &refusal {
                        Some(error) => format!("{message} {}", error.message),
                        None => message.to_owned(),
                    };
                    return Err(GuiError::new(
                        Code::Unknown,
                        message,
                        json!({
                            "reason": "drain_release_unconfirmed",
                            "refusal": refusal,
                            "release_error": release_error,
                        }),
                    ));
                }
                if let Some(error) = refusal {
                    if last_progress.is_some() {
                        progress(UpdateState::Idle);
                    }
                    return Err(error);
                }
                progress(UpdateState::Cancelled);
                return Ok(());
            }
        };

        match wait_for_respawn(target, &observation, clock, progress).await {
            Ok(()) => {
                progress(UpdateState::Done);
                Ok(())
            }
            Err(error) => {
                progress(UpdateState::DaemonFailed {
                    reason: error.reason().unwrap_or("restart_failed").into(),
                });
                Err(observation.seal_error.unwrap_or(error))
            }
        }
    }

    async fn drain_and_seal(
        &self,
        target: &RestartTarget,
        acknowledgement: Option<u32>,
        clock: &dyn Clock,
        flight: &RestartFlight<'_>,
        progress: &mut Progress<'_>,
        lease: &mut DrainLease,
    ) -> Result<Option<RestartObservation>, GuiError> {
        let status = target.status(RPC_TIMEOUT).await?;
        strata_client::require_feature(&status, DRAIN_FEATURE)?;
        let previous = identity(&status)?;
        target.verify_job(previous.pid, true).await?;
        self.check_bundle(target, &status, &previous.version, acknowledgement)
            .await?;

        let mut first_status = Some(status);
        let mut renew_at = clock.monotonic();
        let mut last_progress = None;
        loop {
            if flight.cancelled() {
                return Ok(None);
            }
            let status = match first_status.take() {
                Some(status) => status,
                None => target.status(RPC_TIMEOUT).await?,
            };
            if identity(&status)? != previous {
                *lease = DrainLease::Absent;
                return Err(failure(
                    Code::Busy,
                    "daemon_identity_changed",
                    "The daemon changed while preparing to restart",
                ));
            }
            let Some(draining) = status.get("draining") else {
                return Err(failure(
                    Code::InvalidInput,
                    "invalid_drain_status",
                    "The daemon did not report its drain lease status",
                ));
            };
            if draining.is_null() {
                *lease = DrainLease::Absent;
            } else {
                let (Some(id), Some(sealed)) =
                    (draining["lease_id"].as_str(), draining["sealed"].as_bool())
                else {
                    return Err(failure(
                        Code::InvalidInput,
                        "invalid_drain_status",
                        "The daemon reported an invalid drain lease status",
                    ));
                };
                if lease.id() != Some(id) || sealed {
                    *lease = DrainLease::Absent;
                    return Err(failure(
                        Code::Busy,
                        "draining",
                        "Another operation is draining the daemon",
                    ));
                }
            }

            if flight.cancelled() {
                return Ok(None);
            }

            let now = clock.monotonic();
            if matches!(lease, DrainLease::Absent) || now >= renew_at {
                let existing = lease.id().map(str::to_owned);
                let expected = previous.clone();
                *lease = match target
                    .rpc(RPC_TIMEOUT, move |socket, budget| {
                        if let Some(id) = existing {
                            strata_client::drain_on(socket, Some(&id), budget)
                        } else {
                            strata_client::drain_acquire(socket, budget, |status| {
                                identity(status).is_ok_and(|peer| peer == expected)
                            })
                        }
                    })
                    .await
                {
                    Ok(id) if lease.id().is_none_or(|previous| previous == id) => {
                        DrainLease::Held(id)
                    }
                    Ok(_) => {
                        return Err(failure(
                            Code::Unknown,
                            "unexpected_lease",
                            "The daemon changed the drain lease during renewal",
                        ));
                    }
                    Err(error)
                        if matches!(lease, DrainLease::Held(_)) && busy(&error, "lease_lost") =>
                    {
                        DrainLease::Absent
                    }
                    Err(error) => {
                        if matches!(lease, DrainLease::Absent)
                            && error.code == Code::Unknown.as_str()
                        {
                            *lease = DrainLease::AcquisitionUncertain;
                        }
                        return Err(error);
                    }
                };
                renew_at = now + RENEW_INTERVAL;
                continue;
            }

            let inflight = status_array(&status, "inflight")?.to_vec();
            let background: Vec<Value> = status_array(&status, "holds")?
                .iter()
                .filter(|hold| hold["origin"] == "background")
                .cloned()
                .collect();
            let idle = inflight.is_empty() && background.is_empty();
            let next = UpdateState::Draining {
                inflight,
                background,
            };
            if last_progress.as_ref() != Some(&next) {
                progress(next.clone());
                last_progress = Some(next.clone());
            }
            if idle && !flight.cancelled() {
                let id = lease
                    .id()
                    .expect("lease acquired before checking for idle")
                    .to_owned();
                // Verify the bundle again after waiting. Homebrew may have swapped it.
                let expected_version = self
                    .check_bundle(target, &status, &previous.version, acknowledgement)
                    .await?;
                target.verify_job(previous.pid, true).await?;
                if !flight.begin_seal() {
                    return Ok(None);
                }
                // Mark the expected outage before the daemon can accept the seal.
                let deadline = clock.monotonic() + RESTART_TIMEOUT;
                progress(UpdateState::Stopping);
                match target
                    .rpc(RPC_TIMEOUT, move |socket, budget| {
                        strata_client::drain_seal(socket, &id, "restart", budget)
                    })
                    .await
                {
                    Err(error) if busy(&error, "not_idle") || busy(&error, "lease_lost") => {
                        if error.reason() == Some("lease_lost") {
                            *lease = DrainLease::Absent;
                        }
                        flight.retry_drain();
                        progress(next);
                    }
                    Err(error) if error.code != Code::Unknown.as_str() => {
                        if error.code == Code::InvalidInput.as_str()
                            && error.reason() == Some("sealed")
                        {
                            *lease = DrainLease::Absent;
                        }
                        return Err(error);
                    }
                    // A completed send may have sealed the daemon. Observe it without
                    // replaying the write or attempting to release the lease.
                    result => {
                        return Ok(Some(RestartObservation {
                            previous,
                            expected_version,
                            seal_error: result.err(),
                            deadline,
                        }));
                    }
                }
            }

            if !flight.cancelled() {
                clock.sleep(POLL_INTERVAL).await;
            }
        }
    }

    async fn check_bundle(
        &self,
        target: &RestartTarget,
        status: &Value,
        running_version: &str,
        acknowledgement: Option<u32>,
    ) -> Result<String, GuiError> {
        let inspect = Arc::clone(&self.inspect_bundle);
        let login_home = target.login_home.clone();
        let bundle = blocking(move || inspect(&login_home)).await?;
        let version = bundle
            .daemon_version_output
            .trim()
            .strip_prefix("git-stratad ")
            .unwrap_or_default();
        let (Some(bundled), Some(running)) =
            (release_version(version), release_version(running_version))
        else {
            return Err(failure(
                Code::InvalidInput,
                "daemon_version_unreadable",
                "Could not verify the installed and running daemon versions",
            ));
        };
        if bundled < running {
            return Err(failure(
                Code::Unsupported,
                "older_bundled_daemon",
                format!(
                    "This app is older than the running daemon ({running_version}). Update the app first."
                ),
            ));
        }
        let live = status["store_schema"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok());
        if live.is_some_and(|schema| schema > bundle.store_schema) {
            return Err(failure(
                Code::Unsupported,
                "restart_schema_older",
                "The bundled daemon cannot open the current store schema",
            ));
        }
        if live.is_none_or(|schema| bundle.store_schema > schema)
            && acknowledgement != Some(bundle.store_schema)
        {
            return Err(GuiError::new(
                Code::NotConfirmed,
                "Confirm the store migration warning before restarting the daemon",
                json!({"reason": "schema_acknowledgement_required", "from_schema": live, "to_schema": bundle.store_schema}),
            ));
        }
        Ok(version.to_owned())
    }
}

// The updater offers stable releases. Compare the three release components
// numerically and refuse unrecognized versions before taking the drain.
fn release_version(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let mut number = || {
        let part = parts.next()?;
        if !part.bytes().all(|c| c.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        part.parse().ok()
    };
    let version = [number()?, number()?, number()?];
    parts.next().is_none().then_some(version)
}

struct RestartTarget {
    socket: PathBuf,
    login_home: PathBuf,
    program: PathBuf,
    service: Arc<dyn ServiceManager>,
}

impl RestartTarget {
    async fn rpc<T: Send + 'static>(
        &self,
        budget: Duration,
        call: impl FnOnce(&Path, Duration) -> Result<T, StrataError> + Send + 'static,
    ) -> Result<T, GuiError> {
        let socket = self.socket.clone();
        blocking(move || call(&socket, budget).map_err(GuiError::from)).await
    }

    async fn status(&self, budget: Duration) -> Result<Value, GuiError> {
        self.rpc(budget, |socket, budget| {
            Ok(strata_client::call_within(
                socket,
                "daemon_status",
                json!({"verify": false}),
                strata_client::Kind::Read,
                budget,
            )?)
        })
        .await
    }

    async fn release(&self, lease: String) -> Result<(), GuiError> {
        match self
            .rpc(RPC_TIMEOUT, move |socket, budget| {
                strata_client::drain_cancel(socket, &lease, budget)
            })
            .await
        {
            Err(error) if busy(&error, "lease_lost") => Ok(()),
            result => result,
        }
    }

    async fn verify_job(&self, pid: u32, require_respawn: bool) -> Result<(), GuiError> {
        let program = self.program.clone();
        let plist = plist_for_home(&self.login_home);
        let service = Arc::clone(&self.service);
        blocking(move || {
            if require_respawn && service.print_disabled(AGENT_LABEL)? {
                return Err(failure(
                    Code::Unsupported,
                    "agent_disabled",
                    "The daemon's launch agent is disabled",
                ));
            }
            let dump = service.print(AGENT_LABEL)?.ok_or_else(|| {
                failure(
                    Code::Busy,
                    "agent_unloaded",
                    "The daemon's launch agent is unloaded",
                )
            })?;
            let loaded_plist = scope::loaded_path(&dump).ok_or_else(|| {
                failure(
                    Code::InvalidInput,
                    "agent_path_unknown",
                    "Could not verify the daemon's launch agent path",
                )
            })?;
            if scope::anchored(Path::new(&loaded_plist)) != scope::anchored(&plist) {
                return Err(GuiError::foreign_agent(&loaded_plist, &plist));
            }
            let expected_program = program.canonicalize().ok();
            let loaded_program =
                job_field(&dump, "program").and_then(|path| Path::new(path).canonicalize().ok());
            if expected_program.is_none() || loaded_program != expected_program {
                return Err(failure(
                    Code::Unsupported,
                    "agent_program_mismatch",
                    "The launch agent does not run this app's bundled daemon",
                ));
            }
            if job_field(&dump, "pid").and_then(|value| value.parse().ok()) != Some(pid) {
                return Err(failure(
                    Code::Busy,
                    "daemon_pid_mismatch",
                    "The daemon answering the socket does not match the launch agent's PID",
                ));
            }
            if require_respawn && !has_restart_policy(&dump) {
                return Err(failure(
                    Code::Unsupported,
                    "agent_keepalive_mismatch",
                    "The loaded launch agent must have KeepAlive.SuccessfulExit set to false",
                ));
            }
            Ok(())
        })
        .await
    }
}

fn job_field<'a>(dump: &'a str, field: &str) -> Option<&'a str> {
    job_lines(dump).find_map(|(depth, line)| {
        let (key, value) = line.split_once(" = ")?;
        (depth == 1 && key == field && !value.ends_with('{')).then(|| value.trim())
    })
}

fn job_lines(dump: &str) -> impl Iterator<Item = (i32, &str)> {
    dump.lines().map(str::trim).scan(0, |depth, line| {
        if line.starts_with('}') {
            *depth -= 1;
        }
        let entry = (*depth, line);
        if line.ends_with('{') {
            *depth += 1;
        }
        Some(entry)
    })
}

pub(super) fn has_restart_policy(dump: &str) -> bool {
    // launchctl print reports the loaded SuccessfulExit=false condition
    // under semaphores. The plist on disk could have changed since bootstrap.
    let mut lines = job_lines(dump);
    lines.any(|(depth, line)| depth == 1 && line == "semaphores = {")
        && lines
            .take_while(|(depth, _)| *depth >= 2)
            .any(|(depth, line)| {
                depth == 2
                    && line.split_once("=>").is_some_and(|(condition, value)| {
                        condition.trim() == "successful exit" && value.trim() == "0"
                    })
            })
}

async fn wait_for_respawn(
    target: &RestartTarget,
    observation: &RestartObservation,
    clock: &dyn Clock,
    progress: &mut Progress<'_>,
) -> Result<(), GuiError> {
    let mut deadline = observation.deadline;
    let waiting = UpdateState::WaitingDaemon { since: clock.now() };
    let mut permission_since = None;
    let mut last_mismatch = None;
    progress(waiting.clone());
    loop {
        let left = deadline.saturating_duration_since(clock.monotonic());
        let budget = if permission_since.is_some() {
            RPC_TIMEOUT
        } else if left.is_zero() {
            break;
        } else {
            left.min(RPC_TIMEOUT)
        };
        let status = tokio::time::timeout(budget, target.status(budget))
            .await
            .unwrap_or_else(|_| Err(GuiError::timeout("daemon_status", budget.as_secs())));
        let now = clock.monotonic();
        if permission_since.is_none() && now >= deadline {
            break;
        }
        if status.as_ref().is_err_and(|error| {
            busy(error, "store_unavailable")
                && matches!(
                    error.detail["last_error"]["detail"]["errno"].as_i64(),
                    Some(1 | 4 | 13)
                )
        }) {
            if permission_since.is_none() {
                permission_since = Some(now);
                progress(UpdateState::WaitingPermission);
            }
            clock.sleep(POLL_INTERVAL).await;
            continue;
        }
        if let Some(since) = permission_since.take() {
            deadline += now.duration_since(since);
            progress(waiting.clone());
        }
        if clock.monotonic() >= deadline {
            break;
        }
        if let Ok(status) = &status
            && let Ok(candidate) = identity(status)
            && candidate.pid != observation.previous.pid
            && candidate.started > observation.previous.started
        {
            if candidate.version != observation.expected_version {
                last_mismatch = Some("daemon_version_mismatch");
            } else {
                let left = deadline.saturating_duration_since(clock.monotonic());
                match tokio::time::timeout(left, target.verify_job(candidate.pid, false)).await {
                    Ok(Ok(())) if clock.monotonic() < deadline => return Ok(()),
                    Ok(Err(error)) if error.reason() == Some("daemon_pid_mismatch") => {
                        last_mismatch = Some("daemon_pid_mismatch")
                    }
                    _ => {}
                }
            }
        }
        let left = deadline.saturating_duration_since(clock.monotonic());
        clock.sleep(left.min(POLL_INTERVAL)).await;
    }
    Err(GuiError::new(
        Code::Busy,
        "Could not verify a new daemon with the expected version within 150 seconds",
        json!({"reason": last_mismatch.unwrap_or("restart_timeout"), "expected_version": observation.expected_version}),
    ))
}

fn identity(status: &Value) -> Result<DaemonIdentity, GuiError> {
    DaemonIdentity::deserialize(status)
        .ok()
        .filter(|identity| identity.pid > 0)
        .ok_or_else(|| {
            failure(
                Code::InvalidInput,
                "daemon_identity_unknown",
                "The daemon did not report its PID, start time and version",
            )
        })
}

fn status_array<'a>(status: &'a Value, field: &str) -> Result<&'a [Value], GuiError> {
    status[field].as_array().map(Vec::as_slice).ok_or_else(|| {
        failure(
            Code::InvalidInput,
            "invalid_drain_status",
            format!("The daemon did not report {field} for the drain"),
        )
    })
}

fn busy(error: &GuiError, reason: &str) -> bool {
    error.code == Code::Busy.as_str() && error.reason() == Some(reason)
}

fn failure(code: Code, reason: &str, message: impl Into<String>) -> GuiError {
    GuiError::new(code, message, json!({"reason": reason}))
}

async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, GuiError> + Send + 'static,
) -> Result<T, GuiError> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| GuiError::internal(format!("daemon restart worker failed: {error}")))?
}

pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

pub async fn command(
    window: &WebviewWindow,
    acknowledgement: Option<u32>,
    progress: Channel<Value>,
) -> Result<(), GuiError> {
    let controller = window.try_state::<RestartController>().ok_or_else(|| {
        failure(
            Code::Unsupported,
            "bundle_inspector_unavailable",
            "The installed bundle inspector is unavailable",
        )
    })?;
    let socket = window
        .state::<AppState>()
        .socket()
        .ok_or_else(GuiError::no_home)?;
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(GuiError::no_home)?;
    let login_home = scope::lifecycle_scope(Path::new(&home), scope::login_home().as_deref())?;
    let program = PathBuf::from(Operation::DaemonVersion.argv().0);
    let app_executable = program
        .with_file_name("git-strata-desktop")
        .canonicalize()
        .ok();
    let running = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    if app_executable.is_none() || running != app_executable {
        return Err(failure(
            Code::Unsupported,
            "installed_bundle_required",
            "Open the installed GIT STRATA app to restart its daemon",
        ));
    }
    let target = RestartTarget {
        socket: PathBuf::from(socket),
        login_home,
        program,
        service: Arc::new(Launchctl::new()),
    };
    controller
        .run(&target, acknowledgement, &SystemClock, &mut |state| {
            let view = window.state::<super::state::Presentation>();
            let _ = progress.send(view.wire_progress(state));
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;

    use super::*;

    const OLD_PID: u32 = 41;
    const NEW_PID: u32 = 42;
    const OLD_VERSION: &str = "0.3.10";
    const BUNDLE_VERSION: &str = "0.3.11";
    const RESTART_POLICY: &str = "\tsemaphores = {\n\t\tsuccessful exit => 0\n\t}\n";

    struct FakeClock {
        start: Instant,
        elapsed: AtomicU64,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: AtomicU64::new(0),
            }
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + (self.monotonic() - self.start)
        }
        fn monotonic(&self) -> Instant {
            self.start + Duration::from_secs(self.elapsed.load(Ordering::Relaxed))
        }
        fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
            Box::pin(async move {
                self.elapsed
                    .fetch_add(duration.as_secs(), Ordering::Relaxed);
            })
        }
    }

    struct FakeService {
        pids: Mutex<VecDeque<u32>>,
        disabled: Mutex<VecDeque<bool>>,
        policy: Mutex<&'static str>,
        plist: PathBuf,
        program: PathBuf,
    }

    impl ServiceManager for FakeService {
        fn print(&self, label: &str) -> Result<Option<String>, GuiError> {
            assert_eq!(label, AGENT_LABEL);
            let pid = self
                .pids
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected launch agent probe");
            let exit = if pid == OLD_PID { 0 } else { 75 };
            let policy = *self.policy.lock().unwrap();
            Ok(Some(format!(
                "gui/501/{AGENT_LABEL} = {{\n\tpath = {}\n\tprogram = {}\n\tpid = {pid}\n\tlast exit code = {exit}\n{policy}\tproperties = runatload\n}}",
                self.plist.display(),
                self.program.display(),
            )))
        }

        fn print_disabled(&self, label: &str) -> Result<bool, GuiError> {
            assert_eq!(label, AGENT_LABEL);
            Ok(self
                .disabled
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected disabled-state probe"))
        }

        fn kickstart(&self, _: &str) -> Result<(), GuiError> {
            panic!("restart must leave the spawn to launchd")
        }
        fn bootstrap(&self, _: &Path) -> Result<(), GuiError> {
            panic!("restart must leave the spawn to launchd")
        }
    }

    enum FakeReply {
        Envelope(Value),
        Raw(&'static str),
        Disconnect,
    }

    struct Exchange {
        method: &'static str,
        params: Value,
        reply: FakeReply,
        follow_up: Option<Box<Exchange>>,
    }

    impl Exchange {
        fn status(data: Value) -> Self {
            Self {
                method: "daemon_status",
                params: json!({"verify":false}),
                reply: FakeReply::Envelope(ok(data)),
                follow_up: None,
            }
        }

        fn disconnected_status() -> Self {
            Self {
                method: "daemon_status",
                params: json!({"verify":false}),
                reply: FakeReply::Disconnect,
                follow_up: None,
            }
        }

        fn drain(params: Value, reply: Option<Value>) -> Self {
            Self {
                method: "drain",
                params,
                reply: reply.map_or(FakeReply::Disconnect, FakeReply::Envelope),
                follow_up: None,
            }
        }

        fn followed_by(mut self, exchange: Self) -> Self {
            self.follow_up = Some(Box::new(exchange));
            self
        }
    }

    struct FakeSocket {
        dir: tempfile::TempDir,
        socket: PathBuf,
        finished: Arc<AtomicBool>,
        thread: thread::JoinHandle<()>,
    }

    impl FakeSocket {
        fn new(script: Vec<Exchange>) -> Self {
            Self::with_request_hook(script, |_| {})
        }

        fn with_request_hook(
            script: Vec<Exchange>,
            mut on_request: impl FnMut(&Value) + Send + 'static,
        ) -> Self {
            let dir = tempfile::tempdir_in("/tmp").unwrap();
            let socket = dir.path().join("daemon.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let finished = Arc::new(AtomicBool::new(false));
            let done = finished.clone();
            let thread = thread::spawn(move || {
                let mut script = VecDeque::from(script);
                while !done.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            let mut reader = BufReader::new(&stream);
                            let mut writer = &stream;
                            let mut exchange = script.pop_front().expect("unexpected connection");
                            loop {
                                let mut line = String::new();
                                reader.read_line(&mut line).unwrap();
                                assert!(
                                    !line.is_empty(),
                                    "expected {} on this connection",
                                    exchange.method
                                );
                                let request: Value = serde_json::from_str(&line).unwrap();
                                assert_eq!(request["method"], exchange.method);
                                assert_eq!(request["params"], exchange.params);
                                on_request(&request);
                                match exchange.reply {
                                    FakeReply::Envelope(mut reply) => {
                                        reply
                                            .as_object_mut()
                                            .unwrap()
                                            .entry("id")
                                            .or_insert_with(|| request["id"].clone());
                                        writeln!(writer, "{reply}").unwrap();
                                    }
                                    FakeReply::Raw(line) => writeln!(writer, "{line}").unwrap(),
                                    FakeReply::Disconnect => break,
                                }
                                let Some(follow_up) = exchange.follow_up else {
                                    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
                                    break;
                                };
                                exchange = *follow_up;
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("fake daemon accept: {error}"),
                    }
                }
                assert!(
                    script.is_empty(),
                    "{} fake daemon requests did not arrive",
                    script.len()
                );
            });
            Self {
                dir,
                socket,
                finished,
                thread,
            }
        }

        fn target(&self, pids: &[u32], disabled: &[bool]) -> (RestartTarget, Arc<FakeService>) {
            let login_home = self.dir.path().to_path_buf();
            let program = login_home
                .join("GIT STRATA.app")
                .join("Contents/MacOS/git-stratad");
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(&program, b"fixture only").unwrap();
            let service = Arc::new(FakeService {
                pids: Mutex::new(pids.iter().copied().collect()),
                disabled: Mutex::new(disabled.iter().copied().collect()),
                policy: Mutex::new(RESTART_POLICY),
                plist: plist_for_home(&login_home),
                program: program.clone(),
            });
            let target = RestartTarget {
                socket: self.socket.clone(),
                login_home,
                program,
                service: service.clone(),
            };
            (target, service)
        }

        fn finish(self, service: &FakeService) {
            self.finished.store(true, Ordering::Relaxed);
            self.thread.join().unwrap();
            assert!(service.pids.lock().unwrap().is_empty());
            assert!(service.disabled.lock().unwrap().is_empty());
        }
    }

    fn ok(data: Value) -> Value {
        json!({"ok": true, "data": data, "error": null})
    }

    fn refused(reason: &str) -> Value {
        json!({"ok":false,"data":{},"error":{"code":"Busy","message":reason,"detail":{"reason":reason}}})
    }

    fn status(pid: u32, version: &str, lease: Option<&str>) -> Value {
        let started = if pid == OLD_PID {
            "2026-11-17T17:00:00Z"
        } else {
            "2026-11-17T17:00:01Z"
        };
        json!({
            "pid": pid, "version": version, "started": started, "store_schema": 7,
            "features": [DRAIN_FEATURE], "holds": [], "inflight": [],
            "draining": lease.map(|id| json!({"lease_id":id, "sealed":false, "then":null})),
        })
    }

    fn old(lease: Option<&str>) -> Exchange {
        Exchange::status(status(OLD_PID, OLD_VERSION, lease))
    }

    fn permission_status() -> Exchange {
        let mut permission = refused("store_unavailable");
        permission["error"]["detail"]["last_error"] = json!({"detail": {"errno": 13}});
        Exchange {
            reply: FakeReply::Envelope(permission),
            ..Exchange::disconnected_status()
        }
    }

    fn acquire(id: &str) -> Exchange {
        old(None).followed_by(Exchange::drain(
            json!({"on":true,"ttl_s":120}),
            Some(ok(json!({"lease_id":id}))),
        ))
    }

    fn seal(id: &str, reply: Option<Value>) -> Exchange {
        Exchange::drain(json!({"seal":true,"lease_id":id,"then":"restart"}), reply)
    }

    fn cancel(id: &str) -> Exchange {
        Exchange::drain(json!({"on":false,"lease_id":id}), Some(ok(json!({}))))
    }

    fn bundle(version: &'static str, schema: u32) -> RestartController {
        RestartController::new(move |_| {
            Ok(RestartBundle {
                daemon_version_output: format!("git-stratad {version}\n"),
                store_schema: schema,
            })
        })
    }

    #[tokio::test]
    async fn brew_replaced_bundle_restarts_only_after_verified_drain() {
        let socket = FakeSocket::new(vec![
            old(None),
            acquire("brew-restart"),
            old(Some("brew-restart")),
            seal("brew-restart", Some(ok(json!({})))),
            Exchange::status(status(NEW_PID, BUNDLE_VERSION, None)),
        ]);
        let (target, service) = socket.target(&[OLD_PID, OLD_PID, NEW_PID], &[false; 2]);
        let mut events = Vec::new();
        bundle(BUNDLE_VERSION, 7)
            .run(&target, None, &FakeClock::new(), &mut |state| {
                events.push(state)
            })
            .await
            .unwrap();
        socket.finish(&service);
        assert!(events.contains(&UpdateState::Stopping));
        assert_eq!(events.last(), Some(&UpdateState::Done));
    }

    #[tokio::test]
    async fn busy_drain_shows_work_renews_at_thirty_seconds_and_cancels() {
        let mut busy = status(OLD_PID, OLD_VERSION, Some("first"));
        let confirm = json!({"method":"publish", "phase":"confirming", "repo":"tool", "started_at":"2026-11-17T17:00:00Z"});
        let background = json!({"op":"catch_up", "origin":"background"});
        busy["inflight"] = json!([confirm]);
        busy["holds"] = json!([background, {"origin":"request"}]);
        let mut background_only = busy.clone();
        background_only["inflight"] = json!([]);
        let socket = FakeSocket::new(vec![
            old(None),
            acquire("first"),
            Exchange::status(busy.clone()),
            Exchange::status(busy),
            Exchange::drain(
                json!({"on":true,"ttl_s":120,"lease_id":"first"}),
                Some(ok(json!({"lease_id":"first"}))),
            ),
            Exchange::status(background_only),
            cancel("first"),
        ]);
        let (target, service) = socket.target(&[OLD_PID], &[false]);
        let clock = FakeClock::new();
        let controller = bundle(BUNDLE_VERSION, 7);
        let mut events = Vec::new();
        controller
            .run(&target, None, &clock, &mut |state| {
                match &state {
                    UpdateState::Draining { inflight, .. } if !inflight.is_empty() => {
                        clock.elapsed.store(29, Ordering::Relaxed);
                    }
                    UpdateState::Draining { .. } => {
                        assert_eq!(clock.elapsed.load(Ordering::Relaxed), 30);
                        assert!(controller.request_cancel());
                    }
                    _ => {}
                }
                events.push(state);
            })
            .await
            .unwrap();
        socket.finish(&service);
        assert_eq!(
            events,
            [
                UpdateState::Draining {
                    inflight: vec![confirm],
                    background: vec![background.clone()]
                },
                UpdateState::Draining {
                    inflight: vec![],
                    background: vec![background]
                },
                UpdateState::Cancelled,
            ]
        );
        assert!(!controller.request_cancel());
    }

    #[tokio::test]
    async fn retries_lease_loss_and_not_idle_then_verifies_respawn() {
        let mut before_respawn = status(45, BUNDLE_VERSION, None);
        before_respawn["started"] = json!("2026-11-17T16:00:00Z");
        let script = vec![
            old(None),
            acquire("first"),
            old(Some("first")),
            seal("first", Some(refused("lease_lost"))),
            old(None),
            acquire("second"),
            old(Some("second")),
            seal("second", Some(refused("not_idle"))),
            old(Some("second")),
            seal("second", Some(ok(json!({})))),
            Exchange::disconnected_status(),
            old(None),
            Exchange::status(before_respawn),
            Exchange::status(status(43, BUNDLE_VERSION, None)),
            Exchange::status(status(NEW_PID, BUNDLE_VERSION, None)),
        ];
        let stopping = Arc::new(AtomicU64::new(0));
        let observed_stopping = stopping.clone();
        let mut seals = 0;
        let socket = FakeSocket::with_request_hook(script, move |request| {
            if request["params"]["seal"] == true {
                seals += 1;
                assert_eq!(observed_stopping.load(Ordering::SeqCst), seals);
            }
        });
        let (target, service) = socket.target(
            &[OLD_PID, OLD_PID, OLD_PID, OLD_PID, NEW_PID, NEW_PID],
            &[false; 4],
        );
        let clock = FakeClock::new();
        let controller = bundle(BUNDLE_VERSION, 7);
        let mut events = Vec::new();
        controller
            .run(&target, None, &clock, &mut |state| {
                if state == UpdateState::Stopping {
                    stopping.fetch_add(1, Ordering::SeqCst);
                    assert!(!controller.request_cancel());
                }
                events.push(state);
            })
            .await
            .unwrap();
        socket.finish(&service);
        assert!(matches!(
            events.as_slice(),
            [
                UpdateState::Draining { .. },
                UpdateState::Stopping,
                UpdateState::Draining { .. },
                UpdateState::Stopping,
                UpdateState::Draining { .. },
                UpdateState::Stopping,
                UpdateState::WaitingDaemon { .. },
                UpdateState::Done
            ]
        ));
    }

    #[tokio::test]
    async fn refuses_older_bundle_or_unacknowledged_migration_before_acquire() {
        for (version, schema, reason) in [
            ("0.3.9", 7, "older_bundled_daemon"),
            (BUNDLE_VERSION, 8, "schema_acknowledgement_required"),
        ] {
            let socket = FakeSocket::new(vec![old(None)]);
            let (target, service) = socket.target(&[OLD_PID], &[false]);
            let mut events = Vec::new();
            let error = bundle(version, schema)
                .run(&target, None, &FakeClock::new(), &mut |state| {
                    events.push(state)
                })
                .await
                .unwrap_err();
            socket.finish(&service);
            assert_eq!(error.reason(), Some(reason));
            assert!(events.is_empty(), "preflight refusal emitted {events:?}");
        }
    }

    #[tokio::test]
    async fn refuses_unsupported_daemon_and_disabled_label() {
        let mut unsupported = status(OLD_PID, OLD_VERSION, None);
        unsupported["features"] = json!(["daemon_status.activity"]);
        for (exchange, disabled, reason) in [
            (Exchange::status(unsupported), &[][..], "daemon_too_old"),
            (old(None), &[true][..], "agent_disabled"),
        ] {
            let socket = FakeSocket::new(vec![exchange]);
            let (target, service) = socket.target(&[], disabled);
            let error = bundle(BUNDLE_VERSION, 7)
                .run(&target, None, &FakeClock::new(), &mut |_| {})
                .await
                .unwrap_err();
            socket.finish(&service);
            assert_eq!(error.reason(), Some(reason));
        }
    }

    #[tokio::test]
    async fn checks_identity_and_existing_lease_on_the_acquisition_connection() {
        for (peer, reason) in [
            (
                status(NEW_PID, OLD_VERSION, None),
                "daemon_identity_changed",
            ),
            (status(OLD_PID, OLD_VERSION, Some("foreign")), "draining"),
        ] {
            let socket = FakeSocket::new(vec![old(None), Exchange::status(peer)]);
            let (target, service) = socket.target(&[OLD_PID], &[false]);
            let mut events = Vec::new();
            let error = bundle(BUNDLE_VERSION, 7)
                .run(&target, None, &FakeClock::new(), &mut |state| {
                    events.push(state)
                })
                .await
                .unwrap_err();
            socket.finish(&service);
            assert_eq!(error.reason(), Some(reason));
            assert!(events.is_empty());
        }
    }

    #[tokio::test]
    async fn rechecks_disabled_label_and_releases_lease_before_seal() {
        let socket = FakeSocket::new(vec![
            old(None),
            acquire("first"),
            old(Some("first")),
            cancel("first"),
        ]);
        let (target, service) = socket.target(&[OLD_PID], &[false, true]);
        let error = bundle(BUNDLE_VERSION, 7)
            .run(&target, None, &FakeClock::new(), &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.reason(), Some("agent_disabled"), "{error:?}");
        socket.finish(&service);
    }

    #[tokio::test]
    async fn requires_loaded_restart_policy_before_draining_and_again_before_sealing() {
        for (before_seal, policy) in [
            (false, ""),
            (false, "\tproperties = keepalive\n"),
            (
                false,
                "\tenvironment = {\n\t\tsemaphores = {\n\t\t\tsuccessful exit => 0\n\t\t}\n\t}\n",
            ),
            (true, "\tsemaphores = {\n\t\tsuccessful exit => 1\n\t}\n"),
        ] {
            let mut script = vec![old(None)];
            if before_seal {
                script.extend([acquire("first"), old(Some("first")), cancel("first")]);
            }
            let socket = FakeSocket::new(script);
            let probes = 1 + usize::from(before_seal);
            let (target, service) = socket.target(&[OLD_PID; 2][..probes], &[false; 2][..probes]);
            if !before_seal {
                *service.policy.lock().unwrap() = policy;
            }
            let mut events = Vec::new();
            let error = bundle(BUNDLE_VERSION, 7)
                .run(&target, None, &FakeClock::new(), &mut |state| {
                    if matches!(&state, UpdateState::Draining { .. }) {
                        *service.policy.lock().unwrap() = policy;
                    }
                    events.push(state);
                })
                .await
                .unwrap_err();
            socket.finish(&service);
            assert_eq!(error.reason(), Some("agent_keepalive_mismatch"));
            if before_seal {
                assert!(matches!(
                    events.as_slice(),
                    [UpdateState::Draining { .. }, UpdateState::Idle]
                ));
            } else {
                assert!(events.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn cancel_during_unacknowledged_acquisition_preserves_unknown() {
        let controller = Arc::new(bundle(BUNDLE_VERSION, 7));
        let cancel_controller = controller.clone();
        let socket = FakeSocket::with_request_hook(
            vec![
                old(None),
                old(None).followed_by(Exchange::drain(json!({"on":true,"ttl_s":120}), None)),
            ],
            move |request| {
                if request["method"] == "drain" {
                    assert!(cancel_controller.request_cancel());
                }
            },
        );
        let (target, service) = socket.target(&[OLD_PID], &[false]);
        let mut events = Vec::new();
        let error = controller
            .run(&target, None, &FakeClock::new(), &mut |state| {
                events.push(state)
            })
            .await
            .unwrap_err();
        socket.finish(&service);
        assert_eq!(error.code, Code::Unknown.as_str());
        assert_eq!(error.reason(), Some("drain_release_unconfirmed"));
        assert_eq!(
            error.detail["acquire_error"]["detail"]["reason"],
            "socket_closed"
        );
        assert!(matches!(events.as_slice(), [UpdateState::Draining { .. }]));
    }

    #[tokio::test]
    async fn lost_replacement_lease_reply_keeps_cleanup_uncertain_without_release() {
        let socket = FakeSocket::new(vec![
            old(None),
            acquire("first"),
            old(Some("first")),
            seal("first", Some(refused("lease_lost"))),
            old(None),
            old(None).followed_by(Exchange::drain(json!({"on":true,"ttl_s":120}), None)),
        ]);
        let (target, service) = socket.target(&[OLD_PID, OLD_PID], &[false; 2]);
        let mut events = Vec::new();
        let error = bundle(BUNDLE_VERSION, 7)
            .run(&target, None, &FakeClock::new(), &mut |state| {
                events.push(state)
            })
            .await
            .unwrap_err();
        // The fake socket would reject any release or additional acquire.
        socket.finish(&service);
        assert_eq!(error.code, Code::Unknown.as_str());
        assert_eq!(error.reason(), Some("drain_release_unconfirmed"));
        assert_eq!(
            error.detail["acquire_error"]["detail"]["reason"],
            "socket_closed"
        );
        assert!(matches!(events.last(), Some(UpdateState::Draining { .. })));
        assert!(!events.contains(&UpdateState::Idle));
    }

    #[tokio::test]
    async fn cancel_rejects_a_success_reply_without_a_data_object() {
        for reply in [
            json!({"ok": true}),
            json!({"ok": true, "data": null}),
            json!({"ok": true, "data": []}),
        ] {
            let socket = FakeSocket::new(vec![
                old(None),
                acquire("first"),
                old(Some("first")),
                Exchange {
                    reply: FakeReply::Envelope(reply),
                    ..cancel("first")
                },
            ]);
            let (target, service) = socket.target(&[OLD_PID], &[false]);
            let controller = bundle(BUNDLE_VERSION, 7);
            let mut events = Vec::new();
            let result = controller
                .run(&target, None, &FakeClock::new(), &mut |state| {
                    if matches!(state, UpdateState::Draining { .. }) {
                        assert!(controller.request_cancel());
                    }
                    events.push(state);
                })
                .await;
            socket.finish(&service);
            let error = result.unwrap_err();
            assert_eq!(error.code, Code::Unknown.as_str());
            assert_eq!(error.reason(), Some("drain_release_unconfirmed"));
            assert_eq!(
                error.detail["release_error"]["detail"]["reason"],
                "rpc_failed"
            );
            assert!(matches!(events.as_slice(), [UpdateState::Draining { .. }]));
        }
    }

    #[tokio::test]
    async fn a_failed_status_poll_releases_the_lease_even_if_cancel_arrives_during_the_poll() {
        for cancel_during_poll in [false, true] {
            let controller = Arc::new(bundle(BUNDLE_VERSION, 7));
            let cancel_controller = controller.clone();
            let mut polls = 0;
            let socket = FakeSocket::with_request_hook(
                vec![
                    old(None),
                    acquire("first"),
                    Exchange::disconnected_status(),
                    cancel("first"),
                ],
                move |request| {
                    if request["method"] == "daemon_status" {
                        polls += 1;
                        if polls == 3 && cancel_during_poll {
                            assert!(cancel_controller.request_cancel());
                        }
                    }
                },
            );
            let (target, service) = socket.target(&[OLD_PID], &[false]);
            let mut events = Vec::new();
            let result = controller
                .run(&target, None, &FakeClock::new(), &mut |state| {
                    events.push(state)
                })
                .await;
            socket.finish(&service);
            if cancel_during_poll {
                result.unwrap();
                assert_eq!(events, [UpdateState::Cancelled]);
            } else {
                assert_eq!(result.unwrap_err().reason(), Some("daemon_down"));
                assert!(events.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn definite_seal_refusal_preserves_refusal_and_reports_unconfirmed_release() {
        let release = |reply| Exchange::drain(json!({"on":false,"lease_id":"first"}), reply);
        for (reply, refusal_reason) in [
            (refused("seal_refused"), "seal_refused"),
            (
                json!({"ok": false, "error": {"rpc": "Failed", "message": "invalid params"}}),
                "rpc_failed",
            ),
        ] {
            for (releases, release_error_reason) in [
                (vec![cancel("first")], None),
                (
                    vec![release(None), release(Some(refused("lease_lost")))],
                    None,
                ),
                (vec![release(None), release(None)], Some("socket_closed")),
                (
                    vec![release(Some(refused("release_failed")))],
                    Some("release_failed"),
                ),
            ] {
                let mut script = vec![
                    old(None),
                    acquire("first"),
                    old(Some("first")),
                    seal("first", Some(reply.clone())),
                ];
                script.extend(releases);
                let socket = FakeSocket::new(script);
                let (target, service) = socket.target(&[OLD_PID, OLD_PID], &[false; 2]);
                let mut events = Vec::new();
                let result = bundle(BUNDLE_VERSION, 7)
                    .run(&target, None, &FakeClock::new(), &mut |state| {
                        events.push(state)
                    })
                    .await;
                socket.finish(&service);
                let error = result.unwrap_err();
                let [
                    UpdateState::Draining { .. },
                    UpdateState::Stopping,
                    terminal,
                ] = events.as_slice()
                else {
                    panic!("unexpected progress: {events:?}");
                };
                if let Some(reason) = release_error_reason {
                    assert_eq!(error.code, Code::Unknown.as_str());
                    assert_eq!(error.reason(), Some("drain_release_unconfirmed"));
                    assert_eq!(error.detail["refusal"]["detail"]["reason"], refusal_reason);
                    assert_eq!(error.detail["release_error"]["detail"]["reason"], reason);
                    assert!(matches!(terminal, UpdateState::Draining { .. }));
                } else {
                    assert_eq!(error.reason(), Some(refusal_reason));
                    assert_eq!(*terminal, UpdateState::Idle);
                }
            }
        }
    }

    #[tokio::test]
    async fn releases_lease_if_bundle_is_replaced_with_an_older_version_during_drain() {
        let socket = FakeSocket::new(vec![
            old(None),
            acquire("first"),
            old(Some("first")),
            cancel("first"),
        ]);
        let (target, service) = socket.target(&[OLD_PID], &[false]);
        let versions = Mutex::new([BUNDLE_VERSION, "0.3.9"].into_iter());
        let controller = RestartController::new(move |_| {
            let version = versions.lock().unwrap().next().unwrap();
            Ok(RestartBundle {
                daemon_version_output: format!("git-stratad {version}"),
                store_schema: 7,
            })
        });
        let error = controller
            .run(&target, None, &FakeClock::new(), &mut |_| {})
            .await
            .unwrap_err();
        socket.finish(&service);
        assert_eq!(error.reason(), Some("older_bundled_daemon"));
    }

    #[tokio::test]
    async fn unverified_seal_reply_observes_respawn_without_replaying_or_releasing() {
        let mut wrong_id = ok(json!({}));
        wrong_id["id"] = json!("unrelated");
        let mut missing_ok = refused("not_idle");
        missing_ok.as_object_mut().unwrap().remove("ok");
        let mut non_boolean_ok = refused("lease_lost");
        non_boolean_ok["ok"] = json!("false");
        for reply in [
            FakeReply::Disconnect,
            FakeReply::Raw("{malformed json}"),
            FakeReply::Envelope(wrong_id),
            FakeReply::Envelope(json!({"ok": false, "error": {"rpc": "Unrecognized"}})),
            FakeReply::Envelope(missing_ok),
            FakeReply::Envelope(non_boolean_ok),
        ] {
            let socket = FakeSocket::new(vec![
                old(None),
                acquire("first"),
                old(Some("first")),
                Exchange {
                    reply,
                    ..seal("first", None)
                },
                Exchange::disconnected_status(),
                Exchange::status(status(NEW_PID, BUNDLE_VERSION, None)),
            ]);
            let (target, service) = socket.target(&[OLD_PID, OLD_PID, NEW_PID], &[false; 2]);
            let mut events = Vec::new();
            bundle(BUNDLE_VERSION, 7)
                .run(&target, None, &FakeClock::new(), &mut |state| {
                    events.push(state)
                })
                .await
                .unwrap();
            socket.finish(&service);
            assert_eq!(events.last(), Some(&UpdateState::Done));
        }
    }

    #[tokio::test]
    async fn seal_acknowledgement_time_counts_towards_restart_budget() {
        for reply in [Some(ok(json!({}))), None] {
            let acknowledgement_lost = reply.is_none();
            let clock = Arc::new(FakeClock::new());
            let socket_clock = clock.clone();
            let mut script = vec![
                old(None),
                acquire("first"),
                old(Some("first")),
                seal("first", reply),
            ];
            script.extend((0..145).map(|_| old(None)));
            let socket = FakeSocket::with_request_hook(script, move |request| {
                if request["params"]["seal"] == true {
                    socket_clock.elapsed.fetch_add(5, Ordering::Relaxed);
                }
            });
            let (target, service) = socket.target(&[OLD_PID, OLD_PID], &[false; 2]);
            let mut events = Vec::new();
            let error = bundle(BUNDLE_VERSION, 7)
                .run(&target, None, &*clock, &mut |state| events.push(state))
                .await
                .unwrap_err();
            socket.finish(&service);
            assert_eq!(clock.elapsed.load(Ordering::Relaxed), 150);
            assert_eq!(
                error.reason(),
                Some(if acknowledgement_lost {
                    "socket_closed"
                } else {
                    "restart_timeout"
                })
            );
            assert_eq!(
                events.last(),
                Some(&UpdateState::DaemonFailed {
                    reason: "restart_timeout".into()
                })
            );
        }
    }

    #[tokio::test]
    async fn permission_can_outlast_restart_budget_before_the_daemon_answers() {
        let socket = FakeSocket::new(vec![
            old(None),
            acquire("first"),
            old(Some("first")),
            seal("first", Some(ok(json!({})))),
            permission_status(),
            permission_status(),
            Exchange::status(status(NEW_PID, BUNDLE_VERSION, None)),
        ]);
        let (target, service) = socket.target(&[OLD_PID, OLD_PID, NEW_PID], &[false; 2]);
        let clock = FakeClock::new();
        let mut events = Vec::new();
        bundle(BUNDLE_VERSION, 7)
            .run(&target, None, &clock, &mut |state| {
                if state == UpdateState::WaitingPermission {
                    clock.elapsed.fetch_add(175, Ordering::Relaxed);
                }
                events.push(state);
            })
            .await
            .unwrap();
        socket.finish(&service);
        assert_eq!(clock.elapsed.load(Ordering::Relaxed), 177);
        assert!(matches!(
            events.as_slice(),
            [
                UpdateState::Draining { .. },
                UpdateState::Stopping,
                UpdateState::WaitingDaemon { .. },
                UpdateState::WaitingPermission,
                UpdateState::WaitingDaemon { .. },
                UpdateState::Done,
            ]
        ));
    }

    #[tokio::test]
    async fn wrong_version_times_out_using_the_remaining_budget_after_permission() {
        let mut script = vec![
            old(None),
            acquire("first"),
            old(Some("first")),
            seal("first", Some(ok(json!({})))),
            permission_status(),
        ];
        script.extend((0..30).map(|_| Exchange::status(status(NEW_PID, OLD_VERSION, None))));
        let socket = FakeSocket::new(script);
        let (target, service) = socket.target(&[OLD_PID, OLD_PID], &[false; 2]);
        let clock = FakeClock::new();
        let mut events = Vec::new();
        let error = bundle(BUNDLE_VERSION, 7)
            .run(&target, None, &clock, &mut |state| {
                if matches!(state, UpdateState::WaitingDaemon { .. })
                    && clock.elapsed.load(Ordering::Relaxed) == 0
                {
                    clock.elapsed.store(120, Ordering::Relaxed);
                }
                events.push(state);
            })
            .await
            .unwrap_err();
        socket.finish(&service);
        assert_eq!(error.reason(), Some("daemon_version_mismatch"));
        assert_eq!(clock.elapsed.load(Ordering::Relaxed), 151);
        assert!(events.contains(&UpdateState::WaitingPermission));
        assert_eq!(
            events.last(),
            Some(&UpdateState::DaemonFailed {
                reason: "daemon_version_mismatch".into()
            })
        );
    }
}

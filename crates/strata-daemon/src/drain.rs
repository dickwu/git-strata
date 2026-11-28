//! Admission fence and lease for a graceful daemon shutdown.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::{Value, json};
use strata_types::{Code, RepoName, SessionId, StrataError, serde_rfc3339};

use crate::Result;
use crate::rpc::{DrainParams, DrainThen, Params, Request};

#[path = "dispatcher.rs"]
mod dispatcher;

const DEFAULT_TTL_S: u32 = 120;
const MAX_TTL_S: u32 = 600;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Queued,
    Confirming,
    Locked,
    Running,
}

#[derive(Clone, Serialize)]
struct Inflight {
    method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<RepoName>,
    phase: Phase,
    #[serde(with = "serde_rfc3339")]
    started_at: SystemTime,
    // A legacy lease permits its own controls, sync checks and dry-run GC.
    // Handover still counts these methods from dispatch to return.
    #[serde(skip)]
    blocks_lease: bool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShutdownMode {
    Stop,
    Restart,
    Hold,
    Sigterm,
}

impl ShutdownMode {
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Self::Restart => 75,
            Self::Stop | Self::Hold | Self::Sigterm => 0,
        }
    }
}

struct Lease {
    id: String,
    // The wall clock includes time asleep on macOS. The monotonic
    // deadline also bounds the lease if the wall clock moves backwards.
    deadline: Instant,
    expires_at: SystemTime,
    then: Option<DrainThen>,
}

impl Lease {
    fn renew(&mut self, ttl: Duration) {
        self.deadline = Instant::now() + ttl;
        self.expires_at = SystemTime::now() + ttl;
    }

    fn expired(&self) -> bool {
        self.then.is_none()
            && (Instant::now() >= self.deadline || SystemTime::now() >= self.expires_at)
    }

    fn status(&self) -> Value {
        json!({
            "lease_id": self.id,
            "expires_at": self.then.is_none().then(|| serde_rfc3339::format(&self.expires_at).ok()).flatten(),
            "sealed": self.then.is_some(),
            "then": self.then,
        })
    }
}

#[derive(Default)]
struct State {
    lease: Option<Lease>,
    parking: Option<dispatcher::Parking>,
    connections: BTreeMap<u64, UnixStream>,
    inflight: BTreeMap<u64, Inflight>,
    next: u64,
    // A pass counts from admission through completion, including periods
    // between lock acquisitions or while a child process runs.
    background: usize,
    closing: bool,
    terminal_upgrade: Option<StrataError>,
    shutdown: Option<ShutdownMode>,
    // Generation-bound streams still refusing frames. Terminal stop waits for
    // them before it closes read halves (CR r2 #3).
    refusing: usize,
}

impl State {
    fn expire(&mut self) {
        if self.lease.as_ref().is_some_and(Lease::expired) {
            self.lease = None;
        }
    }
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    replay: Condvar,
}

/// Every admission and seal uses the same mutex. Guards never hold it
/// while a request or scheduler runs.
#[derive(Clone, Default)]
pub(crate) struct Drain {
    inner: Arc<Inner>,
}

impl Drain {
    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn admit(
        &self,
        method: &'static str,
        session: Option<SessionId>,
        repo: Option<RepoName>,
    ) -> Result<Write> {
        #[cfg(any(test, feature = "test-seams"))]
        crate::rpc::admission_audit::check();
        let mut state = self.state();
        state.expire();
        if let Some(parking) = state.parking.as_ref().filter(|p| p.holding()) {
            return Err(parking.refusal());
        }
        self.admit_locked(&mut state, method, session, repo, true)
    }

    fn admit_locked(
        &self,
        state: &mut State,
        method: &'static str,
        session: Option<SessionId>,
        repo: Option<RepoName>,
        blocks_lease: bool,
    ) -> Result<Write> {
        if blocks_lease && state.lease.is_some() {
            return Err(draining());
        }
        if blocks_lease && state.closing {
            return Err(shutting_down());
        }
        let id = state.next;
        state.next += 1;
        state.inflight.insert(
            id,
            Inflight {
                method,
                session,
                repo,
                phase: Phase::Running,
                started_at: SystemTime::now(),
                blocks_lease,
            },
        );
        Ok(Write {
            drain: self.clone(),
            id,
            blocks_lease,
        })
    }

    // The connection dispatcher calls this while still holding the mutex
    // that decided whether to park. Direct handler callers have no stream
    // to retain and receive the same pre-dispatch refusal as overflow.
    fn admit_request_locked(&self, state: &mut State, req: &Request) -> Result<Option<Write>> {
        if !req.method.may_admit() {
            return Ok(None);
        }
        if let Some(error) = &state.terminal_upgrade {
            return Err(error.clone());
        }
        if let Some(parking) = state.parking.as_ref().filter(|p| p.holding()) {
            return Err(parking.refusal());
        }
        state.expire();
        let blocks_lease = match &req.params {
            Params::Drain(_) => false,
            Params::Sync(p) => !p.check,
            Params::Gc(p) => !p.dry_run,
            Params::PublishPlan(_) => state.lease.is_none(),
            _ => true,
        };
        self.admit_locked(
            state,
            req.method.name(),
            crate::handlers::session_of(&req.params),
            crate::handlers::repo_of(&req.params),
            blocks_lease,
        )
        .map(Some)
    }

    pub(crate) fn admit_request(&self, req: &Request) -> Result<Option<Write>> {
        self.admit_request_locked(&mut self.state(), req)
    }

    pub(crate) fn background(&self) -> Option<Background> {
        let mut state = self.state();
        state.expire();
        if state.closing
            || state.lease.is_some()
            || state.parking.as_ref().is_some_and(|p| p.holding())
        {
            return None;
        }
        state.background += 1;
        Some(Background(self.clone()))
    }

    pub(crate) fn command(
        &self,
        params: &DrainParams,
        background_locked: impl FnOnce() -> bool,
    ) -> Result<(Value, bool)> {
        let mut state = self.state();
        state.expire();
        match params {
            DrainParams::Lease {
                on,
                ttl_s,
                lease_id,
            } => {
                if let Some(id) = lease_id
                    && state.lease.as_ref().is_none_or(|lease| lease.id != *id)
                {
                    return Err(lease_lost());
                }
                if *on && lease_id.is_none() && state.lease.is_some() {
                    return Err(draining());
                }
                if let Some(lease) = &state.lease
                    && let Some(then) = lease.then
                {
                    if then != DrainThen::Hold {
                        return Err(sealed());
                    }
                    if *on {
                        return Ok((json!({ "lease_id": lease.id }), false));
                    }
                    if lease_id.is_none() {
                        return Err(lease_lost());
                    }
                }
                if state.closing {
                    return Err(shutting_down());
                }
                if !on {
                    state.lease = None;
                    return Ok((json!({}), false));
                }
                let lease = state.lease.get_or_insert_with(|| Lease {
                    id: random_id(),
                    deadline: Instant::now(),
                    expires_at: SystemTime::now(),
                    then: None,
                });
                lease.renew(Duration::from_secs(u64::from(
                    ttl_s.unwrap_or(DEFAULT_TTL_S).min(MAX_TTL_S),
                )));
                Ok((json!({ "lease_id": lease.id }), false))
            }
            DrainParams::Seal { lease_id, then, .. } => {
                let lease = state
                    .lease
                    .as_ref()
                    .filter(|l| l.id == *lease_id)
                    .ok_or_else(lease_lost)?;
                if lease.then.is_some() {
                    return Err(sealed());
                }
                if state.closing {
                    return Err(shutting_down());
                }
                if state.inflight.values().any(|row| row.blocks_lease)
                    || state.background != 0
                    || background_locked()
                {
                    return Err(refusal(
                        Code::Busy,
                        "not_idle",
                        "the daemon still has work in progress",
                    ));
                }
                state
                    .lease
                    .as_mut()
                    .expect("checked under the same lock")
                    .then = Some(*then);
                Ok((json!({ "lease_id": lease_id }), *then != DrainThen::Hold))
            }
        }
    }

    pub(crate) fn status(&self) -> (Value, Value, Value) {
        let mut state = self.state();
        state.expire();
        let inflight: Vec<_> = state
            .inflight
            .values()
            .filter(|row| row.blocks_lease)
            .collect();
        let handover = state.parking.as_ref().map(|p| p.status(&state));
        (
            state.lease.as_ref().map_or(Value::Null, Lease::status),
            json!(inflight),
            json!(handover),
        )
    }

    /// The accept loop calls this before shutting down the operation queues.
    /// Once it chooses a mode, a late request cannot change it.
    pub(crate) fn finish_serving(&self, poisoned: bool) -> Option<ShutdownMode> {
        // A stop during H2 first gives O's parked frames their disposition.
        // Once this returns with the mutex held all streams are registered.
        let mut state = self.stop_parking(poisoned);
        state.shutdown = match state.lease.as_ref().and_then(|l| l.then) {
            Some(DrainThen::Stop) => Some(ShutdownMode::Stop),
            Some(DrainThen::Restart) => Some(ShutdownMode::Restart),
            Some(DrainThen::Hold) if !poisoned => Some(ShutdownMode::Hold),
            _ if poisoned => None,
            _ => Some(ShutdownMode::Sigterm),
        };
        state.shutdown
    }

    pub(crate) fn shutdown_mode(&self) -> Option<ShutdownMode> {
        self.state().shutdown
    }

    /// Whether serving ended during H2. Its stop already waited for admitted
    /// work and terminal refusals, like a sealed lease.
    pub(crate) fn stopped_handover(&self) -> bool {
        self.state().terminal_upgrade.is_some()
    }
}

pub(crate) struct Write {
    drain: Drain,
    id: u64,
    blocks_lease: bool,
}

impl Write {
    /// A preview admitted during a cold lease can still use cached refs.
    /// Its permission is fixed at dispatch for the whole preview.
    pub(crate) fn check_ref_refresh(&self) -> Result<()> {
        if self.blocks_lease {
            Ok(())
        } else {
            Err(draining())
        }
    }

    pub(crate) fn set_phase(&self, phase: Phase) -> Option<Phase> {
        self.drain
            .state()
            .inflight
            .get_mut(&self.id)
            .map(|row| std::mem::replace(&mut row.phase, phase))
    }

    pub(crate) fn set_session(&self, session: SessionId) {
        if let Some(row) = self.drain.state().inflight.get_mut(&self.id) {
            row.session = Some(session);
        }
    }

    pub(crate) fn set_repo(&self, repo: RepoName) {
        if let Some(row) = self.drain.state().inflight.get_mut(&self.id) {
            row.repo = Some(repo);
        }
    }
}

impl Drop for Write {
    fn drop(&mut self) {
        let mut state = self.drain.state();
        state.inflight.remove(&self.id);
        if state.terminal_upgrade.is_some() && state.inflight.is_empty() {
            self.drain.inner.replay.notify_all();
        }
    }
}

pub(crate) struct Background(Drain);

impl Drop for Background {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.background -= 1;
        if state.terminal_upgrade.is_some() && state.background == 0 {
            self.0.inner.replay.notify_all();
        }
    }
}

fn refusal(code: Code, reason: &str, message: &str) -> StrataError {
    StrataError::new(code, message).with_detail(json!({ "reason": reason }))
}

fn draining() -> StrataError {
    refusal(Code::Busy, "draining", "the daemon is draining")
}

fn lease_lost() -> StrataError {
    refusal(
        Code::Busy,
        "lease_lost",
        "the drain lease is no longer current",
    )
}

fn sealed() -> StrataError {
    refusal(Code::InvalidInput, "sealed", "the drain lease is sealed")
}

fn shutting_down() -> StrataError {
    refusal(
        Code::ReadOnly,
        "shutting_down",
        "the daemon is shutting down",
    )
}

pub(crate) fn random_id() -> String {
    let mut bytes = [0u8; 16];
    // SAFETY: arc4random_buf writes exactly the length of this buffer.
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
    format!("{:032x}", u128::from_be_bytes(bytes))
}

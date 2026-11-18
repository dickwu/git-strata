//! Admission fence and lease for a graceful daemon shutdown.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::{Value, json};
use strata_types::{Code, RepoName, SessionId, StrataError, serde_rfc3339};

use crate::Result;
use crate::rpc::{DrainParams, DrainThen};

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
    inflight: BTreeMap<u64, Inflight>,
    next: u64,
    // A pass counts from admission through completion, including periods
    // between lock acquisitions or while a child process runs.
    background: usize,
    closing: bool,
    shutdown: Option<ShutdownMode>,
}

impl State {
    fn expire(&mut self) {
        if self.lease.as_ref().is_some_and(Lease::expired) {
            self.lease = None;
        }
    }
}

/// Every admission and seal uses the same mutex. Guards never hold it
/// while a request or scheduler runs.
#[derive(Clone, Default)]
pub(crate) struct Drain(Arc<Mutex<State>>);

impl Drain {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn admit(
        &self,
        method: &'static str,
        session: Option<SessionId>,
        repo: Option<RepoName>,
    ) -> Result<Write> {
        let mut state = self.state();
        state.expire();
        if state.lease.is_some() {
            return Err(draining());
        }
        if state.closing {
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
            },
        );
        Ok(Write {
            drain: self.clone(),
            id,
        })
    }

    pub(crate) fn background(&self) -> Option<Background> {
        let mut state = self.state();
        state.expire();
        if state.closing || state.lease.is_some() {
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
                if !state.inflight.is_empty() || state.background != 0 || background_locked() {
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

    pub(crate) fn status(&self) -> (Value, Value) {
        let (draining, inflight) = {
            let mut state = self.state();
            state.expire();
            (
                state.lease.as_ref().map_or(Value::Null, Lease::status),
                state.inflight.values().cloned().collect::<Vec<_>>(),
            )
        };
        (draining, json!(inflight))
    }

    /// The accept loop calls this before shutting down the operation queues.
    /// Once it chooses a mode, a late request cannot change it.
    pub(crate) fn finish_serving(&self, poisoned: bool) -> Option<ShutdownMode> {
        let mut state = self.state();
        state.closing = true;
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
}

pub(crate) struct Write {
    drain: Drain,
    id: u64,
}

impl Write {
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
        self.drain.state().inflight.remove(&self.id);
    }
}

pub(crate) struct Background(Drain);

impl Drop for Background {
    fn drop(&mut self) {
        self.0.state().background -= 1;
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

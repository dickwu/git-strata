//! Freshness is measured from flight start; liveness from its completion.
use crate::{daemon::Read, error::GuiError};
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    pub instant: Instant,
    pub wall: SystemTime,
}
impl Stamp {
    pub fn now() -> Self {
        Self {
            instant: Instant::now(),
            wall: SystemTime::now(),
        }
    }
    pub fn age(self, now: Self) -> Duration {
        now.instant
            .saturating_duration_since(self.instant)
            .max(now.wall.duration_since(self.wall).unwrap_or_default())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Input {
    pub value: Option<Value>,
    pub at: Option<Stamp>,
    pub unsupported: bool,
    pub applied_gen: u64,
    pub last_attempt: Option<Stamp>,
}
impl Input {
    pub fn fresh(&self, now: Stamp, limit: Duration) -> bool {
        self.at.is_some_and(|at| at.age(now) <= limit)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownWhy {
    NotRunning,
    NotResponding,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub daemon_status: Input,
    pub sessions: Input,
    pub deps: Input,
    pub prev_accept_errors: Option<u64>,
    pub fd_pressure: bool,
    pub status_timeouts: u32,
    pub last_timeout_at: Option<Stamp>,
    pub last_status_ok: Option<Stamp>,
    pub last_down: Option<(Stamp, DownWhy)>,
    pub fd_at: Option<Stamp>,
}
impl Snapshot {
    pub fn input(&self, r: &Read) -> Option<&Input> {
        match r {
            Read::DaemonStatus => Some(&self.daemon_status),
            Read::Sessions => Some(&self.sessions),
            Read::DepsList => Some(&self.deps),
            _ => None,
        }
    }
    pub fn input_mut(&mut self, r: &Read) -> Option<&mut Input> {
        match r {
            Read::DaemonStatus => Some(&mut self.daemon_status),
            Read::Sessions => Some(&mut self.sessions),
            Read::DepsList => Some(&mut self.deps),
            _ => None,
        }
    }
    pub fn legacy(&self) -> bool {
        self.daemon_status
            .value
            .as_ref()
            .is_none_or(|d| d.get("api").is_none())
    }
    pub fn has_fds(&self) -> bool {
        self.daemon_status
            .value
            .as_ref()
            .is_some_and(|d| d["fds"].is_object())
    }
    pub fn upstream_enabled(&self) -> bool {
        self.daemon_status
            .value
            .as_ref()
            .and_then(|d| d["features"].as_array())
            .is_some_and(|fs| fs.iter().any(|f| f == "status.upstream"))
    }
    pub fn sessions_limit(&self) -> Duration {
        Duration::from_secs(if self.legacy() { 300 } else { 90 })
    }
    pub fn only_status(&self, now: Stamp) -> bool {
        self.last_down
            .is_some_and(|(at, _)| at.age(now) < Duration::from_secs(60))
            || self
                .fd_at
                .is_some_and(|at| at.age(now) < Duration::from_secs(60))
    }
    /// No new wire fields: older-daemon capabilities are read from existing replies.
    pub fn cadence(&self, r: &Read, visible: bool, now: Stamp) -> Option<Duration> {
        if *r == Read::DaemonStatus {
            return Some(Duration::from_secs(
                if self.daemon_status.at.is_some() && !self.has_fds() {
                    30
                } else {
                    10
                },
            ));
        }
        if self.daemon_status.at.is_none() || self.only_status(now) || (!visible && !self.has_fds())
        {
            return None;
        }
        match r {
            Read::Sessions => Some(Duration::from_secs(if self.legacy() { 120 } else { 30 })),
            Read::DepsList => Some(Duration::from_secs(30)),
            _ => None,
        }
    }
    pub fn due_in(&self, r: &Read, visible: bool, now: Stamp) -> Option<Duration> {
        let cadence = self.cadence(r, visible, now)?;
        Some(
            self.input(r)?
                .last_attempt
                .map_or(Duration::ZERO, |at| cadence.saturating_sub(at.age(now))),
        )
    }
    /// Outcomes are applied once per flight, before releasing its single-flight slot.
    pub fn record(
        &mut self,
        r: &Read,
        generation: Option<u64>,
        started: Stamp,
        completed: Stamp,
        result: &Result<Value, GuiError>,
    ) {
        if let Some(input) = self.input_mut(r) {
            let Some(generation) = generation else {
                return;
            };
            if generation <= input.applied_gen {
                return;
            }
            input.applied_gen = generation;
        }
        match result {
            Ok(value) => {
                if let Some(input) = self.input_mut(r) {
                    input.value = Some(value.clone());
                    input.at = Some(started);
                    input.unsupported = false;
                }
                if *r == Read::DaemonStatus {
                    let recovering = self.last_down.is_some() || self.fd_at.is_some();
                    if recovering {
                        self.sessions.last_attempt = None;
                        self.deps.last_attempt = None;
                    }
                    self.last_status_ok = Some(completed);
                    self.status_timeouts = 0;
                    self.last_timeout_at = None;
                    self.last_down = None;
                    self.fd_at = None;
                    let errors = value["fds"]["accept_errors"].as_u64();
                    self.fd_pressure = matches!((value["fds"]["open"].as_u64(), value["fds"]["limit"].as_u64()), (Some(open), Some(limit)) if limit > 0 && u128::from(open)*10 >= u128::from(limit)*9)
                        || matches!((self.prev_accept_errors, errors), (Some(old), Some(new)) if new > old);
                    self.prev_accept_errors = errors;
                }
            }
            Err(e) => match e.reason() {
                Some("too_many_open_files") if e.code == "Busy" => {
                    self.fd_at = Some(completed);
                    self.last_down = None;
                    self.status_timeouts = 0;
                    self.last_timeout_at = None;
                }
                Some("daemon_down")
                    if self
                        .fd_at
                        .is_none_or(|at| at.age(completed) >= Duration::from_secs(20)) =>
                {
                    self.last_down = Some((completed, DownWhy::NotRunning));
                }
                Some("unknown_method") => {
                    if let Some(input) = self.input_mut(r) {
                        input.unsupported = true;
                        input.value = None;
                        input.at = Some(started);
                    }
                }
                _ => {}
            },
        }
    }
    /// Each caller deadline is an observation, independent of reply generation.
    pub fn timeout(&mut self, r: &Read, now: Stamp) {
        if *r != Read::DaemonStatus
            || self
                .last_timeout_at
                .is_some_and(|at| at.age(now) < Duration::from_secs(4))
        {
            return;
        }
        self.last_timeout_at = Some(now);
        self.status_timeouts = self.status_timeouts.saturating_add(1);
        if self.status_timeouts >= 2
            && self
                .last_status_ok
                .is_none_or(|at| at.age(now) >= Duration::from_secs(20))
        {
            self.last_down = Some((now, DownWhy::NotResponding));
        }
    }
}

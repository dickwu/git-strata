//! Per-command paths, RPC negotiation and retry settings.
use crate::wait::{Deadline, RetryBudget};
use std::time::Duration;

use serde_json::{Value, json};
use strata_types::{Code, Config, MIN_DAEMON, StrataError, release_version};

use crate::Result;
use crate::args::WaitArgs;
use crate::env::Paths;
use crate::rpc::{self, CallError, Kind, Request};
use crate::tty::Pins;

type Reply<T = Value> = std::result::Result<T, CallError>;

#[derive(Clone)]
pub struct Ctx {
    pub paths: Paths,
    pub wait: WaitArgs,
}

impl Ctx {
    pub fn from_env() -> Result<Ctx> {
        Self::with_wait(WaitArgs::default())
    }

    pub fn with_wait(wait: WaitArgs) -> Result<Ctx> {
        Ok(Ctx {
            paths: Paths::from_env()?,
            wait,
        })
    }

    pub fn config(&self) -> Result<Config> {
        self.paths.config()
    }

    pub fn pins(&self) -> Pins {
        Pins::load(&self.paths.pins)
    }

    /// The bootstrap call always uses the original envelope. No capability is
    /// cached in Ctx: the next operation can meet a different daemon.
    pub(crate) fn probe(&self, deadline: Option<&Deadline>) -> Reply {
        let mut retry = RetryBudget::new(&self.wait).map_err(CallError::Failed)?;
        self.probe_with(&mut retry, deadline)
    }

    fn probe_with(&self, retry: &mut RetryBudget, deadline: Option<&Deadline>) -> Reply {
        let params = json!({"verify":false});
        let probe = Request::new("daemon_status", &params);
        retry
            .run("daemon_status", &self.wait, deadline, || {
                let limit = crate::agent::PROBE_DEADLINE;
                let time = deadline
                    .and_then(Deadline::remaining)
                    .unwrap_or(limit)
                    .min(limit);
                rpc::call_request(&self.paths.socket, probe, Kind::Read, Some(time))
            })
            .and_then(check_daemon_version)
    }

    pub(crate) fn begin_call<'a>(
        &'a self,
        method: &'a str,
        kind: Kind,
        deadline: Option<&'a Deadline>,
    ) -> Reply<NegotiatedCall<'a>> {
        self.begin_with(method, kind, deadline, false)
    }

    /// A feature-gated call: the caller checks `status` and then sends through
    /// the same call, so the probe and the target share one retry budget
    /// (CR r2 #7). The gate needs the status, so a failed probe fails the call.
    pub(crate) fn begin_gated<'a>(
        &'a self,
        method: &'a str,
        kind: Kind,
        deadline: Option<&'a Deadline>,
    ) -> Reply<NegotiatedCall<'a>> {
        self.begin_with(method, kind, deadline, true)
    }

    fn begin_with<'a>(
        &'a self,
        method: &'a str,
        kind: Kind,
        deadline: Option<&'a Deadline>,
        gated: bool,
    ) -> Reply<NegotiatedCall<'a>> {
        let mut retry = RetryBudget::new(&self.wait).map_err(CallError::Failed)?;
        let status = if method == "daemon_status" {
            Value::Null
        } else {
            match self.probe_with(&mut retry, deadline) {
                Ok(status) => status,
                Err(other) if gated => return Err(other),
                Err(CallError::Failed(e)) if e.code == Code::InvalidInput => Value::Null,
                Err(CallError::Timeout { .. }) => Value::Null,
                Err(other) => return Err(other),
            }
        };
        let client = status["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|v| v == "envelope.client"))
            .then_some(env!("CARGO_PKG_VERSION"));
        Ok(NegotiatedCall {
            ctx: self,
            method,
            kind,
            deadline,
            retry,
            client,
            status,
        })
    }

    pub fn call(&self, method: &str, params: Value, kind: Kind) -> Result<Value> {
        self.try_call(method, params, kind)
            .map_err(CallError::into_error)
    }

    pub fn try_call(&self, method: &str, params: Value, kind: Kind) -> Reply {
        self.call_until(method, params, kind, None)
    }

    pub(crate) fn call_within(
        &self,
        method: &str,
        params: Value,
        kind: Kind,
        budget: Duration,
    ) -> Reply {
        let deadline = Deadline::fixed(budget).map_err(CallError::Failed)?;
        self.call_until(method, params, kind, Some(&deadline))
    }

    pub(crate) fn call_until(
        &self,
        method: &str,
        params: Value,
        kind: Kind,
        deadline: Option<&Deadline>,
    ) -> Reply {
        self.begin_call(method, kind, deadline)?.send(&params)
    }

    pub fn call_interruptible(
        &self,
        method: &str,
        params: Value,
        kind: Kind,
    ) -> std::result::Result<Result<Value>, rpc::Stopped> {
        let mut operation = match self.begin_call(method, kind, None) {
            Ok(op) => op,
            Err(_) if crate::interrupt::STOP.load(std::sync::atomic::Ordering::SeqCst) => {
                return Err(rpc::Stopped::NotSent);
            }
            Err(error) => return Ok(Err(error.into_error())),
        };
        let result = operation.attempt(|client| {
            let request = Request {
                method,
                params: &params,
                client,
            };
            match rpc::call_interruptible_request(
                &self.paths.socket,
                request,
                kind,
                &crate::interrupt::STOP,
            ) {
                Ok(result) => result.map(Ok),
                Err(stopped) => Ok(Err(stopped)),
            }
        });
        match result {
            Ok(result) => result.map(Ok),
            Err(error) => Ok(Err(error.into_error())),
        }
    }
}

/// One logical RPC, from its capability probe through the last parameter variant.
pub(crate) struct NegotiatedCall<'a> {
    ctx: &'a Ctx,
    method: &'a str,
    kind: Kind,
    deadline: Option<&'a Deadline>,
    retry: RetryBudget,
    client: Option<&'static str>,
    pub(crate) status: Value,
}

impl NegotiatedCall<'_> {
    pub(crate) fn send(&mut self, params: &Value) -> Reply {
        let (ctx, method, kind, deadline) = (self.ctx, self.method, self.kind, self.deadline);
        let data = self.attempt(|client| {
            rpc::call_request(
                &ctx.paths.socket,
                Request {
                    method,
                    params,
                    client,
                },
                kind,
                deadline.and_then(Deadline::remaining),
            )
        })?;
        if method == "daemon_status" {
            check_daemon_version(data)
        } else {
            Ok(data)
        }
    }

    fn attempt<T>(&mut self, mut send: impl FnMut(Option<&'static str>) -> Reply<T>) -> Reply<T> {
        self.retry
            .run(self.method, &self.ctx.wait, self.deadline, || {
                match send(self.client) {
                    Err(CallError::UnknownClientField(_)) if self.client.is_some() => {
                        self.client = None;
                        send(None)
                    }
                    other => other,
                }
            })
            .map_err(|error| match error {
                CallError::UnknownClientField(e) => CallError::Failed(e),
                other => other,
            })
    }
}

/// MIN_DAEMON gate on a successful status reply. Every supported daemon
/// (v0.3.0 on) reports a release `version`; v0.2.0 answers without one, so a
/// missing or unparseable version is below the floor too (CR r2 #6).
fn check_daemon_version(status: Value) -> Reply {
    let floor = release_version(MIN_DAEMON).expect("release floor");
    let version = status["version"].as_str();
    if version
        .and_then(release_version)
        .is_some_and(|release| release >= floor)
    {
        return Ok(status);
    }
    let message = match version {
        Some(v) => format!("git-stratad {v} is too old; use {MIN_DAEMON} or later"),
        None => format!(
            "git-stratad reports no version, so it predates {MIN_DAEMON}; use {MIN_DAEMON} or later"
        ),
    };
    Err(CallError::Failed(
        StrataError::new(Code::Unsupported, message).with_detail(json!({
            "reason": "daemon_too_old",
            "daemon": status["version"].clone(),
            "min_daemon": MIN_DAEMON,
        })),
    ))
}

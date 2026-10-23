//! `Confirmer`: the gate, the prompt and the registry (confirm.md §5.2, §5.9).

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use serde_json::{Value, json};
use strata_types::{Action, Code, ConfirmToken, StrataError};

use crate::Result;
use crate::clock::Clock;
use crate::reason::prompt_reason;
use crate::registry::Registry;

/// What the framework's reply block saw: plain `Send` data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub(crate) success: bool,
    pub(crate) error: Option<ReplyError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplyError {
    /// The error's domain is `LAErrorDomain`.
    pub(crate) la_domain: bool,
    pub(crate) code: isize,
}

/// `evaluatePolicy` raised an Objective-C exception.
pub(crate) struct Thrown;

/// The prompt backend. Production is LocalAuthentication; tests script it.
pub(crate) trait Authenticator: Send + Sync {
    /// Opens a fresh, production-configured context. Lives only on the calling thread.
    fn context(&self) -> Box<dyn AuthContext + '_>;
}

/// One fresh authentication context (an `LAContext` in production). Never reused.
pub(crate) trait AuthContext {
    /// Starts the evaluation; the reply arrives later on `reply`.
    fn evaluate(&mut self, reason: &str, reply: SyncSender<Reply>) -> std::result::Result<(), Thrown>;
    /// Cancels any pending evaluation; idempotent.
    fn invalidate(&mut self);
}

/// The daemon's single issuer of confirm tokens. `Send + Sync`; two mutexes (the prompt
/// gate and the registry), never waiting for the second while holding the first (§5.9).
pub struct Confirmer {
    auth: Box<dyn Authenticator>,
    pub(crate) clock: Box<dyn Clock>,
    pub(crate) registry: Registry,
    gate: Mutex<()>,
    prompt_timeout: Duration,
    cancel_grace: Duration,
}

pub(crate) fn not_confirmed(reason: &str, message: &str, extra: Value) -> StrataError {
    refusal(Code::NotConfirmed, reason, message, extra)
}

pub(crate) fn refusal(code: Code, reason: &str, message: &str, extra: Value) -> StrataError {
    let mut detail = match extra {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    detail.insert("reason".into(), Value::from(reason));
    StrataError::new(code, message).with_detail(Value::Object(detail))
}

/// Waits for a reply until `until_mono_ns` on the sleep-aware clock, in slices of at most
/// one second, re-reading the clock after each slice (§5.2 step 7).
fn wait_reply(rx: &Receiver<Reply>, clock: &dyn Clock, until_mono_ns: u64) -> Waited {
    loop {
        let now = clock.mono_ns();
        if now >= until_mono_ns {
            return Waited::Expired;
        }
        let slice = Duration::from_nanos(until_mono_ns - now).min(Duration::from_secs(1));
        match rx.recv_timeout(slice) {
            Ok(r) => return Waited::Reply(r),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Waited::Gone,
        }
    }
}

enum Waited {
    Reply(Reply),
    Expired,
    /// The reply block was dropped without replying.
    Gone,
}

fn add_ns(base: u64, d: Duration) -> u64 {
    base.saturating_add(d.as_nanos().min(u64::MAX as u128) as u64)
}

impl Confirmer {
    pub(crate) fn build(
        auth: Box<dyn Authenticator>,
        clock: Box<dyn Clock>,
        prompt_timeout: Duration,
        cancel_grace: Duration,
    ) -> Confirmer {
        Confirmer {
            auth,
            clock,
            registry: Registry::default(),
            gate: Mutex::new(()),
            prompt_timeout,
            cancel_grace,
        }
    }

    /// Shows one system authentication prompt for `action`; on success mints a token and
    /// registers its nonce. Blocks for at most `PROMPT_TIMEOUT + CANCEL_GRACE` (plus scheduling).
    pub fn require(&self, action: Action) -> Result<ConfirmToken> {
        let reason = prompt_reason(&action);
        let _gate = match self.gate.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut ctx = self.auth.context();
        let (tx, rx) = sync_channel::<Reply>(1);
        let outcome = (|| {
            if ctx.evaluate(&reason, tx).is_err() {
                return Err(unexpected(&action, None));
            }
            let until = add_ns(self.clock.mono_ns(), self.prompt_timeout);
            match wait_reply(&rx, &*self.clock, until) {
                Waited::Reply(Reply {
                    success: true,
                    error: None,
                }) => Ok(()),
                Waited::Reply(r) => Err(unexpected(&action, r.error.map(|e| e.code))),
                Waited::Gone => Err(unexpected(&action, None)),
                Waited::Expired => Err(not_confirmed(
                    "timeout",
                    "nobody answered the confirmation prompt in time",
                    json!({ "action": action }),
                )),
            }
        })();
        ctx.invalidate();
        drop(ctx);
        outcome?;
        self.registry
            .mint(action, self.clock.instant(), self.clock.mono_ns())
    }

    /// Redeems a token (§5.5). The nonce is spent whenever it is found, whatever the outcome.
    pub fn validate(&self, token: &ConfirmToken, expected: &Action) -> Result<()> {
        self.registry
            .redeem(token, expected, self.clock.instant(), self.clock.mono_ns())
    }
}

fn unexpected(action: &Action, la_code: Option<isize>) -> StrataError {
    let mut extra = json!({ "action": action });
    if let Some(c) = la_code {
        extra["la_code"] = json!(c);
    }
    not_confirmed(
        "unexpected",
        "the authentication framework replied in an unexpected way",
        extra,
    )
}

//! `Confirmer`: the gate, the prompt and the registry (confirm.md §5.2, §5.9).

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use serde_json::{Value, json};
use strata_types::{Action, Code, ConfirmToken, StrataError};

use std::sync::{Mutex, TryLockError};

use crate::Result;
use crate::clock::Clock;
use crate::outcome::{classify, outcome_error, preflight_refusal, to_error};
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

/// What the graphic-session check found (§5.2 step 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuiSession {
    Graphic,
    NoGraphic { attrs: u32 },
    Failed { status: i32 },
}

/// The prompt backend. Production is LocalAuthentication; tests script it.
/// Its four operations are `gui_session`, and on each fresh context `preflight`,
/// `evaluate` and `invalidate`.
pub(crate) trait Authenticator: Send + Sync {
    /// Whether this process is in a security session with graphic access.
    fn gui_session(&self) -> GuiSession;
    /// Opens a fresh, production-configured context. Lives only on the calling thread.
    fn context(&self) -> Box<dyn AuthContext + '_>;
}

/// One fresh authentication context (an `LAContext` in production). Never reused.
pub(crate) trait AuthContext {
    /// canEvaluatePolicy; `Err` carries the LAError code.
    fn preflight(&mut self) -> std::result::Result<(), isize>;
    /// Starts the evaluation; the reply arrives later on `reply`.
    fn evaluate(
        &mut self,
        reason: &str,
        reply: SyncSender<Reply>,
    ) -> std::result::Result<(), Thrown>;
    /// Cancels any pending evaluation.
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
    ///
    /// Codes: `NotConfirmed` (cancelled, failed, interrupted, timeout, prompt_in_progress,
    /// unexpected, la_exception, internal); `Unsupported` (no_gui_session, session_info,
    /// passcode_not_set, not_interactive, cannot_evaluate).
    pub fn require(&self, action: Action) -> Result<ConfirmToken> {
        // 1. The text. Never empty.
        let reason = prompt_reason(&action);
        // 2. One prompt at a time; a second request is refused, not queued (C6). A poisoned
        //    gate protects no data, so it counts as free.
        let _gate = match self.gate.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(to_error(
                    Code::NotConfirmed,
                    "prompt_in_progress",
                    None,
                    Some(&action),
                    json!({}),
                ));
            }
        };
        // 3. Graphic session.
        self.check_gui(Some(&action))?;
        // 4..10 inside an autorelease pool: the daemon's threads are long-lived.
        with_pool(|| self.prompt(&action, &reason))?;
        // 9. Mint, at the moment authentication passed.
        self.registry
            .mint(action, self.clock.instant(), self.clock.mono_ns())
    }

    /// Steps 4 to 8 and 10 of §5.2. Ok means the human passed.
    fn prompt(&self, action: &Action, reason: &str) -> Result<()> {
        let mut ctx = Ctx::new(self.auth.context());
        // 4. Preflight.
        if let Err(code) = ctx.get().preflight() {
            ctx.invalidate();
            return Err(outcome_error(preflight_refusal(code), Some(action)).expect("refusal"));
        }
        // 5. Reply channel.
        let (tx, rx) = sync_channel::<Reply>(1);
        // 6. Evaluate (asynchronous).
        if ctx.get().evaluate(reason, tx).is_err() {
            ctx.invalidate();
            return Err(to_error(
                Code::NotConfirmed,
                "la_exception",
                None,
                Some(action),
                json!({}),
            ));
        }
        let until = add_ns(self.clock.mono_ns(), self.prompt_timeout);
        // 7. Wait, on the clock that runs during sleep.
        let reply = match wait_reply(&rx, &*self.clock, until) {
            Waited::Reply(r) => r,
            Waited::Gone => {
                ctx.invalidate();
                return Err(to_error(
                    Code::NotConfirmed,
                    "unexpected",
                    None,
                    Some(action),
                    json!({}),
                ));
            }
            Waited::Expired => {
                // Cancel the prompt, then give the framework a grace period to reply. Whatever
                // it says, even a success, comes too late.
                ctx.invalidate();
                let grace_until = add_ns(self.clock.mono_ns(), self.cancel_grace);
                let _late = wait_reply(&rx, &*self.clock, grace_until);
                return Err(to_error(
                    Code::NotConfirmed,
                    "timeout",
                    None,
                    Some(action),
                    json!({}),
                ));
            }
        };
        // 8. Judge.
        let outcome = classify(reply, false);
        // 10. Every path invalidates its context (once) and drops it.
        ctx.invalidate();
        match outcome_error(outcome, Some(action)) {
            None => Ok(()),
            Some(e) => Err(e),
        }
    }

    fn check_gui(&self, action: Option<&Action>) -> Result<()> {
        match self.auth.gui_session() {
            GuiSession::Graphic => Ok(()),
            GuiSession::NoGraphic { attrs } => Err(to_error(
                Code::Unsupported,
                "no_gui_session",
                None,
                action,
                json!({ "attrs": attrs }),
            )),
            GuiSession::Failed { status } => Err(to_error(
                Code::Unsupported,
                "session_info",
                None,
                action,
                json!({ "status": status }),
            )),
        }
    }

    /// Answers "can a human confirm right now?" without a prompt (§5.2 steps 3 and 4).
    /// Codes: `Unsupported`, as for `require`.
    pub fn probe(&self) -> Result<()> {
        self.check_gui(None)?;
        with_pool(|| {
            let mut ctx = Ctx::new(self.auth.context());
            let r = ctx.get().preflight();
            ctx.invalidate();
            match r {
                Ok(()) => Ok(()),
                Err(code) => Err(outcome_error(preflight_refusal(code), None).expect("refusal")),
            }
        })
    }

    /// Redeems a token (§5.5). The nonce is spent whenever it is found, whatever the outcome.
    pub fn validate(&self, token: &ConfirmToken, expected: &Action) -> Result<()> {
        self.registry
            .redeem(token, expected, self.clock.instant(), self.clock.mono_ns())
    }
}

/// A context that is invalidated exactly once, on whichever path ends the prompt.
struct Ctx<'a> {
    inner: Box<dyn AuthContext + 'a>,
    invalidated: bool,
}

impl<'a> Ctx<'a> {
    fn new(inner: Box<dyn AuthContext + 'a>) -> Self {
        Ctx {
            inner,
            invalidated: false,
        }
    }
    fn get(&mut self) -> &mut (dyn AuthContext + 'a) {
        &mut *self.inner
    }
    fn invalidate(&mut self) {
        if !self.invalidated {
            self.invalidated = true;
            self.inner.invalidate();
        }
    }
}

impl Drop for Ctx<'_> {
    fn drop(&mut self) {
        self.invalidate();
    }
}

#[cfg(target_os = "macos")]
fn with_pool<R>(f: impl FnOnce() -> R) -> R {
    objc2::rc::autoreleasepool(|_| f())
}

#[cfg(not(target_os = "macos"))]
fn with_pool<R>(f: impl FnOnce() -> R) -> R {
    f()
}

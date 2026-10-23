//! Test seams (cargo feature `test-seams`; never in a shipped binary, confirm.md §3, C8).

use std::collections::VecDeque;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::Confirmer;
use crate::clock::{Clock, real_mono_ns};
use crate::confirmer::{AuthContext, Authenticator, GuiSession, Reply, ReplyError, Thrown};

/// `LAErrorDomain`.
pub const LA_ERROR_DOMAIN: &str = "com.apple.LocalAuthentication";

/// How one scripted prompt ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Reply (success, error); error is Some((domain, code)).
    Reply {
        success: bool,
        error: Option<(String, isize)>,
    },
    /// Never replies; after invalidate replies AppCancel (-9), like the real framework.
    Hang,
    /// Never replies, not even after invalidate.
    Silent,
    /// Never replies; after invalidate replies success = YES.
    PassAfterCancel,
    /// evaluatePolicy raises an Objective-C exception.
    Throw,
}

impl Verdict {
    /// The human passed: success = YES, no error.
    pub fn pass() -> Verdict {
        Verdict::Reply {
            success: true,
            error: None,
        }
    }
    /// success = NO, error = (LAErrorDomain, code).
    pub fn la(code: isize) -> Verdict {
        Verdict::Reply {
            success: false,
            error: Some((LA_ERROR_DOMAIN.to_string(), code)),
        }
    }
}

fn to_reply(success: bool, error: &Option<(String, isize)>) -> Reply {
    Reply {
        success,
        error: error.as_ref().map(|(d, c)| ReplyError {
            la_domain: d == LA_ERROR_DOMAIN,
            code: *c,
        }),
    }
}

#[derive(Debug, Default)]
struct LogInner {
    shown: Vec<String>,
    invalidated: u32,
    cancelled_while_pending: u32,
    probes: u32,
}

/// Everything the scripted authenticator saw; each reader returns a snapshot.
#[derive(Debug, Default)]
pub struct PromptLog {
    inner: Mutex<LogInner>,
}

impl PromptLog {
    fn with<R>(&self, f: impl FnOnce(&mut LogInner) -> R) -> R {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        f(&mut g)
    }
    /// The text of every prompt shown, in order.
    pub fn shown(&self) -> Vec<String> {
        self.with(|l| l.shown.clone())
    }
    /// Total invalidate calls.
    pub fn invalidated(&self) -> u32 {
        self.with(|l| l.invalidated)
    }
    /// Invalidations that happened while an evaluation had not replied yet.
    pub fn cancelled_while_pending(&self) -> u32 {
        self.with(|l| l.cancelled_while_pending)
    }
    /// Calls to the graphic-session check plus calls to the preflight.
    pub fn probes(&self) -> u32 {
        self.with(|l| l.probes)
    }
}

/// A scripted authenticator.
pub struct ScriptedAuth {
    gui: bool,
    can_evaluate: std::result::Result<(), isize>,
    verdicts: Mutex<VecDeque<Verdict>>,
    log: Arc<PromptLog>,
}

impl ScriptedAuth {
    /// `gui`: whether SessionGetInfo reports graphic access; `can_evaluate`: canEvaluatePolicy;
    /// `verdicts`: one per prompt, then `Verdict::la(-2)` forever.
    pub fn new(
        gui: bool,
        can_evaluate: std::result::Result<(), isize>,
        verdicts: Vec<Verdict>,
    ) -> ScriptedAuth {
        ScriptedAuth {
            gui,
            can_evaluate,
            verdicts: Mutex::new(verdicts.into()),
            log: Arc::new(PromptLog::default()),
        }
    }

    /// The log shared with this authenticator.
    pub fn log(&self) -> Arc<PromptLog> {
        self.log.clone()
    }

    fn next_verdict(&self) -> Verdict {
        let mut q = match self.verdicts.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        q.pop_front().unwrap_or_else(|| Verdict::la(-2))
    }
}

struct ScriptedCtx<'a> {
    auth: &'a ScriptedAuth,
    pending: Option<(Verdict, SyncSender<Reply>)>,
    /// A `Silent` sender is kept alive until the context is dropped.
    silent: Option<SyncSender<Reply>>,
}

impl Authenticator for ScriptedAuth {
    fn gui_session(&self) -> GuiSession {
        self.log.with(|l| l.probes += 1);
        if self.gui {
            GuiSession::Graphic
        } else {
            // An ssh-like session: no sessionHasGraphicAccess (0x10) bit.
            GuiSession::NoGraphic { attrs: 0x0020 }
        }
    }

    fn context(&self) -> Box<dyn AuthContext + '_> {
        Box::new(ScriptedCtx {
            auth: self,
            pending: None,
            silent: None,
        })
    }
}

impl AuthContext for ScriptedCtx<'_> {
    fn preflight(&mut self) -> std::result::Result<(), isize> {
        self.auth.log.with(|l| l.probes += 1);
        self.auth.can_evaluate
    }

    fn evaluate(
        &mut self,
        reason: &str,
        reply: SyncSender<Reply>,
    ) -> std::result::Result<(), Thrown> {
        self.auth.log.with(|l| l.shown.push(reason.to_string()));
        match self.auth.next_verdict() {
            Verdict::Reply { success, error } => {
                let _ = reply.try_send(to_reply(success, &error));
                Ok(())
            }
            Verdict::Throw => Err(Thrown),
            v => {
                self.pending = Some((v, reply));
                Ok(())
            }
        }
    }

    fn invalidate(&mut self) {
        let pending = self.pending.take();
        self.auth.log.with(|l| {
            l.invalidated += 1;
            if pending.is_some() {
                l.cancelled_while_pending += 1;
            }
        });
        match pending {
            Some((Verdict::Hang, tx)) => {
                let _ = tx.try_send(to_reply(false, &Some((LA_ERROR_DOMAIN.into(), -9))));
            }
            Some((Verdict::PassAfterCancel, tx)) => {
                let _ = tx.try_send(to_reply(true, &None));
            }
            Some((_, tx)) => self.silent = Some(tx),
            None => {}
        }
    }
}

#[derive(Debug, Default)]
struct Offsets {
    advance: Duration,
    sleep: Duration,
}

/// A real clock plus offsets the test dials in. `advance` moves both clocks;
/// `sleep` moves only CLOCK_MONOTONIC (system sleep).
#[derive(Clone, Debug, Default)]
pub struct FakeClock {
    offsets: Arc<Mutex<Offsets>>,
}

impl FakeClock {
    pub fn new() -> FakeClock {
        FakeClock::default()
    }
    pub fn advance(&self, d: Duration) {
        self.lock().advance += d;
    }
    pub fn sleep(&self, d: Duration) {
        self.lock().sleep += d;
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Offsets> {
        match self.offsets.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }
}

impl Clock for FakeClock {
    fn instant(&self) -> Instant {
        Instant::now() + self.lock().advance
    }
    fn mono_ns(&self) -> u64 {
        let o = self.lock();
        let extra = (o.advance + o.sleep).as_nanos() as u64;
        real_mono_ns().saturating_add(extra)
    }
}

impl Confirmer {
    /// Scripted authenticator + dialable clock; prompt_timeout and cancel_grace can be short.
    pub fn with_seams(
        auth: ScriptedAuth,
        clock: FakeClock,
        prompt_timeout: Duration,
        cancel_grace: Duration,
    ) -> Confirmer {
        Confirmer::build(
            Box::new(auth),
            Box::new(clock),
            prompt_timeout,
            cancel_grace,
        )
    }

    /// Current number of records in the registry.
    pub fn record_count(&self) -> usize {
        self.registry.count()
    }
}

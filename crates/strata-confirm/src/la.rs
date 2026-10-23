//! The LocalAuthentication bridge (confirm.md §5.3), through objc2-local-authentication.
//!
//! `LAContext` is not `Send`: each context is created, configured, evaluated, invalidated and
//! dropped on the thread that called `require`. The reply block only reads the `Bool` and the
//! error's `domain()` and `code()`, and sends a [`Reply`] down a channel.

use std::panic::AssertUnwindSafe;
use std::sync::mpsc::SyncSender;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAErrorDomain, LAPolicy};
use objc2_security::{SessionAttributeBits, SessionGetInfo, callerSecuritySession};

use crate::Confirmer;
use crate::clock::RealClock;
use crate::confirmer::{AuthContext, Authenticator, GuiSession, Reply, ReplyError, Thrown};
use crate::{CANCEL_GRACE, PROMPT_TIMEOUT};

/// Device owner authentication: Touch ID, falling back to the login password.
const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

/// Deviations from production, reachable only through the test seams.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaTweaks {
    /// `interactionNotAllowed = true`: the framework fails with -1004 instead of showing UI.
    pub(crate) interaction_not_allowed: bool,
    /// Invalidate the context before evaluating (provokes -10).
    pub(crate) invalidate_first: bool,
}

/// The production [`Authenticator`].
pub(crate) struct LocalAuth {
    tweaks: LaTweaks,
}

impl LocalAuth {
    pub(crate) fn new(tweaks: LaTweaks) -> LocalAuth {
        LocalAuth { tweaks }
    }
}

/// A fresh context in the production configuration (§5.3): reuse duration explicitly 0,
/// `interactionNotAllowed` left false, fallback and cancel titles left to the system.
pub(crate) fn production_context() -> Retained<LAContext> {
    // SAFETY: plain Objective-C object creation and a property setter on our own object.
    unsafe {
        let ctx = LAContext::new();
        ctx.setTouchIDAuthenticationAllowableReuseDuration(0.0);
        ctx
    }
}

/// What a production-configured context reports, for `seams::read_back_la_config`.
pub(crate) fn read_back() -> (f64, bool, bool) {
    let ctx = production_context();
    // SAFETY: property getters and invalidate on our own context.
    unsafe {
        let r = (
            ctx.touchIDAuthenticationAllowableReuseDuration(),
            ctx.interactionNotAllowed(),
            ctx.localizedFallbackTitle().is_none(),
        );
        ctx.invalidate();
        r
    }
}

fn graphic_session() -> GuiSession {
    let mut sid = 0u32;
    let mut attrs = SessionAttributeBits(0);
    // SAFETY: both out-pointers are valid for writes.
    #[allow(unused_unsafe)]
    let status = unsafe { SessionGetInfo(callerSecuritySession, &mut sid, &mut attrs) };
    if status != 0 {
        GuiSession::Failed { status }
    } else if attrs.0 & SessionAttributeBits::sessionHasGraphicAccess.0 != 0 {
        GuiSession::Graphic
    } else {
        GuiSession::NoGraphic { attrs: attrs.0 }
    }
}

fn reply_error(e: &NSError) -> ReplyError {
    // SAFETY: LAErrorDomain is an immutable framework constant.
    let la = unsafe { LAErrorDomain };
    ReplyError {
        la_domain: *e.domain() == *la,
        code: e.code(),
    }
}

impl Authenticator for LocalAuth {
    fn gui_session(&self) -> GuiSession {
        graphic_session()
    }

    fn context(&self) -> Box<dyn AuthContext + '_> {
        let ctx = production_context();
        if self.tweaks.interaction_not_allowed {
            // SAFETY: property setter on our own context.
            unsafe { ctx.setInteractionNotAllowed(true) };
        }
        Box::new(LaCtx {
            ctx,
            tweaks: self.tweaks,
        })
    }
}

struct LaCtx {
    ctx: Retained<LAContext>,
    tweaks: LaTweaks,
}

impl AuthContext for LaCtx {
    fn preflight(&mut self) -> Result<(), isize> {
        // SAFETY: canEvaluatePolicy on our own context.
        match unsafe { self.ctx.canEvaluatePolicy_error(POLICY) } {
            Ok(()) => Ok(()),
            Err(e) => Err(e.code()),
        }
    }

    fn evaluate(&mut self, reason: &str, reply: SyncSender<Reply>) -> Result<(), Thrown> {
        if self.tweaks.invalidate_first {
            // SAFETY: invalidate on our own context.
            unsafe { self.ctx.invalidate() };
        }
        let text = NSString::from_str(reason);
        // The block runs once, on a private framework queue. It only reads the reply and sends it.
        let block = RcBlock::new(move |success: Bool, error: *mut NSError| {
            // SAFETY: the framework passes nil or a valid NSError for the block's duration.
            let error = unsafe { error.as_ref() }.map(reply_error);
            let _ = reply.try_send(Reply {
                success: success.as_bool(),
                error,
            });
        });
        let ctx = &self.ctx;
        let call = AssertUnwindSafe(|| {
            // SAFETY: the block is Send (it owns only a SyncSender) and 'static; `ctx` stays
            // alive until the confirmer invalidates and drops it (§5.2 step 10).
            unsafe { ctx.evaluatePolicy_localizedReason_reply(POLICY, &text, &block) }
        });
        #[allow(unused_unsafe)]
        let caught = unsafe { objc2::exception::catch(call) };
        caught.map_err(|_| Thrown)
    }

    fn invalidate(&mut self) {
        // SAFETY: invalidate on our own context; a second call has no effect.
        unsafe { self.ctx.invalidate() };
    }
}

impl Confirmer {
    /// The daemon's confirmer: real LocalAuthentication, real clocks. Never fails; it touches
    /// LocalAuthentication only when `require` or `probe` runs.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Confirmer {
        Confirmer::with_local_auth(LaTweaks::default(), PROMPT_TIMEOUT)
    }

    pub(crate) fn with_local_auth(
        tweaks: LaTweaks,
        prompt_timeout: std::time::Duration,
    ) -> Confirmer {
        Confirmer::build(
            Box::new(LocalAuth::new(tweaks)),
            Box::new(RealClock),
            prompt_timeout,
            CANCEL_GRACE,
        )
    }
}

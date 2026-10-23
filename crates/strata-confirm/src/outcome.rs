//! From the framework's reply to a code (confirm.md §5.4), and the preflight refusals (§5.2 step 4).

use serde_json::json;
use strata_types::{Action, Code, StrataError};

use crate::confirmer::{Reply, refusal};

/// What one reply means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Mint a token.
    Pass,
    Refuse {
        code: Code,
        reason: &'static str,
        la_code: Option<isize>,
    },
}

fn refuse(code: Code, reason: &'static str, la_code: Option<isize>) -> Outcome {
    Outcome::Refuse {
        code,
        reason,
        la_code,
    }
}

/// The §5.4 table. `we_cancelled`: we invalidated this context at the deadline.
/// (After the deadline the confirmer does not look at replies at all; the flag keeps the
/// table complete.)
pub(crate) fn classify(reply: Reply, we_cancelled: bool) -> Outcome {
    use Code::{NotConfirmed as NC, Unsupported as US};
    match (reply.success, reply.error) {
        (true, None) => Outcome::Pass,
        // Contract breaches: success with an error, failure without one, a foreign domain.
        (true, Some(e)) => refuse(NC, "unexpected", e.la_domain.then_some(e.code)),
        (false, None) => refuse(NC, "unexpected", None),
        (false, Some(e)) if !e.la_domain => refuse(NC, "unexpected", None),
        (false, Some(e)) => {
            let c = Some(e.code);
            match e.code {
                -1 => refuse(NC, "failed", c),                  // AuthenticationFailed
                -2 => refuse(NC, "cancelled", c),               // UserCancel
                -3 => refuse(NC, "cancelled", c),               // UserFallback
                -4 => refuse(NC, "interrupted", c),             // SystemCancel
                -5 => refuse(US, "passcode_not_set", c),        // PasscodeNotSet
                -8 => refuse(NC, "failed", c),                  // BiometryLockout
                -9 if we_cancelled => refuse(NC, "timeout", c), // AppCancel
                -9 => refuse(NC, "interrupted", c),
                -1004 => refuse(US, "not_interactive", c), // NotInteractive
                // -6, -7 (biometry not available / enrolled), -10 InvalidContext,
                // -11..-14, and any code the SDK does not name.
                _ => refuse(NC, "unexpected", c),
            }
        }
    }
}

/// canEvaluatePolicy failed with `code` (§5.2 step 4).
pub(crate) fn preflight_refusal(code: isize) -> Outcome {
    match code {
        -5 => refuse(Code::Unsupported, "passcode_not_set", Some(code)),
        -1004 => refuse(Code::Unsupported, "not_interactive", Some(code)),
        _ => refuse(Code::Unsupported, "cannot_evaluate", Some(code)),
    }
}

fn message(reason: &str) -> &'static str {
    match reason {
        "failed" => "authentication failed",
        "cancelled" => "the confirmation prompt was cancelled",
        "interrupted" => "the system interrupted the confirmation prompt",
        "timeout" => "nobody answered the confirmation prompt in time",
        "passcode_not_set" => "this account has no login password, so nobody can confirm",
        "not_interactive" => "the system does not allow a confirmation prompt here",
        "cannot_evaluate" => "this Mac cannot authenticate its owner right now",
        "no_gui_session" => {
            "the daemon is not in a graphical login session, so no prompt can be shown"
        }
        "session_info" => "could not read the daemon's security session",
        "prompt_in_progress" => "another confirmation prompt is already open; finish it first",
        "la_exception" => "the authentication framework raised an exception",
        "internal" => "the confirm registry is unusable; restart the daemon",
        _ => "the authentication framework replied in an unexpected way",
    }
}

/// A named refusal for `action`, with `detail = {reason, action, la_code?}` plus `extra`.
pub(crate) fn to_error(
    code: Code,
    reason: &str,
    la_code: Option<isize>,
    action: Option<&Action>,
    extra: serde_json::Value,
) -> StrataError {
    let mut detail = match extra {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    if let Some(a) = action {
        detail.insert("action".into(), json!(a));
    }
    if let Some(c) = la_code {
        detail.insert("la_code".into(), json!(c));
    }
    refusal(
        code,
        reason,
        message(reason),
        serde_json::Value::Object(detail),
    )
}

pub(crate) fn outcome_error(o: Outcome, action: Option<&Action>) -> Option<StrataError> {
    match o {
        Outcome::Pass => None,
        Outcome::Refuse {
            code,
            reason,
            la_code,
        } => Some(to_error(code, reason, la_code, action, json!({}))),
    }
}

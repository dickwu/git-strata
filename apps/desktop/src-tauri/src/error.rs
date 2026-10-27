//! gui.md §2.3: every command returns `Result<T, GuiError>`; `GuiError`
//! has the shape of the cli envelope's `error` (`code`, `message`,
//! `detail`), so the front end reads one error shape for both.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_types::{Code, StrataError};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuiError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub detail: Value,
}

impl GuiError {
    fn new(code: Code, message: impl Into<String>, detail: Value) -> GuiError {
        GuiError {
            code: code.as_str().to_string(),
            message: message.into(),
            detail,
        }
    }

    /// `detail.reason`, the machine-readable cause the front end maps to copy.
    pub fn reason(&self) -> Option<&str> {
        self.detail.get("reason").and_then(Value::as_str)
    }

    /// The daemon accepted the request but did not answer within `secs`.
    /// `strata_client::call` has no read timeout (P0 moved it verbatim and
    /// the cli keeps that behaviour), so the GUI bounds the wait itself.
    pub fn timeout(method: &str, secs: u64) -> GuiError {
        GuiError::new(
            Code::Busy,
            format!("the daemon did not answer `{method}` within {secs} s"),
            json!({ "reason": "daemon_timeout", "method": method, "seconds": secs }),
        )
    }

    /// The same read is still waiting for an answer; a second connection
    /// would only queue behind it.
    pub fn in_flight(method: &str) -> GuiError {
        GuiError::new(
            Code::Busy,
            format!("an earlier `{method}` request is still waiting for the daemon"),
            json!({ "reason": "request_in_flight", "method": method }),
        )
    }

    pub fn no_home() -> GuiError {
        GuiError::new(
            Code::InvalidInput,
            "HOME is not set, so the daemon socket cannot be found",
            json!({ "reason": "no_home" }),
        )
    }

    pub fn internal(message: impl Into<String>) -> GuiError {
        GuiError::new(Code::Unknown, message, json!({ "reason": "gui_internal" }))
    }
}

impl From<StrataError> for GuiError {
    fn from(e: StrataError) -> GuiError {
        GuiError {
            code: e.code.as_str().to_string(),
            message: e.message,
            detail: e.detail.unwrap_or(Value::Null),
        }
    }
}

impl From<strata_client::CallError> for GuiError {
    fn from(e: strata_client::CallError) -> GuiError {
        e.into_error().into()
    }
}

impl std::fmt::Display for GuiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for GuiError {}

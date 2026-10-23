//! Error constructors (§6).

use std::path::Path;

use serde_json::{Value, json};
use strata_types::{Code, StrataError};

pub(crate) fn err(code: Code, message: impl Into<String>) -> StrataError {
    StrataError::new(code, message)
}

pub(crate) fn errd(code: Code, message: impl Into<String>, detail: Value) -> StrataError {
    StrataError::new(code, message).with_detail(detail)
}

pub(crate) fn invalid(message: impl Into<String>) -> StrataError {
    err(Code::InvalidInput, message)
}

pub(crate) fn unknown_reason(message: impl Into<String>, reason: &str) -> StrataError {
    errd(Code::Unknown, message, json!({ "reason": reason }))
}

pub(crate) fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Wrap any error as `Unknown`, keeping the original code and message in
/// `detail` (used after the first write, G4).
pub(crate) fn as_unknown(e: StrataError, context: &str) -> StrataError {
    if e.code == Code::Unknown {
        return e;
    }
    errd(
        Code::Unknown,
        format!("{context}: {}", e.message),
        json!({ "code": e.code.as_str(), "message": e.message, "detail": e.detail }),
    )
}

pub(crate) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

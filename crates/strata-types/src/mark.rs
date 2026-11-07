use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Code, StrataError};

/// A work-completion mark, independent of the session's directory lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkState {
    Active,
    Done,
}

/// Informational attribution; marking never requires a confirmation gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkByKind {
    Agent,
    Cli,
    Gui,
    /// The daemon's auto-close fallback: never accepted from the `mark` RPC.
    Daemon,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkBy {
    pub kind: MarkByKind,
}

/// Present only for marked sessions. No row means active, including old stores.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMark {
    pub state: MarkState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub by: MarkBy,
    #[serde(with = "crate::serde_rfc3339")]
    pub at: SystemTime,
}

/// Q8: the cap is UTF-8 bytes, with tab/newline the only allowed C0 controls.
pub fn validate_mark_note(note: Option<&str>) -> Result<(), StrataError> {
    if note.is_some_and(|s| {
        s.is_empty() || s.len() > 2000 || s.bytes().any(|b| b < 0x20 && b != b'\t' && b != b'\n')
    }) {
        return Err(StrataError::new(
            Code::InvalidInput,
            "note must be 1–2000 UTF-8 bytes; only tab and newline control characters are allowed",
        )
        .with_detail(json!({ "reason": "note" })));
    }
    Ok(())
}

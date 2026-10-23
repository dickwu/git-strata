//! Error construction (store.md §6.1, §6.2). Every `detail` is a JSON object;
//! `io::Error` goes through `apfs::classify_io_error`, SQLite errors are
//! classified here, and store adds `op` and `outcome`.

use std::io;
use std::path::Path;

use rusqlite::ErrorCode;
use serde_json::{Map, Value, json};
use strata_apfs::{IoSide, classify_io_error};
use strata_types::{Code, StrataError};

use crate::Result;

pub(crate) const NOT_WRITTEN: &str = "not_written";
pub(crate) const UNCERTAIN: &str = "uncertain";
pub(crate) const PARTIAL: &str = "partial";

pub(crate) fn err(code: Code, msg: impl Into<String>, detail: Value) -> StrataError {
    StrataError::new(code, msg).with_detail(detail)
}

/// Merge `key = v` into the error's detail object (creating it).
pub(crate) fn with(mut e: StrataError, key: &str, v: impl Into<Value>) -> StrataError {
    let mut m = match e.detail.take() {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.insert(key.to_string(), v.into());
    e.detail = Some(Value::Object(m));
    e
}

/// Set `key` only when absent.
pub(crate) fn with_default(e: StrataError, key: &str, v: impl Into<Value>) -> StrataError {
    if let Some(Value::Object(m)) = &e.detail {
        if m.contains_key(key) {
            return e;
        }
    }
    with(e, key, v)
}

pub(crate) fn detail_str<'a>(e: &'a StrataError, key: &str) -> Option<&'a str> {
    e.detail.as_ref()?.get(key)?.as_str()
}

pub(crate) fn corrupt(reason: &str, msg: impl Into<String>) -> StrataError {
    err(Code::Corrupt, msg, json!({ "reason": reason }))
}

pub(crate) fn bad_row(msg: impl Into<String>) -> StrataError {
    corrupt("bad_row", msg)
}

pub(crate) fn invalid(reason: &str, msg: impl Into<String>) -> StrataError {
    err(Code::InvalidInput, msg, json!({ "reason": reason }))
}

pub(crate) fn invalid_field(field: &str, msg: impl Into<String>) -> StrataError {
    err(Code::InvalidInput, msg, json!({ "field": field }))
}

pub(crate) fn read_only_handle() -> StrataError {
    err(Code::ReadOnly, "the store is open read-only", json!({ "reason": "read_only_handle" }))
}

pub(crate) fn poisoned() -> StrataError {
    err(
        Code::Unknown,
        "the store writer is poisoned after an uncertain commit; reopen the store",
        json!({ "reason": "poisoned", "outcome": NOT_WRITTEN }),
    )
}

pub(crate) fn too_large(limit: &str) -> StrataError {
    err(Code::TooLarge, format!("{limit} reached i64::MAX"), json!({ "limit": limit, "max": i64::MAX }))
}

pub(crate) fn no_such_session(id: &str) -> StrataError {
    err(Code::NoSuchSession, format!("no such session: {id}"), json!({ "session": id }))
}

pub(crate) fn no_such_repo(session: &str, repo: &str) -> StrataError {
    err(Code::NoSuchRepo, format!("session {session} has no workspace for {repo}"), json!({ "session": session, "repo": repo }))
}

/// `io::Error` → `StrataError` via apfs, plus `op`; `outcome` on `Unknown`.
pub(crate) fn io(e: &io::Error, side: IoSide, path: &Path, op: &str, outcome: &str) -> StrataError {
    let s = classify_io_error(e, side, path);
    annotate(s, op, outcome)
}

/// Add `op`, and `outcome` when the code is `Unknown` (only there it matters).
pub(crate) fn annotate(e: StrataError, op: &str, outcome: &str) -> StrataError {
    let e = with_default(e, "op", op);
    if e.code == Code::Unknown { with_default(e, "outcome", outcome) } else { e }
}

/// Raw errno → `StrataError` with apfs' classification.
pub(crate) fn errno(n: i32, side: IoSide, path: &Path, op: &str, outcome: &str) -> StrataError {
    io(&io::Error::from_raw_os_error(n), side, path, op, outcome)
}

/// SQLite error classification (§6.1): busy → `Busy`, corrupt → `Corrupt`,
/// read-only recovery → `ReadOnly`, everything else `Unknown` with `outcome`.
pub(crate) fn sql(e: rusqlite::Error, op: &str, outcome: &str) -> StrataError {
    match &e {
        rusqlite::Error::SqliteFailure(f, msg) => {
            let text = msg.clone().unwrap_or_else(|| f.to_string());
            match f.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => err(
                    Code::Busy,
                    format!("SQLite write lock held by another process: {text}"),
                    json!({ "lock": "sqlite", "op": op, "outcome": NOT_WRITTEN }),
                ),
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => err(
                    Code::Corrupt,
                    format!("SQLite reports a damaged database: {text}"),
                    json!({ "reason": "sqlite", "op": op, "message": text }),
                ),
                ErrorCode::ReadOnly if f.extended_code == rusqlite::ffi::SQLITE_READONLY_RECOVERY => err(
                    Code::ReadOnly,
                    "the WAL needs recovery, which a read-only connection cannot do; start the daemon first",
                    json!({ "reason": "recovery_needed" }),
                ),
                _ => err(
                    Code::Unknown,
                    format!("SQLite error during {op}: {text}"),
                    json!({ "op": op, "outcome": outcome, "sqlite_code": f.extended_code, "message": text }),
                ),
            }
        }
        other => err(Code::Unknown, format!("SQLite error during {op}: {other}"), json!({ "op": op, "outcome": outcome })),
    }
}

/// Read-side SQLite errors (no outcome matters: nothing was written).
pub(crate) fn sql_read(e: rusqlite::Error) -> StrataError {
    sql(e, "read", NOT_WRITTEN)
}

pub(crate) fn json_err(e: serde_json::Error, what: &str) -> StrataError {
    bad_row(format!("bad JSON in {what}: {e}"))
}

pub(crate) type R<T> = Result<T>;

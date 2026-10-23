//! errno classification (§4.2 I) and error construction helpers.

use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};
use strata_types::{Code, StrataError};

/// Which side of an operation an `io::Error` came from (§4.2 I).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoSide {
    /// Nothing was changed: permission errors are `Unknown`, never `ReadOnly`.
    Read,
    /// Failed before anything changed: permission errors are `ReadOnly`.
    Write,
    /// Something already changed on disk (e.g. the file was created, then
    /// `write_all` failed): always `Unknown`.
    AfterWrite,
}

/// Pure mapping of an `io::Error` to a `StrataError` per the §4.2 I table.
/// `detail` carries `errno` (number) and `path` (string), plus `reason` where
/// the table names one.
pub fn classify_io_error(err: &io::Error, side: IoSide, path: &Path) -> StrataError {
    let Some(errno) = err.raw_os_error() else {
        let mut e = StrataError::new(Code::Unknown, format!("{}: {err}", path.display()));
        e.detail = Some(json!({ "path": path.to_string_lossy(), "io_kind": format!("{:?}", err.kind()) }));
        return e;
    };
    classify_errno(errno, side, path)
}

pub(crate) fn classify_errno(errno: i32, side: IoSide, path: &Path) -> StrataError {
    let (code, reason): (Code, Option<&str>) = if side == IoSide::AfterWrite {
        (Code::Unknown, None)
    } else {
        match errno {
            libc::ENOENT => (Code::NoSuchPath, None),
            libc::EEXIST => (Code::Exists, None),
            libc::ENOTDIR => (Code::NotDirectory, None),
            libc::EISDIR => (Code::IsDirectory, None),
            libc::ENOTEMPTY => (Code::NotEmpty, None),
            libc::EXDEV => (Code::CrossVolume, None),
            libc::ENOTSUP | libc::EOPNOTSUPP => (Code::Unsupported, None),
            libc::ENAMETOOLONG | libc::EINVAL => (Code::InvalidInput, None),
            libc::EACCES | libc::EPERM => match side {
                IoSide::Write => (Code::ReadOnly, Some("no_permission")),
                _ => (Code::Unknown, None),
            },
            libc::EROFS => match side {
                IoSide::Write => (Code::ReadOnly, Some("read_only_volume")),
                _ => (Code::Unknown, None),
            },
            libc::EBUSY => (Code::Unknown, Some("busy")),
            _ => (Code::Unknown, None),
        }
    };
    errno_error(code, errno, path, reason)
}

/// Build an error whose message names the errno and the path.
pub(crate) fn errno_error(code: Code, errno: i32, path: &Path, reason: Option<&str>) -> StrataError {
    let text = io::Error::from_raw_os_error(errno);
    let msg = match reason {
        Some(r) => format!("{}: {text} ({r})", path.display()),
        None => format!("{}: {text}", path.display()),
    };
    let mut d = Map::new();
    d.insert("errno".into(), json!(errno));
    d.insert("path".into(), json!(path.to_string_lossy()));
    if let Some(r) = reason {
        d.insert("reason".into(), json!(r));
    }
    StrataError::new(code, msg).with_detail(Value::Object(d))
}

/// Error without an errno (a refusal decided by this crate).
pub(crate) fn refusal(code: Code, path: &Path, reason: &str, msg: &str) -> StrataError {
    StrataError::new(code, format!("{}: {msg}", path.display()))
        .with_detail(json!({ "path": path.to_string_lossy(), "reason": reason }))
}

/// Insert or replace one detail key.
pub(crate) fn set_detail(e: &mut StrataError, key: &str, v: Value) {
    match &mut e.detail {
        Some(Value::Object(m)) => {
            m.insert(key.into(), v);
        }
        _ => {
            let mut m = Map::new();
            m.insert(key.into(), v);
            e.detail = Some(Value::Object(m));
        }
    }
}

pub(crate) fn with(mut e: StrataError, key: &str, v: Value) -> StrataError {
    set_detail(&mut e, key, v);
    e
}

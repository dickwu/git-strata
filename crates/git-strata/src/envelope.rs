//! The stdout envelope (cli.md §4): exactly one compact JSON line
//! `{"ok":…,"data":…,"error":…}` followed by one `\n` (D12). All three keys
//! are always present; `error` is `null` on success and `data` is never
//! `null`. Writing and the explicit flush are both checked: a failure is
//! exit code 1 with `stdout_write_failed` on stderr, never a success.

use std::io::{self, Write};

use serde_json::{Map, Value};
use strata_types::{Code, StrataError};

/// One call's outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub ok: bool,
    pub data: Value,
    pub error: Option<StrataError>,
}

impl Envelope {
    /// Success; a `null` result becomes `{}`.
    pub fn ok(data: Value) -> Envelope {
        Envelope { ok: true, data: non_null(data), error: None }
    }

    /// Failure with no result: `data` is `{}`.
    pub fn err(error: StrataError) -> Envelope {
        Envelope { ok: false, data: Value::Object(Map::new()), error: Some(error) }
    }

    /// Failure that still carries a result (a publish whose push timed
    /// out keeps `data.pushes`, §7 `envelope_keys_always_present`).
    pub fn err_with(error: StrataError, data: Value) -> Envelope {
        Envelope { ok: false, data: non_null(data), error: Some(error) }
    }

    pub fn from_result(r: Result<Value, StrataError>) -> Envelope {
        match r {
            Ok(v) => Envelope::ok(v),
            Err(e) => Envelope::err(e),
        }
    }

    /// The error as JSON (`null` on success).
    pub fn error_json(&self) -> Value {
        match &self.error {
            None => Value::Null,
            Some(e) => error_value(e),
        }
    }

    /// The line written to stdout, `\n` included: the three keys in a
    /// fixed order (`ok`, `data`, `error`), compact.
    pub fn line(&self) -> String {
        format!("{{\"ok\":{},\"data\":{},\"error\":{}}}\n", self.ok, self.data, self.error_json())
    }

    /// The process exit code once the envelope was (or was not) written.
    pub fn exit_code(&self, written: &io::Result<()>) -> i32 {
        if self.ok && written.is_ok() { 0 } else { 1 }
    }
}

fn non_null(v: Value) -> Value {
    if v.is_null() { Value::Object(Map::new()) } else { v }
}

/// `{code, message, detail?}`; the code name comes from types' `Code`.
pub fn error_value(e: &StrataError) -> Value {
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code_name(e.code).to_string()));
    m.insert("message".into(), Value::String(e.message.clone()));
    if let Some(d) = &e.detail {
        m.insert("detail".into(), d.clone());
    }
    Value::Object(m)
}

/// The variant name of `code`, read from types (no second copy here).
pub fn code_name(code: Code) -> &'static str {
    code.as_str()
}

/// Write the line with `write_fmt` (the formatting path) and flush
/// explicitly: a `BufWriter`'s drop would swallow a flush failure.
pub fn emit(env: &Envelope, out: &mut dyn Write) -> io::Result<()> {
    let line = env.line();
    out.write_fmt(format_args!("{line}"))?;
    out.flush()
}

/// Emit on the real stdout; on failure say `stdout_write_failed` on
/// stderr. Returns the exit code.
pub fn emit_stdout(env: &Envelope) -> i32 {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    let r = emit(env, &mut lock);
    if let Err(e) = &r {
        let _ = writeln!(io::stderr(), "git-strata: stdout_write_failed: {e}");
    }
    env.exit_code(&r)
}

//! The metadata engine: a small synchronous facade over Turso (crate
//! `turso`, the Rust rewrite of SQLite). See docs/impl-progress/store.md,
//! "Engine: Turso".
//!
//! Turso's API is async. The store's API is sync and every call is short, so
//! the bridge is a minimal owned `block_on`: poll the future on the calling
//! thread, park until Turso's waker fires (or 1 ms, whichever is first) and
//! poll again. No runtime, no extra threads, nothing shared between calls.
//! Turso's I/O on macOS is synchronous syscalls driven from `step`, so the
//! future always makes progress when polled.
//!
//! The shapes mirror the rusqlite subset the store used before (`query_row`,
//! `prepare(..).query_map`, `params!`, `OptionalExtension`) so the SQL in the
//! modules did not change when the engine did.

use std::future::Future;
use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

pub use turso::Value;

pub type Error = turso::Error;
pub type SqlResult<T> = std::result::Result<T, Error>;

struct Unpark(std::thread::Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark()
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark()
    }
}

/// Run one Turso future to completion on this thread.
pub fn block_on<F: Future>(f: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

// ------------------------------------------------------------- values

/// A value that can be bound as a statement parameter.
pub trait ToSql {
    fn to_sql(&self) -> SqlResult<Value>;
}

macro_rules! to_sql_int {
    ($($t:ty),*) => {$(
        impl ToSql for $t {
            fn to_sql(&self) -> SqlResult<Value> {
                i64::try_from(*self)
                    .map(Value::Integer)
                    .map_err(|_| Error::ToSqlConversionFailure(format!("{} does not fit in an i64", self).into()))
            }
        }
    )*};
}
to_sql_int!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl ToSql for bool {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(Value::Integer(*self as i64))
    }
}
impl ToSql for f64 {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(Value::Real(*self))
    }
}
impl ToSql for str {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(Value::Text(self.to_string()))
    }
}
impl ToSql for String {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(Value::Text(self.clone()))
    }
}
impl ToSql for [u8] {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(Value::Blob(self.to_vec()))
    }
}
impl ToSql for Vec<u8> {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(Value::Blob(self.clone()))
    }
}
impl ToSql for Value {
    fn to_sql(&self) -> SqlResult<Value> {
        Ok(self.clone())
    }
}
impl<T: ToSql + ?Sized> ToSql for &T {
    fn to_sql(&self) -> SqlResult<Value> {
        (**self).to_sql()
    }
}
impl<T: ToSql> ToSql for Option<T> {
    fn to_sql(&self) -> SqlResult<Value> {
        match self {
            Some(v) => v.to_sql(),
            None => Ok(Value::Null),
        }
    }
}

/// A column value that can be read out of a row.
pub trait FromSql: Sized {
    fn from_sql(v: Value) -> SqlResult<Self>;
}

fn conv(what: &str, v: &Value) -> Error {
    Error::ConversionFailure(format!("cannot read {v:?} as {what}"))
}

impl FromSql for Value {
    fn from_sql(v: Value) -> SqlResult<Self> {
        Ok(v)
    }
}
impl FromSql for i64 {
    fn from_sql(v: Value) -> SqlResult<Self> {
        match v {
            Value::Integer(i) => Ok(i),
            o => Err(conv("i64", &o)),
        }
    }
}
macro_rules! from_sql_int {
    ($($t:ty),*) => {$(
        impl FromSql for $t {
            fn from_sql(v: Value) -> SqlResult<Self> {
                match v {
                    Value::Integer(i) => <$t>::try_from(i).map_err(|_| conv(stringify!($t), &Value::Integer(i))),
                    o => Err(conv(stringify!($t), &o)),
                }
            }
        }
    )*};
}
from_sql_int!(i32, u32, u64, usize);
impl FromSql for bool {
    fn from_sql(v: Value) -> SqlResult<Self> {
        match v {
            Value::Integer(i) => Ok(i != 0),
            o => Err(conv("bool", &o)),
        }
    }
}
impl FromSql for f64 {
    fn from_sql(v: Value) -> SqlResult<Self> {
        match v {
            Value::Real(f) => Ok(f),
            Value::Integer(i) => Ok(i as f64),
            o => Err(conv("f64", &o)),
        }
    }
}
impl FromSql for String {
    fn from_sql(v: Value) -> SqlResult<Self> {
        match v {
            Value::Text(s) => Ok(s),
            o => Err(conv("text", &o)),
        }
    }
}
impl FromSql for Vec<u8> {
    fn from_sql(v: Value) -> SqlResult<Self> {
        match v {
            Value::Blob(b) => Ok(b),
            o => Err(conv("blob", &o)),
        }
    }
}
impl<T: FromSql> FromSql for Option<T> {
    fn from_sql(v: Value) -> SqlResult<Self> {
        match v {
            Value::Null => Ok(None),
            o => T::from_sql(o).map(Some),
        }
    }
}

// ------------------------------------------------------------- params

/// Statement parameters: `()`, `[x; N]`, `params![..]`, or a `Vec<Value>`.
pub trait Params {
    fn values(self) -> SqlResult<Vec<Value>>;
}

impl Params for () {
    fn values(self) -> SqlResult<Vec<Value>> {
        Ok(Vec::new())
    }
}
/// `[]`: no parameters (one concrete type, so `[]` infers).
impl Params for [&dyn ToSql; 0] {
    fn values(self) -> SqlResult<Vec<Value>> {
        Ok(Vec::new())
    }
}
macro_rules! params_arrays {
    ($($n:literal)*) => {$(
        impl<T: ToSql> Params for [T; $n] {
            fn values(self) -> SqlResult<Vec<Value>> {
                self.iter().map(ToSql::to_sql).collect()
            }
        }
    )*};
}
params_arrays!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16);
impl Params for &[&dyn ToSql] {
    fn values(self) -> SqlResult<Vec<Value>> {
        self.iter().map(|v| v.to_sql()).collect()
    }
}
impl<const N: usize> Params for &[&dyn ToSql; N] {
    fn values(self) -> SqlResult<Vec<Value>> {
        self.iter().map(|v| v.to_sql()).collect()
    }
}
impl Params for Vec<Value> {
    fn values(self) -> SqlResult<Vec<Value>> {
        Ok(self)
    }
}

/// `params![a, b, c]`, as in rusqlite.
#[macro_export]
#[doc(hidden)]
macro_rules! __strata_params {
    () => { &[] as &[&dyn $crate::sql::ToSql] };
    ($($p:expr),+ $(,)?) => { &[$(&$p as &dyn $crate::sql::ToSql),+] as &[&dyn $crate::sql::ToSql] };
}
pub use crate::__strata_params as params;

// ------------------------------------------------------------- rows

/// One result row.
#[derive(Debug)]
pub struct Row(turso::Row);

impl Row {
    pub fn get<I: RowIndex, T: FromSql>(&self, idx: I) -> SqlResult<T> {
        T::from_sql(self.0.get_value(idx.index())?)
    }
    pub fn column_count(&self) -> usize {
        self.0.column_count()
    }
}

pub trait RowIndex {
    fn index(self) -> usize;
}
impl RowIndex for usize {
    fn index(self) -> usize {
        self
    }
}

/// A fully read result set (store queries are small and bounded).
pub struct Rows {
    rows: std::vec::IntoIter<Row>,
    cur: Option<Row>,
}

impl Rows {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> SqlResult<Option<&Row>> {
        self.cur = self.rows.next();
        Ok(self.cur.as_ref())
    }
}

/// `query_row(..).optional()`.
pub trait OptionalExtension<T> {
    fn optional(self) -> SqlResult<Option<T>>;
}

impl<T> OptionalExtension<T> for SqlResult<T> {
    fn optional(self) -> SqlResult<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ------------------------------------------------------------- connection

/// How to open the database file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The live store: multi-process WAL coordination on (every process
    /// that opens a store must use the same mode).
    Shared,
    /// A private file nobody else opens (the init temp db).
    Private,
}

/// One Turso connection plus the database it belongs to.
pub struct Connection {
    conn: turso::Connection,
    db: turso::Database,
    path: String,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").field("path", &self.path).finish()
    }
}

/// Open (or create) the database at `path`.
///
/// Never uses Turso's `read_only` flag: Turso keeps one `Database` per file
/// per process and the first opener's flags win, so a read-only open would
/// make a later writer in the same process read-only (and vice versa).
/// Read-only handles are enforced with `PRAGMA query_only = 1` instead.
pub fn open(path: &Path, mode: Mode) -> SqlResult<Connection> {
    let p = path.to_str().ok_or_else(|| Error::Misuse(format!("database path is not UTF-8: {}", path.display())))?;
    #[allow(unused_mut)]
    let mut b = turso::Builder::new_local(p).experimental_multiprocess_wal(mode == Mode::Shared);
    #[cfg(feature = "test-seams")]
    {
        b = b.with_io_impl(crate::vfs::io());
    }
    let db = block_on(b.build())?;
    let conn = db.connect()?;
    Ok(Connection { conn, db, path: p.to_string() })
}

/// A private in-memory database (the schema reference).
pub fn open_in_memory() -> SqlResult<Connection> {
    let db = block_on(turso::Builder::new_local(":memory:").build())?;
    let conn = db.connect()?;
    Ok(Connection { conn, db, path: ":memory:".into() })
}

impl Connection {
    /// Another connection to the same database.
    pub fn connect(&self) -> SqlResult<Connection> {
        Ok(Connection { conn: self.db.connect()?, db: self.db.clone(), path: self.path.clone() })
    }

    pub fn path(&self) -> Option<&str> {
        Some(&self.path)
    }

    pub fn execute_batch(&self, sql: &str) -> SqlResult<()> {
        block_on(self.conn.execute_batch(sql))
    }

    pub fn execute<P: Params>(&self, sql: &str, p: P) -> SqlResult<usize> {
        let v = p.values()?;
        block_on(async {
            let mut st = self.conn.prepare(sql).await?;
            st.execute(v).await
        })
        .map(|n| n as usize)
    }

    fn collect(&self, sql: &str, v: Vec<Value>, limit: Option<usize>) -> SqlResult<Vec<Row>> {
        block_on(async {
            let mut st = self.conn.prepare(sql).await?;
            let mut rows = st.query(v).await?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().await? {
                out.push(Row(r));
                if limit.is_some_and(|l| out.len() >= l) {
                    break;
                }
            }
            Ok(out)
        })
    }

    pub fn query_row<T, P: Params, F: FnOnce(&Row) -> SqlResult<T>>(&self, sql: &str, p: P, f: F) -> SqlResult<T> {
        let rows = self.collect(sql, p.values()?, None)?;
        match rows.first() {
            Some(r) => f(r),
            None => Err(Error::QueryReturnedNoRows),
        }
    }

    pub fn prepare(&self, sql: &str) -> SqlResult<Statement<'_>> {
        Ok(Statement { conn: self, sql: sql.to_string() })
    }

    pub fn is_autocommit(&self) -> bool {
        self.conn.is_autocommit().unwrap_or(false)
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.conn.last_insert_rowid()
    }

    pub fn busy_timeout(&self, d: Duration) -> SqlResult<()> {
        self.conn.busy_timeout(d)
    }

    pub fn close(self) -> SqlResult<()> {
        if !self.conn.is_autocommit().unwrap_or(true) {
            let _ = self.execute_batch("ROLLBACK");
        }
        drop(self);
        Ok(())
    }
}

/// A statement (re-prepared per call; Turso caches nothing we rely on).
pub struct Statement<'c> {
    conn: &'c Connection,
    sql: String,
}

impl Statement<'_> {
    pub fn query_map<T, P: Params, F: FnMut(&Row) -> SqlResult<T>>(
        &mut self,
        p: P,
        mut f: F,
    ) -> SqlResult<std::vec::IntoIter<SqlResult<T>>> {
        let rows = self.conn.collect(&self.sql, p.values()?, None)?;
        Ok(rows.iter().map(&mut f).collect::<Vec<_>>().into_iter())
    }

    pub fn query<P: Params>(&mut self, p: P) -> SqlResult<Rows> {
        let rows = self.conn.collect(&self.sql, p.values()?, None)?;
        Ok(Rows { rows: rows.into_iter(), cur: None })
    }

    /// Number of result columns (prepares the statement).
    pub fn column_count(&self) -> SqlResult<usize> {
        block_on(async { Ok(self.conn.conn.prepare(&self.sql).await?.column_count()) })
    }

    pub fn exists<P: Params>(&mut self, p: P) -> SqlResult<bool> {
        Ok(!self.conn.collect(&self.sql, p.values()?, Some(1))?.is_empty())
    }

    pub fn execute<P: Params>(&mut self, p: P) -> SqlResult<usize> {
        self.conn.execute(&self.sql, p)
    }
}

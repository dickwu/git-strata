//! The write-transaction helper (store.md §5.1, §6.4): L2, `BEGIN IMMEDIATE`,
//! commit classification and writer poisoning; store-assigned sequence numbers.

use std::sync::atomic::Ordering;

use rusqlite::{Connection, ErrorCode, OptionalExtension};
use serde_json::json;
use strata_types::Code;

use crate::errors::{NOT_WRITTEN, UNCERTAIN, err, poisoned, read_only_handle, sql, too_large};
use crate::seams::Seam;
use crate::{Result, Store};

/// What a write body wants done with its transaction.
pub(crate) enum Tx<T> {
    Commit(T),
    /// Nothing to write (a compare-and-swap that did not match).
    Rollback(T),
}

impl Store {
    /// Write-side precondition: read-only handles first, then poisoning.
    pub(crate) fn writable(&self) -> Result<()> {
        if self.read_only {
            return Err(read_only_handle());
        }
        if self.poisoned.load(Ordering::SeqCst) {
            return Err(poisoned());
        }
        Ok(())
    }

    /// Run `f` in one `BEGIN IMMEDIATE` write transaction under L2.
    /// Statement errors inside `f` roll back (`outcome = not_written`); an
    /// I/O error on COMMIT itself is `Unknown` / `uncertain` and poisons
    /// the writer (§6.4).
    pub(crate) fn write<T>(&self, op: &str, f: impl FnOnce(&Connection) -> Result<Tx<T>>) -> Result<T> {
        self.writable()?;
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        // Re-check under L2: another writer may have poisoned us meanwhile.
        if self.poisoned.load(Ordering::SeqCst) {
            return Err(poisoned());
        }
        if !db.is_autocommit() {
            // A previous failure left a transaction open; never build on it.
            let _ = db.execute_batch("ROLLBACK");
        }
        db.execute_batch("BEGIN IMMEDIATE").map_err(|e| sql(e, op, NOT_WRITTEN))?;
        let out = f(&db);
        let v = match out {
            Ok(Tx::Commit(v)) => v,
            Ok(Tx::Rollback(v)) => {
                rollback(&db);
                return Ok(v);
            }
            Err(e) => {
                rollback(&db);
                return Err(e);
            }
        };
        self.seams.pause_if(&Seam::PauseBeforeCommit);
        // CommitSyncFails: the instrumented VFS fails the next WAL xSync.
        #[cfg(feature = "test-seams")]
        let fail = self.seams.hit(&Seam::CommitSyncFails).then(|| {
            let c = crate::vfs::counters(db.path().unwrap_or_default());
            c.fail_next.store(true, Ordering::SeqCst);
            c
        });
        let res = db.execute_batch("COMMIT");
        #[cfg(feature = "test-seams")]
        if let Some(c) = fail {
            c.fail_next.store(false, Ordering::SeqCst);
        }
        match res {
            Ok(()) => Ok(v),
            Err(e) => {
                let definite = matches!(
                    &e,
                    rusqlite::Error::SqliteFailure(f, _)
                        if matches!(f.code, ErrorCode::ConstraintViolation | ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                );
                if definite {
                    // Deferred FK violation or lock: SQLite left the
                    // transaction open and nothing reached the WAL.
                    rollback(&db);
                    Err(sql(e, op, NOT_WRITTEN))
                } else {
                    Err(self.commit_failed(&db, op, e.to_string()))
                }
            }
        }
    }

    fn commit_failed(&self, db: &Connection, op: &str, msg: String) -> strata_types::StrataError {
        self.poisoned.store(true, Ordering::SeqCst);
        rollback(db);
        err(
            Code::Unknown,
            format!("COMMIT of {op} failed; whether it reached the disk is unknown: {msg}"),
            json!({ "op": op, "outcome": UNCERTAIN, "reason": "commit_failed", "message": msg }),
        )
    }
}

fn rollback(db: &Connection) {
    if !db.is_autocommit() {
        let _ = db.execute_batch("ROLLBACK");
    }
}

/// The next AUTOINCREMENT value for `table` (never reused, §4.4); at the
/// top of i64 → `TooLarge` with `limit`.
pub(crate) fn next_seq(c: &Connection, table: &str, limit: &str, op: &str) -> Result<i64> {
    let cur: Option<i64> = c
        .query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| r.get(0))
        .optional()
        .map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let max: Option<i64> = c
        .query_row(&format!("SELECT max(id) FROM {table}"), [], |r| r.get(0))
        .map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let top = cur.unwrap_or(0).max(max.unwrap_or(0)).max(0);
    top.checked_add(1).ok_or_else(|| too_large(limit))
}

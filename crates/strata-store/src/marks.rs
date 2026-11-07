//! Completion marks (schema v7). Older read-only stores have no marks.

use std::time::SystemTime;

use crate::codec::{ns, time};
use crate::errors::{NOT_WRITTEN, bad_row, sql, sql_read};
use crate::rm_state::user_version;
use crate::sessions::session_lifecycle;
use crate::sql::{OptionalExtension, params};
use crate::txn::Tx;
use crate::{Result, Store};
use serde_json::json;
use strata_types::{
    Code, MarkBy, MarkByKind, MarkState, SessionId, SessionMark, StrataError, validate_mark_note,
};

impl Store {
    pub fn mark(&self, session: &SessionId) -> Result<Option<SessionMark>> {
        self.read(|c| {
            if user_version(c)? < 7 {
                return Ok(None);
            }
            let row = c
                .query_row(
                    "SELECT state, note, by_kind, at_ns FROM session_marks WHERE session = ?1",
                    [session.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_read)?;
            row.map(|(state, note, by, at)| {
                if state != "done" {
                    return Err(bad_row(format!("session_marks.state {state:?}")));
                }
                let kind = match by.as_str() {
                    "agent" => MarkByKind::Agent,
                    "cli" => MarkByKind::Cli,
                    "gui" => MarkByKind::Gui,
                    "daemon" => MarkByKind::Daemon,
                    _ => return Err(bad_row(format!("session_marks.by_kind {by:?}"))),
                };
                Ok(SessionMark {
                    state: MarkState::Done,
                    note,
                    by: MarkBy { kind },
                    at: time(at),
                })
            })
            .transpose()
        })
    }

    /// The caller holds the session queue; the lifecycle check and mark write
    /// also share one store transaction, including undo of an absent mark.
    pub fn set_mark(
        &self,
        session: &SessionId,
        state: MarkState,
        note: Option<&str>,
        by: MarkByKind,
        at: SystemTime,
    ) -> Result<Option<SessionMark>> {
        const OP: &str = "set_mark";
        self.writable()?;
        validate_mark_note(note)?;
        if state == MarkState::Active && note.is_some() {
            return Err(
                StrataError::new(Code::InvalidInput, "mark active does not take a note")
                    .with_detail(json!({ "reason": "note" })),
            );
        }
        let at_ns = ns(at, "at")?;
        let fail = |e| sql(e, OP, NOT_WRITTEN);
        self.write(OP, |c| {
            match session_lifecycle(c, session.as_str()).map_err(fail)?.as_deref() {
                None => return Err(StrataError::new(Code::NoSuchSession, format!("no session {session}"))),
                Some("active" | "unknown") => {}
                Some(lifecycle) => {
                    return Err(StrataError::new(Code::InvalidInput, "mark requires an Active or Unknown session")
                        .with_detail(json!({ "reason": "lifecycle", "session": session, "lifecycle": lifecycle })));
                }
            }
            if state == MarkState::Active {
                c.execute("DELETE FROM session_marks WHERE session = ?1", [session.as_str()])
                    .map_err(fail)?;
                return Ok(Tx::Commit(None));
            }
            let kind = match by {
                MarkByKind::Agent => "agent",
                MarkByKind::Cli => "cli",
                MarkByKind::Gui => "gui",
                MarkByKind::Daemon => "daemon",
            };
            c.execute(
                "INSERT INTO session_marks (session, state, note, by_kind, at_ns) VALUES (?1, 'done', ?2, ?3, ?4) \
                 ON CONFLICT (session) DO UPDATE SET note = excluded.note, by_kind = excluded.by_kind, at_ns = excluded.at_ns",
                params![session.as_str(), note, kind, at_ns],
            )
            .map_err(fail)?;
            Ok(Tx::Commit(Some(SessionMark {
                state,
                note: note.map(str::to_owned),
                by: MarkBy { kind: by },
                at,
            })))
        })
    }
}

//! The store-only session summary (status-perf C): what `ls` and
//! `status --brief` print, from the daemon (`status{summary:true}`) or the
//! local read-only path. Reads the store only: no workspace stat or
//! canonicalize, no git, no tree walk. The cwd selector reads the real
//! path of the cwd and of `<root>/sessions/` once each (`resolve_in`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_store::{Health, Store};
use strata_types::{CheckpointId, Code, RepoName, SessionId, StrataError};

use crate::rm_note::{RmNotes, rm_notes};
use crate::resolve::{live_sorted, resolve_in, select_in};
use crate::{Result, SessionState, WorkspaceHealth};

/// Which sessions a summary covers.
#[derive(Clone, Copy, Debug)]
pub enum SummarySelect<'a> {
    /// Every live session.
    All,
    /// `--session`: an id, or a ticket among live sessions.
    Session(&'a str),
    /// The session whose workspace holds this directory.
    Cwd(&'a Path),
}

/// One session in `ls` / `status --brief`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    /// Null when the session has none (always present, as in `SessionStatus`).
    pub ticket: Option<String>,
    /// The store lifecycle; never `Missing` (that needs the filesystem).
    pub state: SessionState,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub created: SystemTime,
    pub repos: Vec<RepoName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Only for `status --brief` (`workspaces:true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<WorkspaceSummary>>,
    /// rm-followups R3: last rm, interrupted teardown, deferred rebases
    /// (store reads plus two `lstat`s for an interrupted teardown).
    #[serde(flatten, default)]
    pub rm: RmNotes,
}

/// One workspace in `status --brief`, from the store alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub repo: RepoName,
    /// The health the store records (`Ok` or `Unknown`); a missing
    /// directory is not seen here.
    pub health_stored: WorkspaceHealth,
    pub path: PathBuf,
    /// The latest checkpoint; none when untracked or never checkpointed.
    pub checkpoint: Option<CheckpointId>,
}

/// The summaries `sel` names, by session id. A cwd in no session's
/// workspace is `NoSuchSession` (as the daemon's `status`).
pub fn summary(
    store: &Store,
    root: &Path,
    sel: SummarySelect<'_>,
    workspaces: bool,
) -> Result<Vec<SessionSummary>> {
    let mut recs = match sel {
        SummarySelect::All => live_sorted(store)?,
        SummarySelect::Session(s) => {
            let id = select_in(store, s).map_err(|e| crate::rm_note::with_leftover_detail(store, root, s, e))?;
            store.session(&id)?.into_iter().collect()
        }
        SummarySelect::Cwd(cwd) => match resolve_in(store, root, cwd)? {
            Some((id, _)) => store.session(&id)?.into_iter().collect(),
            None => {
                return Err(StrataError::new(
                    Code::NoSuchSession,
                    format!("{} is in no session's workspace", cwd.display()),
                )
                .with_detail(json!({ "path": cwd.to_string_lossy() })));
            }
        },
    };
    recs.sort_by(|a, b| a.manifest.id.as_str().cmp(b.manifest.id.as_str()));
    let mut out = Vec::with_capacity(recs.len());
    for rec in recs {
        let ws = if workspaces {
            let mut latest: BTreeMap<RepoName, CheckpointId> = BTreeMap::new();
            for c in store.checkpoints(&rec.manifest.id, None)? {
                let e = latest.entry(c.repo.clone()).or_insert(c.id);
                if c.id > *e {
                    *e = c.id;
                }
            }
            let mut v = Vec::new();
            for (repo, st) in &rec.manifest.repos {
                let health = match rec.workspaces.get(repo).map(|w| w.health) {
                    Some(Health::Unknown) => WorkspaceHealth::Unknown,
                    _ => WorkspaceHealth::Ok,
                };
                v.push(WorkspaceSummary {
                    repo: repo.clone(),
                    health_stored: health,
                    path: st.path.clone(),
                    checkpoint: if st.tracked {
                        latest.get(repo).copied()
                    } else {
                        None
                    },
                });
            }
            Some(v)
        } else {
            None
        };
        let rm = rm_notes(store, root, &rec)?;
        out.push(SessionSummary {
            id: rec.manifest.id.clone(),
            ticket: rec.manifest.ticket.clone(),
            state: SessionState::from(rec.lifecycle),
            created: rec.created,
            repos: rec.manifest.repos.keys().cloned().collect(),
            phase: rec.phase.clone(),
            workspaces: ws,
            rm,
        });
    }
    Ok(out)
}

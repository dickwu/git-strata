//! Unknown scenes (§6.1 rule 3, §6.3): what an operation left behind after
//! its first visible write, stored as JSON on the session or workspace row
//! so a daemon restart reports the same scene.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::Result;
use crate::sys::lstat;

/// An Unknown scene, stored as JSON on the session or workspace row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub op: SceneOp,
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoName>,
    pub phase: String,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub at: SystemTime,
    pub kept: Vec<PathBuf>,
    /// The Unknown error returned to the caller then (§6.3).
    pub error: StrataError,
}

/// Serialised lowercase, the same as the Unknown's `detail.op` (M19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SceneOp {
    New,
    Move,
    Remove,
}

impl SceneOp {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SceneOp::New => "new",
            SceneOp::Move => "move",
            SceneOp::Remove => "remove",
        }
    }
}

/// A stored scene back as a `Scene`; a row that does not parse is `Corrupt`.
pub(crate) fn scene_of(v: &Value, what: &str) -> Result<Scene> {
    serde_json::from_value(v.clone()).map_err(|e| {
        StrataError::new(Code::Corrupt, format!("the stored {what} scene does not parse: {e}"))
            .with_detail(json!({ "field": "scene", "of": what }))
    })
}

/// `detail.cause` (§6.3): the lower error as it was (code, message,
/// detail). An I/O error also carries its errno at the top level, so the
/// errno and the original text are both there.
pub(crate) fn cause_of(e: &StrataError) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or_else(|_| json!({ "message": e.message }));
    if let Some(errno) = e.detail.as_ref().and_then(|d| d.get("errno")).cloned()
        && let Value::Object(m) = &mut v
    {
        m.insert("errno".into(), errno);
    }
    v
}

fn joined(kept: &[PathBuf]) -> String {
    if kept.is_empty() {
        return "nothing".into();
    }
    kept.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

/// The scene's Unknown error (§6.3): fixed fields `op`, `session`, `phase`,
/// `cause` and `kept`, plus `extra` (e.g. `repo` and `path` for `new`).
/// `msg_repo` adds `/<repo>` to the message (operations on one repo).
pub(crate) fn unknown_error(
    op: SceneOp,
    session: &SessionId,
    msg_repo: Option<&RepoName>,
    phase: &str,
    cause: Value,
    kept: &[PathBuf],
    extra: Map<String, Value>,
) -> StrataError {
    let who = match msg_repo {
        Some(r) => format!("{session}/{r}"),
        None => session.to_string(),
    };
    let message = format!(
        "{} {who}: {phase} failed after writing began; kept {}; nothing was rolled back, cleaned up or retried; see git-strata status {session}",
        op.as_str(),
        joined(kept),
    );
    let mut d = Map::new();
    d.insert("op".into(), json!(op.as_str()));
    d.insert("session".into(), json!(session.as_str()));
    d.insert("phase".into(), json!(phase));
    d.extend(extra);
    d.insert("cause".into(), cause);
    d.insert("kept".into(), Value::from(kept.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()));
    StrataError::new(Code::Unknown, message).with_detail(Value::Object(d))
}

/// The session's directories that exist now (§5.1 "第 12–14 步的任何失败",
/// §5.8 step 1): the session root and each shadow repository directory.
pub(crate) fn kept_dirs(paths_root: &Path, session: &SessionId, repos: impl IntoIterator<Item = RepoName>) -> Vec<PathBuf> {
    let exists = |p: &Path| matches!(lstat(p), Ok(Some(_)));
    let mut kept = Vec::new();
    let root = paths_root.join("sessions").join(session.as_str());
    if exists(&root) {
        kept.push(root);
    }
    for r in repos {
        let sh = paths_root.join("sessions/.repos").join(session.as_str()).join(format!("{r}.git"));
        if exists(&sh) {
            kept.push(sh);
        }
    }
    kept
}

/// The scene a restart finds (§5.8 step 1): the process stopped during
/// `phase`; there is no lower error to report.
pub(crate) fn interrupted(op: SceneOp, session: &SessionId, phase: &str, kept: &[PathBuf]) -> StrataError {
    let cause = json!({ "reason": "interrupted", "message": "the process stopped before this phase finished" });
    let mut extra = Map::new();
    extra.insert("reason".into(), json!("interrupted"));
    unknown_error(op, session, None, phase, cause, kept, extra)
}

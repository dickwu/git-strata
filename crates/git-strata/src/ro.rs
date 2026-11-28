//! Read-only store access (cli.md §5.1, §5.2, §5.3): session selection,
//! cwd resolution and path mapping, without `Sessions::open` and without
//! any store write.

use std::path::{Path, PathBuf};

use serde_json::json;
use strata_store::{Lifecycle, SessionRecord, Store};
use strata_types::{Code, Config, RepoName, RepoPath, SessionId, StrataError};

use crate::Result;

pub fn open_store(config: &Config) -> Result<Store> {
    Store::open_read_only(&config.paths.root).map_err(|e| {
        local_version_error(e, &config.paths.root, || {
            let path = crate::env::Paths::from_env()
                .ok()?
                .config_dir
                .join("store_state.json");
            serde_json::from_slice(&std::fs::read(path).ok()?).ok()
        })
    })
}

fn local_version_error(
    mut error: StrataError,
    root: &Path,
    snapshot: impl FnOnce() -> Option<serde_json::Value>,
) -> StrataError {
    let Some(detail) = error.detail.as_mut() else {
        return error;
    };
    if error.code != Code::Corrupt || detail["reason"] != "user_version" {
        return error;
    }
    let actual = detail["actual"]
        .as_u64()
        .or_else(|| detail["actual"].as_str()?.parse().ok());
    let Some(schema) = actual.filter(|v| *v > u64::from(strata_types::STORE_SCHEMA)) else {
        return error;
    };
    // A checkpoint can fail locally before its first RPC. Use only the
    // snapshot from the writer of this root and this schema.
    let version = snapshot()
        .filter(|s| s["root"].as_str().map(Path::new) == Some(root) && s["user_version"] == schema)
        .and_then(|s| {
            s["daemon_version"]
                .as_str()
                .filter(|v| strata_types::release_version(v).is_some())
                .map(str::to_owned)
        });
    let target = match version {
        Some(v) => {
            detail["required_version"] = json!(v);
            format!("git-strata {v}")
        }
        None => format!("the git-strata release for store schema v{schema}"),
    };
    let fix = format!("use {target} from the updated installation on PATH");
    error.code = Code::Unsupported;
    error.message = format!(
        "this CLI ({}) cannot read store schema v{schema}; {fix}",
        env!("CARGO_PKG_VERSION")
    );
    detail["reason"] = json!("cli_too_old");
    detail["fix"] = json!(fix);
    error
}

/// Live sessions (not Removed), by creation time then id.
pub fn live(store: &Store) -> Result<Vec<SessionRecord>> {
    let mut v: Vec<SessionRecord> = store
        .sessions()?
        .into_iter()
        .filter(|r| r.lifecycle != Lifecycle::Removed)
        .collect();
    v.sort_by(|a, b| {
        a.created
            .cmp(&b.created)
            .then_with(|| a.manifest.id.as_str().cmp(b.manifest.id.as_str()))
    });
    Ok(v)
}

/// A session by id, or by ticket among live sessions (as `Sessions::select`).
pub fn select(store: &Store, selector: &str) -> Result<SessionId> {
    let none = || {
        StrataError::new(
            Code::NoSuchSession,
            format!("no session matches {selector:?}"),
        )
        .with_detail(json!({ "selector": selector }))
    };
    if let Ok(id) = SessionId::try_from(selector.to_string()) {
        return match store.session(&id)? {
            Some(r) if r.lifecycle != Lifecycle::Removed => Ok(id),
            _ => Err(none()),
        };
    }
    let hits: Vec<SessionRecord> = live(store)?
        .into_iter()
        .filter(|r| r.manifest.ticket.as_deref() == Some(selector))
        .collect();
    match hits.len() {
        0 => Err(none()),
        1 => Ok(hits[0].manifest.id.clone()),
        n => {
            let candidates: Vec<&str> = hits.iter().map(|r| r.manifest.id.as_str()).collect();
            Err(StrataError::new(
                Code::AmbiguousMatch,
                format!("ticket {selector:?} names {n} sessions"),
            )
            .with_detail(json!({ "ticket": selector, "candidates": candidates })))
        }
    }
}

pub fn record(store: &Store, id: &SessionId) -> Result<SessionRecord> {
    match store.session(id)? {
        Some(r) if r.lifecycle != Lifecycle::Removed => Ok(r),
        _ => Err(
            StrataError::new(Code::NoSuchSession, format!("no session {id}"))
                .with_detail(json!({ "session": id.as_str() })),
        ),
    }
}

fn real(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}

/// Where an absolute path falls: the live session workspace containing it
/// (by real path prefix) and the repo-relative path. `None` when outside
/// every workspace. A symlink as the last component names the link entry
/// itself, not its target (review P1-9: `restore link` restored the file it
/// pointed at); only when the link's own location is outside every
/// workspace is it followed (a link from elsewhere into a workspace).
pub fn locate(
    store: &Store,
    path: &Path,
) -> Result<Option<(SessionId, RepoName, Option<RepoPath>)>> {
    let leaf_is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if leaf_is_link
        && let (Some(name), Some(parent)) = (path.file_name(), path.parent())
        && let Some(mut at) = resolved(parent)
    {
        at.push(name);
        if let Some(hit) = in_workspace(store, &at)?
            && hit.2.is_some()
        {
            return Ok(Some(hit));
        }
    }
    match resolved(path) {
        Some(real_path) => in_workspace(store, &real_path),
        None => Ok(None),
    }
}

/// `path` with every existing component resolved: the path may not exist
/// (a deleted file), so resolve its nearest existing ancestor and re-append
/// the rest.
fn resolved(path: &Path) -> Option<PathBuf> {
    let mut probe = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Some(mut r) = real(&probe) {
            for t in tail.iter().rev() {
                r.push(t);
            }
            return Some(r);
        }
        match (probe.file_name().map(|s| s.to_os_string()), probe.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                probe = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

fn in_workspace(
    store: &Store,
    real_path: &Path,
) -> Result<Option<(SessionId, RepoName, Option<RepoPath>)>> {
    for rec in live(store)? {
        for (repo, ws) in &rec.manifest.repos {
            let Some(root) = real(&ws.path) else { continue };
            if let Ok(rest) = real_path.strip_prefix(&root) {
                let rel = rest.to_string_lossy().into_owned();
                let rp = if rel.is_empty() {
                    None
                } else {
                    RepoPath::try_from(rel).ok()
                };
                return Ok(Some((rec.manifest.id.clone(), repo.clone(), rp)));
            }
        }
    }
    Ok(None)
}

/// The session (and repo) of the current directory, if any.
pub fn cwd_session(store: &Store) -> Result<Option<(SessionId, RepoName)>> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(None);
    };
    Ok(locate(store, &cwd)?.map(|(s, r, _)| (s, r)))
}

/// `--session S`, else the cwd's session; neither → `NoSuchSession`.
pub fn session_or_cwd(
    store: &Store,
    selector: Option<&str>,
) -> Result<(SessionId, Option<RepoName>)> {
    if let Some(s) = selector {
        let id = select(store, s)?;
        let cwd_repo = cwd_session(store)?
            .filter(|(c, _)| *c == id)
            .map(|(_, r)| r);
        return Ok((id, cwd_repo));
    }
    match cwd_session(store)? {
        Some((s, r)) => Ok((s, Some(r))),
        None => Err(StrataError::new(
            Code::NoSuchSession,
            "the current directory is not in a session workspace; pass --session",
        )
        .with_detail(json!({ "reason": "no_cwd_session" }))),
    }
}

pub fn repo_name(name: &str) -> Result<RepoName> {
    RepoName::try_from(name.to_string()).map_err(|_| {
        StrataError::new(Code::InvalidInput, format!("{name:?} is not a repo name"))
            .with_detail(json!({ "reason": "repo_name", "repo": name }))
    })
}

/// §5.3 / D20: `{id, repo}` for each proposal still Pending for the
/// session's repos, by repo then id. Failures here read as none.
pub fn pending_proposals(config: &Config, session: &SessionId) -> Vec<serde_json::Value> {
    let Ok(store) = open_store(config) else {
        return Vec::new();
    };
    let Ok(Some(rec)) = store.session(session) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for repo in rec.manifest.repos.keys() {
        let Ok(ps) = strata_deps::open_proposals(&store, Some(repo)) else {
            continue;
        };
        for p in ps
            .iter()
            .filter(|p| p.state == strata_deps::ProposalState::Pending)
        {
            out.push(json!({ "id": p.id.get(), "repo": p.repo.as_str() }));
        }
    }
    out
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn c1_newer_local_schema_names_only_the_matching_writers_release() {
        let root = Path::new("/fixture/store-root");
        let next = strata_types::STORE_SCHEMA + 1;
        let failure = |actual| {
            StrataError::new(Code::Corrupt, "schema mismatch")
                .with_detail(json!({"reason":"user_version", "actual":actual}))
        };
        for actual in [json!(next), json!(next.to_string())] {
            let error = local_version_error(failure(actual), root, || {
                Some(json!({"root":root, "user_version":next, "daemon_version":"1.2.3"}))
            });
            assert_eq!(error.code, Code::Unsupported);
            assert!(error.message.contains("1.2.3"));
            let detail = error.detail.unwrap();
            assert_eq!(detail["reason"], "cli_too_old");
            assert_eq!(detail["required_version"], "1.2.3");
        }
        for evidence in [
            None,
            Some(json!({"root":"/other","user_version":next,"daemon_version":"1.2.3"})),
            Some(json!({"root":root, "user_version":next-1, "daemon_version":"1.2.3"})),
        ] {
            let e = local_version_error(failure(json!(next)), root, || evidence);
            assert_eq!(e.code, Code::Unsupported);
            assert!(e.detail.unwrap().get("required_version").is_none());
            assert!(e.message.contains("PATH"));
        }
        let broken = failure(json!(0));
        assert_eq!(
            local_version_error(broken.clone(), root, || panic!("unexpected lookup")),
            broken
        );
    }
}

//! Explicit/Agent move cwd policy. The request-scoped override never
//! exempts descriptors or mapped files in dependency directories.
use crate::Result;
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use strata_deps::{Hold, OpenFile};
use strata_types::{Code, StrataError};

#[derive(Clone, Debug, Default)]
pub struct MoveCwd {
    pub allow_cwd: bool,
    pub allow_cwd_holders: bool,
    pub caller_pids: Vec<i32>,
    pub daemon_pid: Option<i32>,
}
thread_local! { static POLICY: RefCell<MoveCwd> = RefCell::new(MoveCwd::default()); }

/// Request-thread scope, restored on every exit, including unwinding.
pub fn with_move_cwd<T>(policy: MoveCwd, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<MoveCwd>);
    impl Drop for Restore {
        fn drop(&mut self) {
            POLICY.with(|p| *p.borrow_mut() = self.0.take().unwrap());
        }
    }
    let _restore = Restore(Some(POLICY.with(|p| p.replace(policy))));
    f()
}

pub fn check_holders(ws: &Path) -> Result<()> {
    let policy = POLICY.with(|p| p.borrow().clone());
    let holders: Vec<_> = strata_deps::open_files(ws)?
        .into_iter()
        .filter(|h| {
            h.hold == Hold::Cwd
                && !(policy.allow_cwd && policy.caller_pids.contains(&h.pid))
                && !policy.daemon_pid.is_some_and(|pid| strata_deps::process_parent(h.pid) == Some(pid))
        })
        .collect();
    if holders.is_empty() || policy.allow_cwd_holders {
        return Ok(());
    }
    Err(StrataError::new(Code::OpenFiles, "other processes have their cwd in this workspace; nothing moved")
        .with_detail(json!({"reason":"cwd_holders", "workspace":ws, "holders":holders,
            "fix":"exit those shells or cd out, or pass --allow-cwd-holders"})))
}

pub(crate) fn held_warnings(old_tree: Option<&Path>, holders: &[OpenFile]) -> Vec<serde_json::Value> {
    let Some(path) = old_tree else { return Vec::new() };
    let mut out = Vec::new();
    for (reason, cwd) in [("cwd_stale", true), ("held_old_tree", false)] {
        let selected: Vec<&OpenFile> = holders.iter().filter(|h| (h.hold == Hold::Cwd) == cwd).collect();
        if !selected.is_empty() {
            out.push(json!({"reason":reason,"holders":selected,"old_tree":path}));
        }
    }
    out
}

/// Canonical, component-wise containment; sibling names are never prefixes.
pub fn inside(cwd: &Path, root: &Path) -> bool {
    match (std::fs::canonicalize(cwd), std::fs::canonicalize(root)) {
        (Ok(cwd), Ok(root)) => cwd.starts_with(root),
        _ => false,
    }
}

pub fn refuse_caller(
    cwd: &Path,
    session: &str,
    workspace: &Path,
    request: &serde_json::Value,
) -> StrataError {
    let mut fix = format!("cd ~ && git-strata sync --session {session}");
    for (field, flag) in [
        ("repos", "--repo"),
        ("resolved", "--resolved"),
        ("keep", "--keep"),
    ] {
        for value in request[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
        {
            fix.push_str(&format!(" {flag} {}", shell_word(value)));
        }
    }
    if let Some(target) = request["target"].as_str() {
        fix.push_str(&format!(" --target {}", shell_word(target)));
    }
    if let Some(generation) = request["generation"].as_u64() {
        fix.push_str(&format!(" --generation {generation}"));
    }
    for (field, value, flag) in [
        ("fetch", false, "--no-fetch"),
        ("accept_diverged", true, "--accept-diverged"),
        ("allow_cwd_holders", true, "--allow-cwd-holders"),
    ] {
        if request[field] == value {
            fix.push_str(&format!(" {flag}"));
        }
    }
    StrataError::new(
        Code::OpenFiles,
        "the caller's cwd is in a workspace that sync may replace",
    )
    .with_detail(
        json!({"reason":"caller_cwd", "session":session,"workspace":workspace,
            "cwd":std::fs::canonicalize(cwd).unwrap_or_else(|_|cwd.to_path_buf()),
            "fix":fix}),
    )
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub fn stale(session: &str, workspace: &Path, cwd: &Path) -> StrataError {
    StrataError::new(Code::NoSuchSession, "the caller's cwd is in an old workspace tree")
        .with_detail(json!({"reason":"stale_cwd","session":session,"workspace":workspace,"cwd":cwd,
            "fix":format!("cd {}", shell_path(workspace))}))
}
fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Staging/retained paths are recorded by move intents; resolve the actual
/// inode path first; recognized staging names also work during preparation.
pub fn stale_in(store: &strata_store::Store, root: &Path, cwd: &Path) -> Result<Option<StrataError>> {
    let Ok(real) = std::fs::canonicalize(cwd) else { return Ok(None) };
    let Ok(sessions) = std::fs::canonicalize(root.join("sessions")) else { return Ok(None) };
    let Ok(rest) = real.strip_prefix(&sessions) else { return Ok(None) };
    let Some(session) = rest.components().next().and_then(|c| c.as_os_str().to_str()) else { return Ok(None) };
    let Ok(id) = strata_types::SessionId::try_from(session.to_string()) else { return Ok(None) };
    let Some(rec) = store.session(&id)? else { return Ok(None) };
    if rec.lifecycle == strata_store::Lifecycle::Removed {
        return Ok(None);
    }
    let segment = rest.components().nth(1).and_then(|c| c.as_os_str().to_str()).unwrap_or("");
    if rec.manifest.repos.keys().any(|r| r.as_str() == segment) {
        return Ok(None);
    }
    for intent in store.move_intents(&id)? {
        let path: &PathBuf = intent.old_tree.as_ref().unwrap_or(&intent.staging);
        if inside(&real, path)
            && let Some(ws) = rec.manifest.repos.get(&intent.repo)
        {
            return Ok(Some(stale(id.as_str(), &ws.path, &real)));
        }
    }
    if let Some((_, ws)) = rec.manifest.repos.iter()
        .filter(|(repo, _)| segment.starts_with(&format!(".strata-tmp-move-{repo}-")))
        .max_by_key(|(repo, _)| repo.as_str().len()) {
        return Ok(Some(stale(id.as_str(), &ws.path, &real)));
    }
    Ok(None)
}

//! Additive params with strict old-daemon parser rejection fallback.
use crate::{Result, args::CwdArgs, ctx::Ctx, rpc::Kind};
use serde_json::{Value, json};
use std::path::Path;
use strata_types::{Code, StrataError};

pub fn caller_pids() -> Vec<i32> {
    let mut out = Vec::new();
    let mut pid = std::process::id() as i32;
    while pid > 0 && out.len() < 64 && !out.contains(&pid) {
        out.push(pid);
        let Some(parent) = strata_deps::process_parent(pid) else { break };
        pid = parent;
    }
    out
}
pub fn add(params: &mut Value, args: &CwdArgs) {
    params["caller_pids"] = json!(caller_pids());
    params["allow_cwd"] = json!(args.allow_cwd);
    params["allow_cwd_holders"] = json!(args.allow_cwd_holders);
}
pub fn rejected(error: &StrataError) -> bool {
    error.code == Code::InvalidInput
        && error.detail.as_ref().is_some_and(|d| d["rpc"] == "Failed")
        && ["caller_pids", "allow_cwd", "allow_cwd_holders"]
            .iter()
            .any(|field| error.message.contains(&format!("unknown field `{field}`")))
}
pub fn strip(params: &mut Value) {
    if let Some(params) = params.as_object_mut() {
        for field in ["caller_pids", "allow_cwd", "allow_cwd_holders"] {
            params.remove(field);
        }
    }
}

/// A legacy sync can be guarded without opening its store: the configured
/// sessions root and the selected repo component identify the affected cwd.
pub fn local_sync(ctx: &Ctx, params: &Value, args: &CwdArgs, cwd: &Path) -> Result<()> {
    if args.allow_cwd || params["check"] == true {
        return Ok(());
    }
    let config = ctx.config()?;
    let Ok(sessions) = std::fs::canonicalize(config.paths.root.join("sessions")) else { return Ok(()) };
    let Ok(real) = std::fs::canonicalize(cwd) else { return Ok(()) };
    let Ok(rest) = real.strip_prefix(&sessions) else { return Ok(()) };
    let mut parts = rest.components();
    let Some(session) = parts.next().and_then(|p| p.as_os_str().to_str()) else { return Ok(()) };
    if strata_types::SessionId::try_from(session.to_owned()).is_err() {
        return Ok(());
    }
    // An id selecting a different session cannot move this cwd. Tickets
    // require a store lookup only to disambiguate their target.
    if let Some(selected) = params["session"].as_str() {
        let id = if selected.starts_with("s-") {
            selected.to_string()
        } else {
            let store = crate::ro::open_store(&config)?;
            crate::ro::select(&store, selected)?.to_string()
        };
        if id != session {
            return Ok(());
        }
    }
    let Some(segment) = parts.next().and_then(|p| p.as_os_str().to_str()) else { return Ok(()) };
    // A repo name can itself contain '-'; prefer the longest matching
    // name before applying the request's repo selection.
    let repo = config.repos.keys().filter(|repo| {
        segment == repo.as_str() || segment.starts_with(&format!(".strata-tmp-move-{repo}-"))
    }).max_by_key(|repo| repo.as_str().len());
    if let Some(repo) = repo {
        let named = params["repos"].as_array().filter(|a| !a.is_empty());
        if named.is_none_or(|a| a.iter().any(|v| v.as_str() == Some(repo.as_str()))) {
            return Err(strata_session::cwd::refuse_caller(&real, session, &sessions.join(session).join(repo.as_str())));
        }
    }
    Ok(())
}

pub fn deps_call(ctx: &Ctx, method: &str, mut params: Value, args: &CwdArgs) -> Result<Value> {
    add(&mut params, args);
    match ctx.call(method, params.clone(), Kind::Write) {
        Err(e) if rejected(&e) => {
            // Old daemons lack the final mover guard; refuse visible cwd
            // holders locally before sending a legacy request.
            let config = ctx.config()?;
            let store = crate::ro::open_store(&config)?;
            let wanted_repo = if method == "deps_propose" {
                params["repo"].as_str().unwrap_or_default().to_owned()
            } else {
                let id = params["proposal"]
                    .as_u64()
                    .ok_or_else(|| StrataError::new(Code::InvalidInput, "missing proposal"))?;
                strata_deps::get_proposal(&store, strata_types::ProposalId::try_from(id)?)?.repo.to_string()
            };
            for rec in crate::ro::live(&store)? {
                if method == "deps_propose" && params["session"].as_str() != Some(rec.manifest.id.as_str()) {
                    continue;
                }
                if let Some(ids) = params["sessions"].as_array()
                    && !ids.iter().any(|id| id.as_str() == Some(rec.manifest.id.as_str()))
                {
                    continue;
                }
                for (repo, ws) in &rec.manifest.repos {
                    if wanted_repo != repo.as_str() {
                        continue;
                    }
                    strata_session::cwd::with_move_cwd(
                        strata_session::cwd::MoveCwd {
                            allow_cwd: args.allow_cwd,
                            allow_cwd_holders: args.allow_cwd_holders,
                            caller_pids: caller_pids(),
                            daemon_pid: None,
                        },
                        || strata_session::cwd::check_holders(&ws.path),
                    )?;
                }
            }
            strip(&mut params);
            ctx.call(method, params, Kind::Write)
        }
        other => other,
    }
}

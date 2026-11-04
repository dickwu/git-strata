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

/// Legacy sync applies the daemon's tracked-workspace and conflict-report
/// selection before guarding the caller's cwd. Unselected siblings cannot move.
pub fn local_sync(ctx: &Ctx, params: &Value, args: &CwdArgs, cwd: &Path) -> Result<()> {
    if args.allow_cwd || params["check"] == true {
        return Ok(());
    }
    let config = ctx.config()?;
    let Ok(sessions) = std::fs::canonicalize(config.paths.root.join("sessions")) else {
        return Ok(());
    };
    let Ok(real) = std::fs::canonicalize(cwd) else {
        return Ok(());
    };
    let Ok(rest) = real.strip_prefix(&sessions) else {
        return Ok(());
    };
    let mut parts = rest.components();
    let Some(session) = parts.next().and_then(|p| p.as_os_str().to_str()) else {
        return Ok(());
    };
    let Ok(id) = strata_types::SessionId::try_from(session.to_owned()) else {
        return Ok(());
    };
    let store = crate::ro::open_store(&config)?;
    // Use the daemon's full SessionId validation before ticket lookup:
    // an s-prefixed ticket can still select the caller's workspace.
    if let Some(selected) = params["session"].as_str()
        && crate::ro::select(&store, selected)? != id
    {
        return Ok(());
    }
    let rec = crate::ro::record(&store, &id)?;
    let Some(segment) = parts.next().and_then(|p| p.as_os_str().to_str()) else {
        return Ok(());
    };
    // A repo name can itself contain '-'; prefer the longest matching
    // name before applying the request's repo selection.
    let repo = rec
        .manifest
        .repos
        .keys()
        .filter(|repo| {
            segment == repo.as_str() || segment.starts_with(&format!(".strata-tmp-move-{repo}-"))
        })
        .max_by_key(|repo| repo.as_str().len());
    if let Some(repo) = repo {
        let explicit = params["repos"].as_array().filter(|a| !a.is_empty());
        let mut repos: Vec<_> = rec
            .manifest
            .repos
            .iter()
            .filter(|(r, ws)| {
                ws.tracked
                    && explicit.is_none_or(|a| a.iter().any(|v| v.as_str() == Some(r.as_str())))
            })
            .map(|(r, _)| r)
            .collect();
        let named: std::collections::BTreeSet<_> = ["resolved", "keep"]
            .iter()
            .filter_map(|field| params[field].as_array())
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        // Keep this narrowing in step with strata_daemon::sync_move::run:
        // both saved reports and legacy workspace conflicts identify a repo.
        if !named.is_empty() && repos.len() > 1 {
            let mut candidates = Vec::new();
            for r in repos {
                if store
                    .sync_report(&id, r)?
                    .iter()
                    .any(|row| named.contains(row.path.as_str()))
                    || rec.workspaces[r]
                        .conflicts
                        .iter()
                        .any(|p| named.contains(p.as_str()))
                {
                    candidates.push(r);
                }
            }
            if candidates.len() != 1 {
                return Err(StrataError::new(
                    Code::InvalidInput,
                    "use --repo to identify the conflict report",
                )
                .with_detail(json!({"reason":"ambiguous_resolution"})));
            }
            repos = candidates;
        }
        let ws = &rec.manifest.repos[repo];
        if repos.contains(&repo) {
            return Err(strata_session::cwd::refuse_caller(
                &real, session, &ws.path, params,
            ));
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
            let workspaces = planned_deps_workspaces(&config, &store, method, &params)?;
            let policy = strata_session::cwd::MoveCwd {
                allow_cwd: args.allow_cwd,
                allow_cwd_holders: args.allow_cwd_holders,
                caller_pids: caller_pids(),
                daemon_pid: daemon_pid(ctx),
            };
            strata_session::cwd::with_move_cwd(policy, || -> Result<()> {
                for ws in workspaces {
                    strata_session::cwd::check_holders(&ws.path)?;
                }
                Ok(())
            })?;
            strip(&mut params);
            ctx.call(method, params, Kind::Write)
        }
        other => other,
    }
}

/// The old daemon keeps its instance-lock fd open. Only an unambiguous
/// holder of that exact path supplies a pid; a missing/ambiguous scan grants
/// no exemption. This works without adding an RPC field to old daemons.
fn daemon_pid(ctx: &Ctx) -> Option<i32> {
    let path = std::fs::canonicalize(strata_daemon::lock_path(&ctx.paths.config_dir)).ok()?;
    let pids: std::collections::BTreeSet<_> = strata_deps::open_files(&ctx.paths.config_dir)
        .ok()?
        .into_iter()
        .filter(|h| h.hold == strata_deps::Hold::Fd && h.path == path)
        .map(|h| h.pid)
        .collect();
    (pids.len() == 1).then(|| *pids.first().unwrap())
}

/// Read-only eligibility at the legacy boundary. A not-yet-built target
/// still needs guarding: approval builds it before planning, and propose
/// --switch builds from the proposer's drift before recording its proposal.
fn planned_deps_workspaces(
    config: &strata_types::Config,
    store: &strata_store::Store,
    method: &str,
    params: &Value,
) -> Result<Vec<strata_types::WorkspaceState>> {
    let proposal = if method == "deps_approve" {
        let id = params["proposal"]
            .as_u64()
            .ok_or_else(|| StrataError::new(Code::InvalidInput, "missing proposal"))?;
        Some(strata_deps::get_proposal(
            store,
            strata_types::ProposalId::try_from(id)?,
        )?)
    } else {
        None
    };
    let repo = match &proposal {
        Some(p) => p.repo.clone(),
        None => crate::ro::repo_name(params["repo"].as_str().unwrap_or_default())?,
    };
    let cfg = config.repo(&repo)?;
    let git = strata_gitio::Git::open(&cfg.path)?;
    let env = strata_deps::InstallEnv {
        path: String::new(),
        timeout: std::time::Duration::ZERO,
    };
    let deps = strata_deps::Ctx {
        store,
        paths: &config.paths,
        repo: &repo,
        cfg,
        git: &git,
        env: &env,
        locks: &crate::local::NoLock,
    };
    let candidates: Vec<_> = crate::ro::live(store)?
        .into_iter()
        .filter(|rec| {
            method != "deps_propose" || params["session"].as_str() == Some(rec.manifest.id.as_str())
        })
        .filter(|rec| {
            params["sessions"].as_array().is_none_or(|ids| {
                ids.iter()
                    .any(|id| id.as_str() == Some(rec.manifest.id.as_str()))
            })
        })
        .filter_map(|rec| {
            rec.manifest
                .repos
                .get(&repo)
                .cloned()
                .map(|ws| (rec.manifest.id, ws))
        })
        .collect();
    let plan = proposal
        .as_ref()
        .map(|p| strata_deps::plan_rollout(&deps, p.id, &candidates, false))
        .transpose()?;
    let mut out = Vec::new();
    for (session, ws) in candidates {
        if !ws.tracked {
            continue;
        }
        let pinned = strata_deps::find_set(store, &repo, &ws.base.deps)?
            .ok_or_else(|| StrataError::new(Code::Corrupt, "the pinned deps set has no record"))?;
        if let Some(plan) = &plan {
            match plan.steps.iter().find(|step| match step {
                strata_deps::RolloutStep::Move { session: id, .. }
                | strata_deps::RolloutStep::Skip { session: id, .. } => id == &session,
            }) {
                Some(strata_deps::RolloutStep::Move { .. }) => {}
                Some(strata_deps::RolloutStep::Skip {
                    reason: strata_deps::SkipReason::NotReady,
                    ..
                }) => {
                    if strata_deps::classify_rollout(
                        &deps,
                        &session,
                        &ws,
                        &proposal.as_ref().unwrap().to_inputs,
                    )?
                    .is_some()
                    {
                        continue;
                    }
                }
                _ => continue,
            }
        } else if strata_deps::drift(cfg, &git, &ws.path, &pinned)?.is_none() {
            continue;
        }
        // FD/mapping holds already make the legacy planner skip this move.
        let dirs: Vec<_> = cfg
            .deps_dirs
            .iter()
            .chain(&pinned.dirs)
            .map(|d| ws.path.join(d.as_str()))
            .collect();
        let refs: Vec<_> = dirs.iter().map(|d| d.as_path()).collect();
        if strata_deps::open_files_many(&refs)?
            .iter()
            .any(|h| h.hold != strata_deps::Hold::Cwd)
        {
            continue;
        }
        out.push(ws);
    }
    Ok(out)
}

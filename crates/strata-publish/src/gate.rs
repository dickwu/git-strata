//! Refuse a parked base before staging or asking for confirmation.
use crate::{Ctx, Result};
use serde_json::json;
use strata_types::{Code, CommitOid, ReleaseGroup, StrataError};

pub fn rerun_command(group: &ReleaseGroup) -> String {
    let mut cmd = String::from("git-strata publish");
    for b in &group.batches {
        for p in &b.paths {
            let selector = format!("{}:{}:{}", b.session, b.repo, p);
            cmd.push_str(&format!(" --from '{}'", selector.replace('\'', "'\\''")));
        }
    }
    cmd
}

pub fn check_with_messages(
    ctx: &Ctx,
    group: &ReleaseGroup,
    b: &CommitOid,
    messages: &crate::message::Messages,
) -> Result<()> {
    check(ctx, group, b).map_err(|mut e| {
        if e.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some("baseline_moved") {
            let mut command = rerun_command(group);
            let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
            if let Some(message) = &messages.message {
                command.push_str(&format!(" -m {}", quote(message)));
            }
            for (repo, message) in &messages.message_for {
                if !group.batches.iter().any(|b| &b.repo == repo) {
                    continue;
                }
                command.push_str(&format!(
                    " --message-for {}",
                    quote(&format!("{repo}={message}"))
                ));
            }
            if let Some(detail) = &mut e.detail {
                detail["fix"] = json!(command);
            }
        }
        e
    })
}

/// Check the local ref snapshot; this never writes.
pub fn check_cached(
    ctx: &Ctx,
    group: &ReleaseGroup,
    messages: &crate::message::Messages,
) -> Result<()> {
    let b = crate::merge_base(ctx.git, ctx.cfg)?;
    check_with_messages(ctx, group, &b, messages)
}

/// A missing target or a rejected cached ancestry needs a fresh cache before
/// handing a refusal to the human. Baseline moves require new checkpoints.
pub fn needs_refresh(error: &StrataError) -> bool {
    error.code == Code::NoSuchBranch
        || error.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some("base_not_on_main")
}

/// Caller holds the repo lock. Fill Git's object and private remote-ref caches;
/// no publish/anchor refs, workspaces, or SQLite rows change here.
pub fn refresh_refs(ctx: &Ctx) -> Result<()> {
    ctx.git.fetch("origin", &ctx.cfg.default_branch)?;
    if ctx.cfg.publish.branch != ctx.cfg.default_branch {
        ctx.git.fetch("origin", &ctx.cfg.publish.branch)?;
    }
    Ok(())
}

/// The execution preflight already owns the repo lock.
pub fn check_fresh_if_needed(
    ctx: &Ctx,
    group: &ReleaseGroup,
    messages: &crate::message::Messages,
) -> Result<()> {
    match check_cached(ctx, group, messages) {
        Err(e) if needs_refresh(&e) => {
            refresh_refs(ctx)?;
            check_cached(ctx, group, messages)
        }
        result => result,
    }
}

/// Publish holds the repo lock. The read-only plan uses an optimistic
/// local snapshot; publish rechecks after fetch before staging exists.
pub fn check(ctx: &Ctx, group: &ReleaseGroup, b: &CommitOid) -> Result<()> {
    let main = strata_deps::main_tip(ctx.cfg, ctx.git)?
        .ok_or_else(|| StrataError::new(Code::NoSuchBranch, "main_tip has not been fetched"))?;
    for batch in group.batches.iter().filter(|batch| &batch.repo == ctx.repo) {
        let session = ctx.store.session(&batch.session)?.ok_or_else(|| {
            StrataError::new(Code::NoSuchSession, format!("no session {}", batch.session))
        })?;
        let ws = session.manifest.repos.get(ctx.repo).ok_or_else(|| {
            StrataError::new(Code::NoSuchRepo, format!("no workspace {}", ctx.repo))
        })?;
        let cp = ctx.store.checkpoint(&batch.session, batch.checkpoint)?;
        if cp.base.tree != ctx.git.tree_of(&ws.head)? {
            return Err(StrataError::new(
                Code::HeadMoved,
                "the workspace baseline moved after the publish checkpoint; rerun this publish",
            )
            .with_detail(
                json!({"reason":"baseline_moved", "session":batch.session, "repo":ctx.repo,
                    "fix":rerun_command(group)}),
            ));
        }
        if ctx.git.is_ancestor(&ws.head, b)? || ctx.git.is_ancestor(&ws.head, &main)? {
            continue;
        }
        let commits = ctx.git.history_between(b, &ws.head, 21)?;
        let ahead = ctx.git.is_ancestor(&main, &ws.head)?;
        let fix = if ahead {
            format!(
                "ask the human: if git-strata push {} has unpushed commits, run it first; push {} from the main checkout ({}) to origin/{}, then rerun this publish (after git-strata sync --session {} if origin moved)",
                ctx.repo,
                ws.head,
                ctx.cfg.path.display(),
                ctx.cfg.default_branch,
                batch.session
            )
        } else {
            format!(
                "ask the human: recover the publish chain first; only if that is not possible, approve git-strata sync --session {} --repo {} --accept-diverged, which drops base commits including {}; then rerun this publish",
                batch.session, ctx.repo, ws.head
            )
        };
        return Err(StrataError::new(Code::InvalidInput, "the workspace base is on neither the publish target nor main_tip")
            .with_detail(json!({"reason":"base_not_on_main", "session":batch.session, "repo":ctx.repo, "base":ws.head,
                "b":b, "main":main, "base_only":commits.iter().take(20).map(|c| json!({"head":c.oid,"subject":c.subject})).collect::<Vec<_>>(),
                "more":commits.len()>20, "fix":fix})));
    }
    Ok(())
}

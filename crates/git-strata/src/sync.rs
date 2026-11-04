//! Main-sync CLI and the optional, bounded Claude Code Stop hook.
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use strata_types::{Code, StrataError, Upstream};

use crate::Result;
use crate::args::SyncArgs;
use crate::ctx::Ctx;
use crate::envelope::Envelope;
use crate::rpc::{self, Kind};

pub const HOOK_BUDGET: Duration = Duration::from_secs(5);

pub(crate) struct Deadline(mpsc::Sender<()>);
impl Deadline {
    pub(crate) fn start(start: Instant) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("stop-hook-deadline".into())
            .spawn(move || {
                if matches!(
                    rx.recv_timeout(HOOK_BUDGET.saturating_sub(start.elapsed())),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    std::process::exit(0);
                }
            })
            .ok()?;
        Some(Self(tx))
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn params(args: &SyncArgs, cwd: &Path) -> Value {
    json!({ "session": args.session, "cwd": cwd, "repos": args.repos, "check": args.check,
        "fetch": !args.no_fetch, "resolved": args.resolved, "keep": args.keep,
        "target": args.target, "generation": args.generation, "accept_diverged": args.accept_diverged,
        "hook": args.hook })
}

pub fn old_daemon(mut e: StrataError) -> StrataError {
    if e.code == Code::InvalidInput
        && e.detail.as_ref().and_then(|d| d["reason"].as_str()) == Some("unknown_method")
    {
        e.code = Code::Unsupported;
        e.message = "this daemon does not support sync; ask the human to upgrade the CLI and daemon together".into();
        e.detail = Some(json!({ "reason": "daemon_too_old", "method": "sync" }));
    }
    e
}

pub fn run(ctx: &Ctx, args: &SyncArgs) -> Result<Envelope> {
    let cwd = std::env::current_dir()
        .map_err(|e| StrataError::new(Code::NoSuchPath, format!("cannot read cwd: {e}")))?;
    let kind = if args.check { Kind::Read } else { Kind::Write };
    let data = ctx
        .call("sync", params(args, &cwd), kind)
        .map_err(old_daemon)?;
    Ok(Envelope::ok(data))
}

#[derive(Deserialize)]
struct StopInput {
    cwd: std::path::PathBuf,
    #[serde(default)]
    stop_hook_active: bool,
}

pub(crate) fn hook(args: &SyncArgs, started: Instant) -> i32 {
    let read = || -> Option<Value> {
        let mut input = Vec::new();
        std::io::stdin()
            .take(1024 * 1024)
            .read_to_end(&mut input)
            .ok()?;
        let input: StopInput = serde_json::from_slice(&input).ok()?;
        if input.stop_hook_active {
            return None;
        }
        let ctx = Ctx::from_env().ok()?;
        let budget = HOOK_BUDGET
            .checked_sub(started.elapsed())
            .filter(|b| !b.is_zero())?;
        let mut request = params(args, &input.cwd);
        // Hook ownership is determined from its input cwd, never a stale
        // caller's --session flag; checks never perform a fetch.
        request["session"] = Value::Null;
        request["fetch"] = json!(false);
        let data = rpc::call_within(&ctx.paths.socket, "sync", request, Kind::Read, budget).ok()?;
        let upstream: Upstream = serde_json::from_value(data.get("upstream")?.clone()).ok()?;
        if started.elapsed() >= HOOK_BUDGET {
            return None;
        }
        for warning in data["base_warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) { eprintln!("note: {warning}"); }
        block_reason(&upstream).map(|reason| json!({ "decision": "block", "reason": reason }))
    };
    if let Some(block) = read() {
        let _ = writeln!(std::io::stdout().lock(), "{block}");
    }
    0
}

pub fn block_reason(upstream: &Upstream) -> Option<String> {
    let mut reasons = Vec::new();
    for (repo, u) in &upstream.repos {
        if let Some(auto) = &u.auto
            && auto.escalation_new && auto.state == "deferred" {
                let cause = strata_session::rebase_cause(auto.reason.as_deref().unwrap_or("busy"), auto.detail.as_ref());
                let action = if u.following {
                    format!("ask the user to release the named holders or wait for a stopping point, then run `{}` and cd back into the workspace", u.fix.as_deref().unwrap_or("cd ~ && git-strata sync --session <session>"))
                } else { "ask the user to release the named holders or pause writes, then review the dependency/rebase plan for this non-following repo".into() };
                reasons.push(format!("auto-rebase of {repo} has been deferred {} times since {}: {cause}; {action}.", auto.attempts, auto.since));
                continue;
        }
        let behind = if u.unchecked {
            0
        } else {
            u.behind.unwrap_or(0)
        };
        if !u.following
            || (behind == 0 && u.deps.is_none())
            || !(u.unpublished.unwrap_or(true)
                || u.auto.as_ref().and_then(|a| a.reason.as_deref()) == Some("conflicts")
                || !u.conflicts.is_empty())
        {
            continue;
        }
        let summary = u
            .deps
            .as_ref()
            .and_then(|d| d.summary.as_deref())
            .filter(|s| !s.is_empty());
        let prefix = if behind > 0 {
            let count = if u.capped {
                format!("at least {behind}")
            } else {
                behind.to_string()
            };
            let deps = summary.map(|s| format!(" (deps: {s})")).unwrap_or_default();
            format!("main moved {count} commits since your base in {repo}{deps}. Run")
        } else {
            let deps = summary.map(|s| format!(" ({s})")).unwrap_or_default();
            format!("main's dependencies changed in {repo}{deps}; run")
        };
        reasons.push(format!("{prefix} `git-strata sync`, resolve any conflicts it returns (merge upstream's change, then `sync --resolved PATH --target … --generation …`), re-run checks, then publish."));
    }
    (!reasons.is_empty()).then(|| reasons.join("\n"))
}

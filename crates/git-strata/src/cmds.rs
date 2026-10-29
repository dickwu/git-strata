//! The non-confirming subcommands (cli.md §3, §3.2): reads try the daemon
//! and fall back to the local read-only path only when nothing listens
//! (§5.1); every other command needs the daemon (`Busy daemon_down`, D2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use strata_overlay::Overlay;
use strata_search::{GrepOpts, Search, Which};
use strata_store::Store;
use strata_types::{CheckpointId, CheckpointReason, Code, Config, EntryKind, RepoName, RepoPath, SessionId, StrataError};

use crate::args::Select;
use crate::ctx::Ctx;
use crate::envelope::{Envelope, code_name};
use crate::local::{Local, ScanSource, overlay_of};
use crate::ro::{self, repo_name};
use crate::rpc::{CallError, Kind};
use crate::{Result, hint};

/// cli.md §3.2: a NUL in the first 8000 bytes is `Binary`.
pub const BINARY_WINDOW: usize = 8000;
/// cli.md §3.2: larger than 64 MiB is `TooLarge`, its content unread.
pub const TOO_LARGE: u64 = strata_overlay::MERGE_MAX_BYTES;

pub fn invalid(reason: &str, message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

fn both_selectors(sel: &Select) -> Result<()> {
    if sel.all && sel.session.is_some() {
        return Err(invalid("both_selectors", "--all and --session are exclusive"));
    }
    Ok(())
}

fn alert(v: &Value) {
    eprintln!("alert: {v}");
}

fn alerts(v: &Value) {
    if let Some(a) = v.as_array() {
        a.iter().for_each(alert);
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

fn absolute(p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() { p.to_path_buf() } else { cwd().join(p) }
}

fn sid(v: &Value) -> Option<SessionId> {
    v.as_str().and_then(|s| SessionId::try_from(s.to_string()).ok())
}

/// A known repo (`NoSuchRepo` otherwise).
fn known_repo(config: &Config, name: &str) -> Result<RepoName> {
    let r = repo_name(name)?;
    config.repo(&r)?;
    Ok(r)
}

/// `--repo`, else the cwd's repo when the cwd is in this session, else the
/// session's only repo.
fn repo_for(config: &Config, store: &Store, s: &SessionId, flag: Option<&str>, cwd_repo: Option<RepoName>) -> Result<RepoName> {
    if let Some(r) = flag {
        return known_repo(config, r);
    }
    if let Some(r) = cwd_repo {
        return Ok(r);
    }
    let rec = ro::record(store, s)?;
    let mut it = rec.manifest.repos.keys();
    match (it.next(), it.next()) {
        (Some(r), None) => Ok(r.clone()),
        _ => Err(invalid("repo", format!("session {s} has several repos; pass --repo"))),
    }
}

// ---- new --------------------------------------------------------------------

pub fn new(ctx: &Ctx, ticket_flag: Option<String>, ticket: Option<String>, only: &[String], repos: &[String], agent_flag: Option<&str>) -> Result<Envelope> {
    if !only.is_empty() && !repos.is_empty() {
        return Err(invalid(
            "repo_flags",
            "pass either --repo (the session gets only those repos) or --repos (every repo gets a workspace), not both",
        ));
    }
    let list = if only.is_empty() { repos } else { only };
    if list.is_empty() || list.iter().any(|r| r.is_empty()) {
        return Err(invalid("repos", "pass --repo R (repeatable) or --repos A,B with at least one repo name"));
    }
    let ticket = match (ticket_flag, ticket) {
        (Some(a), Some(b)) => return Err(invalid("ticket", format!("the ticket is given twice ({a:?} and {b:?})"))),
        (a, b) => a.or(b),
    };
    let names: Vec<RepoName> = list.iter().map(|r| repo_name(r)).collect::<Result<_>>()?;
    let agent = crate::agent::detect(agent_flag, |k| std::env::var_os(k))?;
    // `only` is sent only when set, so this CLI still talks to a daemon
    // that predates it (its params refuse unknown fields).
    let mut params = json!({ "ticket": ticket, "repos": names });
    if !only.is_empty() {
        params["only"] = json!(true);
    } else {
        // `--repos` only picks the tracked repos (D51); say so, since it is
        // easy to take for `--repo`.
        eprintln!(
            "note: --repos tracks {}; every configured repo still gets a workspace (pass --repo R for only these)",
            list.join(",")
        );
    }
    // `agent` goes only to a daemon that lists `new.agent`; an older one
    // would refuse the unknown field.
    let mut dropped = None;
    if let Some(a) = &agent {
        if crate::agent::daemon_takes_agent(&ctx.paths.socket) {
            params["agent"] = json!(a.as_str());
        } else {
            dropped = agent.as_ref();
        }
    }
    let v = match ctx.try_call("new", params.clone(), Kind::Write) {
        Ok(v) => v,
        // The daemon was swapped for an older one since the probe: it
        // refused the field before running anything, so send it again
        // without (only this refusal; nothing else is replayed).
        Err(CallError::Failed(e)) if params.get("agent").is_some() && crate::agent::refused_unknown_agent(&e) => {
            params.as_object_mut().expect("an object").remove("agent");
            dropped = agent.as_ref();
            ctx.call("new", params, Kind::Write)?
        }
        Err(e) => return Err(e.into_error()),
    };
    for w in v["anchor_unknown"].as_array().into_iter().flatten() {
        eprintln!("warning: anchor unknown: {w}");
    }
    let mut report = v.get("session").cloned().unwrap_or(Value::Null);
    if let Some(a) = dropped {
        eprintln!("warning: the daemon predates agent recording; the session does not record agent {a} (install the matching git-strata and restart the daemon)");
        if let Some(ws) = report.get_mut("warnings").and_then(Value::as_array_mut) {
            ws.push(crate::agent::not_recorded(a));
        } else if let Some(o) = report.as_object_mut() {
            o.insert("warnings".into(), json!([crate::agent::not_recorded(a)]));
        }
    }
    if let Some(s) = sid(&report["manifest"]["id"]) {
        hint(s);
    }
    Ok(Envelope::ok(report))
}

// ---- add --------------------------------------------------------------------

/// cli.md `add`: more repos for an existing session (session.md D52).
pub fn add(ctx: &Ctx, session: Option<String>, repos: &[String]) -> Result<Envelope> {
    if repos.is_empty() || repos.iter().any(|r| r.is_empty()) {
        return Err(invalid("repos", "pass --repo R (repeatable) with at least one repo name"));
    }
    let config = ctx.config()?;
    let names: Vec<RepoName> = repos.iter().map(|r| known_repo(&config, r)).collect::<Result<_>>()?;
    let store = ro::open_store(&config)?;
    let (s, _) = ro::session_or_cwd(&store, session.as_deref())?;
    let v = ctx.call("add", json!({ "session": s, "repos": names }), Kind::Write)?;
    for w in v["anchor_unknown"].as_array().into_iter().flatten() {
        eprintln!("warning: anchor unknown: {w}");
    }
    hint(s);
    Ok(Envelope::ok(v.get("report").cloned().unwrap_or(Value::Null)))
}

// ---- status -------------------------------------------------------------------

/// `choices` on every Unknown-health workspace (§3.2).
fn with_choices(mut st: Value) -> Value {
    if let Some(ws) = st.get_mut("workspaces").and_then(Value::as_object_mut) {
        for w in ws.values_mut() {
            if w["health"] == "Unknown" {
                w["choices"] = json!(["abandon", "accept"]);
            }
        }
    }
    st
}

pub fn status(ctx: &Ctx, sel: &Select) -> Result<Envelope> {
    both_selectors(sel)?;
    let params = json!({ "cwd": cwd(), "session": sel.session, "all": sel.all });
    let mut list: Vec<Value> = match ctx.try_call("status", params, Kind::Read) {
        Ok(v) => {
            alerts(&v["warnings"]);
            for l in v["leftovers"].as_array().into_iter().flatten() {
                eprintln!("{}", leftover_text(l));
            }
            v["sessions"].as_array().cloned().unwrap_or_default().into_iter().map(|s| s["status"].clone()).collect()
        }
        Err(CallError::Down(_)) => {
            let (list, left) = local_status(ctx, sel)?;
            for l in &left {
                eprintln!("{}", leftover_text(l));
            }
            list
        }
        Err(CallError::Failed(e)) => return Err(e),
    };
    list = list.into_iter().map(with_choices).collect();
    for st in &list {
        print_notes(&st["manifest"]["id"], st);
    }
    if sel.all {
        list.sort_by(|a, b| a["manifest"]["id"].as_str().cmp(&b["manifest"]["id"].as_str()));
        return Ok(Envelope::ok(json!({ "sessions": list })));
    }
    let one = list.into_iter().next().ok_or_else(|| StrataError::new(Code::NoSuchSession, "no session"))?;
    if let Some(s) = sid(&one["manifest"]["id"]) {
        hint(s);
    }
    Ok(Envelope::ok(one))
}

// ---- ls, status --brief (status-perf C) ---------------------------------------

/// One `leftovers` entry as a stderr line: R3's removed-session form, or
/// the JSON as before.
fn leftover_text(l: &Value) -> String {
    match (l["kind"].as_str(), l["session"].as_str(), l["path"].as_str()) {
        (Some("RemovedSession"), Some(s), Some(p)) => strata_session::leftover_line(s, Path::new(p)),
        _ => format!("leftover: {l}"),
    }
}

/// R2/R3: a session's `notes` (last rm, interrupted rm, deferred rebase)
/// on stderr, `note: <sid>: <line>`.
fn print_notes(id: &Value, v: &Value) {
    let id = id.as_str().unwrap_or("?");
    for n in v["notes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        eprintln!("note: {id}: {n}");
    }
}

/// The removed-session leftovers on stderr and, when there are any, as
/// `data.leftovers`.
fn add_leftovers(data: &mut Value, left: Vec<Value>) {
    for l in &left {
        eprintln!("{}", leftover_text(l));
    }
    if !left.is_empty()
        && let Some(m) = data.as_object_mut()
    {
        m.insert("leftovers".into(), Value::Array(left));
    }
}

/// A daemon that predates `summary`/`workspaces` refuses them before
/// dispatch (`deny_unknown_fields`): an RPC `Failed` naming the field.
fn predates_summary(e: &StrataError) -> bool {
    let rpc_failed = e.code == Code::InvalidInput
        && e.detail.as_ref().and_then(|d| d.get("rpc")).and_then(Value::as_str) == Some("Failed");
    rpc_failed && (e.message.contains("unknown field `summary`") || e.message.contains("unknown field `workspaces`"))
}

/// `status{summary:true}` from the daemon; the store read only when no
/// daemon listens (D5) or the daemon predates `summary`. Returns the
/// sessions, whether they came from the store directly, and the
/// removed-session leftovers (R3).
fn summaries(ctx: &Ctx, sel: &Select, workspaces: bool) -> Result<(Vec<Value>, bool, Vec<Value>)> {
    let here = cwd();
    let mut params = json!({ "cwd": here, "session": sel.session, "all": sel.all, "summary": true });
    if workspaces {
        params["workspaces"] = json!(true);
    }
    match ctx.try_call("status", params, Kind::Read) {
        Ok(v) => Ok((
            v["sessions"].as_array().cloned().unwrap_or_default(),
            false,
            v["leftovers"].as_array().cloned().unwrap_or_default(),
        )),
        Err(CallError::Failed(e)) if !predates_summary(&e) => Err(e),
        Err(_) => {
            let config = ctx.config()?;
            let store = ro::open_store(&config)?;
            let which = if sel.all {
                strata_session::SummarySelect::All
            } else if let Some(s) = sel.session.as_deref() {
                strata_session::SummarySelect::Session(s)
            } else {
                strata_session::SummarySelect::Cwd(&here)
            };
            // `summary` adds R3's orphan detail to a NoSuchSession itself.
            let v = strata_session::summary(&store, &config.paths.root, which, workspaces)?;
            let v = serde_json::to_value(v).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))?;
            let left = strata_session::removed_leftovers(&store, &config.paths.root)?;
            let left = serde_json::to_value(left).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))?;
            Ok((v.as_array().cloned().unwrap_or_default(), true, left.as_array().cloned().unwrap_or_default()))
        }
    }
}

/// `ls`: every live session, by id.
pub fn ls(ctx: &Ctx) -> Result<Envelope> {
    let (list, local, left) = summaries(ctx, &Select { session: None, all: true }, false)?;
    for s in &list {
        print_notes(&s["id"], s);
    }
    let mut data = json!({ "sessions": list });
    add_leftovers(&mut data, left);
    if local {
        data["source"] = json!("local");
    }
    Ok(Envelope::ok(data))
}

/// `status --brief`: as `status` (one session, or `{sessions}` with
/// `--all`), from the store alone.
pub fn status_brief(ctx: &Ctx, sel: &Select) -> Result<Envelope> {
    both_selectors(sel)?;
    let (list, local, left) = summaries(ctx, sel, true)?;
    for s in &list {
        print_notes(&s["id"], s);
    }
    let mut data = if sel.all {
        json!({ "sessions": list })
    } else {
        let one = list.into_iter().next().ok_or_else(|| StrataError::new(Code::NoSuchSession, "no session"))?;
        if let Some(s) = sid(&one["id"]) {
            hint(s);
        }
        one
    };
    // Like `ls` and `status`: every removed session's leftovers, whatever
    // the selector.
    add_leftovers(&mut data, left);
    if local && let Some(m) = data.as_object_mut() {
        m.insert("source".into(), json!("local"));
    }
    Ok(Envelope::ok(data))
}

/// `ls --table` on success: `ID TICKET STATE REPOS CREATED`, one row per
/// session. Returns the exit code.
pub fn emit_table(env: &Envelope) -> i32 {
    use std::io::Write;
    let text = table_of(&env.data);
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let r = lock.write_all(text.as_bytes()).and_then(|()| lock.flush());
    if let Err(e) = &r {
        let _ = writeln!(std::io::stderr(), "git-strata: stdout_write_failed: {e}");
    }
    if env.data["source"] == "local" {
        eprintln!("note: read from the store directly (no daemon answering summary)");
    }
    env.exit_code(&r)
}

/// The `ls --table` text for `ls`'s `data`.
pub fn table_of(data: &Value) -> String {
    let head = ["ID", "TICKET", "STATE", "REPOS", "CREATED"];
    let mut rows: Vec<[String; 5]> = vec![head.map(str::to_string)];
    for s in data["sessions"].as_array().into_iter().flatten() {
        let text = |v: &Value| v.as_str().map(str::to_string).unwrap_or_else(|| "-".into());
        let repos: Vec<&str> = s["repos"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        rows.push([
            text(&s["id"]),
            text(&s["ticket"]),
            text(&s["state"]),
            if repos.is_empty() { "-".into() } else { repos.join(",") },
            text(&s["created"]),
        ]);
    }
    let mut width = [0usize; 5];
    for r in &rows {
        for (w, c) in width.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in &rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(c);
            } else {
                line.push_str(c);
                line.push_str(&" ".repeat(width[i] - c.chars().count() + 2));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    // R2/R3: the notes under the table, one line each.
    for s in data["sessions"].as_array().into_iter().flatten() {
        for n in s["notes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.push_str(&format!("note: {}: {n}\n", s["id"].as_str().unwrap_or("?")));
        }
    }
    out
}

/// §5.1: the daemon is down; `Sessions::open` over the reader store.
/// Returns the statuses and the removed-session leftovers (R3).
fn local_status(ctx: &Ctx, sel: &Select) -> Result<(Vec<Value>, Vec<Value>)> {
    let local = Local::open(ctx.config()?)?;
    let sessions = local.sessions()?;
    // status-perf A1, D10: no old-tree walks without the daemon; old trees
    // report `unchecked`.
    sessions.set_old_tree_mode(strata_session::OldTreeMode::Deferred);
    let ids: Vec<SessionId> = if sel.all {
        ro::live(&local.store)?.into_iter().map(|r| r.manifest.id).collect()
    } else {
        let root = &local.config.paths.root;
        vec![
            ro::session_or_cwd(&local.store, sel.session.as_deref())
                .map_err(|e| match sel.session.as_deref() {
                    Some(s) => strata_session::with_leftover_detail(&local.store, root, s, e),
                    None => e,
                })?
                .0,
        ]
    };
    let mut out = Vec::new();
    for st in sessions.status_many(&ids) {
        let st = st?;
        out.push(serde_json::to_value(st).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))?);
    }
    let left = strata_session::removed_leftovers(&local.store, &local.config.paths.root)?;
    let left = serde_json::to_value(left).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))?;
    Ok((out, left.as_array().cloned().unwrap_or_default()))
}

// ---- checkpoint -------------------------------------------------------------

/// The hook input's `tool_use_id` (cli.md §3.5, D22): used when
/// `--tool-call` has no value or an empty one. Missing, empty, not a
/// string, or not JSON → `None`.
pub fn hook_tool_use_id(input: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(input).ok()?;
    v["tool_use_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// D5: the Claude Code hook input on stdin (one JSON object). Unreadable
/// or not JSON → no paths (a Bash call has none either).
pub fn hook_paths(input: &str) -> Vec<PathBuf> {
    let Ok(v) = serde_json::from_str::<Value>(input) else { return Vec::new() };
    let base = v["cwd"].as_str().map(PathBuf::from).unwrap_or_else(cwd);
    let ti = &v["tool_input"];
    let mut out = Vec::new();
    let mut push = |p: &Value| {
        if let Some(s) = p.as_str().filter(|s| !s.is_empty()) {
            let p = Path::new(s);
            out.push(if p.is_absolute() { p.to_path_buf() } else { base.join(p) });
        }
    };
    push(&ti["file_path"]);
    for p in ti["file_paths"].as_array().into_iter().flatten() {
        push(p);
    }
    for e in ti["edits"].as_array().into_iter().flatten() {
        push(&e["file_path"]);
    }
    out
}

fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().take(16 * 1024 * 1024).read_to_string(&mut s);
    s
}

fn checkpoint_json(store: &Store, s: &SessionId, id: CheckpointId) -> Result<Value> {
    let c = store.checkpoint(s, id)?;
    serde_json::to_value(c).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))
}

pub fn checkpoint(ctx: &Ctx, tool_call: Option<String>, message: Option<String>, session: Option<String>, paths: &[String]) -> Result<Envelope> {
    // Hook mode reads stdin once (non-TTY only); the id falls back to the
    // hook JSON's `tool_use_id` when `--tool-call` has no value (D22).
    let hook_input = if tool_call.is_some() && message.is_none() && !crate::tty::stdin_is_tty() {
        Some(read_stdin())
    } else {
        None
    };
    let reason = match (tool_call, message) {
        (Some(id), None) if !id.is_empty() => CheckpointReason::ToolCall { id },
        (Some(_), None) => match hook_input.as_deref().and_then(hook_tool_use_id) {
            Some(id) => CheckpointReason::ToolCall { id },
            None => {
                return Err(invalid(
                    "checkpoint_reason",
                    "--tool-call has no id and the hook JSON on stdin has no tool_use_id",
                ));
            }
        },
        (None, Some(message)) => CheckpointReason::Manual { message },
        _ => return Err(invalid("checkpoint_reason", "give exactly one of --tool-call ID and -m MSG")),
    };
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let (s, _) = ro::session_or_cwd(&store, session.as_deref())?;
    let rec = ro::record(&store, &s)?;
    let mut abs: Vec<PathBuf> = paths.iter().map(|p| absolute(p)).collect();
    if let Some(input) = &hook_input {
        abs.extend(hook_paths(input));
    }
    // Every path lands on the repo of this session it falls in; paths
    // outside the session are skipped (§3.5).
    let mut per_repo: BTreeMap<RepoName, BTreeSet<RepoPath>> = BTreeMap::new();
    let mut whole: BTreeSet<RepoName> = BTreeSet::new();
    for p in &abs {
        match ro::locate(&store, p)? {
            Some((ls, repo, rp)) if ls == s => match rp {
                Some(rp) => {
                    per_repo.entry(repo).or_default().insert(rp);
                }
                None => {
                    whole.insert(repo);
                }
            },
            _ => eprintln!("skipped (not in session {s}): {}", p.display()),
        }
    }
    // No path in the session (Bash, or every path outside): every tracked
    // repo of the session, full scan.
    if per_repo.is_empty() && whole.is_empty() {
        whole = rec.manifest.repos.iter().filter(|(_, w)| w.tracked).map(|(r, _)| r.clone()).collect();
    }
    let mut todo: BTreeMap<RepoName, Vec<RepoPath>> = BTreeMap::new();
    for r in &whole {
        todo.insert(r.clone(), Vec::new());
    }
    for (r, ps) in per_repo {
        todo.entry(r).or_insert_with(|| ps.into_iter().collect());
    }
    let mut done: BTreeMap<RepoName, Value> = BTreeMap::new();
    // Idempotency (§3.5): the (session, repo, id) rows already in the store.
    if let CheckpointReason::ToolCall { id } = &reason {
        for info in store.checkpoints(&s, None)? {
            if todo.contains_key(&info.repo)
                && !done.contains_key(&info.repo)
                && matches!(&info.reason, CheckpointReason::ToolCall { id: x } if x == id)
            {
                done.insert(info.repo.clone(), checkpoint_json(&store, &s, info.id)?);
            }
        }
    }
    let missing: Vec<(RepoName, Vec<RepoPath>)> = todo.into_iter().filter(|(r, _)| !done.contains_key(r)).collect();
    let reason_v = serde_json::to_value(&reason).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))?;
    for (repo, paths) in missing {
        let params = json!({ "session": s, "repo": repo, "reason": reason_v, "paths": paths });
        match ctx.call("checkpoint", params, Kind::Write) {
            Ok(v) => {
                alerts(&v["warnings"]);
                done.insert(repo, v["checkpoint"].clone());
            }
            Err(e) => {
                if done.is_empty() {
                    return Err(e);
                }
                let got: Vec<Value> = done.into_values().collect();
                return Ok(Envelope::err_with(e, json!({ "checkpoints": got })));
            }
        }
    }
    hint(s);
    Ok(Envelope::ok(json!({ "checkpoints": done.into_values().collect::<Vec<_>>() })))
}

// ---- restore ----------------------------------------------------------------

pub fn restore(ctx: &Ctx, checkpoint: &str, paths: &[String], session: Option<String>) -> Result<Envelope> {
    let n = crate::confirm::parse_id(checkpoint, "checkpoint")?;
    let id = CheckpointId::try_from(n).map_err(|_| invalid("id", format!("checkpoint {checkpoint} is not an id")))?;
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let (s, _) = ro::session_or_cwd(&store, session.as_deref())?;
    let cp = store.checkpoint(&s, id)?;
    let mut rps = Vec::new();
    for p in paths {
        let located = ro::locate(&store, &absolute(p))?;
        let rp = match located {
            Some((ls, r, Some(rp))) if ls == s && r == cp.repo => rp,
            _ => RepoPath::try_from(p.clone())
                .map_err(|_| invalid("path", format!("{p:?} is not a path of {} in session {s}", cp.repo)))?,
        };
        rps.push(rp);
    }
    let params = json!({
        "session": s, "repo": cp.repo, "checkpoint": id,
        "paths": if rps.is_empty() { Value::Null } else { json!(rps) },
    });
    ctx.call("restore", params, Kind::Write)?;
    hint(s.clone());
    Ok(Envelope::ok(json!({ "checkpoint": id, "session": s })))
}

// ---- grep / files -------------------------------------------------------------

/// The (session, repo, workspace path) views a selector names.
fn targets(store: &Store, sel: &Select) -> Result<Vec<(SessionId, RepoName, PathBuf)>> {
    let recs = if sel.all {
        ro::live(store)?
    } else {
        let (s, _) = ro::session_or_cwd(store, sel.session.as_deref())?;
        vec![ro::record(store, &s)?]
    };
    let mut out = Vec::new();
    for r in recs {
        for (repo, ws) in &r.manifest.repos {
            out.push((r.manifest.id.clone(), repo.clone(), ws.path.clone()));
        }
    }
    out.sort();
    Ok(out)
}

/// How the cli reads one view member on disk.
enum Probe {
    Text(Vec<u8>),
    Binary,
    TooLarge,
    Skip,
}

fn probe(root: &Path, p: &RepoPath) -> Probe {
    let full = root.join(p.as_str());
    let Ok(m) = std::fs::symlink_metadata(&full) else { return Probe::Skip };
    if !m.file_type().is_file() {
        return Probe::Skip;
    }
    if m.len() > TOO_LARGE {
        return Probe::TooLarge;
    }
    match std::fs::read(&full) {
        // A NUL in the window is Binary (§3.2); a later NUL also, since
        // search does not index such a file (search.md §5.4) and it must
        // not silently fall out of both hits and skipped.
        Ok(b) if b.contains(&0) => Probe::Binary,
        Ok(b) => Probe::Text(b),
        Err(_) => Probe::Skip,
    }
}

/// The members of one view, through the daemon or the local search.
type Members = BTreeMap<(SessionId, RepoName), (PathBuf, Vec<RepoPath>)>;

fn hit_json(s: &SessionId, r: &RepoName, p: &RepoPath, line: u64, text: &str) -> Value {
    json!({ "session": s, "repo": r, "path": p, "line": line, "text": text })
}

fn hit_key(v: &Value) -> (String, String, Vec<u8>, u64) {
    (
        v["session"].as_str().unwrap_or("").to_string(),
        v["repo"].as_str().unwrap_or("").to_string(),
        v["path"].as_str().unwrap_or("").as_bytes().to_vec(),
        v["line"].as_u64().unwrap_or(0),
    )
}

pub fn grep(ctx: &Ctx, pattern: &str, sel: &Select) -> Result<Envelope> {
    if pattern.is_empty() {
        return Err(invalid("pattern", "the pattern is empty"));
    }
    both_selectors(sel)?;
    let re = regex::Regex::new(pattern).map_err(|e| invalid("pattern", format!("invalid pattern: {e}")))?;
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let ts = targets(&store, sel)?;
    let session = if sel.all { None } else { ts.first().map(|t| t.0.clone()) };
    let params = json!({ "pattern": pattern, "session": session, "all": sel.all, "ignore_case": false });
    let (mut hits, mut skipped, members) = match ctx.try_call("grep", params, Kind::Read) {
        Ok(v) => {
            let hits = v["hits"].as_array().cloned().unwrap_or_default();
            let skipped: Vec<Value> = v["skipped"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| json!({ "session": s["session"], "repo": s["repo"], "code": s["error"]["code"], "error": s["error"] }))
                .collect();
            let mut members = Members::new();
            for (s, r, path) in &ts {
                let got = ctx.try_call("files", json!({ "session": s, "repo": r, "glob": "**" }), Kind::Read);
                if let Ok(f) = got {
                    let ps: Vec<RepoPath> = serde_json::from_value(f["paths"].clone()).unwrap_or_default();
                    members.insert((s.clone(), r.clone()), (path.clone(), ps));
                }
            }
            (hits, skipped, members)
        }
        Err(CallError::Down(_)) => local_grep(&config, pattern, &ts)?,
        Err(CallError::Failed(e)) => return Err(e),
    };
    // Per member on disk: Binary / TooLarge go to skipped; lines that are
    // not UTF-8 (search drops them, search.md D13) are decoded lossily
    // here, one U+FFFD per bad sequence (D9), and matched by the same
    // pattern.
    let mut out_of_hits: BTreeSet<(String, String, String)> = BTreeSet::new();
    for ((s, r), (root, ps)) in &members {
        for p in ps {
            match probe(root, p) {
                Probe::Binary => {
                    skipped.push(json!({ "session": s, "repo": r, "path": p, "code": "Binary" }));
                    out_of_hits.insert((s.to_string(), r.to_string(), p.to_string()));
                }
                Probe::TooLarge => {
                    skipped.push(json!({ "session": s, "repo": r, "path": p, "code": "TooLarge" }));
                    out_of_hits.insert((s.to_string(), r.to_string(), p.to_string()));
                }
                Probe::Text(b) => {
                    if std::str::from_utf8(&b).is_ok() {
                        continue;
                    }
                    for (i, line) in b.split(|c| *c == b'\n').enumerate() {
                        let line = line.strip_suffix(b"\r").unwrap_or(line);
                        if std::str::from_utf8(line).is_ok() {
                            continue;
                        }
                        let text = String::from_utf8_lossy(line);
                        if re.is_match(&text) {
                            hits.push(hit_json(s, r, p, i as u64 + 1, &text));
                        }
                    }
                }
                Probe::Skip => {}
            }
        }
    }
    hits.retain(|h| {
        !out_of_hits.contains(&(
            h["session"].as_str().unwrap_or("").to_string(),
            h["repo"].as_str().unwrap_or("").to_string(),
            h["path"].as_str().unwrap_or("").to_string(),
        ))
    });
    hits.sort_by_key(hit_key);
    hits.dedup_by(|a, b| hit_key(a) == hit_key(b));
    skipped.sort_by_key(|v| {
        (
            v["session"].as_str().unwrap_or("").to_string(),
            v["repo"].as_str().unwrap_or("").to_string(),
            v["path"].as_str().unwrap_or("").as_bytes().to_vec(),
        )
    });
    if let Some(s) = session {
        hint(s);
    }
    let mut data = Map::new();
    data.insert("hits".into(), json!(hits));
    data.insert("skipped".into(), json!(skipped));
    Ok(Envelope::ok(Value::Object(data)))
}

type GrepParts = (Vec<Value>, Vec<Value>, Members);

fn local_grep(config: &Config, pattern: &str, ts: &[(SessionId, RepoName, PathBuf)]) -> Result<GrepParts> {
    let local = Local::open(config.clone())?;
    let sessions = local.sessions()?;
    let src = ScanSource { local: &local };
    let search = Search::open(&local.config, &sessions, &src);
    let (mut hits, mut skipped, mut members) = (Vec::new(), Vec::new(), Members::new());
    for (s, r, path) in ts {
        let view = match search.attach(s, r) {
            Ok(v) => v,
            Err(e) => {
                skipped.push(json!({ "session": s, "repo": r, "code": code_name(e.code), "error": crate::envelope::error_value(&e) }));
                continue;
            }
        };
        let got = search.grep(Which::One(view.clone()), pattern, &GrepOpts::default());
        let files = search.files(view.clone(), "**");
        let _ = search.detach(view);
        for h in got? {
            hits.push(hit_json(&h.session, &h.repo, &h.path, h.line as u64, &h.text));
        }
        members.insert((s.clone(), r.clone()), (path.clone(), files?));
    }
    Ok((hits, skipped, members))
}

pub fn files(ctx: &Ctx, glob: Option<&str>, sel: &Select) -> Result<Envelope> {
    both_selectors(sel)?;
    let glob = glob.unwrap_or("**");
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let ts = targets(&store, sel)?;
    let mut out: Vec<(SessionId, RepoName, RepoPath)> = Vec::new();
    let mut local: Option<Local> = None;
    for (s, r, _) in &ts {
        let got = match ctx.try_call("files", json!({ "session": s, "repo": r, "glob": glob }), Kind::Read) {
            Ok(v) => serde_json::from_value::<Vec<RepoPath>>(v["paths"].clone())
                .map_err(|e| StrataError::new(Code::Unknown, format!("files reply: {e}"))),
            Err(CallError::Down(_)) => {
                if local.is_none() {
                    local = Some(Local::open(config.clone())?);
                }
                local_files(local.as_ref().unwrap(), s, r, glob)
            }
            Err(CallError::Failed(e)) => Err(e),
        };
        match got {
            Ok(ps) => out.extend(ps.into_iter().map(|p| (s.clone(), r.clone(), p))),
            // --all lists what it can; one session is all or nothing.
            Err(e) if sel.all && e.code != Code::InvalidInput => {
                eprintln!("skipped {s}/{r}: {} ({})", e.message, code_name(e.code));
            }
            Err(e) => return Err(e),
        }
    }
    out.sort_by(|a, b| (a.0.as_str(), a.1.as_str(), a.2.as_str().as_bytes()).cmp(&(b.0.as_str(), b.1.as_str(), b.2.as_str().as_bytes())));
    if !sel.all
        && let Some(t) = ts.first()
    {
        hint(t.0.clone());
    }
    let paths: Vec<Value> = out.iter().map(|(s, r, p)| json!({ "session": s, "repo": r, "path": p })).collect();
    Ok(Envelope::ok(json!({ "paths": paths })))
}

fn local_files(local: &Local, s: &SessionId, r: &RepoName, glob: &str) -> Result<Vec<RepoPath>> {
    let sessions = local.sessions()?;
    let src = ScanSource { local };
    let search = Search::open(&local.config, &sessions, &src);
    let view = search.attach(s, r)?;
    let got = search.files(view.clone(), glob);
    let _ = search.detach(view);
    got
}

// ---- diff -------------------------------------------------------------------------

fn change_kinds(o: &Overlay) -> BTreeMap<RepoPath, &'static str> {
    let mut m = BTreeMap::new();
    for e in &o.entries {
        match &e.kind {
            EntryKind::Added { .. } | EntryKind::Untracked { .. } => {
                m.insert(e.path.clone(), "Added");
            }
            EntryKind::Modified { .. } => {
                m.insert(e.path.clone(), "Modified");
            }
            EntryKind::Deleted => {
                m.insert(e.path.clone(), "Deleted");
            }
            EntryKind::Renamed { from, .. } => {
                m.insert(e.path.clone(), "Added");
                m.entry(from.clone()).or_insert("Deleted");
            }
        }
    }
    m
}

fn sorted_changes(m: &BTreeMap<RepoPath, &'static str>) -> Vec<Value> {
    let mut v: Vec<(&RepoPath, &&str)> = m.iter().collect();
    v.sort_by(|a, b| a.0.as_str().as_bytes().cmp(b.0.as_str().as_bytes()));
    v.into_iter().map(|(p, k)| json!({ "path": p, "kind": k })).collect()
}

pub fn diff(ctx: &Ctx, base: bool, session: Option<String>, between: Option<Vec<String>>, repo: Option<String>) -> Result<Envelope> {
    if between.is_some() && (base || session.is_some()) {
        return Err(invalid("diff_mode", "--between is exclusive with --base and --session"));
    }
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    match between {
        None => {
            let (s, cwd_repo) = ro::session_or_cwd(&store, session.as_deref())?;
            let picked = match (repo.as_deref(), cwd_repo) {
                (None, None) => diff_default_repo(&config, &store, &s)?,
                (flag, cwd) => {
                    let r = repo_for(&config, &store, &s, flag, cwd)?;
                    let git = strata_gitio::Git::open(&config.repo(&r)?.path)?;
                    let o = overlay_of(&config, &store, &git, &s, &r)?;
                    Some((r, change_kinds(&o)))
                }
            };
            hint(s);
            Ok(Envelope::ok(match picked {
                Some((r, kinds)) => json!({ "repo": r, "only_a": sorted_changes(&kinds), "only_b": [], "both": [] }),
                None => json!({ "repo": null, "only_a": [], "only_b": [], "both": [] }),
            }))
        }
        Some(ab) => {
            let Some(r) = repo.as_deref() else {
                return Err(invalid("repo", "--between needs --repo"));
            };
            let r = known_repo(&config, r)?;
            let a = ro::select(&store, &ab[0])?;
            let b = ro::select(&store, &ab[1])?;
            let raw = match ctx.try_call("diff", json!({ "a": a, "b": b, "repo": r }), Kind::Read) {
                Ok(v) => v,
                Err(CallError::Down(_)) => local_between(&config, &a, &b, &r)?,
                Err(CallError::Failed(e)) => return Err(e),
            };
            let git = strata_gitio::Git::open(&config.repo(&r)?.path)?;
            let ka = overlay_of(&config, &store, &git, &a, &r).map(|o| change_kinds(&o)).unwrap_or_default();
            let kb = overlay_of(&config, &store, &git, &b, &r).map(|o| change_kinds(&o)).unwrap_or_default();
            Ok(Envelope::ok(between_json(&raw, &ka, &kb)))
        }
    }
}

/// `diff --session S` with no `--repo` and no cwd repo (§3.2): the one
/// repo of the session with changes or checkpoints, with its changes.
/// None qualifies → `None` (an empty diff); several → `InvalidInput`,
/// `reason = "repo"`, with `detail.candidates`. Every workspace is scanned
/// (reads only), a few at a time.
fn diff_default_repo(config: &Config, store: &Store, s: &SessionId) -> Result<Option<(RepoName, BTreeMap<RepoPath, &'static str>)>> {
    let repos: Vec<RepoName> = ro::record(store, s)?.manifest.repos.keys().cloned().collect();
    type Found = Result<Option<BTreeMap<RepoPath, &'static str>>>;
    let scan = |r: &RepoName| -> Found {
        let kinds = {
            let git = strata_gitio::Git::open(&config.repo(r)?.path)?;
            change_kinds(&overlay_of(config, store, &git, s, r)?)
        };
        if !kinds.is_empty() || !store.checkpoints(s, Some(r))?.is_empty() {
            return Ok(Some(kinds));
        }
        Ok(None)
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<std::sync::Mutex<Option<Found>>> = repos.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..DIFF_SCAN_WORKERS.min(repos.len()).max(1) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(r) = repos.get(i) else { return };
                let res = scan(r);
                *out[i].lock().unwrap_or_else(|p| p.into_inner()) = Some(res);
            });
        }
    });
    let mut hits = Vec::new();
    for (r, cell) in repos.into_iter().zip(out) {
        match cell.into_inner().unwrap_or_else(|p| p.into_inner()) {
            Some(Ok(Some(k))) => hits.push((r, k)),
            Some(Ok(None)) => {}
            Some(Err(e)) => return Err(e),
            None => return Err(StrataError::new(Code::Unknown, format!("diff scan of {r} did not run"))),
        }
    }
    match hits.len() {
        0 => Ok(None),
        1 => Ok(hits.pop()),
        _ => {
            let names: Vec<&str> = hits.iter().map(|(r, _)| r.as_str()).collect();
            Err(StrataError::new(Code::InvalidInput, format!("session {s} has changes in several repos ({}); pass --repo", names.join(", ")))
                .with_detail(json!({ "reason": "repo", "candidates": names })))
        }
    }
}

/// Workspaces `diff_default_repo` scans at once.
const DIFF_SCAN_WORKERS: usize = 8;

fn path_list(v: &Value, kinds: &BTreeMap<RepoPath, &'static str>) -> Vec<Value> {
    let mut m = BTreeMap::new();
    for p in v.as_array().into_iter().flatten() {
        if let Ok(rp) = RepoPath::try_from(p.as_str().unwrap_or("").to_string()) {
            let k = kinds.get(&rp).copied().unwrap_or("Modified");
            m.insert(rp, k);
        }
    }
    sorted_changes(&m)
}

/// The daemon's `diff` reply (publish's `BetweenDiff`) as §3.2's shape.
fn between_json(raw: &Value, ka: &BTreeMap<RepoPath, &'static str>, kb: &BTreeMap<RepoPath, &'static str>) -> Value {
    let mut both: Vec<(Vec<u8>, Value)> = Vec::new();
    for e in raw["both"].as_array().into_iter().flatten() {
        let p = e["path"].as_str().unwrap_or("").to_string();
        let kind = RepoPath::try_from(p.clone()).ok().and_then(|rp| ka.get(&rp).copied()).unwrap_or("Modified");
        let mut item = json!({ "path": p, "kind": kind });
        if let Some(err) = e.get("error") {
            item["code"] = err["code"].clone();
        } else {
            let tag = if e["merged"].as_bool().unwrap_or(false) { "Clean" } else { "Conflict" };
            item["preview"] = json!({ "kind": tag, "hunks": e["hunks"].as_array().cloned().unwrap_or_default() });
        }
        both.push((p.into_bytes(), item));
    }
    both.sort_by(|a, b| a.0.cmp(&b.0));
    json!({
        "only_a": path_list(&raw["only_a"], ka),
        "only_b": path_list(&raw["only_b"], kb),
        "both": both.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
    })
}

/// §5.1 for `diff --between`: publish's read-only `diff_between`.
fn local_between(config: &Config, a: &SessionId, b: &SessionId, r: &RepoName) -> Result<Value> {
    let local = Local::open(config.clone())?;
    let sessions = local.sessions()?;
    let rc = local.config.repo(r)?;
    let git = local.git(r)?;
    let deps = strata_deps::Ctx {
        store: &local.store,
        paths: &local.config.paths,
        repo: r,
        cfg: rc,
        git,
        env: &local.env,
        locks: &crate::local::NoLock,
    };
    let pctx = strata_publish::Ctx { store: &local.store, git, cfg: rc, repo: r, deps: &deps, locks: &crate::local::NoLock, sessions: &sessions };
    let d = strata_publish::diff_between(&pctx, a.clone(), b.clone(), r)?;
    let both: Vec<Value> = d
        .both
        .into_iter()
        .map(|(p, res)| match res {
            Ok(m) => json!({
                "path": p, "merged": m.merged,
                "hunks": m.hunks.iter().map(|h| json!({ "start": h.start, "ours": h.ours, "theirs": h.theirs, "truncated": h.truncated })).collect::<Vec<_>>(),
            }),
            Err(e) => json!({ "path": p, "error": crate::envelope::error_value(&e) }),
        })
        .collect();
    Ok(json!({ "only_a": d.only_a, "only_b": d.only_b, "both": both }))
}

// ---- deps propose / list ----------------------------------------------------------

/// cli.md `deps propose`. With `--switch` (review M3) the daemon also runs
/// deps §5.10 `plan_switch` for this session only: the origin and open
/// files are checked there; a skipped move is reported in `switched` with
/// its code, `ok: true`, like a skipped session of `deps approve` (D17).
pub fn deps_propose(ctx: &Ctx, session: Option<String>, repo: Option<String>, switch: bool) -> Result<Envelope> {
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let (s, cwd_repo) = ro::session_or_cwd(&store, session.as_deref())?;
    let r = repo_for(&config, &store, &s, repo.as_deref(), cwd_repo)?;
    let v = ctx.call("deps_propose", json!({ "session": s, "repo": r, "switch": switch }), Kind::Write)?;
    hint(s);
    match v.get("proposal").filter(|p| !p.is_null()) {
        Some(p) if switch => {
            let step = v["switched"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
            if step["kind"] != json!("moved") {
                eprintln!("proposal recorded; this session did not move ({})", step["code"].as_str().unwrap_or("skipped"));
            }
            Ok(Envelope::ok(json!({ "proposal": p, "switched": step })))
        }
        Some(p) => Ok(Envelope::ok(p.clone())),
        None => {
            eprintln!("no drift in {r}: nothing to propose");
            Ok(Envelope::ok(json!({})))
        }
    }
}

pub fn deps_list(ctx: &Ctx, repo: Option<String>) -> Result<Envelope> {
    let config = ctx.config()?;
    let r = repo.as_deref().map(|r| known_repo(&config, r)).transpose()?;
    let mut ps: Vec<Value> = match ctx.try_call("deps_list", json!({ "repo": r }), Kind::Read) {
        Ok(v) => v["proposals"].as_array().cloned().unwrap_or_default(),
        Err(CallError::Down(_)) => {
            let store = ro::open_store(&config)?;
            let got = strata_deps::open_proposals(&store, r.as_ref())?;
            got.iter().map(|p| serde_json::to_value(p).unwrap_or(Value::Null)).collect()
        }
        Err(CallError::Failed(e)) => return Err(e),
    };
    ps.sort_by_key(|p| p["id"].as_u64().unwrap_or(u64::MAX));
    Ok(Envelope::ok(json!({ "proposals": ps })))
}

// ---- gc -------------------------------------------------------------------------------

pub fn gc(ctx: &Ctx, store: bool, dry_run: bool) -> Result<Envelope> {
    if dry_run {
        let v = ctx.call("gc", json!({ "repo": null, "store": true, "dry_run": true }), Kind::Write)?;
        return Ok(Envelope::ok(json!({ "dry_run": true, "preview": v["preview"] })));
    }
    let v = ctx.call("gc", json!({ "repo": null, "store": store }), Kind::Write)?;
    for s in v["skipped"].as_array().into_iter().flatten() {
        eprintln!("gc skipped: {s}");
    }
    let t = &v["trash"];
    if t.is_object() {
        eprintln!("gc trash: removed {} young {}", t["removed"], t["young"]);
        for p in t["refused"].as_array().into_iter().flatten() {
            eprintln!("gc trash refused (still referenced): {p}");
        }
        for p in t["stuck"].as_array().into_iter().flatten() {
            eprintln!("gc trash stuck: {p}");
        }
    }
    let s = &v["store"];
    if s.is_object() {
        for p in s["trash_refused"].as_array().into_iter().flatten() {
            eprintln!("gc trash refused (still referenced): {p}");
        }
        for p in s["stuck"].as_array().into_iter().flatten() {
            eprintln!("gc stuck: {p}");
        }
        for b in s["corrupt"].as_array().into_iter().flatten() {
            eprintln!("gc scrub: corrupt blob {b} (kept; tell the human)");
        }
        return Ok(Envelope::ok(json!({ "store": s })));
    }
    Ok(Envelope::ok(json!({})))
}

// ---- du -------------------------------------------------------------------------------

/// §5.12: the daemon's cached sizes; `refresh` starts a full background pass.
pub fn du(ctx: &Ctx, refresh: bool) -> Result<Envelope> {
    Ok(Envelope::ok(ctx.call("du", json!({ "refresh": refresh }), Kind::Read)?))
}

// ---- daemon status ----------------------------------------------------------------------

/// How long `daemon status` waits for its cwd-session lookup when the
/// daemon has already answered.
pub const STATUS_LOOKUP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// §3.3. Never fails for a missing config or a down daemon (SessionStart).
pub fn daemon_status() -> Result<Envelope> {
    let paths = crate::env::Paths::from_env()?;
    let ctx = Ctx { paths };
    let mut data = json!({
        "running": false,
        "socket": ctx.paths.socket.to_string_lossy(),
        "holders": [],
        "unknown_publishes": [],
        "alerts": [],
    });
    let mut up = false;
    match ctx.try_call("daemon_status", json!({ "verify": false }), Kind::Read) {
        Ok(v) => {
            up = true;
            data["running"] = json!(true);
            data["holders"] = json!(v["holds"].as_array().cloned().unwrap_or_default());
            let warnings = v["warnings"].as_array().cloned().unwrap_or_default();
            let unknown: Vec<Value> = warnings.iter().filter_map(|w| w.get("UnknownPublish").map(|u| u["id"].clone())).collect();
            data["unknown_publishes"] = json!(unknown);
            warnings.iter().for_each(alert);
            data["alerts"] = json!(warnings);
            // The daemon's descriptor count (open, soft limit, ...), as is.
            if let Some(f) = v.get("fds") {
                data["fds"] = f.clone();
            }
            // Optional request features (absent from older daemons).
            if let Some(f) = v.get("features") {
                data["features"] = f.clone();
            }
        }
        Err(CallError::Down(_)) => {}
        Err(CallError::Failed(e)) => return Err(e),
    }
    let lookup = move || ctx.config().ok().and_then(|c| ro::open_store(&c).ok()).and_then(|s| ro::cwd_session(&s).ok().flatten());
    // With the daemon up the answer is already in hand; the cwd lookup reads
    // the store on the volume, which can block (a wedged volume), so it gets
    // STATUS_LOOKUP_WAIT on its own thread and is dropped after that.
    let found = if up {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(lookup());
        });
        match rx.recv_timeout(STATUS_LOOKUP_WAIT) {
            Ok(f) => f,
            Err(_) => {
                data["session_lookup"] = json!("timed_out");
                None
            }
        }
    } else {
        lookup()
    };
    if let Some((s, _)) = found {
        data["session"] = json!(s);
    }
    Ok(Envelope::ok(data))
}

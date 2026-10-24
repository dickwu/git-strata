//! The commands that can ask a human (cli.md §3 table, §5 steps 2 and the
//! "confirm class" display): `deps approve`, `push`, `publish`, `rm`,
//! `resolve`, `chain`, `config relax`, plus `assign` (`publish --assign`
//! is the same call, D13). Every TTY check runs before any connection.

use std::collections::BTreeMap;
use std::io::BufRead;

use serde_json::{Value, json};
use strata_gitio::Git;
use strata_store::Store;
use strata_types::{CheckpointId, Code, Config, ProposalId, RepoName, RepoPath, SessionId, StrataError};

use crate::args::AcceptOrigin;
use crate::ctx::Ctx;
use crate::envelope::Envelope;
use crate::ro::{self, repo_name};
use crate::rpc::Kind;
use crate::tty::{require_tty_for, stdin_is_tty};
use crate::{Result, show};

fn invalid(reason: &str, message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

/// A decimal id within `i64::MAX`.
pub fn parse_id(s: &str, what: &str) -> Result<u64> {
    let n: u64 = s.parse().map_err(|_| invalid("id", format!("{what} {s:?} is not a decimal number")))?;
    if n > i64::MAX as u64 {
        return Err(invalid("id", format!("{what} {s} exceeds i64::MAX")));
    }
    Ok(n)
}

/// A configured repo: `NoSuchRepo` when the configuration loads and does
/// not name it (no connection).
fn known_repo(config: Option<&Config>, name: &RepoName) -> Result<()> {
    if let Some(c) = config {
        c.repo(name)?;
    }
    Ok(())
}

/// A push outcome as the envelope: `Pushed` succeeds, `Refused` is
/// `PushRefused`, `Unknown` is `Unknown`; the outcome stays in `data`.
fn outcome_result(repo: &RepoName, outcome: &Value, data: Value) -> Envelope {
    match outcome.get("kind").and_then(Value::as_str) {
        Some("Pushed") => Envelope::ok(data),
        Some("Refused") => {
            let msg = outcome.get("message").and_then(Value::as_str).unwrap_or("the remote refused the push");
            Envelope::err_with(
                StrataError::new(Code::PushRefused, msg.to_string()).with_detail(json!({ "repo": repo.as_str() })),
                data,
            )
        }
        _ => Envelope::err_with(
            StrataError::new(Code::Unknown, format!("whether the push of {repo} landed is unknown; the chain stays"))
                .with_detail(json!({ "reason": "push_unknown", "repo": repo.as_str() })),
            data,
        ),
    }
}

// ---- push ---------------------------------------------------------------

pub fn push(ctx: &Ctx, repo: &str) -> Result<Envelope> {
    let name = repo_name(repo)?;
    let config = ctx.config().ok();
    known_repo(config.as_ref(), &name)?;
    let mut shown = None;
    if ctx.pins().needs_confirm(config.as_ref(), &name) {
        require_tty_for("push")?;
        if let Some(rc) = config.as_ref().and_then(|c| c.repos.get(&name)) {
            shown = show::push_plan(&name, rc, "confirm");
        }
    }
    // Review H2: the daemon pushes only the chain top shown here.
    let data = ctx.call("push", json!({ "repo": name.as_str(), "shown": shown }), Kind::Write)?;
    let outcome = data.get("push").cloned().unwrap_or(Value::Null);
    Ok(outcome_result(&name, &outcome, outcome.clone()))
}

// ---- config relax --------------------------------------------------------

pub fn config_relax(ctx: &Ctx, repo: &str) -> Result<Envelope> {
    let name = repo_name(repo)?;
    require_tty_for("config relax")?;
    let config = ctx.config().ok();
    known_repo(config.as_ref(), &name)?;
    eprintln!("confirm relaxing the pinned safety settings of {name} to what config.toml says");
    let data = ctx.call("config_relax", json!({ "repo": name.as_str() }), Kind::Write)?;
    Ok(Envelope::ok(data))
}

// ---- rm --------------------------------------------------------------------

pub fn rm(ctx: &Ctx, session: &str, discard: bool) -> Result<Envelope> {
    if discard {
        require_tty_for("rm --discard")?;
    }
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let id = ro::select(&store, session)?;
    if discard {
        eprintln!("remove session {id}: its unpublished changes will be discarded");
    }
    let data = ctx.call("rm", json!({ "session": id.as_str(), "discard": discard }), Kind::Write)?;
    Ok(Envelope::ok(data))
}

// ---- resolve -------------------------------------------------------------

/// `resolve` / `chain` with no or two choices (§3.1), before anything else.
pub fn resolve_choice_error(n: usize) -> Result<Envelope> {
    one_choice(n).map(|_| Envelope::ok(Value::Null))
}

fn one_choice(n: usize) -> Result<()> {
    match n {
        0 => Err(invalid("no_choice", "choose exactly one; there is no default")),
        1 => Ok(()),
        _ => Err(invalid("both_choices", "choose exactly one")),
    }
}

pub fn resolve(ctx: &Ctx, session: &str, repo: &str, abandon: bool, accept: bool) -> Result<Envelope> {
    one_choice(abandon as usize + accept as usize)?;
    require_tty_for("resolve")?;
    let name = repo_name(repo)?;
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let id = ro::select(&store, session)?;
    let rec = ro::record(&store, &id)?;
    let scene = rec.scene.clone().unwrap_or(Value::Null);
    let ws = rec.workspaces.get(&name).map(|w| format!("{w:?}")).unwrap_or_else(|| "(no workspace record)".into());
    eprintln!("session {id} repo {name}: phase {:?}", rec.phase);
    eprintln!("  scene {scene}");
    eprintln!("  workspace {ws}");
    eprintln!("  choices: abandon, accept; chosen: {}", if abandon { "abandon" } else { "accept" });
    let data = ctx.call(
        "resolve_move",
        json!({ "session": id.as_str(), "repo": name.as_str(), "abandon": abandon, "accept": accept }),
        Kind::Write,
    )?;
    Ok(Envelope::ok(data))
}

// ---- chain -----------------------------------------------------------------

pub fn chain(ctx: &Ctx, repo: &str, accept_origin: Option<AcceptOrigin>, restore: bool) -> Result<Envelope> {
    one_choice(accept_origin.is_some() as usize + restore as usize)?;
    require_tty_for("chain")?;
    let name = repo_name(repo)?;
    let config = ctx.config()?;
    let rc = config.repo(&name)?;
    let git = Git::open(&rc.path)?;
    let branch = rc.publish.branch.clone();
    let ch = strata_deps::publish_chain(&git, &branch)?;
    if let Some(ch) = &ch {
        eprintln!(
            "{name} {branch}: origin {} chain top {} anchor {} ({:?})",
            ch.origin,
            ch.top.as_ref().map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
            ch.anchor.as_ref().map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
            ch.state
        );
    }
    let mut shown = None;
    let choice = match accept_origin {
        Some(AcceptOrigin::Replay) => "AcceptOriginReplay",
        Some(AcceptOrigin::Drop) => "AcceptOriginDrop",
        None => "Restore",
    };
    if restore {
        let Some(ch) = &ch else {
            return Err(invalid("no_chain", format!("{name} has no origin/{branch}")));
        };
        let Some(top) = &ch.top else {
            return Err(invalid("no_chain", format!("{name} has no local publish chain to restore")));
        };
        if !git.is_ancestor(&ch.origin, top)? {
            return Err(StrataError::new(Code::InvalidInput, "origin is not an ancestor of the chain; restoring would not fast-forward")
                .with_detail(json!({ "reason": "not_ancestor", "repo": name.as_str(), "origin": ch.origin, "top": top })));
        }
        if ctx.pins().needs_confirm(Some(&config), &name) {
            show::push_plan(&name, rc, "confirm");
        }
        shown = Some(top.clone());
    } else {
        eprintln!("chosen: {}; nothing is pushed", if accept_origin == Some(AcceptOrigin::Replay) { "replay onto origin" } else { "drop the local chain" });
    }
    let data = ctx.call("chain_resolve", json!({ "repo": name.as_str(), "choice": choice, "shown": shown }), Kind::Write)?;
    for w in data.get("warnings").and_then(Value::as_array).into_iter().flatten() {
        eprintln!("warning: {w}");
    }
    Ok(match accept_origin {
        Some(AcceptOrigin::Replay) => Envelope::ok(json!({ "repo": name.as_str(), "choice": "accept_origin_replay", "refs_moved": true })),
        Some(AcceptOrigin::Drop) => Envelope::ok(json!({ "repo": name.as_str(), "choice": "accept_origin_drop", "refs_moved": true })),
        None => {
            let outcome = if data.get("pushed") == Some(&Value::Bool(true)) { json!({ "kind": "Pushed" }) } else { json!({ "kind": "Unknown" }) };
            outcome_result(&name, &outcome, json!({ "repo": name.as_str(), "choice": "restore", "outcome": outcome }))
        }
    })
}

// ---- deps approve ----------------------------------------------------------

/// Sessions a read-only rollout plan would move (best effort; `None` when
/// the plan cannot be computed here).
fn planned_moves(config: &Config, store: &Store, id: ProposalId, only: &[SessionId]) -> Option<(RepoName, Vec<String>)> {
    struct NoLock;
    impl strata_deps::RepoWriteLock for NoLock {
        fn with_write(&self, repo: &RepoName, _f: &mut dyn FnMut() -> strata_deps::Result<()>) -> strata_deps::Result<strata_deps::Result<()>> {
            Err(StrataError::new(Code::Busy, format!("the cli takes no lock on {repo}")).with_detail(json!({ "reason": "readonly_client" })))
        }
    }
    let p = strata_deps::get_proposal(store, id).ok()?;
    let rc = config.repos.get(&p.repo)?;
    let git = Git::open(&rc.path).ok()?;
    let env = strata_deps::InstallEnv { path: std::env::var("PATH").unwrap_or_default(), timeout: std::time::Duration::from_secs(1) };
    let ctx = strata_deps::Ctx { store, paths: &config.paths, repo: &p.repo, cfg: rc, git: &git, env: &env, locks: &NoLock };
    let list: Vec<(SessionId, strata_types::WorkspaceState)> = ro::live(store)
        .ok()?
        .into_iter()
        .filter(|r| only.is_empty() || only.contains(&r.manifest.id))
        .filter_map(|r| r.manifest.repos.get(&p.repo).cloned().map(|ws| (r.manifest.id.clone(), ws)))
        .collect();
    let plan = strata_deps::plan_rollout(&ctx, id, &list, only.is_empty()).ok()?;
    let moves = plan
        .steps
        .iter()
        .filter_map(|s| match s {
            strata_deps::RolloutStep::Move { session, .. } => Some(session.to_string()),
            _ => None,
        })
        .collect();
    Some((p.repo, moves))
}

pub fn deps_approve(ctx: &Ctx, id: &str, sessions: &[String]) -> Result<Envelope> {
    let n = parse_id(id, "proposal id")?;
    require_tty_for("deps approve")?;
    let pid = ProposalId::try_from(n)?;
    let config = ctx.config()?;
    let store = ro::open_store(&config).ok();
    let mut only = Vec::new();
    for s in sessions {
        match &store {
            Some(st) => only.push(ro::select(st, s)?),
            None => only.push(SessionId::try_from(s.clone()).map_err(|_| invalid("session", format!("{s:?} is not a session id")))?),
        }
    }
    // The proposal and the sessions it would move, read without
    // `Sessions::open` (a store reader).
    match store.as_ref().map(|st| strata_deps::get_proposal(st, pid)) {
        Some(Ok(p)) => show::proposal("approve proposal", &p),
        Some(Err(e)) => eprintln!("(cannot read proposal {n}: {})", e.message),
        None => eprintln!("(cannot read the store to show proposal {n})"),
    }
    match store.as_ref().and_then(|st| planned_moves(&config, st, pid, &only)) {
        Some((repo, moves)) => {
            eprintln!("sessions of {repo} this approval moves:");
            for s in &moves {
                eprintln!("  {s}");
            }
            if moves.is_empty() {
                eprintln!("  (none)");
            }
        }
        None => eprintln!("(the sessions to move could not be planned here; the daemon decides)"),
    }
    drop(store);
    let params = json!({
        "proposal": n,
        "sessions": if only.is_empty() { Value::Null } else { json!(only.iter().map(|s| s.as_str()).collect::<Vec<_>>()) },
    });
    match ctx.call("deps_approve", params, Kind::Write) {
        Ok(data) => Ok(Envelope::ok(json!({ "results": data.get("results").cloned().unwrap_or_else(|| json!([])) }))),
        Err(e) => {
            if e.code == Code::NoSuchProposal {
                let d = e.detail.clone().unwrap_or(Value::Null);
                eprintln!("proposal {n} can no longer be approved: {d}");
                if let Some(r) = d.get("replacement").filter(|r| r.is_object()) {
                    show::proposal_value("replacement proposal", r);
                }
                if let Some(by) = d.get("state").and_then(|s| s.get("by")) {
                    eprintln!("superseded by proposal {by}; approve it with `git-strata deps approve {by}` after reading it");
                }
            } else if e.code == Code::HeadMoved {
                let d = e.detail.clone().unwrap_or(Value::Null);
                eprintln!("main moved; the proposal's head is not main's tip: {}", d.get("tip").unwrap_or(&Value::Null));
            }
            Err(e)
        }
    }
}

// ---- assign ------------------------------------------------------------------

pub fn assign(ctx: &Ctx, file: &str, session: &str) -> Result<Envelope> {
    let path = RepoPath::try_from(file.to_string())
        .map_err(|e| invalid("path", format!("{file:?} is not a repo path: {}", e.message)))?;
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let id = ro::select(&store, session)?;
    let records = store.publishes()?;
    let mentions = |o: &Option<Value>| o.as_ref().is_some_and(|v| v.to_string().contains(&format!("\"{}\"", path.as_str())));
    let group = records
        .iter()
        .rev()
        .find(|r| mentions(&r.outcome))
        .or(records.last())
        .map(|r| r.group.id)
        .unwrap_or(0);
    drop(store);
    let data = ctx.call("assign", json!({ "group": group, "path": path.as_str(), "session": id.as_str() }), Kind::Write)?;
    Ok(Envelope::ok(data))
}

// ---- publish -------------------------------------------------------------------

/// One (session, repo) piece of the batch with its paths.
type Batch = BTreeMap<(SessionId, RepoName), Vec<RepoPath>>;

fn which_repo(store: &Store, id: &SessionId, path: &RepoPath) -> Result<RepoName> {
    let rec = ro::record(store, id)?;
    let repos: Vec<&RepoName> = rec.manifest.repos.keys().collect();
    if repos.len() == 1 {
        return Ok(repos[0].clone());
    }
    let mut hits = Vec::new();
    for (repo, ws) in &rec.manifest.repos {
        let on_disk = std::fs::symlink_metadata(ws.path.join(path.as_str())).is_ok();
        let in_base = store.manifest(repo, &ws.base.tree).ok().flatten().is_some_and(|m| m.entries.contains_key(path));
        if on_disk || in_base {
            hits.push(repo.clone());
        }
    }
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(StrataError::new(Code::NoSuchPath, format!("{path} is in no workspace of {id}"))
            .with_detail(json!({ "session": id.as_str(), "path": path.as_str() }))),
        _ => Err(StrataError::new(Code::AmbiguousMatch, format!("{path} is in several repos of {id}")).with_detail(
            json!({ "reason": "from_repo", "session": id.as_str(), "path": path.as_str(), "candidates": hits.iter().map(|r| r.as_str()).collect::<Vec<_>>() }),
        )),
    }
}

fn parse_from(entries: &[String]) -> Result<Vec<(String, RepoPath)>> {
    entries
        .iter()
        .map(|e| {
            let (s, p) = e.split_once(':').ok_or_else(|| invalid("from_syntax", format!("{e:?} is not SESSION:PATH")))?;
            if s.is_empty() {
                return Err(invalid("from_syntax", format!("{e:?} names no session")));
            }
            let path = RepoPath::try_from(p.to_string()).map_err(|x| invalid("from_syntax", format!("{p:?} is not a repo path: {}", x.message)))?;
            Ok((s.to_string(), path))
        })
        .collect()
}

fn build_batch(config: &Config, from: &[(String, RepoPath)]) -> Result<Batch> {
    let store = ro::open_store(config)?;
    let mut batch = Batch::new();
    for (s, path) in from {
        let id = ro::select(&store, s)?;
        let repo = which_repo(&store, &id, path)?;
        let v = batch.entry((id, repo)).or_default();
        if !v.contains(path) {
            v.push(path.clone());
        }
    }
    Ok(batch)
}

/// §5.4: number the cwd session's changed paths (its newest checkpoints),
/// read numbers or paths from stdin up to an empty line.
fn interactive_batch(config: &Config) -> Result<Batch> {
    let store = ro::open_store(config)?;
    let (id, _) = ro::session_or_cwd(&store, None)?;
    let mut items: Vec<(RepoName, RepoPath)> = Vec::new();
    let rec = ro::record(&store, &id)?;
    for repo in rec.manifest.repos.keys() {
        let infos = store.checkpoints(&id, Some(repo))?;
        if let Some(last) = infos.iter().max_by_key(|c| c.id) {
            let c = store.checkpoint(&id, last.id)?;
            for e in c.entries {
                items.push((repo.clone(), e.path));
            }
        }
    }
    items.sort();
    items.dedup();
    if items.is_empty() {
        return Err(invalid("no_batch", format!("session {id} has no checkpointed changes to publish")));
    }
    eprintln!("changed paths of {id} (newest checkpoint per repo):");
    for (i, (r, p)) in items.iter().enumerate() {
        eprintln!("  {:>3}  {r}  {p}", i + 1);
    }
    eprintln!("numbers or paths, one per line; an empty line ends");
    let mut batch = Batch::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| invalid("stdin", format!("cannot read stdin: {e}")))?;
        let t = line.trim();
        if t.is_empty() {
            break;
        }
        let pick = match t.parse::<usize>() {
            Ok(n) if n >= 1 && n <= items.len() => vec![items[n - 1].clone()],
            Ok(_) => return Err(invalid("pick", format!("{t} is not one of the numbers shown"))),
            Err(_) => {
                let v: Vec<_> = items.iter().filter(|(_, p)| p.as_str() == t).cloned().collect();
                if v.is_empty() {
                    return Err(invalid("pick", format!("{t:?} is neither a number nor a listed path")));
                }
                v
            }
        };
        for (r, p) in pick {
            let v = batch.entry((id.clone(), r)).or_default();
            if !v.contains(&p) {
                v.push(p);
            }
        }
    }
    if batch.is_empty() {
        return Err(invalid("no_batch", "nothing was chosen"));
    }
    Ok(batch)
}

pub fn publish(ctx: &Ctx, from: &[String], assign_to: Option<&[String]>) -> Result<Envelope> {
    if let Some(a) = assign_to {
        if !from.is_empty() {
            return Err(invalid("assign_with_from", "--assign is its own call; do not combine it with --from"));
        }
        return assign(ctx, &a[0], &a[1]);
    }
    let config = ctx.config()?;
    let pins = ctx.pins();
    let gate = |repos: &mut dyn Iterator<Item = &RepoName>| -> Result<Vec<RepoName>> {
        let front: Vec<RepoName> =
            repos.filter(|r| pins.pushes(Some(&config), r) && pins.needs_confirm(Some(&config), r)).cloned().collect();
        if !front.is_empty() {
            require_tty_for("publish")?;
        }
        Ok(front)
    };
    let batch = if from.is_empty() {
        if !stdin_is_tty() {
            return Err(invalid("no_batch", "give --from SESSION:PATH, or run in a terminal to choose"));
        }
        interactive_batch(&config)?
    } else {
        let parsed = parse_from(from)?;
        match build_batch(&config, &parsed) {
            Ok(b) => b,
            Err(e) => {
                // The batch's repos are unknown: gate on every configured
                // repo before reporting (no connection either way).
                gate(&mut config.repos.keys())?;
                return Err(e);
            }
        }
    };
    let repos: Vec<RepoName> = {
        let mut v: Vec<RepoName> = batch.keys().map(|(_, r)| r.clone()).collect();
        v.dedup();
        v
    };
    // Review-2 N2: the TTY gate and the confirmations cover only the repos
    // this publish will still commit. When a pushing, guarded repo is in the
    // batch, the before-publish checkpoints and the read-only `publish_plan`
    // come first, so a re-run whose guarded repos already landed is not
    // refused on a non-TTY. Before the gate nothing but those is sent: no
    // `publish`, no prompt, no credential. A failure of either step on a
    // non-TTY is the gate's `NotTty` (as before), with the failure attached.
    let guarded: Vec<RepoName> =
        repos.iter().filter(|r| pins.pushes(Some(&config), r) && pins.needs_confirm(Some(&config), r)).cloned().collect();
    let tty_refused = |e: StrataError| -> StrataError {
        match require_tty_for("publish") {
            Err(mut t) => {
                if let Some(d) = t.detail.as_mut().and_then(Value::as_object_mut) {
                    d.insert("plan_error".into(), json!({ "code": e.code, "message": e.message, "detail": e.detail }));
                }
                t
            }
            Ok(()) => e,
        }
    };
    // Before-publish checkpoints, then the plan, the gate, the publish.
    let mut batches = Vec::new();
    for ((s, r), paths) in &batch {
        let ps: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
        let cid = (|| {
            let c = ctx.call(
                "checkpoint",
                json!({ "session": s.as_str(), "repo": r.as_str(), "reason": { "kind": "BeforePublish" }, "paths": ps }),
                Kind::Write,
            )?;
            let cid = c.get("checkpoint").and_then(|c| c.get("id")).and_then(Value::as_u64).ok_or_else(|| {
                invalid("rpc_failed", "the daemon's checkpoint reply has no checkpoint id")
            })?;
            let _ = CheckpointId::try_from(cid)?;
            Ok::<u64, StrataError>(cid)
        })()
        .map_err(|e| if guarded.is_empty() { e } else { tty_refused(e) })?;
        batches.push(json!({ "session": s.as_str(), "repo": r.as_str(), "checkpoint": cid, "paths": ps }));
    }
    let remaining: Vec<RepoName> = if guarded.is_empty() {
        Vec::new()
    } else {
        let plan = ctx
            .call("publish_plan", json!({ "group": { "id": 0, "batches": batches } }), Kind::Read)
            .map_err(tty_refused)?;
        plan.get("repos")
            .and_then(Value::as_array)
            .ok_or_else(|| tty_refused(invalid("rpc_failed", "the daemon's publish_plan reply has no repos")))?
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|r| RepoName::try_from(r.to_string()).ok())
            .collect()
    };
    let front = gate(&mut guarded.iter().filter(|r| remaining.contains(r)))?;
    let mut shown = serde_json::Map::new();
    for r in &front {
        if let Some(rc) = config.repos.get(r) {
            let top = show::push_plan(r, rc, "publish will ask to confirm");
            shown.insert(r.as_str().to_string(), json!(top));
        }
    }
    let reply = ctx.call("publish", json!({ "group": { "id": 0, "batches": batches }, "shown": shown }), Kind::Write);
    match reply {
        Ok(data) => {
            let published = data.get("published").and_then(Value::as_array).cloned().unwrap_or_default();
            let already = data.get("already_published").and_then(Value::as_array).cloned().unwrap_or_default();
            Ok(publish_envelope(&config, &published, &already, None))
        }
        Err(e) => {
            // Review H4: what landed before the stop (in this call or an
            // earlier one) is reported in `data`; the error names the repo.
            let list = |k: &str| e.detail.as_ref().and_then(|d| d.get(k)).and_then(Value::as_array).cloned().unwrap_or_default();
            // Review M4: the failing repo's commit (and what is known of
            // its push) when the error came after its commit was written.
            let (mut before, already) = (list("published_before"), list("already_published"));
            if let Some(l) = e.detail.as_ref().and_then(|d| d.get("landed")).filter(|l| l.is_object()) {
                before.push(l.clone());
            }
            if before.is_empty() && already.is_empty() {
                return Err(e);
            }
            // A definite remote refusal after the commit is `PushRefused`
            // (cli.md §5), not the publish layer's `Unknown`.
            let refused = e.code == Code::Unknown && e.detail.as_ref().and_then(|d| d.get("kind")).and_then(Value::as_str) == Some("PushRefused");
            Ok(publish_envelope(&config, &before, &already, (!refused).then_some(e)))
        }
    }
}

/// `{commits, checks_ran, waiting, pushes, already_published}`; `ok` only
/// if every push outcome is `Pushed`. `already_published` (review H4): the
/// repos of this group that an earlier publish already landed, not
/// committed again (`[{repo, publish, commit, push}]`).
fn publish_envelope(config: &Config, published: &[Value], already: &[Value], failed: Option<StrataError>) -> Envelope {
    let mut commits = serde_json::Map::new();
    let mut checks = 0u64;
    let mut pushes = Vec::new();
    let mut bad: Option<StrataError> = None;
    let mut touched = Vec::new();
    for p in published {
        let repo = p.get("repo").and_then(Value::as_str).unwrap_or_default().to_string();
        if let Some(c) = p.get("commit") {
            commits.insert(repo.clone(), c.clone());
        }
        checks += p.get("checks_ran").and_then(Value::as_u64).unwrap_or(0);
        for w in p.get("warnings").and_then(Value::as_array).into_iter().flatten() {
            eprintln!("warning ({repo}): {w}");
        }
        touched.push(repo.clone());
        if let Some(o) = p.get("push").filter(|o| !o.is_null()) {
            pushes.push(json!({ "repo": repo, "outcome": o }));
            if bad.is_none() {
                match o.get("kind").and_then(Value::as_str) {
                    Some("Pushed") => {}
                    Some("Refused") => {
                        bad = Some(StrataError::new(Code::PushRefused, o.get("message").and_then(Value::as_str).unwrap_or("refused").to_string())
                            .with_detail(json!({ "repo": repo })))
                    }
                    _ => {
                        bad = Some(StrataError::new(Code::Unknown, format!("whether the push of {repo} landed is unknown; the chain stays"))
                            .with_detail(json!({ "reason": "push_unknown", "repo": repo })))
                    }
                }
            }
        }
    }
    let waiting = waiting_proposals(config, &touched);
    let data = json!({ "commits": commits, "checks_ran": checks, "waiting": waiting, "pushes": pushes, "already_published": already });
    match failed.or(bad) {
        Some(e) => Envelope::err_with(e, data),
        None => Envelope::ok(data),
    }
}

fn waiting_proposals(config: &Config, repos: &[String]) -> Vec<Value> {
    let Ok(store) = ro::open_store(config) else { return Vec::new() };
    let mut out = Vec::new();
    for r in repos {
        let Ok(name) = RepoName::try_from(r.clone()) else { continue };
        if let Ok(ps) = strata_deps::open_proposals(&store, Some(&name)) {
            out.extend(ps.into_iter().filter(|p| p.state == strata_deps::ProposalState::AwaitingPublish).map(|p| json!(p)));
        }
    }
    out
}

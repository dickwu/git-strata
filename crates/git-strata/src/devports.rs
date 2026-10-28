//! `git-strata session ports` / `git-strata session exec` (design
//! dev-ports §3.2–§3.4): each workspace's leased block of 4 dev ports, the
//! env that hands them to dev servers and tauri-connector, an advisory
//! availability check, and `exec` with that env.
//!
//! Leased reads come from the store, read-only: they never need the daemon.
//! Only an unleased (legacy) workspace or `--reassign` calls the daemon
//! (`ports_ensure` / `ports_reassign`, write RPCs); the CLI never writes the
//! store and never starts the daemon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};
use strata_store::{DevPortLease, Probe, SessionRecord, Store, bind_probe};
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::ctx::Ctx;
use crate::envelope::Envelope;
use crate::ro::{self, repo_name};
use crate::rpc::Kind;
use crate::{Result, hint};

/// The four ports of a lease, in block order.
pub const ROLES: [&str; 4] = ["web", "connector", "connector_mcp", "aux"];

fn invalid(reason: &str, message: impl Into<String>, mut detail: Value) -> StrataError {
    detail["reason"] = json!(reason);
    StrataError::new(Code::InvalidInput, message).with_detail(detail)
}

// ---- leases -------------------------------------------------------------------

/// A lease as the CLI reports it (from the store or a daemon reply).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub session: SessionId,
    pub repo: RepoName,
    pub base: u16,
}

impl Lease {
    pub fn ports(&self) -> [u16; 4] {
        [self.base, self.base + 1, self.base + 2, self.base + 3]
    }

    fn ports_json(&self) -> Value {
        let p = self.ports();
        json!({ "web": p[0], "connector": p[1], "connector_mcp": p[2], "aux": p[3] })
    }
}

impl From<DevPortLease> for Lease {
    fn from(l: DevPortLease) -> Lease {
        Lease { session: l.session, repo: l.repo, base: l.base }
    }
}

/// The Tauri dev-config patch that points `build.devUrl` at the web port.
pub fn tauri_dev_config(web: u16) -> String {
    json!({ "build": { "devUrl": format!("http://localhost:{web}") } }).to_string()
}

/// The env `exec` sets (and `ports` reports), overriding inherited values.
pub fn lease_env(l: &Lease, project: &Path) -> BTreeMap<String, String> {
    let p = l.ports();
    let mut m = BTreeMap::new();
    m.insert("PORT".into(), p[0].to_string());
    m.insert("STRATA_DEV_PORT".into(), p[0].to_string());
    m.insert("STRATA_CONNECTOR_PORT".into(), p[1].to_string());
    m.insert("STRATA_CONNECTOR_MCP_PORT".into(), p[2].to_string());
    m.insert("STRATA_AUX_PORT".into(), p[3].to_string());
    m.insert("STRATA_SESSION".into(), l.session.as_str().to_string());
    m.insert("STRATA_REPO".into(), l.repo.as_str().to_string());
    m.insert("TAURI_CONNECTOR_PROJECT".into(), project.to_string_lossy().into_owned());
    m.insert("STRATA_TAURI_DEV_CONFIG".into(), tauri_dev_config(p[0]));
    m
}

/// Whether `name` is a variable `exec` sets (a package script may not
/// assign it).
fn exec_sets(name: &str) -> bool {
    name == "PORT" || name == "TAURI_CONNECTOR_PROJECT" || name == "TAURI_CONFIG" || name.starts_with("STRATA_")
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The Tauri app directory (parent of `src-tauri/tauri.conf.json`) nearest
/// at or above `from`, within `root` (both canonical).
pub fn tauri_app_root(root: &Path, from: &Path) -> Option<PathBuf> {
    if !from.starts_with(root) {
        return None;
    }
    from.ancestors().take_while(|d| d.starts_with(root)).find(|d| d.join("src-tauri/tauri.conf.json").is_file()).map(Path::to_path_buf)
}

/// The daemon's lease reply.
fn lease_of(v: &Value, s: &SessionId, r: &RepoName) -> Result<Lease> {
    let base = v["base"].as_u64().and_then(|b| u16::try_from(b).ok()).ok_or_else(|| {
        StrataError::new(Code::InvalidInput, "the daemon's lease reply has no base").with_detail(json!({ "reason": "rpc_failed" }))
    })?;
    Ok(Lease { session: s.clone(), repo: r.clone(), base })
}

fn reason_of(e: &StrataError) -> Option<&str> {
    e.detail.as_ref().and_then(|d| d.get("reason")).and_then(Value::as_str)
}

/// `ports_ensure` / `ports_reassign`. A daemon that predates them refuses
/// the method before running anything: `Unsupported daemon_too_old`.
fn daemon_lease(ctx: &Ctx, method: &str, params: Value, s: &SessionId, r: &RepoName) -> Result<Lease> {
    match ctx.call(method, params, Kind::Write) {
        Ok(v) => lease_of(&v, s, r),
        Err(e) if reason_of(&e) == Some("unknown_method") => {
            Err(StrataError::new(
                Code::Unsupported,
                "the running daemon has no dev-port leases; install the matching git-strata and restart the daemon",
            )
            .with_detail(json!({ "reason": "daemon_too_old", "method": method })))
        }
        Err(e) => Err(e),
    }
}

/// The stored lease of one workspace (`None`: unleased, or a store that
/// predates leases).
fn stored(store: &Store, s: &SessionId, r: &RepoName) -> Result<Option<Lease>> {
    Ok(store.dev_port_leases(Some(s))?.unwrap_or_default().into_iter().find(|l| &l.repo == r).map(Lease::from))
}

/// The stored lease, else `ports_ensure`; unleased after that is
/// `Busy ports_exhausted` (the daemon's own refusal).
fn ensured(ctx: &Ctx, store: &Store, s: &SessionId, r: &RepoName) -> Result<Lease> {
    match stored(store, s, r)? {
        Some(l) => Ok(l),
        None => daemon_lease(ctx, "ports_ensure", json!({ "session": s.as_str(), "repo": r.as_str() }), s, r),
    }
}

// ---- selection ----------------------------------------------------------------

/// One selected workspace: its session record's root for this repo.
#[derive(Clone, Debug)]
pub struct Target {
    pub session: SessionId,
    pub repo: RepoName,
    /// Canonical workspace root.
    pub path: PathBuf,
}

fn target(rec: &SessionRecord, repo: &RepoName) -> Result<Target> {
    match rec.manifest.repos.get(repo) {
        Some(ws) => Ok(Target { session: rec.manifest.id.clone(), repo: repo.clone(), path: real(&ws.path) }),
        None => Err(StrataError::new(Code::NoSuchRepo, format!("session {} has no workspace for {repo}", rec.manifest.id))
            .with_detail(json!({ "session": rec.manifest.id.as_str(), "repo": repo.as_str() }))),
    }
}

/// `--session` / cwd session, then `--repo` / cwd repo / every repo.
fn targets(store: &Store, session: Option<&str>, repo: Option<&str>) -> Result<Vec<Target>> {
    let (id, cwd_repo) = ro::session_or_cwd(store, session)?;
    hint(id.clone());
    let rec = ro::record(store, &id)?;
    if let Some(r) = repo {
        return Ok(vec![target(&rec, &repo_name(r)?)?]);
    }
    if let Some(r) = cwd_repo {
        return Ok(vec![target(&rec, &r)?]);
    }
    rec.manifest.repos.keys().map(|r| target(&rec, r)).collect()
}

// ---- availability (§3.4) ------------------------------------------------------

/// A process listening on a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    pub command: String,
    pub in_workspace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Free,
    Ours,
    Foreign,
    Unidentified,
    ProbeError,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Free => "free",
            State::Ours => "ours",
            State::Foreign => "foreign",
            State::Unidentified => "unidentified",
            State::ProbeError => "probe_error",
        }
    }
}

/// One port's check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortCheck {
    pub state: State,
    pub listeners: Vec<Listener>,
}

/// §3.4 states from the listeners (`None`: lsof missing or failed) and the
/// bind probe.
pub fn classify(listeners: Option<Vec<Listener>>, probe: Probe) -> PortCheck {
    match listeners {
        Some(ls) if !ls.is_empty() => {
            let state = if ls.iter().all(|l| l.in_workspace) { State::Ours } else { State::Foreign };
            PortCheck { state, listeners: ls }
        }
        ls => {
            let state = match probe {
                Probe::Free => State::Free,
                Probe::InUse => State::Unidentified,
                Probe::Error(_) => State::ProbeError,
            };
            PortCheck { state, listeners: ls.unwrap_or_default() }
        }
    }
}

/// `lsof -F` field output: `(pid, command)` per process entry.
pub fn parse_lsof_pc(out: &str) -> Vec<(u32, String)> {
    let mut v: Vec<(u32, String)> = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            if let Ok(pid) = p.parse() {
                v.push((pid, String::new()));
            }
        } else if let Some(c) = line.strip_prefix('c')
            && let Some(last) = v.last_mut()
        {
            last.1 = c.to_string();
        }
    }
    v
}

fn lsof() -> Command {
    // lsof lives in /usr/sbin on macOS, which a minimal PATH may lack.
    let p = Path::new("/usr/sbin/lsof");
    Command::new(if p.exists() { p } else { Path::new("lsof") })
}

/// Every TCP listener on `port` (any address). `None`: lsof missing or
/// failed (exit 1 with no output is "none").
fn listeners(port: u16, root: &Path) -> Option<Vec<Listener>> {
    let out = lsof().args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"]).output().ok()?;
    if !out.status.success() && !(out.status.code() == Some(1) && out.stdout.is_empty()) {
        return None;
    }
    let mut v = Vec::new();
    for (pid, command) in parse_lsof_pc(&String::from_utf8_lossy(&out.stdout)) {
        let in_workspace = cwd_of(pid).is_some_and(|c| real(&c).starts_with(root));
        v.push(Listener { pid, command, in_workspace });
    }
    Some(v)
}

/// A process's cwd (`None`: unreadable).
fn cwd_of(pid: u32) -> Option<PathBuf> {
    let out = lsof().args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix('n').map(PathBuf::from))
}

/// The check of a lease's 4 ports, in `ROLES` order.
pub fn check(l: &Lease, root: &Path) -> Vec<PortCheck> {
    l.ports().iter().map(|&p| classify(listeners(p, root), bind_probe(p))).collect()
}

fn check_json(c: &[PortCheck]) -> Value {
    let mut m = Map::new();
    for (role, pc) in ROLES.iter().zip(c) {
        let ls: Vec<Value> =
            pc.listeners.iter().map(|l| json!({ "pid": l.pid, "command": l.command, "in_workspace": l.in_workspace })).collect();
        m.insert((*role).into(), json!({ "state": pc.state.as_str(), "listeners": ls }));
    }
    Value::Object(m)
}

/// Which command judges the check (§3.4 action table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judge {
    Exec,
    Reassign,
}

/// The §3.4 action table: the first matching row wins.
pub fn judge(c: &[PortCheck], who: Judge, t: &Target) -> Result<()> {
    let fix = format!("git-strata session ports --reassign --session {} --repo {}", t.session, t.repo);
    let detail = |reason: &str| json!({ "reason": reason, "session": t.session.as_str(), "repo": t.repo.as_str(), "check": check_json(c) });
    if c.iter().any(|p| p.listeners.iter().any(|l| l.in_workspace)) {
        let mut d = detail("already_running");
        d["fix"] = json!(match who {
            Judge::Exec => "a process of this workspace already listens on its ports: reuse it, or stop it first",
            Judge::Reassign => "stop this workspace's own server first, then reassign",
        });
        return Err(StrataError::new(Code::PortInUse, "a process of this workspace already listens on its dev ports").with_detail(d));
    }
    if who == Judge::Reassign {
        return Ok(());
    }
    if c.iter().any(|p| p.state == State::Foreign) {
        let mut d = detail("foreign");
        d["fix"] = json!(fix);
        return Err(StrataError::new(Code::PortInUse, "another process listens on this workspace's dev ports").with_detail(d));
    }
    if c.iter().any(|p| p.state == State::Unidentified) {
        let mut d = detail("port_unidentified");
        d["fix"] = json!(format!("retry shortly; if it persists: {fix}"));
        return Err(StrataError::new(Code::Busy, "a dev port is taken by an unidentified socket").with_detail(d));
    }
    if c.iter().any(|p| p.state == State::ProbeError) {
        let mut d = detail("port_probe_error");
        d["fix"] = json!("retry");
        return Err(StrataError::new(Code::Busy, "a dev port could not be probed").with_detail(d));
    }
    Ok(())
}

// ---- session ports ------------------------------------------------------------

pub struct PortsArgs<'a> {
    pub session: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub all: bool,
    pub check: bool,
    pub reassign: bool,
}

fn lease_json(t: &Target, l: Option<&Lease>, cwd: &Path, with_check: bool) -> Value {
    let app = tauri_app_root(&t.path, cwd).or_else(|| tauri_app_root(&t.path, &t.path));
    let mut v = json!({
        "session": t.session.as_str(),
        "repo": t.repo.as_str(),
        "path": t.path.to_string_lossy(),
        "tauri_app_root": app.as_ref().map(|p| p.to_string_lossy().into_owned()),
    });
    match l {
        Some(l) => {
            let project = app.unwrap_or_else(|| t.path.clone());
            v["ports"] = l.ports_json();
            v["env"] = json!(lease_env(l, &project));
            if with_check {
                v["check"] = check_json(&check(l, &t.path));
            }
        }
        None => {
            v["ports"] = Value::Null;
            v["reason"] = json!("unleased");
        }
    }
    v
}

pub fn ports(ctx: &Ctx, a: &PortsArgs) -> Result<Envelope> {
    if a.all && a.session.is_some() {
        return Err(invalid("both_selectors", "--all and --session are exclusive", json!({})));
    }
    if a.reassign && a.all {
        return Err(invalid("reassign_one", "--reassign moves exactly one workspace's lease; pass --session and --repo", json!({})));
    }
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let cwd = real(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    if a.all {
        // Never writes: an unleased workspace is listed as such.
        let mut out = Vec::new();
        for rec in ro::live(&store)? {
            for repo in rec.manifest.repos.keys() {
                let t = target(&rec, repo)?;
                let l = stored(&store, &t.session, repo)?;
                out.push(lease_json(&t, l.as_ref(), &cwd, a.check));
            }
        }
        return Ok(Envelope::ok(json!({ "leases": out })));
    }
    let ts = targets(&store, a.session, a.repo)?;
    if a.reassign {
        let [t] = ts.as_slice() else {
            let repos: Vec<&str> = ts.iter().map(|t| t.repo.as_str()).collect();
            return Err(invalid("ambiguous_repo", "--reassign moves exactly one workspace's lease; pass --repo", json!({ "repos": repos })));
        };
        // The daemon moves the lease only if it is still the one checked
        // here (`Busy lease_changed` otherwise), so a stale check never
        // replaces a lease another caller just took.
        let old = stored(&store, &t.session, &t.repo)?;
        if let Some(old) = &old {
            judge(&check(old, &t.path), Judge::Reassign, t)?;
        }
        let params = json!({ "session": t.session.as_str(), "repo": t.repo.as_str(), "checked": old.as_ref().map(|l| l.base) });
        let l = daemon_lease(ctx, "ports_reassign", params, &t.session, &t.repo)?;
        return Ok(Envelope::ok(json!({ "leases": [lease_json(t, Some(&l), &cwd, a.check)] })));
    }
    let mut out = Vec::new();
    for t in &ts {
        // design §2.4 step 4: a workspace with no free block is an entry
        // (`ports: null, reason: ports_exhausted`), not a failed call; the
        // other workspaces still render. `exec` keeps the error.
        match ensured(ctx, &store, &t.session, &t.repo) {
            Ok(l) => out.push(lease_json(t, Some(&l), &cwd, a.check)),
            Err(e) if e.code == Code::Busy && reason_of(&e) == Some("ports_exhausted") => {
                let mut v = lease_json(t, None, &cwd, false);
                v["reason"] = json!("ports_exhausted");
                out.push(v);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Envelope::ok(json!({ "leases": out })))
}

// ---- session exec -------------------------------------------------------------

pub struct ExecArgs<'a> {
    pub session: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub tauri: bool,
    pub no_check: bool,
    pub cmd: &'a [String],
}

/// §3.3 step 1: the one selected workspace and the directory to run in.
fn exec_target(store: &Store, session: Option<&str>, repo: Option<&str>, cwd: &Path) -> Result<(Target, PathBuf)> {
    let ts = targets(store, session, repo)?;
    let t = match ts.as_slice() {
        [t] => t.clone(),
        _ => {
            let repos: Vec<&str> = ts.iter().map(|t| t.repo.as_str()).collect();
            return Err(invalid("ambiguous_repo", "session exec runs in exactly one workspace; pass --repo", json!({ "repos": repos })));
        }
    };
    if cwd.starts_with(&t.path) {
        return Ok((t, cwd.to_path_buf()));
    }
    match ro::locate(store, cwd)? {
        None => {
            let root = t.path.clone();
            Ok((t, root))
        }
        Some((s, r, _)) => Err(invalid(
            "cwd_mismatch",
            format!("the current directory is in {s}/{r}, not in the selected {}/{}", t.session, t.repo),
            json!({ "cwd_session": s.as_str(), "cwd_repo": r.as_str(), "session": t.session.as_str(), "repo": t.repo.as_str() }),
        )),
    }
}

/// How `--tauri` changes argv: insert the `--config` patch at `at`; for
/// `npm run`, a `--` first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TauriPlan {
    pub at: usize,
    pub npm_separator: bool,
}

fn launcher_error(message: impl Into<String>, argv: &[String]) -> StrataError {
    invalid(
        "tauri_launcher",
        message,
        json!({
            "argv": argv,
            "fix": "run without --tauri and pass --config \"$STRATA_TAURI_DEV_CONFIG\" to tauri dev yourself, e.g. git-strata session exec -- sh -c 'tauri dev --config \"$STRATA_TAURI_DEV_CONFIG\"'"
        }),
    )
}

fn is_literal(w: &str) -> bool {
    !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./:=@,+%-".contains(&b))
}

fn is_name(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// What a package script `--tauri` may extend runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptKind {
    /// `tauri dev [opts]`: the patch goes after the script's own words.
    Dev,
    /// The bare CLI (`"tauri": "tauri"`): the command must pass `dev` itself.
    Cli,
}

/// §3.3 step 5: a package script `--tauri` may extend — one static command
/// of literal words: `[NAME=value…] (tauri|bunx tauri|npx tauri) [dev [opts]]`.
pub fn script_is_tauri_dev(script: &str) -> std::result::Result<ScriptKind, String> {
    if script.contains(['\n', '\r']) {
        return Err("the script spans several lines".into());
    }
    let words: Vec<&str> = script.split([' ', '\t']).filter(|w| !w.is_empty()).collect();
    if let Some(w) = words.iter().find(|w| !is_literal(w)) {
        return Err(format!("the script word {w:?} is not a plain literal (no quotes, $, globs, operators)"));
    }
    let mut i = 0;
    while let Some((name, _)) = words.get(i).and_then(|w| w.split_once('=')) {
        if !is_name(name) {
            break;
        }
        if exec_sets(name) {
            return Err(format!("the script assigns {name}, which session exec sets"));
        }
        i += 1;
    }
    let rest = &words[i..];
    let opts = match rest {
        ["tauri"] | ["bunx" | "npx", "tauri"] => return Ok(ScriptKind::Cli),
        ["tauri", "dev", o @ ..] | ["bunx" | "npx", "tauri", "dev", o @ ..] => o,
        _ => return Err("the script is not `tauri [dev]`, `bunx tauri [dev]` or `npx tauri [dev]`".into()),
    };
    if opts.contains(&"--") {
        return Err("the script has a bare `--`".into());
    }
    Ok(ScriptKind::Dev)
}

/// Options that make a package manager run a different package's script
/// (or none) even after the script name. npm takes any option before its
/// `--`; these are refused for every manager.
const SELECTORS: &[&str] = &[
    "-C", "-F", "-g", "-r", "-w", "-ws", "--cwd", "--dir", "--filter", "--filter-prod", "--global", "--if-present",
    "--include-workspace-root", "--prefix", "--recursive", "--workspace", "--workspace-root", "--workspaces",
];

fn is_selector(w: &str) -> bool {
    SELECTORS.iter().any(|s| w == *s || w.strip_prefix(s).is_some_and(|r| r.starts_with('=')))
}

/// §3.3 step 5: where `--tauri` puts the patch, or why it refuses.
/// `scripts` reads `package.json` scripts in the directory the command runs in.
pub fn tauri_plan(argv: &[String], scripts: impl Fn(&str) -> Option<String>) -> Result<TauriPlan> {
    let w: Vec<&str> = argv.iter().map(String::as_str).collect();
    let direct = match w.as_slice() {
        ["tauri", "dev", ..] => Some(2),
        ["cargo" | "npx" | "bunx" | "pnpx", "tauri", "dev", ..] => Some(3),
        ["pnpm", "exec", "tauri", "dev", ..] => Some(4),
        _ => None,
    };
    if let Some(_after) = direct {
        let at = w.iter().position(|x| *x == "--").unwrap_or(w.len());
        return Ok(TauriPlan { at, npm_separator: false });
    }
    // Package scripts: (manager, index of the script name).
    let (pm, si) = match w.as_slice() {
        // Always `run`: without it the manager may dispatch to its own
        // command of that name (`bun test`, `pnpm help`, `yarn workspace`).
        ["bun" | "pnpm" | "yarn" | "npm", "run", _, ..] => (w[0], 2),
        ["bun" | "pnpm" | "yarn", s, ..] if !s.starts_with('-') => {
            return Err(launcher_error(format!("--tauri needs `{0} run {s}`: without `run`, {0} may run its own command {s:?}", w[0]), argv));
        }
        _ => return Err(launcher_error("unsupported launcher for --tauri", argv)),
    };
    let name = w[si];
    if name.starts_with('-') {
        return Err(launcher_error(format!("{name:?} is an option, not a script name"), argv));
    }
    let Some(script) = scripts(name) else {
        return Err(launcher_error(format!("package.json has no script {name:?} here"), argv));
    };
    let kind = script_is_tauri_dev(&script).map_err(|why| launcher_error(format!("script {name:?}: {why}"), argv))?;
    let tail = &w[si + 1..];
    // Before npm's `--` everything is npm's; after it (and for the others,
    // after the name) the words reach the script.
    let npm_own = if pm == "npm" { tail.iter().position(|x| *x == "--").unwrap_or(tail.len()) } else { 0 };
    if let Some(o) = tail.iter().find(|x| is_selector(x)).or_else(|| tail[..npm_own].iter().find(|x| x.starts_with('-'))) {
        return Err(launcher_error(format!("{o:?} would let {pm} run another package's script than the one checked"), argv));
    }
    // What reaches the script: npm drops its own `--` and forwards the rest.
    let fwd = tail.iter().enumerate().filter(|&(i, x)| !(pm == "npm" && *x == "--" && i == npm_own)).map(|(_, x)| *x);
    if kind == ScriptKind::Cli && fwd.clone().next() != Some("dev") {
        return Err(launcher_error(format!("script {name:?} is the bare tauri CLI; pass `dev` right after it"), argv));
    }
    let seps = tail.iter().filter(|x| **x == "--").count();
    if pm == "npm" {
        // npm's own separator at most once, nothing after a second one.
        if seps > 1 {
            return Err(launcher_error("a second `--` would reach the app, not tauri", argv));
        }
        return Ok(TauriPlan { at: w.len(), npm_separator: seps == 0 });
    }
    if seps > 0 {
        return Err(launcher_error("a bare `--` would reach the app, not tauri", argv));
    }
    Ok(TauriPlan { at: w.len(), npm_separator: false })
}

/// argv with the plan applied.
pub fn apply_plan(argv: &[String], p: &TauriPlan, patch: &str) -> Vec<String> {
    let mut v = argv.to_vec();
    let mut ins = Vec::new();
    if p.npm_separator {
        ins.push("--".to_string());
    }
    ins.push("--config".to_string());
    ins.push(patch.to_string());
    v.splice(p.at..p.at, ins);
    v
}

fn package_scripts(dir: &Path) -> impl Fn(&str) -> Option<String> {
    let pkg: Option<Value> = std::fs::read(dir.join("package.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    move |name: &str| pkg.as_ref().and_then(|p| p["scripts"][name].as_str()).map(str::to_string)
}

/// Runs the command in place of this process. Returns only on a failure
/// before or at `execvp`; then the caller prints the envelope.
pub fn exec(ctx: &Ctx, a: &ExecArgs) -> Result<Envelope> {
    use std::os::unix::process::CommandExt;
    let Some(program) = a.cmd.first() else {
        return Err(invalid("args", "pass the command after --", json!({})));
    };
    let config = ctx.config()?;
    let store = ro::open_store(&config)?;
    let cwd = real(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let (t, dir) = exec_target(&store, a.session, a.repo, &cwd)?;
    let plan = if a.tauri { Some(tauri_plan(a.cmd, package_scripts(&dir))?) } else { None };
    let l = ensured(ctx, &store, &t.session, &t.repo)?;
    if !a.no_check {
        judge(&check(&l, &t.path), Judge::Exec, &t)?;
    }
    let project = tauri_app_root(&t.path, &dir).unwrap_or_else(|| t.path.clone());
    let argv = match &plan {
        Some(p) => apply_plan(a.cmd, p, &tauri_dev_config(l.ports()[0])),
        None => a.cmd.to_vec(),
    };
    let err = Command::new(program).args(&argv[1..]).envs(lease_env(&l, &project)).current_dir(&dir).exec();
    let code = if err.kind() == std::io::ErrorKind::NotFound { Code::NoSuchPath } else { Code::InvalidInput };
    Err(StrataError::new(code, format!("cannot run {program:?}: {err}"))
        .with_detail(json!({ "reason": "exec", "program": program, "errno": err.raw_os_error() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    fn scripts(m: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |n| m.iter().find(|(k, _)| *k == n).map(|(_, v)| v.to_string())
    }

    const PKG: &[(&str, &str)] = &[
        ("tauri:dev", "tauri dev --features connector"),
        ("bx", "RUST_LOG=info bunx tauri dev"),
        ("dollar", "tauri dev --config $X"),
        ("chain", "next build && tauri dev"),
        ("sets", "PORT=1 tauri dev"),
        ("sep", "tauri dev -- --app"),
        ("other", "next dev"),
        ("tauri", "tauri"),
        ("test", "tauri dev"),
        ("workspace", "tauri dev"),
        ("help", "tauri dev"),
        ("--prefix=../other", "tauri dev"),
        ("-C", "tauri dev"),
    ];

    fn plan(w: &[&str]) -> Result<Vec<String>> {
        let argv = sv(w);
        tauri_plan(&argv, scripts(PKG)).map(|p| apply_plan(&argv, &p, "P"))
    }

    #[test]
    fn direct_launchers_get_the_patch_before_the_app_separator() {
        assert_eq!(plan(&["tauri", "dev"]).unwrap(), sv(&["tauri", "dev", "--config", "P"]));
        assert_eq!(plan(&["cargo", "tauri", "dev", "--release"]).unwrap(), sv(&["cargo", "tauri", "dev", "--release", "--config", "P"]));
        assert_eq!(plan(&["npx", "tauri", "dev", "--", "--x"]).unwrap(), sv(&["npx", "tauri", "dev", "--config", "P", "--", "--x"]));
        assert_eq!(plan(&["pnpm", "exec", "tauri", "dev"]).unwrap(), sv(&["pnpm", "exec", "tauri", "dev", "--config", "P"]));
    }

    #[test]
    fn package_scripts_must_be_literal_tauri_dev() {
        assert_eq!(plan(&["bun", "run", "tauri:dev"]).unwrap(), sv(&["bun", "run", "tauri:dev", "--config", "P"]));
        assert_eq!(plan(&["yarn", "run", "bx"]).unwrap(), sv(&["yarn", "run", "bx", "--config", "P"]));
        assert_eq!(plan(&["npm", "run", "tauri:dev"]).unwrap(), sv(&["npm", "run", "tauri:dev", "--", "--config", "P"]));
        assert_eq!(plan(&["npm", "run", "tauri:dev", "--", "-v"]).unwrap(), sv(&["npm", "run", "tauri:dev", "--", "-v", "--config", "P"]));
        // explicit `run` for a name the manager also uses
        assert_eq!(plan(&["bun", "run", "test"]).unwrap(), sv(&["bun", "run", "test", "--config", "P"]));
        // `"tauri": "tauri"` with `dev` from the command
        assert_eq!(plan(&["bun", "run", "tauri", "dev"]).unwrap(), sv(&["bun", "run", "tauri", "dev", "--config", "P"]));
        assert_eq!(plan(&["pnpm", "run", "tauri", "dev", "--release"]).unwrap(), sv(&["pnpm", "run", "tauri", "dev", "--release", "--config", "P"]));
        assert_eq!(plan(&["npm", "run", "tauri", "dev"]).unwrap(), sv(&["npm", "run", "tauri", "dev", "--", "--config", "P"]));
        assert_eq!(plan(&["npm", "run", "tauri", "--", "dev"]).unwrap(), sv(&["npm", "run", "tauri", "--", "dev", "--config", "P"]));
        for bad in [
            &["bun", "run", "dollar"][..],
            &["bun", "run", "chain"],
            &["bun", "run", "sets"],
            &["bun", "run", "sep"],
            &["bun", "run", "other"],
            &["bun", "run", "missing"],
            &["bun", "run", "tauri:dev", "--", "x"],
            &["npm", "run", "tauri:dev", "--", "a", "--", "b"],
            &["npm", "tauri:dev"],
            &["sh", "-c", "tauri dev"],
            &["bun", "x", "tauri", "dev"],
            // npm or a selector would run another package's script
            &["npm", "run", "tauri:dev", "--prefix", "../other"],
            &["npm", "run", "tauri:dev", "--prefix=../other"],
            &["npm", "run", "tauri:dev", "-w", "app"],
            &["npm", "run", "tauri:dev", "--workspace=app"],
            &["npm", "run", "tauri:dev", "--if-present"],
            &["pnpm", "run", "tauri:dev", "--filter", "app"],
            &["yarn", "tauri:dev", "--cwd", "../other"],
            &["bun", "run", "tauri:dev", "--cwd=../other"],
            // without `run` the manager may run its own command of that name
            &["bun", "test"],
            &["pnpm", "test"],
            &["pnpm", "help"],
            &["bun", "tauri:dev"],
            &["yarn", "workspace", "app", "tauri:dev"],
            // an option where the script name goes
            &["npm", "run", "--prefix=../other", "dev"],
            &["bun", "run", "-C", "dev"],
            &["npm", "run", "--prefix=../other"],
            // the bare CLI needs `dev`
            &["bun", "run", "tauri"],
            &["bun", "run", "tauri", "build"],
            &["npm", "run", "tauri", "--", "build"],
        ] {
            let e = plan(bad).unwrap_err();
            assert_eq!(e.detail.as_ref().unwrap()["reason"], "tauri_launcher", "{bad:?}");
        }
    }

    #[test]
    fn states_follow_listeners_then_the_probe() {
        let l = |w| Listener { pid: 1, command: "x".into(), in_workspace: w };
        assert_eq!(classify(Some(vec![]), Probe::Free).state, State::Free);
        assert_eq!(classify(None, Probe::Free).state, State::Free);
        assert_eq!(classify(Some(vec![l(true)]), Probe::InUse).state, State::Ours);
        assert_eq!(classify(Some(vec![l(true), l(false)]), Probe::InUse).state, State::Foreign);
        assert_eq!(classify(Some(vec![]), Probe::InUse).state, State::Unidentified);
        assert_eq!(classify(None, Probe::InUse).state, State::Unidentified);
        assert_eq!(classify(Some(vec![]), Probe::Error(24)).state, State::ProbeError);
    }

    #[test]
    fn judge_follows_the_action_table() {
        let t = Target { session: SessionId::try_from("s-aaaaaaaa".to_string()).unwrap(), repo: repo_name("tool").unwrap(), path: "/w".into() };
        let pc = |state, own: Option<bool>| PortCheck {
            state,
            listeners: own.map(|w| vec![Listener { pid: 9, command: "n".into(), in_workspace: w }]).unwrap_or_default(),
        };
        let free = pc(State::Free, None);
        let reason = |r: Result<()>| r.unwrap_err().detail.unwrap()["reason"].as_str().unwrap().to_string();
        let mixed = [pc(State::Foreign, Some(true)), free.clone(), free.clone(), free.clone()];
        assert_eq!(reason(judge(&mixed, Judge::Exec, &t)), "already_running");
        assert_eq!(reason(judge(&mixed, Judge::Reassign, &t)), "already_running");
        let foreign = [free.clone(), pc(State::Foreign, Some(false)), free.clone(), free.clone()];
        let e = judge(&foreign, Judge::Exec, &t).unwrap_err();
        assert_eq!((e.code, e.detail.as_ref().unwrap()["reason"].as_str()), (Code::PortInUse, Some("foreign")));
        assert_eq!(e.detail.unwrap()["fix"], "git-strata session ports --reassign --session s-aaaaaaaa --repo tool");
        assert!(judge(&foreign, Judge::Reassign, &t).is_ok());
        let unid = [free.clone(), free.clone(), pc(State::Unidentified, None), pc(State::ProbeError, None)];
        let e = judge(&unid, Judge::Exec, &t).unwrap_err();
        assert_eq!((e.code, e.detail.unwrap()["reason"].as_str().unwrap()), (Code::Busy, "port_unidentified"));
        assert!(judge(&unid, Judge::Reassign, &t).is_ok());
        let perr = [free.clone(), free.clone(), free.clone(), pc(State::ProbeError, None)];
        assert_eq!(reason(judge(&perr, Judge::Exec, &t)), "port_probe_error");
        assert!(judge(&[free.clone(), free.clone(), free.clone(), free], Judge::Exec, &t).is_ok());
    }

    #[test]
    fn lsof_fields_parse_per_process() {
        assert_eq!(parse_lsof_pc("p12\ncnode\nf20\np34\ncstaff tool\n"), vec![(12, "node".into()), (34, "staff tool".into())]);
        assert_eq!(parse_lsof_pc(""), vec![]);
    }

    #[test]
    fn env_and_app_root() {
        let d = tempfile::tempdir().unwrap();
        let root = real(d.path());
        std::fs::create_dir_all(root.join("apps/desktop/src-tauri/src")).unwrap();
        std::fs::write(root.join("apps/desktop/src-tauri/tauri.conf.json"), b"{}").unwrap();
        assert_eq!(tauri_app_root(&root, &root.join("apps/desktop/src-tauri/src")), Some(root.join("apps/desktop")));
        assert_eq!(tauri_app_root(&root, &root), None);
        assert_eq!(tauri_app_root(&root, Path::new("/")), None);
        let l = Lease { session: SessionId::try_from("s-k3q7xm2a".to_string()).unwrap(), repo: repo_name("tool").unwrap(), base: 20412 };
        let e = lease_env(&l, Path::new("/p"));
        assert_eq!(e["PORT"], "20412");
        assert_eq!(e["STRATA_CONNECTOR_PORT"], "20413");
        assert_eq!(e["STRATA_CONNECTOR_MCP_PORT"], "20414");
        assert_eq!(e["STRATA_AUX_PORT"], "20415");
        assert_eq!(e["TAURI_CONNECTOR_PROJECT"], "/p");
        assert_eq!(e["STRATA_TAURI_DEV_CONFIG"], r#"{"build":{"devUrl":"http://localhost:20412"}}"#);
        assert_eq!(e.len(), 9);
    }
}

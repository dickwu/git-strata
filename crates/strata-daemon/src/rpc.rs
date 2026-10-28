//! The json-lines envelope (daemon.md §4, D7).
//!
//! One request per line: `{"id", "method", "params"}`; one reply per line:
//! `{"id", "ok": true, "data"}` or `{"id", "ok": false, "error"}`. `id` is a
//! string or a number and is echoed back unchanged. There is no handshake,
//! no token, no scope and no binary framing: the channel is a same-user unix
//! socket (mode 0600) and authority comes from `strata-confirm` per action,
//! never from the connection.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use strata_types::{
    CheckpointId, CheckpointReason, Code, CommitOid, ProposalId, ReleaseGroup, RepoName, RepoPath, SessionId, StrataError,
};

use crate::{ChainChoice, MAX_FRAME, RpcReject};

macro_rules! methods {
    ($($variant:ident = $name:literal, ro = $ro:literal;)*) => {
        /// The 22 methods (daemon.md §4).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Method { $($variant),* }

        impl Method {
            pub const ALL: &'static [Method] = &[$(Method::$variant),*];

            /// The wire name.
            pub fn name(self) -> &'static str {
                match self { $(Method::$variant => $name),* }
            }

            pub fn from_name(s: &str) -> Option<Method> {
                match s { $($name => Some(Method::$variant),)* _ => None }
            }

            /// I5: read-only methods never queue and never take a write lock.
            pub fn is_read_only(self) -> bool {
                match self { $(Method::$variant => $ro),* }
            }
        }
    };
}

methods! {
    New = "new", ro = false;
    Add = "add", ro = false;
    Status = "status", ro = true;
    Checkpoint = "checkpoint", ro = false;
    Restore = "restore", ro = false;
    Grep = "grep", ro = true;
    Files = "files", ro = true;
    Diff = "diff", ro = true;
    DepsPropose = "deps_propose", ro = false;
    DepsApprove = "deps_approve", ro = false;
    DepsList = "deps_list", ro = true;
    Publish = "publish", ro = false;
    PublishPlan = "publish_plan", ro = true;
    Push = "push", ro = false;
    Assign = "assign", ro = false;
    Rm = "rm", ro = false;
    Gc = "gc", ro = false;
    ResolveMove = "resolve_move", ro = false;
    ChainResolve = "chain_resolve", ro = false;
    ConfigRelax = "config_relax", ro = false;
    DaemonStatus = "daemon_status", ro = true;
    Du = "du", ro = true;
}

/// A field that must be present but may be `null` (a missing one is
/// `Failed`, like any other missing field).
fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewParams {
    #[serde(deserialize_with = "nullable")]
    pub ticket: Option<String>,
    pub repos: Vec<RepoName>,
    /// Only `repos` get a workspace (session.md D51); absent means false.
    #[serde(default)]
    pub only: bool,
}

/// `add` (session.md D52): more repos for an existing session.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddParams {
    pub session: SessionId,
    pub repos: Vec<RepoName>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusParams {
    #[serde(deserialize_with = "nullable")]
    pub cwd: Option<std::path::PathBuf>,
    #[serde(deserialize_with = "nullable")]
    pub session: Option<String>,
    pub all: bool,
    /// Compute each repo's publish chain (status-perf D2). Off by default:
    /// the reply then has no `chains` key at all.
    #[serde(default)]
    pub chains: bool,
    /// The store-only summary `ls` / `status --brief` print (status-perf
    /// C): `{sessions:[SessionSummary]}`. Not with `chains`.
    #[serde(default)]
    pub summary: bool,
    /// With `summary`: each session's workspaces from the store.
    #[serde(default)]
    pub workspaces: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointParams {
    pub session: SessionId,
    pub repo: RepoName,
    pub reason: CheckpointReason,
    pub paths: Vec<RepoPath>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreParams {
    pub session: SessionId,
    pub repo: RepoName,
    pub checkpoint: CheckpointId,
    #[serde(deserialize_with = "nullable")]
    pub paths: Option<Vec<RepoPath>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrepParams {
    pub pattern: String,
    #[serde(deserialize_with = "nullable")]
    pub session: Option<SessionId>,
    pub all: bool,
    pub ignore_case: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesParams {
    pub session: SessionId,
    pub repo: RepoName,
    pub glob: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffParams {
    pub a: SessionId,
    pub b: SessionId,
    pub repo: RepoName,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepsProposeParams {
    pub session: SessionId,
    pub repo: RepoName,
    pub switch: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepsApproveParams {
    pub proposal: ProposalId,
    #[serde(deserialize_with = "nullable")]
    pub sessions: Option<Vec<SessionId>>,
}

/// D33: `store` adds the store-wide gc (`Store::gc_with`: removed
/// sessions, proposals, attempts, blobs, tmp, trash, scrub) after the repo
/// gcs; `dry_run` (only with `store`) answers `Store::gc_preview` and
/// writes nothing. Both default to false (the old `{ "repo": .. }` call).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcParams {
    #[serde(deserialize_with = "nullable")]
    pub repo: Option<RepoName>,
    #[serde(default)]
    pub store: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoOptParams {
    #[serde(deserialize_with = "nullable")]
    pub repo: Option<RepoName>,
}

/// Review-2 N2: the read-only plan of a `publish` of this group: which
/// repos it would still commit (`repos`) and which already landed
/// (`already_published`). Nothing is written.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishPlanParams {
    pub group: ReleaseGroup,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishParams {
    pub group: ReleaseGroup,
    /// Review H2: the publish chain top the cli showed for each repo it
    /// asked to confirm (`null`: it showed no local chain). A repo that
    /// needs a confirmation is refused (`NotConfirmed`, reason
    /// `chain_moved`) when its chain top under the lock is not this one;
    /// a repo missing here counts as "showed no chain top".
    #[serde(default)]
    pub shown: BTreeMap<RepoName, Option<CommitOid>>,
    /// Review-3 T1: the pushing, guarded repos the cli planned to commit
    /// (the ones its TTY gate covered). When present, a repo that needs a
    /// confirmation but is not in it is refused (`NotConfirmed`, reason
    /// `plan_changed`) before any confirmation is asked for. Absent: no
    /// such check (callers other than the cli).
    #[serde(default)]
    pub plan: Option<BTreeSet<RepoName>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushParams {
    pub repo: RepoName,
    /// Review H2: the chain top the cli showed (absent or `null`: none).
    #[serde(default, deserialize_with = "nullable")]
    pub shown: Option<CommitOid>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoParams {
    pub repo: RepoName,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignParams {
    pub group: u64,
    pub path: RepoPath,
    pub session: SessionId,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RmParams {
    pub session: SessionId,
    pub discard: bool,
}

/// No credential field of any kind: `deny_unknown_fields` turns one into
/// `Failed`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveMoveParams {
    pub session: SessionId,
    pub repo: RepoName,
    pub abandon: bool,
    pub accept: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainResolveParams {
    pub repo: RepoName,
    pub choice: ChainChoice,
    /// Review H2: for `Restore`, the chain top the cli showed.
    #[serde(default, deserialize_with = "nullable")]
    pub shown: Option<CommitOid>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonStatusParams {
    #[serde(default)]
    pub verify: bool,
}

/// `du` (§5.12): `refresh` re-sizes every unit in the background.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuParams {
    #[serde(default)]
    pub refresh: bool,
}

/// The typed params of one request.
#[derive(Clone, Debug, PartialEq)]
pub enum Params {
    New(NewParams),
    Add(AddParams),
    Status(StatusParams),
    Checkpoint(CheckpointParams),
    Restore(RestoreParams),
    Grep(GrepParams),
    Files(FilesParams),
    Diff(DiffParams),
    DepsPropose(DepsProposeParams),
    DepsApprove(DepsApproveParams),
    DepsList(RepoOptParams),
    Publish(PublishParams),
    PublishPlan(PublishPlanParams),
    Push(PushParams),
    Assign(AssignParams),
    Rm(RmParams),
    Gc(GcParams),
    ResolveMove(ResolveMoveParams),
    ChainResolve(ChainResolveParams),
    ConfigRelax(RepoParams),
    DaemonStatus(DaemonStatusParams),
    Du(DuParams),
}

fn typed<T: DeserializeOwned>(v: Value, wrap: fn(T) -> Params) -> Result<Params, String> {
    serde_json::from_value::<T>(v).map(wrap).map_err(|e| e.to_string())
}

impl Params {
    /// Parse `params` for `method`; any missing, unknown or mistyped field
    /// is an error message (→ `Failed`).
    pub fn parse(method: Method, v: Value) -> Result<Params, String> {
        if !v.is_object() {
            return Err("params must be an object".into());
        }
        match method {
            Method::New => typed(v, Params::New),
            Method::Add => typed(v, Params::Add),
            Method::Status => typed(v, Params::Status),
            Method::Checkpoint => typed(v, Params::Checkpoint),
            Method::Restore => typed(v, Params::Restore),
            Method::Grep => typed(v, Params::Grep),
            Method::Files => typed(v, Params::Files),
            Method::Diff => typed(v, Params::Diff),
            Method::DepsPropose => typed(v, Params::DepsPropose),
            Method::DepsApprove => typed(v, Params::DepsApprove),
            Method::DepsList => typed(v, Params::DepsList),
            Method::Publish => typed(v, Params::Publish),
            Method::PublishPlan => typed(v, Params::PublishPlan),
            Method::Push => typed(v, Params::Push),
            Method::Assign => typed(v, Params::Assign),
            Method::Rm => typed(v, Params::Rm),
            Method::Gc => typed(v, Params::Gc),
            Method::ResolveMove => {
                let p: ResolveMoveParams = serde_json::from_value(v).map_err(|e| e.to_string())?;
                if p.abandon == p.accept {
                    return Err("exactly one of abandon and accept must be true".into());
                }
                Ok(Params::ResolveMove(p))
            }
            Method::ChainResolve => typed(v, Params::ChainResolve),
            Method::ConfigRelax => typed(v, Params::ConfigRelax),
            Method::DaemonStatus => typed(v, Params::DaemonStatus),
            Method::Du => typed(v, Params::Du),
        }
    }
}

/// One parsed request.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// A JSON string or number, echoed back.
    pub id: Value,
    pub method: Method,
    pub params: Params,
}

/// One reply line.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply(pub Value);

impl Reply {
    /// `{id, ok: true, data}`; `data` is `{}` when the method returns nothing.
    pub fn ok(id: &Value, data: Value) -> Reply {
        let data = if data.is_null() { json!({}) } else { data };
        Reply(json!({ "id": id, "ok": true, "data": data }))
    }

    /// `{id, ok: false, error: {code, message, detail?}}`: an upstream error.
    pub fn error(id: &Value, e: &StrataError) -> Reply {
        let mut err = Map::new();
        err.insert("code".into(), serde_json::to_value(e.code).unwrap_or(Value::Null));
        err.insert("message".into(), Value::String(e.message.clone()));
        if let Some(d) = &e.detail {
            err.insert("detail".into(), d.clone());
        }
        Reply(json!({ "id": id, "ok": false, "error": Value::Object(err) }))
    }

    /// `{id, ok: false, error: {rpc, message}}`: no `code`.
    pub fn rejected(id: &Value, rpc: RpcReject, message: impl Into<String>) -> Reply {
        let rpc = match rpc {
            RpcReject::UnknownMethod => "UnknownMethod",
            RpcReject::Failed => "Failed",
        };
        Reply(json!({ "id": id, "ok": false, "error": { "rpc": rpc, "message": message.into() } }))
    }

    /// A line over `MAX_FRAME`: `TooLarge`, `id` null (it was never read).
    pub fn too_large(limit: usize) -> Reply {
        Reply::error(
            &Value::Null,
            &StrataError::new(Code::TooLarge, format!("request line is longer than {limit} bytes"))
                .with_detail(json!({ "limit": limit })),
        )
    }

    /// The line, newline included.
    pub fn to_line(&self) -> Vec<u8> {
        let mut v = serde_json::to_vec(&self.0).expect("reply serializes");
        v.push(b'\n');
        v
    }

    pub fn write_to(&self, w: &mut dyn Write) -> io::Result<()> {
        w.write_all(&self.to_line())?;
        w.flush()
    }
}

/// Parse one request line. An `Err` is the reply to send instead.
pub fn parse_request(line: &[u8]) -> Result<Request, Reply> {
    let failed = |id: &Value, m: String| Reply::rejected(id, RpcReject::Failed, m);
    let v: Value = serde_json::from_slice(line).map_err(|e| failed(&Value::Null, format!("not json: {e}")))?;
    let Value::Object(mut obj) = v else {
        return Err(failed(&Value::Null, "a request is a json object".into()));
    };
    let id = match obj.remove("id") {
        Some(id @ (Value::String(_) | Value::Number(_))) => id,
        Some(_) => return Err(failed(&Value::Null, "id must be a string or a number".into())),
        None => return Err(failed(&Value::Null, "missing id".into())),
    };
    let method = match obj.remove("method") {
        Some(Value::String(m)) => m,
        Some(_) => return Err(failed(&id, "method must be a string".into())),
        None => return Err(failed(&id, "missing method".into())),
    };
    let params = obj.remove("params");
    if let Some(k) = obj.keys().next() {
        return Err(failed(&id, format!("unknown envelope field {k:?}")));
    }
    let Some(method) = Method::from_name(&method) else {
        return Err(Reply::rejected(&id, RpcReject::UnknownMethod, format!("unknown method {method:?}")));
    };
    let Some(params) = params else {
        return Err(failed(&id, "missing params".into()));
    };
    let params = Params::parse(method, params).map_err(|m| failed(&id, format!("{}: {m}", method.name())))?;
    Ok(Request { id, method, params })
}

/// What one read off the socket produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// One line, the newline stripped.
    Line(Vec<u8>),
    /// A line longer than the limit; it was read to its newline and dropped.
    TooLarge,
    /// End of stream (a trailing partial line is dropped).
    Eof,
}

/// Read one frame with the `MAX_FRAME` limit.
pub fn read_frame(r: &mut dyn BufRead) -> io::Result<Frame> {
    read_frame_limited(r, MAX_FRAME)
}

/// Read one newline-terminated frame of at most `limit` bytes.
pub fn read_frame_limited(r: &mut dyn BufRead, limit: usize) -> io::Result<Frame> {
    let mut line = Vec::new();
    let mut over = false;
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(Frame::Eof);
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], Some(i + 1)),
            None => (buf, None),
        };
        if !over {
            if line.len() + chunk.len() > limit {
                over = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let used = done.unwrap_or(buf.len());
        r.consume(used);
        if done.is_some() {
            return Ok(if over { Frame::TooLarge } else { Frame::Line(line) });
        }
    }
}

/// Readiness is "the socket accepts a connection" (no ping method, no pid
/// file): what launchd and the CLI both use.
pub fn ready(socket: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Serve one connection: frame by frame, parse, hand the request to
/// `handle`, write its reply. Envelope errors are answered here. Returns at
/// end of stream or when a reply cannot be written (the peer is gone; the
/// request that produced it is not replayed).
pub fn serve_connection(
    r: &mut dyn BufRead,
    w: &mut dyn Write,
    limit: usize,
    handle: &mut dyn FnMut(Request) -> Reply,
) -> io::Result<()> {
    loop {
        let reply = match read_frame_limited(r, limit)? {
            Frame::Eof => return Ok(()),
            Frame::TooLarge => Reply::too_large(limit),
            Frame::Line(line) => match parse_request(&line) {
                Ok(req) => handle(req),
                Err(reply) => reply,
            },
        };
        reply.write_to(w)?;
    }
}

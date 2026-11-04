//! Status/sync deadlines and optional stderr progress. No request is retried
//! after a timeout; the diagnostic probe is read-only and separately bounded.
use crate::{
    Result,
    args::WaitArgs,
    ctx::Ctx,
    rpc::{self, CallError, Kind},
};
use serde_json::{Value, json};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use strata_types::{Code, StrataError};

pub fn budget(args: &WaitArgs, default_secs: u64) -> Result<Option<Duration>> {
    let seconds = match args.timeout {
        Some(v) => v,
        None => match std::env::var("GIT_STRATA_TIMEOUT") {
            Ok(v) => v.parse().map_err(|_| invalid_timeout())?,
            Err(std::env::VarError::NotPresent) => default_secs as f64,
            Err(_) => return Err(invalid_timeout()),
        },
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(invalid_timeout());
    }
    if seconds == 0.0 {
        return Ok(None);
    }
    Duration::try_from_secs_f64(seconds).map(Some).map_err(|_| invalid_timeout())
}
fn invalid_timeout() -> StrataError {
    StrataError::new(
        Code::InvalidInput,
        "timeout must be finite, non-negative seconds (0 disables the deadline)",
    )
    .with_detail(json!({ "reason": "timeout" }))
}

pub fn deadline(args: &WaitArgs, default_secs: u64) -> Result<Option<Instant>> {
    budget(args, default_secs)?
        .map(|b| Instant::now().checked_add(b).ok_or_else(invalid_timeout))
        .transpose()
}

pub fn call(
    ctx: &Ctx,
    method: &str,
    params: Value,
    kind: Kind,
    args: &WaitArgs,
    default_secs: u64,
) -> std::result::Result<Value, CallError> {
    let deadline = deadline(args, default_secs).map_err(CallError::Failed)?;
    call_until(ctx, method, params, kind, args, deadline)
}

/// Parser-rejection fallbacks share this deadline with the initial call.
pub fn call_until(
    ctx: &Ctx,
    method: &str,
    params: Value,
    kind: Kind,
    args: &WaitArgs,
    deadline: Option<Instant>,
) -> std::result::Result<Value, CallError> {
    let session = params["session"].as_str().map(str::to_owned);
    let _progress = if args.progress || crate::tty::stderr_is_tty() {
        Progress::start(
            ctx.paths.socket.clone(),
            method.to_string(),
            session.clone(),
        )
    } else {
        None
    };
    let result = match deadline {
        Some(end) => rpc::call_within(
            &ctx.paths.socket,
            method,
            params,
            kind,
            end.saturating_duration_since(Instant::now()),
        ),
        None => rpc::call(&ctx.paths.socket, method, params, kind),
    };
    match result {
        Err(error @ CallError::Timeout { .. }) => {
            let mut error = error.into_error();
            let detail = error.detail.as_mut().expect("timeout detail");
            if error.code == Code::Unknown {
                let command = session
                    .as_ref()
                    .map(|s| format!("git-strata status --session {s}"))
                    .unwrap_or_else(|| "git-strata status".into());
                detail["fix"] = json!(format!("{command} shows the result when it finishes; do not retry, gc, or rm"));
            }
            match rpc::call_within(&ctx.paths.socket, "daemon_status", json!({}), Kind::Read, Duration::from_secs(2)) {
                Ok(v) => {
                    for key in ["holds", "queues"] {
                        if let Some(value) = v.get(key) {
                            detail[key] = value.clone();
                        }
                    }
                    if let Some(s) = session.as_ref()
                        && let Some(queue) =
                            v["queues"].as_array().and_then(|q| q.iter().find(|q| q["session"].as_str() == Some(s)))
                    {
                        detail["session"] = json!({ "id": queue["session"], "in_flight": queue["holder"],
                            "since": queue["since"], "queued": queue["queued"] });
                    }
                }
                Err(_) => detail["daemon_status"] = json!("unresponsive"),
            }
            Err(CallError::Failed(error))
        }
        other => other,
    }
}

struct Progress(mpsc::Sender<()>);
impl Progress {
    fn start(socket: std::path::PathBuf, method: String, session: Option<String>) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("rpc-progress".into())
            .spawn(move || {
                let start = Instant::now();
                let interval = Duration::from_secs(15);
                let mut next = start + interval;
                loop {
                    if rx.recv_timeout(next.saturating_duration_since(Instant::now())) != Err(mpsc::RecvTimeoutError::Timeout) {
                        break;
                    }
                    let state =
                        rpc::call_within(&socket, "daemon_status", json!({}), Kind::Read, Duration::from_secs(2)).ok();
                    if rx.try_recv() != Err(mpsc::TryRecvError::Empty) {
                        break;
                    }
                    eprintln!("{}", line(&method, session.as_deref(), start.elapsed(), state.as_ref()));
                    next += interval;
                }
            })
            .ok()?;
        Some(Self(tx))
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn line(method: &str, session: Option<&str>, elapsed: Duration, state: Option<&Value>) -> String {
    let mut text = format!(
        "still waiting ({}s): {method}{}",
        elapsed.as_secs(),
        session.map(|s| format!(" {s}")).unwrap_or_default()
    );
    let Some(state) = state else { return format!("{text}; daemon status unresponsive") };
    if let Some(queue) = state["queues"].as_array().and_then(|q| q.iter().find(|q| q["session"].as_str() == session))
        && let Some(holder) = queue["holder"].as_str()
    {
        if holder == method {
            text.push_str(&format!("; session held by {holder}"));
        } else {
            text.push_str(&format!(" queued behind {holder}"));
        }
    }
    for hold in state["holds"].as_array().into_iter().flatten() {
        if let (Some(repo), Some(holder)) = (hold["repo"].as_str(), hold["method"].as_str()) {
            text.push_str(&format!("; {repo} held by {holder}"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_accepts_old_and_new_daemon_shapes() {
        let old = json!({"holds":[{"repo":"r2", "method":"publish"}]});
        assert_eq!(
            line("sync", Some("s-ab12"), Duration::from_secs(45), Some(&old)),
            "still waiting (45s): sync s-ab12; r2 held by publish"
        );
        let new = json!({"queues":[{"session":"s-ab12", "holder":"old_tree"}], "holds":[]});
        assert!(line("sync", Some("s-ab12"), Duration::from_secs(45), Some(&new)).contains("queued behind old_tree"));
    }
}

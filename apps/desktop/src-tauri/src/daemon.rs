//! The read methods P1 uses (gui.md §8.1: existing methods only) and how
//! they are sent: one connection per request through `strata_client::call`
//! on a blocking thread, a per-method deadline, and at most one request
//! of each kind in flight.
//!
//! The deadline matters because `strata_client::call` has no read timeout
//! (P0 moved it verbatim; the cli keeps blocking). The blocking thread
//! cannot be cancelled, so the in-flight slot stays taken until the
//! daemon answers or closes: a stuck daemon costs one waiting connection
//! per read kind, not one per poll.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use strata_client::Kind;

use crate::error::GuiError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// `daemon_status{verify:false}`: in-memory counters, milliseconds.
    DaemonStatus,
    /// `status{all:true}`: runs drift / open-file checks per session, so
    /// it is slow on a busy store. P1 lists sessions with it at low
    /// frequency until P2 adds `sessions_summary` (gui.md §8.1).
    Sessions,
    /// `status{session}`: one session, the same per-workspace checks.
    Session(String),
    /// `du{refresh:false}`: the cached usage report.
    Du,
    /// `deps_list{repo:null}`: every deps proposal.
    DepsList,
}

impl Read {
    pub fn method(&self) -> &'static str {
        match self {
            Read::DaemonStatus => "daemon_status",
            Read::Sessions | Read::Session(_) => "status",
            Read::Du => "du",
            Read::DepsList => "deps_list",
        }
    }

    /// The params exactly as sent; tests/fixtures/p1/requests.json holds
    /// the same objects and crates/git-strata/tests/gui_fixtures.rs sends
    /// them to the real daemon.
    pub fn params(&self) -> Value {
        match self {
            Read::DaemonStatus => json!({ "verify": false }),
            Read::Sessions => json!({ "cwd": null, "session": null, "all": true }),
            Read::Session(id) => json!({ "cwd": null, "session": id, "all": false }),
            Read::Du => json!({ "refresh": false }),
            Read::DepsList => json!({ "repo": null }),
        }
    }

    /// gui.md ⓪: daemon_status 5 s (an answer takes milliseconds; none
    /// means the daemon is stuck or out of descriptors); status{all} 60 s
    /// (slow but alive on a busy store).
    pub fn deadline(&self) -> Duration {
        Duration::from_secs(match self {
            Read::DaemonStatus => 5,
            Read::Sessions => 60,
            Read::Session(_) => 30,
            Read::Du | Read::DepsList => 15,
        })
    }

    fn flight_key(&self) -> String {
        match self {
            Read::Session(id) => format!("status:{id}"),
            Read::Sessions => "status:*".to_string(),
            other => other.method().to_string(),
        }
    }
}

#[derive(Clone)]
pub struct Daemon {
    socket: PathBuf,
    flights: Arc<Mutex<HashSet<String>>>,
    /// Tests shorten every deadline to this.
    deadline_override: Option<Duration>,
}

/// Frees the in-flight slot when the blocking call returns (not when the
/// caller stops waiting).
struct Flight {
    flights: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.flights
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.key);
    }
}

impl Daemon {
    pub fn new(socket: PathBuf) -> Daemon {
        Daemon {
            socket,
            flights: Arc::new(Mutex::new(HashSet::new())),
            deadline_override: None,
        }
    }

    pub fn with_deadline(mut self, d: Duration) -> Daemon {
        self.deadline_override = Some(d);
        self
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub async fn read(&self, r: Read) -> Result<Value, GuiError> {
        let key = r.flight_key();
        if !self
            .flights
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.clone())
        {
            return Err(GuiError::in_flight(r.method()));
        }
        let flight = Flight {
            flights: self.flights.clone(),
            key,
        };
        let deadline = self.deadline_override.unwrap_or_else(|| r.deadline());
        let (socket, method, params) = (self.socket.clone(), r.method(), r.params());
        let task = tokio::task::spawn_blocking(move || {
            let _flight = flight;
            strata_client::call(&socket, method, params, Kind::Read)
        });
        match tokio::time::timeout(deadline, task).await {
            Err(_) => Err(GuiError::timeout(method, r.deadline().as_secs())),
            Ok(Err(join)) => Err(GuiError::internal(format!(
                "the `{method}` request thread failed: {join}"
            ))),
            Ok(Ok(reply)) => reply.map_err(GuiError::from),
        }
    }
}

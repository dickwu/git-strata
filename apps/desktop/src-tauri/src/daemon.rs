//! Caller-aware single-flight reads. Pollers join; window callers wait out
//! poller flights and start their own read, preserving refresh-after-event gates.
//! Blocking tasks record before freeing their slot, publish the result, then notify
//! the renderer. Caller deadlines never cancel a flight or discard its late reply.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use strata_client::Kind;
use strata_types::Code;

use crate::error::GuiError;
use crate::state::SnapshotStore;
use crate::tray_snapshot::Stamp;
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// `daemon_status{verify:false}`: in-memory counters, milliseconds.
    DaemonStatus,
    /// `status{all:true}`: runs drift / open-file checks per session, so
    /// it is slow on a busy store. P1 lists sessions with it at low
    /// frequency until P2 adds `sessions_summary` (gui.md §8.1).
    Sessions,
    /// `status{session, chains:true}`: one session, the same
    /// per-workspace checks, plus each repo's publish chain (a daemon
    /// with status-perf computes chains only when asked; an older one
    /// rejects the field and is asked again without it, see `read`).
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
            Read::Session(id) => {
                json!({ "cwd": null, "session": id, "all": false, "chains": true })
            }
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
    reads: Arc<Mutex<ReadFlights>>,
    pub(crate) store: Arc<SnapshotStore>,
    clock: Arc<dyn Fn() -> Stamp + Send + Sync>,
}

type Reply = Result<Value, GuiError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caller {
    Window,
    Poller,
}
struct ReadFlight {
    started_by: Caller,
    reply: watch::Receiver<Option<Reply>>,
}
#[derive(Default)]
struct ReadFlights {
    slots: HashMap<String, ReadFlight>,
    generations: HashMap<String, u64>,
}
struct ReadSlot {
    flights: Arc<Mutex<ReadFlights>>,
    key: String,
}
impl Drop for ReadSlot {
    fn drop(&mut self) {
        self.flights
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slots
            .remove(&self.key);
    }
}
async fn receive(mut rx: watch::Receiver<Option<Reply>>) -> Reply {
    loop {
        // A result may have arrived before this waiter subscribed.
        if let Some(reply) = rx.borrow().clone() {
            return reply;
        }
        if rx.changed().await.is_err() {
            return Err(GuiError::internal(
                "the request thread ended without a reply",
            ));
        }
    }
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
            reads: Arc::default(),
            store: Arc::default(),
            clock: Arc::new(Stamp::now),
        }
    }

    pub fn with_deadline(mut self, d: Duration) -> Daemon {
        self.deadline_override = Some(d);
        self
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// Take the in-flight slot `key`, or `request_in_flight` when it is
    /// already taken.
    fn take_flight(&self, key: String, method: &str) -> Result<Flight, GuiError> {
        if !self
            .flights
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.clone())
        {
            return Err(GuiError::in_flight(method));
        }
        Ok(Flight {
            flights: self.flights.clone(),
            key,
        })
    }

    pub async fn read(&self, r: Read) -> Reply {
        self.read_as(r, Caller::Window).await
    }
    pub async fn poll_read(&self, r: Read) -> Reply {
        self.read_as(r, Caller::Poller).await
    }

    async fn read_as(&self, r: Read, caller: Caller) -> Reply {
        let deadline = self.deadline_override.unwrap_or_else(|| r.deadline());
        let wait_until = tokio::time::Instant::now() + deadline;
        let key = r.flight_key();
        let shared = matches!(r, Read::DaemonStatus | Read::Sessions | Read::DepsList);
        if caller == Caller::Poller {
            self.store.attempt(&r, (self.clock)());
        }
        loop {
            let (rx, waiting) = {
                let mut flights = self.reads.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(flight) = flights.slots.get(&key) {
                    if !shared || (caller == Caller::Window && flight.started_by == Caller::Window)
                    {
                        return Err(GuiError::in_flight(r.method()));
                    }
                    (flight.reply.clone(), caller == Caller::Window)
                } else {
                    let generation = if shared {
                        let generation = flights.generations.entry(key.clone()).or_default();
                        *generation += 1;
                        Some(*generation)
                    } else {
                        None
                    };
                    let started = (self.clock)();
                    if caller == Caller::Window {
                        self.store.attempt(&r, started);
                    }
                    let (tx, rx) = watch::channel(None);
                    flights.slots.insert(
                        key.clone(),
                        ReadFlight {
                            started_by: caller,
                            reply: rx.clone(),
                        },
                    );
                    let slot = ReadSlot {
                        flights: self.reads.clone(),
                        key: key.clone(),
                    };
                    let (socket, r, store, clock) = (
                        self.socket.clone(),
                        r.clone(),
                        self.store.clone(),
                        self.clock.clone(),
                    );
                    tokio::task::spawn_blocking(move || {
                        // This local drops before captured tx even on unwind. Closed watches
                        // therefore never leave a dead slot for a window waiter to spin on.
                        let guard = slot;
                        let (method, mut params) = (r.method(), r.params());
                        let result = match strata_client::call(
                            &socket,
                            method,
                            params.clone(),
                            Kind::Read,
                        ) {
                            Err(strata_client::CallError::Failed(e))
                                if predates_chains(&params, &e) =>
                            {
                                if let Some(o) = params.as_object_mut() {
                                    o.remove("chains");
                                }
                                strata_client::call(&socket, method, params, Kind::Read)
                            }
                            other => other,
                        }
                        .map_err(GuiError::from);
                        store.record(&r, generation, started, clock(), &result);
                        drop(guard);
                        let _ = tx.send(Some(result));
                        store.notify();
                    });
                    (rx, false)
                }
            };
            let expires = if waiting {
                wait_until
            } else {
                tokio::time::Instant::now() + deadline
            };
            match tokio::time::timeout_at(expires, receive(rx)).await {
                Err(_) => {
                    self.store.timeout(&r, (self.clock)());
                    return Err(GuiError::timeout(r.method(), r.deadline().as_secs()));
                }
                Ok(_) if waiting => {
                    // Discard the old flight, including errors/closed watches. Recheck
                    // under the lock; the wait budget never restarts for another poller.
                    if tokio::time::Instant::now() >= wait_until {
                        self.store.timeout(&r, (self.clock)());
                        return Err(GuiError::timeout(r.method(), r.deadline().as_secs()));
                    }
                }
                Ok(result) => return result,
            }
        }
    }
}

/// `deps_approve` params (gui-approve design, D2=b): `{proposal,
/// sessions}` only. `sessions` is `null` (every session the daemon finds
/// on the repo) or a non-empty list; `[]` is refused here, before any
/// socket I/O (gui.md §5.4 item 3). Nothing else is sent: the daemon's
/// `DepsApproveParams` has `deny_unknown_fields`, and the human gate is
/// the daemon's own system prompt, so the GUI has no field that could
/// stand for it.
pub fn approve_params(proposal: u64, sessions: Option<Vec<String>>) -> Result<Value, GuiError> {
    if proposal == 0 {
        return Err(GuiError::new(
            Code::InvalidInput,
            "proposal ids start at 1",
            json!({ "reason": "bad_proposal", "proposal": proposal }),
        ));
    }
    if let Some(list) = &sessions {
        if list.is_empty() {
            return Err(GuiError::new(
                Code::InvalidInput,
                "an empty session list would approve nothing; send null for every session",
                json!({ "reason": "empty_sessions" }),
            ));
        }
        if list.iter().any(|s| s.trim().is_empty()) {
            return Err(GuiError::new(
                Code::InvalidInput,
                "a session id is empty",
                json!({ "reason": "bad_session" }),
            ));
        }
    }
    Ok(json!({ "proposal": proposal, "sessions": sessions }))
}

impl Daemon {
    /// The GUI's one daemon write: `deps_approve`. The daemon shows the
    /// Touch ID / password prompt itself and moves nothing unless the
    /// human passes it.
    ///
    /// No deadline: the prompt alone may wait 60 s, and a cold target
    /// installs dependencies first (minutes). Giving up early would
    /// only turn a reply into "unknown". The front end offers "Stop
    /// waiting", which detaches the modal; the flight slot stays taken
    /// until the daemon answers, so a second approve cannot be sent
    /// meanwhile (`request_in_flight`). Reads keep their own keys.
    pub async fn deps_approve(
        &self,
        proposal: u64,
        sessions: Option<Vec<String>>,
    ) -> Result<Value, GuiError> {
        let params = approve_params(proposal, sessions)?;
        let flight = self.take_flight(APPROVE.to_string(), APPROVE)?;
        let socket = self.socket.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _flight = flight;
            strata_client::call(&socket, APPROVE, params, Kind::Write)
        });
        match task.await {
            Err(join) => Err(GuiError::internal(format!(
                "the `{APPROVE}` request thread failed: {join}"
            ))),
            Ok(reply) => reply.map_err(GuiError::from),
        }
    }
}

const APPROVE: &str = "deps_approve";

/// The request carried `chains` and the daemon refused it as an unknown
/// param: RPC `Failed` (cli D7 maps it to `InvalidInput` with
/// `detail.rpc == "Failed"`) whose message names the field.
fn predates_chains(params: &Value, e: &strata_types::StrataError) -> bool {
    params.get("chains").is_some()
        && e.detail
            .as_ref()
            .and_then(|d| d.get("rpc"))
            .and_then(Value::as_str)
            == Some("Failed")
        && e.message.contains("unknown field `chains`")
}

#[cfg(test)]
#[path = "daemon_tray_tests.rs"]
mod tray_tests;

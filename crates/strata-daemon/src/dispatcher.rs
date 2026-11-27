//! The serving worker owns a parked socket and its BufReader together.
//! Its connection thread sleeps without retaining a second parsed request.

use std::collections::VecDeque;
use std::io::{self, BufReader, Read};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use strata_types::StrataError;

use super::{Drain, State, Write};
use crate::busy::BusyLog;
use crate::rpc::{self, DrainParams, Frame, Method, Params, Reply, Request};
use crate::{MAX_FRAME, Result};

const PARKED_LIMIT: usize = 64;
const PARKED_BYTES: usize = 32 * 1024 * 1024;
const REFUSING_IDLE: Duration = Duration::from_secs(5);
// A refusing stream notices terminal stop within one poll slice.
const REFUSING_POLL: Duration = Duration::from_millis(50);

struct Parked {
    seq: u64,
    connection: u64,
    // reader.buffer() owns *all* bytes read past the first newline, including
    // another complete frame or part of a multibyte character.
    reader: BufReader<UnixStream>,
    frame: Box<[u8]>,
    received_at: Instant,
    // Keep the shutdown clone in custody too. Reattachment cannot fail at
    // the descriptor limit and shutdown cannot find either fd while parked.
    shutdown_stream: UnixStream,
}

struct ConnectionGuard<'a> {
    drain: &'a Drain,
    id: u64,
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.drain.state().connections.remove(&self.id);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Park,
    Replay,
    Stop,
}

pub(super) struct Parking {
    id: String,
    to: u32,
    entries: VecDeque<Parked>,
    bytes: usize,
    disposition: Disposition,
    active: usize,
}

impl Parking {
    pub(super) fn holding(&self) -> bool {
        self.disposition == Disposition::Park
    }

    pub(super) fn refusal(&self) -> StrataError {
        StrataError::upgrading(&self.id, "park", 2000)
    }

    pub(super) fn status(&self, state: &State) -> Value {
        json!({
            "id": self.id, "phase": if self.disposition == Disposition::Replay { "resuming" } else { "park" },
            "to": self.to, "parked": self.entries.len(),
            "waiting_on": { "inflight": state.inflight.values().collect::<Vec<_>>(),
                            "background": state.background }
        })
    }
}

enum Route {
    Serve(BufReader<UnixStream>, Result<Option<Write>>),
    RefuseGeneration(BufReader<UnixStream>, StrataError),
    Parked(u64),
}

// Release the next first frame after the handler returns, even on unwind.
// Socket writes and subsequent reads never hold another stream's turn.
struct ReplayTurn<'a>(&'a Drain);

impl Drop for ReplayTurn<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        let parking = state.parking.as_mut().expect("active replay");
        parking.active -= 1;
        if parking.active == 0 && parking.entries.is_empty() {
            state.parking = None;
        }
        self.0.inner.replay.notify_all();
    }
}

// Counts a generation-bound stream from its first refusal until it ends, so
// terminal stop can wait for its queued replies (CR r2 #3).
struct Refusing<'a>(&'a Drain);

impl Drop for Refusing<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.refusing -= 1;
        self.0.inner.replay.notify_all();
    }
}

// Bytes already queued on a stream, for a stop that answers only those.
fn queued_bytes(socket: &UnixStream) -> usize {
    let mut queued: libc::c_int = 0;
    // SAFETY: this socket is live and queued points to one writable int.
    if unsafe { libc::ioctl(socket.as_raw_fd(), libc::FIONREAD, &mut queued) } < 0 {
        return 0;
    }
    queued.max(0) as usize
}

// Only generation-bound refusals expire on idle. Poll before each socket
// read so fresh bytes extend the idle interval even within a partial frame.
// Once terminal stop begins, read only the bytes queued at that moment, as
// refuse_parked does, then end: stop waits for this stream, never for a
// client that keeps sending.
struct RefusingSocket<'a> {
    socket: &'a UnixStream,
    drain: &'a Drain,
    queued: Option<usize>,
}

impl Read for RefusingSocket<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let deadline = Instant::now() + REFUSING_IDLE;
        let mut fd = libc::pollfd {
            fd: self.socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            if self.queued.is_none() && self.drain.state().closing {
                self.queued = Some(queued_bytes(self.socket));
            }
            if let Some(left) = self.queued {
                if left == 0 || buf.is_empty() {
                    return Ok(0);
                }
                let take = buf.len().min(left);
                let n = self.socket.read(&mut buf[..take])?;
                self.queued = Some(if n == 0 { 0 } else { left - n });
                return Ok(n);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(0);
            }
            let slice = left.min(REFUSING_POLL);
            // SAFETY: fd is one initialized pollfd for a live borrowed socket.
            let ready = unsafe { libc::poll(&mut fd, 1, slice.as_millis() as i32 + 1) };
            if ready > 0 {
                return self.socket.read(buf);
            }
            if ready == 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn refused_frame(line: &[u8], error: &StrataError, log: &mut dyn io::Write) -> Reply {
    let id = match rpc::received(line, log) {
        Ok(req) => req.id,
        Err(mut reply) => reply.0["id"].take(),
    };
    Reply::error(&id, error)
}

// The first frame and every complete frame already held by this reader have
// never run. Finish their replies before shutdown regains the stream.
fn refuse_parked(
    reader: BufReader<UnixStream>,
    first: &Value,
    error: &StrataError,
    log: &mut dyn io::Write,
) -> io::Result<()> {
    let mut socket = reader.get_ref();
    socket.set_write_timeout(Some(REFUSING_IDLE))?;
    // Snapshot bytes already queued on this stream. A frame can span the saved
    // read-ahead and those bytes. Stop never waits for the rest of a partial frame.
    let queued = queued_bytes(socket);
    Reply::error(first, error).write_to(&mut socket)?;
    let suffix = reader.buffer().chain(socket.take(queued as u64));
    refuse_suffix(suffix, socket, error, log)
}

fn refuse_generation(
    drain: &Drain,
    reader: BufReader<UnixStream>,
    first: Request,
    error: &StrataError,
    log: &mut dyn io::Write,
) -> io::Result<()> {
    let mut socket = reader.get_ref();
    socket.set_write_timeout(Some(REFUSING_IDLE))?;
    Reply::error(&first.id, error).write_to(&mut socket)?;
    drop(first);
    let rest = RefusingSocket {
        socket,
        drain,
        queued: None,
    };
    refuse_suffix(reader.buffer().chain(rest), socket, error, log)
}

fn refuse_suffix(
    input: impl Read,
    mut socket: &UnixStream,
    error: &StrataError,
    log: &mut dyn io::Write,
) -> io::Result<()> {
    let mut input = BufReader::new(input);
    loop {
        let reply = match rpc::read_frame(&mut input)? {
            Frame::Eof => return Ok(()),
            Frame::TooLarge => Reply::too_large(MAX_FRAME),
            Frame::Line(line) => refused_frame(&line, error, log),
        };
        reply.write_to(&mut socket)?;
    }
}

impl Drain {
    pub(crate) fn begin_parking(&self, id: &str, to: u32) -> bool {
        let mut state = self.state();
        if state.closing || state.parking.is_some() {
            return false;
        }
        state.parking = Some(Parking {
            id: id.to_owned(),
            to,
            entries: VecDeque::new(),
            bytes: 0,
            disposition: Disposition::Park,
            active: 0,
        });
        true
    }

    fn release_parked(&self, state: &mut State, disposition: Disposition) {
        if let Some(p) = &mut state.parking {
            p.disposition = disposition;
            if p.entries.is_empty() && p.active == 0 {
                state.parking = None;
            }
        }
        self.inner.replay.notify_all();
    }

    pub(crate) fn abort_parking(&self, id: &str) -> bool {
        let mut state = self.state();
        if state.closing || state.parking.as_ref().is_none_or(|p| p.id != id) {
            return false;
        }
        self.release_parked(&mut state, Disposition::Replay);
        true
    }

    pub(super) fn stop_parking(&self, poisoned: bool) -> MutexGuard<'_, State> {
        let mut state = self.state();
        state.closing = true;
        if let Some(refusal) = state.parking.as_ref().map(Parking::refusal) {
            state.terminal_upgrade = Some(refusal);
            self.release_parked(&mut state, Disposition::Stop);
        } else if state.refusing == 0 {
            return state;
        }
        // Keep the lock queues available to work admitted before H2. A
        // poisoned store retains its existing recovery shutdown policy.
        // Generation-bound streams (which can outlive an aborted H2) finish
        // the frames already queued on them before read halves close.
        self.inner
            .replay
            .wait_while(state, |s| {
                s.refusing != 0
                    || s.parking.is_some()
                    || (s.terminal_upgrade.is_some()
                        && !poisoned
                        && (!s.inflight.is_empty() || s.background != 0))
            })
            .unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn register(&self, id: u64, stream: UnixStream) {
        self.state().connections.insert(id, stream);
    }

    pub(crate) fn open_connections(&self) -> usize {
        self.state().connections.len()
    }

    // Detached connections still consume a place in the process's accept budget.
    pub(crate) fn held_connections(&self) -> usize {
        let state = self.state();
        state.connections.len() + state.parking.as_ref().map_or(0, |p| p.entries.len())
    }

    pub(crate) fn shutdown_connections(&self, how: Shutdown) {
        for stream in self.state().connections.values() {
            let _ = stream.shutdown(how);
        }
    }

    fn route(
        &self,
        id: u64,
        reader: BufReader<UnixStream>,
        mut frame: Vec<u8>,
        received_at: Instant,
        req: &Request,
        bound: bool,
    ) -> Route {
        let mut state = self.state();
        let Some(p) = state.parking.as_ref().filter(|p| p.holding()) else {
            return Route::Serve(reader, self.admit_request_locked(&mut state, req));
        };
        if bound {
            let mut error = p.refusal();
            error.detail.as_mut().expect("upgrading detail")["generation_changed"] = json!(true);
            // Counted under the lock that saw the hold, so a stop that starts
            // later always waits for this stream.
            state.refusing += 1;
            return Route::RefuseGeneration(reader, error);
        }
        let bytes = frame.len() + 1 + reader.buffer().len();
        if p.entries.len() >= PARKED_LIMIT || bytes > PARKED_BYTES - p.bytes {
            return Route::Serve(reader, Err(p.refusal()));
        }
        let seq = state.next;
        state.next += 1;
        let shutdown_stream = state.connections.remove(&id).expect("registered reader");
        let p = state.parking.as_mut().expect("under the drain lock");
        frame.push(b'\n');
        p.entries.push_back(Parked {
            seq,
            connection: id,
            reader,
            frame: frame.into_boxed_slice(),
            received_at,
            shutdown_stream,
        });
        p.bytes += bytes;
        Route::Parked(seq)
    }

    fn take_parked(&self, seq: u64) -> (BufReader<UnixStream>, Request, ReplayTurn<'_>) {
        let state = self.state();
        let mut state = self
            .inner
            .replay
            .wait_while(state, |s| {
                let p = s.parking.as_ref().expect("a parked reader owns an entry");
                match p.disposition {
                    Disposition::Park => true,
                    Disposition::Replay => {
                        p.active != 0 || p.entries.front().is_none_or(|e| e.seq != seq)
                    }
                    Disposition::Stop => false,
                }
            })
            .unwrap_or_else(|p| p.into_inner());
        let p = state.parking.as_mut().expect("active replay");
        // Abort admits FIFO. Terminal refusals are independent so one peer
        // that stopped reading cannot hold the replies to every other peer.
        let index = p
            .entries
            .iter()
            .position(|entry| entry.seq == seq)
            .expect("owned parked frame");
        p.active += 1;
        let entry = p.entries.remove(index).expect("owned parked frame");
        p.bytes -= entry.frame.len() + entry.reader.buffer().len();
        state
            .connections
            .insert(entry.connection, entry.shutdown_stream);
        drop(state);
        let turn = ReplayTurn(self);
        tracing::debug!(
            seq,
            held_ms = entry.received_at.elapsed().as_millis(),
            "released parked RPC"
        );
        let req = rpc::parse_request(&entry.frame).expect("frame validated before parking");
        (entry.reader, req, turn)
    }

    pub(crate) fn serve_stream(
        &self,
        id: u64,
        stream: UnixStream,
        busy: &BusyLog,
        log: &mut dyn io::Write,
        handle: &(dyn Fn(Request, Option<Write>) -> Reply + Sync),
    ) -> io::Result<()> {
        let _connection = ConnectionGuard { drain: self, id };
        let mut reader = BufReader::new(stream);
        let mut bound = false;
        loop {
            let line = match rpc::read_frame(&mut reader)? {
                Frame::Eof => return Ok(()),
                Frame::TooLarge => {
                    Reply::too_large(MAX_FRAME).write_to(reader.get_mut())?;
                    continue;
                }
                Frame::Line(line) => line,
            };
            let received_at = Instant::now();
            let mut req = match rpc::received(&line, log) {
                Ok(req) => req,
                Err(reply) => {
                    reply.write_to(reader.get_mut())?;
                    continue;
                }
            };
            // The App's identity probe is an ordinary daemon_status. A lease id
            // can also arrive on a fresh stream for a renewal, seal or cancel.
            bound |= matches!(
                &req.params,
                Params::Drain(
                    DrainParams::Seal { .. }
                        | DrainParams::Lease {
                            lease_id: Some(_),
                            ..
                        }
                )
            );
            let mut turn = None;
            let admission = if req.method.may_admit() {
                match self.route(id, reader, line, received_at, &req, bound) {
                    Route::Serve(r, admission) => {
                        reader = r;
                        admission
                    }
                    Route::RefuseGeneration(r, error) => {
                        let _refusing = Refusing(self);
                        return refuse_generation(self, r, req, &error, log);
                    }
                    Route::Parked(seq) => {
                        drop(req);
                        let (r, request, replay) = self.take_parked(seq);
                        let mut state = self.state();
                        if let Some(error) = state.terminal_upgrade.clone() {
                            drop(state);
                            return refuse_parked(r, &request.id, &error, log);
                        }
                        let admission = self.admit_request_locked(&mut state, &request);
                        reader = r;
                        req = request;
                        turn = Some(replay);
                        admission
                    }
                }
            } else {
                Ok(None)
            };
            let binds = req.method == Method::DaemonStatus
                || matches!(
                    &req.params,
                    Params::Drain(DrainParams::Lease { on: true, .. })
                );
            let reply = match admission {
                Ok(write) => req.serve(|req| handle(req, write)),
                Err(error) => {
                    busy.record(
                        req.method.name(),
                        crate::handlers::session_of(&req.params).as_ref(),
                        &error,
                    );
                    Reply::error(&req.id, &error)
                }
            };
            drop(turn);
            bound |= binds && reply.0["ok"] == true;
            reply.write_to(reader.get_mut())?;
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/dispatcher.rs"]
mod tests;

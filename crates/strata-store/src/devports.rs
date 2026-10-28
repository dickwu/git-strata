//! Per-workspace dev-port leases (store.md §4.4a): a `dev_ports` row gives
//! one (session, repo) the 4 ports `base..base+3` (web, connector,
//! connector_mcp, aux) for the life of the session. The daemon allocates at
//! `create_session` / `add_workspaces` and on `ensure_dev_ports`; the row is
//! released with the session (`set_lifecycle` → Removed).

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::sql::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::json;
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::errors::{NOT_WRITTEN, bad_row, err, no_such_repo, no_such_session, sql, sql_read};
use crate::sessions::{session_lifecycle, workspace};
use crate::txn::Tx;
use crate::{Result, Store};

/// Ports per lease.
pub const DEV_PORT_BLOCK: u16 = 4;
/// At most this many unleased blocks are bind-probed per allocation.
const MAX_PROBES: usize = 64;

/// One lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DevPortLease {
    pub session: SessionId,
    pub repo: RepoName,
    pub base: u16,
}

impl DevPortLease {
    pub fn web(&self) -> u16 {
        self.base
    }
    pub fn connector(&self) -> u16 {
        self.base + 1
    }
    pub fn connector_mcp(&self) -> u16 {
        self.base + 2
    }
    pub fn aux(&self) -> u16 {
        self.base + 3
    }
}

/// What the daemon allocates from: the usable block bases (config
/// `[dev_ports]`, `DevPorts::blocks`) and the test of one port (production:
/// [`bind_probe`] is `Free`).
#[derive(Clone)]
pub struct DevPortsPolicy {
    pub blocks: Vec<u16>,
    pub port_ok: Arc<dyn Fn(u16) -> bool + Send + Sync>,
}

impl DevPortsPolicy {
    /// The production policy: a port is usable when [`bind_probe`] finds it free.
    pub fn probing(blocks: Vec<u16>) -> Self {
        Self { blocks, port_ok: Arc::new(|p| bind_probe(p) == Probe::Free) }
    }
}

impl std::fmt::Debug for DevPortsPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevPortsPolicy").field("blocks", &self.blocks.len()).finish_non_exhaustive()
    }
}

/// Result of [`bind_probe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Free,
    InUse,
    /// Another errno (EMFILE, EACCES, …).
    Error(i32),
}

/// Bind-probe `port` the way a dev server will bind it: `SO_REUSEADDR` (as
/// Node/libuv sets it, so TIME_WAIT remnants do not count), on `0.0.0.0`,
/// `[::]` (dual-stack) and `127.0.0.1`, each its own socket, closed at once.
/// A family the host lacks (`EAFNOSUPPORT` / `EADDRNOTAVAIL`) is skipped.
pub fn bind_probe(port: u16) -> Probe {
    for addr in [Addr::Any4, Addr::Any6, Addr::Loopback4] {
        match probe_one(port, addr) {
            Ok(()) => {}
            Err(e) if e == libc::EAFNOSUPPORT || e == libc::EADDRNOTAVAIL || e == libc::EPROTONOSUPPORT => {}
            Err(e) if e == libc::EADDRINUSE => return Probe::InUse,
            Err(e) => return Probe::Error(e),
        }
    }
    Probe::Free
}

#[derive(Clone, Copy)]
enum Addr {
    Any4,
    Any6,
    Loopback4,
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn probe_one(port: u16, addr: Addr) -> std::result::Result<(), i32> {
    let v6 = matches!(addr, Addr::Any6);
    // SAFETY: plain socket calls on a descriptor this function owns and
    // closes on every path; the sockaddr structs are zeroed then filled.
    unsafe {
        let fd = libc::socket(if v6 { libc::AF_INET6 } else { libc::AF_INET }, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(errno());
        }
        let r = (|| {
            let one: libc::c_int = 1;
            let sz = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            if libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, (&one as *const libc::c_int).cast(), sz) < 0 {
                return Err(errno());
            }
            let rc = if v6 {
                let zero: libc::c_int = 0;
                if libc::setsockopt(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, (&zero as *const libc::c_int).cast(), sz) < 0 {
                    return Err(errno());
                }
                let mut a: libc::sockaddr_in6 = std::mem::zeroed();
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                {
                    a.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
                }
                a.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                a.sin6_port = port.to_be();
                libc::bind(fd, (&a as *const libc::sockaddr_in6).cast(), std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t)
            } else {
                let mut a: libc::sockaddr_in = std::mem::zeroed();
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                {
                    a.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
                }
                a.sin_family = libc::AF_INET as libc::sa_family_t;
                a.sin_port = port.to_be();
                a.sin_addr.s_addr = match addr {
                    Addr::Loopback4 => u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
                    _ => libc::INADDR_ANY,
                };
                libc::bind(fd, (&a as *const libc::sockaddr_in).cast(), std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t)
            };
            if rc < 0 {
                return Err(errno());
            }
            if libc::listen(fd, 1) < 0 {
                return Err(errno());
            }
            Ok(())
        })();
        libc::close(fd);
        r
    }
}

/// FNV-1a 64 of `session/repo`: the preferred block index.
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn base_of(v: i64) -> Result<u16> {
    u16::try_from(v).ok().filter(|b| b % DEV_PORT_BLOCK == 0).ok_or_else(|| bad_row(format!("dev_ports.base {v} is not a 4-aligned port")))
}

/// Pick a block for (session, repo): walk the policy's blocks from the
/// preferred one (or from just after `after`), skipping leased bases, and
/// take the first whose 4 ports all pass `port_ok`. At most [`MAX_PROBES`]
/// candidates are probed. `None`: nothing usable.
pub(crate) fn allocate(c: &Connection, policy: &DevPortsPolicy, session: &str, repo: &str, after: Option<u16>) -> crate::sql::SqlResult<Option<u16>> {
    let n = policy.blocks.len();
    if n == 0 {
        return Ok(None);
    }
    let mut st = c.prepare("SELECT base FROM dev_ports")?;
    let leased: BTreeSet<i64> = st.query_map([], |r| r.get::<_, i64>(0))?.collect::<crate::sql::SqlResult<_>>()?;
    let start = match after {
        Some(old) => policy.blocks.iter().position(|b| *b > old).unwrap_or(0),
        None => (fnv1a64(&format!("{session}/{repo}")) % n as u64) as usize,
    };
    let mut probed = 0;
    for i in 0..n {
        let b = policy.blocks[(start + i) % n];
        if leased.contains(&i64::from(b)) || Some(b) == after {
            continue;
        }
        if probed == MAX_PROBES {
            break;
        }
        probed += 1;
        if (b..b + DEV_PORT_BLOCK).all(|p| (policy.port_ok)(p)) {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

/// Allocate and insert a lease for a workspace row just inserted in this
/// transaction (no lease when nothing is usable: never blocks `new`/`add`).
pub(crate) fn lease_new(c: &Connection, policy: Option<&DevPortsPolicy>, session: &str, repo: &str) -> crate::sql::SqlResult<()> {
    let Some(policy) = policy else { return Ok(()) };
    if let Some(b) = allocate(c, policy, session, repo, None)? {
        c.execute("INSERT INTO dev_ports (session, repo, base) VALUES (?1, ?2, ?3)", params![session, repo, i64::from(b)])?;
    }
    Ok(())
}

fn exhausted(session: &str, repo: &str) -> StrataError {
    err(
        Code::Busy,
        format!("no free dev-port block for {session}/{repo}; leases free up as sessions are removed"),
        json!({ "reason": "ports_exhausted", "session": session, "repo": repo }),
    )
}

/// The lifecycle gate of `ensure` / `reassign` (inside their txn).
fn require_active(c: &Connection, op: &'static str, id: &str, repo: &str) -> Result<()> {
    let w = |e| sql(e, op, NOT_WRITTEN);
    let lc = session_lifecycle(c, id).map_err(w)?.ok_or_else(|| no_such_session(id))?;
    match lc.as_str() {
        "active" => {}
        "unknown" => {
            return Err(err(
                Code::InvalidInput,
                format!("session {id} is in lifecycle unknown; it keeps any lease it has but gets no new one. Inspect it with `git-strata status --session {id}` and hand it to the human"),
                json!({ "reason": "session_unknown", "session": id, "lifecycle": lc }),
            ));
        }
        _ => {
            return Err(err(
                Code::NoSuchSession,
                format!("session {id} is {lc}, not active"),
                json!({ "reason": "not_active", "session": id, "lifecycle": lc }),
            ));
        }
    }
    if workspace(c, id, repo).map_err(w)?.is_none() {
        return Err(no_such_repo(id, repo));
    }
    Ok(())
}

fn lease_of(c: &Connection, id: &str, repo: &str) -> crate::sql::SqlResult<Option<i64>> {
    c.query_row("SELECT base FROM dev_ports WHERE session = ?1 AND repo = ?2", [id, repo], |r| r.get(0)).optional()
}

impl Store {
    /// Set the allocation policy (the daemon, from config, after `open`).
    /// Without one, `create_session` / `add_workspaces` lease nothing and
    /// `ensure` / `reassign` fail `Unsupported`.
    pub fn set_dev_ports_policy(&self, policy: Option<DevPortsPolicy>) {
        *self.dev_ports.lock().unwrap_or_else(|p| p.into_inner()) = policy.map(Arc::new);
    }

    pub(crate) fn dev_ports_policy(&self) -> Option<Arc<DevPortsPolicy>> {
        self.dev_ports.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn required_policy(&self) -> Result<Arc<DevPortsPolicy>> {
        self.dev_ports_policy().ok_or_else(|| err(Code::Unsupported, "this store handle has no dev-ports policy", json!({ "reason": "no_policy" })))
    }

    /// Every lease (of `session`, if given), by session then repo. `None`
    /// when the store predates the `dev_ports` table (schema v1, read-only
    /// handle): every workspace counts as unleased.
    pub fn dev_port_leases(&self, session: Option<&SessionId>) -> Result<Option<Vec<DevPortLease>>> {
        self.read(|c| {
            let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(sql_read)?;
            if v < 2 {
                return Ok(None);
            }
            let mut st = c
                .prepare("SELECT session, repo, base FROM dev_ports WHERE ?1 IS NULL OR session = ?1 ORDER BY session, repo")
                .map_err(sql_read)?;
            let rows = st
                .query_map([session.map(SessionId::as_str)], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>()
                .map_err(sql_read)?;
            let mut out = Vec::with_capacity(rows.len());
            for (s, r, b) in rows {
                out.push(DevPortLease {
                    session: SessionId::try_from(s).map_err(|e| bad_row(format!("dev_ports.session: {}", e.message)))?,
                    repo: RepoName::try_from(r).map_err(|e| bad_row(format!("dev_ports.repo: {}", e.message)))?,
                    base: base_of(b)?,
                });
            }
            Ok(Some(out))
        })
    }

    /// The lease of an `Active` session's workspace, allocating one if it
    /// has none (sessions created before the table). Idempotent.
    /// `NoSuchSession reason=not_active` (creating/removing/removed),
    /// `InvalidInput reason=session_unknown`, `NoSuchRepo`, or
    /// `Busy reason=ports_exhausted`.
    pub fn ensure_dev_ports(&self, id: &SessionId, repo: &RepoName) -> Result<DevPortLease> {
        const OP: &str = "ensure_dev_ports";
        self.writable()?;
        let policy = self.required_policy()?;
        let (sid, r) = (id.as_str(), repo.as_str());
        let base = self.write(OP, |c| {
            require_active(c, OP, sid, r)?;
            if let Some(b) = lease_of(c, sid, r).map_err(|e| sql(e, OP, NOT_WRITTEN))? {
                return Ok(Tx::Rollback(base_of(b)?));
            }
            let b = allocate(c, &policy, sid, r, None).map_err(|e| sql(e, OP, NOT_WRITTEN))?.ok_or_else(|| exhausted(sid, r))?;
            c.execute("INSERT INTO dev_ports (session, repo, base) VALUES (?1, ?2, ?3)", params![sid, r, i64::from(b)])
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(Tx::Commit(b))
        })?;
        Ok(DevPortLease { session: id.clone(), repo: repo.clone(), base })
    }

    /// Move the lease of an `Active` session's workspace to another usable
    /// block (searching from just after the old one); with no lease, this
    /// is `ensure`. Nothing else usable: the old lease stays, `Busy
    /// reason=ports_exhausted`. Same lifecycle errors as `ensure`.
    pub fn reassign_dev_ports(&self, id: &SessionId, repo: &RepoName) -> Result<DevPortLease> {
        const OP: &str = "reassign_dev_ports";
        self.writable()?;
        let policy = self.required_policy()?;
        let (sid, r) = (id.as_str(), repo.as_str());
        let base = self.write(OP, |c| {
            require_active(c, OP, sid, r)?;
            let old = lease_of(c, sid, r).map_err(|e| sql(e, OP, NOT_WRITTEN))?.map(base_of).transpose()?;
            let b = allocate(c, &policy, sid, r, old).map_err(|e| sql(e, OP, NOT_WRITTEN))?.ok_or_else(|| exhausted(sid, r))?;
            c.execute("DELETE FROM dev_ports WHERE session = ?1 AND repo = ?2", [sid, r]).map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            c.execute("INSERT INTO dev_ports (session, repo, base) VALUES (?1, ?2, ?3)", params![sid, r, i64::from(b)])
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(Tx::Commit(b))
        })?;
        Ok(DevPortLease { session: id.clone(), repo: repo.clone(), base })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_probe_sees_a_listener_and_its_absence() {
        let l = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert_eq!(bind_probe(port), Probe::InUse);
        drop(l);
        assert_eq!(bind_probe(port), Probe::Free);
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert_eq!(bind_probe(l.local_addr().unwrap().port()), Probe::InUse);
    }

    #[test]
    fn fnv_is_the_reference_function() {
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
    }
}

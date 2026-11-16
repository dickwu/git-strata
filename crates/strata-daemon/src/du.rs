//! `du` (daemon.md §5.12): cached, CoW-aware disk usage of the strata root.
//!
//! The root is cut into units, each sized by [`strata_apfs::usage_with`]
//! (logical, allocated and private bytes) on a background pass and kept in
//! `<root>/daemon/du/cache.json` (the store schema is pinned, so the cache
//! and the session records live beside it, not in it):
//!
//! | unit | kind |
//! |---|---|
//! | `store/store.db*`, `store/LOCK` | live: sized on every call (a few files) |
//! | `store/blobs/<shard>`, `store/trash/<entry>`, `bases/<repo>/<tree>`, `deps/<repo>/<set>` | immutable: valid while the unit root's inode and mtime are unchanged, for at most a day |
//! | `store/tmp`, `bases|deps/<repo>/.<name>`, `sessions/<sid>`, `sessions/.repos/<sid>` | mutable: also stale after ten minutes |
//!
//! `report` lists the units (one readdir per directory level, one lstat per
//! unit), answers from the cache with an as-of time per figure and queues
//! every missing or stale unit for the background pass; `refresh` queues
//! all of them. One pass at a time, [`WORKERS`] threads, I/O at utility
//! priority, cancelled when the daemon stops.
//!
//! Session savings: what full copies would occupy (logical) minus what the
//! session owns alone (private). `sessions.jsonl` gets one line per session
//! the first time it is sized (`new` for sessions created by this daemon,
//! `seen` for older ones) and one at `rm` (the last cached figures), so the
//! cumulative figure covers every session since the first line; sessions
//! removed before that are unknown. A `new` record also carries
//! `create_ms`, the wall time of the `new` handler that made the session
//! (absent on records written before this field, and on `seen`).
//!
//! Scan progress has two readers. `report` (`scan` in the `du` reply) locks
//! the scan state; `daemon_status.scan` reads [`Du::counts`], copies of the
//! counters stored each time the scan state is unlocked after a change, so
//! it never waits for `scan` or `cache` (a worker holds `cache` under
//! `scan`, and `report` holds `cache` across its lstats).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_apfs::{Usage, usage_of, usage_with};

/// Mutable units are re-sized after this.
pub const MUTABLE_TTL: Duration = Duration::from_secs(600);
/// Immutable units are re-sized after this even when their root looks unchanged.
pub const IMMUTABLE_TTL: Duration = Duration::from_secs(24 * 3600);
/// Scan threads of one pass.
pub const WORKERS: usize = 4;
/// How long a hold waits for a running scan of its session to stop
/// (status-perf D8); past it the caller gets `Busy du_cancel_timeout`.
pub const HOLD_WAIT: Duration = Duration::from_secs(5);
/// store's gc age floor for trash entries (store.md §5.11).
pub const TRASH_FLOOR: Duration = Duration::from_secs(3600);
const SAVE_EVERY: Duration = Duration::from_secs(3);
const CACHE_VERSION: u32 = 1;
const MAX_FAILED: usize = 20;

/// The command that empties eligible trash (never run by `du`).
pub const GC_COMMAND: &str = "git-strata gc";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn ts(ms: u64) -> Value {
    strata_types::serde_rfc3339::format(&(UNIX_EPOCH + Duration::from_millis(ms)))
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The sizes of one unit or a sum of units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Figures {
    pub logical: u64,
    pub allocated: u64,
    pub private: u64,
    pub files: u64,
    pub errors: u64,
    /// Every file reported a private size (APFS).
    pub exact: bool,
}

impl Figures {
    fn zero() -> Figures {
        Figures {
            exact: true,
            ..Figures::default()
        }
    }
    fn add(&mut self, o: &Figures) {
        self.logical += o.logical;
        self.allocated += o.allocated;
        self.private += o.private;
        self.files += o.files;
        self.errors += o.errors;
        self.exact &= o.exact;
    }
}

impl From<Usage> for Figures {
    fn from(u: Usage) -> Figures {
        Figures {
            logical: u.logical,
            allocated: u.allocated,
            private: u.private,
            files: u.files,
            errors: u.errors,
            exact: u.private_exact,
        }
    }
}

/// One cached unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub fig: Figures,
    /// Inode and mtime of the unit root when its scan started.
    pub ino: u64,
    pub mtime_ns: i64,
    /// When its scan started (ms since the epoch).
    pub as_of_ms: u64,
    pub took_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullPass {
    pub started_ms: u64,
    pub ended_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    pub units: BTreeMap<String, Entry>,
    #[serde(default)]
    pub last_full: Option<FullPass>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Always,
    Immutable,
    Mutable,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Db,
    Blobs,
    Tmp,
    Trash,
    StoreOther,
    Base(String),
    Deps(String),
    Session(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub key: String,
    pub kind: Kind,
    pub group: Group,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Fresh,
    Stale,
    Pending,
}

/// Cache invalidation (§5.12): a unit's entry is fresh while its root has
/// the same inode and mtime and the entry is younger than its kind's TTL.
pub fn status(e: Option<&Entry>, kind: Kind, ino: u64, mtime_ns: i64, now_ms: u64) -> Status {
    let Some(e) = e else { return Status::Pending };
    let ttl = match kind {
        Kind::Always => return Status::Stale,
        Kind::Immutable => IMMUTABLE_TTL,
        Kind::Mutable => MUTABLE_TTL,
    };
    if e.ino != ino
        || e.mtime_ns != mtime_ns
        || now_ms.saturating_sub(e.as_of_ms) > ttl.as_millis() as u64
    {
        Status::Stale
    } else {
        Status::Fresh
    }
}

/// The units of a [`list_units`] call, and the directories it could not read.
#[derive(Debug, Default)]
pub struct Listing {
    pub units: Vec<Unit>,
    /// Key prefixes (`sessions/`, `bases/app/` …) of directories that exist
    /// but could not be listed: a unit under one is unknown, not gone.
    pub unlisted: Vec<String>,
}

impl Listing {
    /// Whether `key` may still exist although this listing lacks it.
    fn unknown(&self, key: &str) -> bool {
        self.unlisted.iter().any(|p| key.starts_with(p.as_str()))
    }
}

/// The sorted names in `root/rel`. A missing directory has none; one that
/// cannot be read (`EACCES`, `EIO` …, also part way through) has none either
/// and goes to `unlisted` as `rel/` (status-bar §11.4: only a confirmed
/// disappearance drops a unit).
fn names(root: &Path, rel: &str, unlisted: &mut Vec<String>) -> Vec<String> {
    let read = fs::read_dir(root.join(rel)).and_then(|d| {
        d.map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
    });
    match read {
        Ok(v) => {
            let mut v: Vec<String> = v.into_iter().filter_map(|n| n.into_string().ok()).collect();
            v.sort();
            v
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            unlisted.push(format!("{rel}/"));
            Vec::new()
        }
    }
}

/// Every unit under `root` (§5.12 table). Cheap: readdir of the top levels.
pub fn list_units(root: &Path) -> Listing {
    let mut out = Vec::new();
    let mut unlisted = Vec::new();
    let mut push = |key: String, kind: Kind, group: Group| out.push(Unit { key, kind, group });
    for n in names(root, "store", &mut unlisted) {
        match n.as_str() {
            "blobs" => {
                for s in names(root, "store/blobs", &mut unlisted) {
                    push(format!("store/blobs/{s}"), Kind::Immutable, Group::Blobs);
                }
            }
            "trash" => {
                for s in names(root, "store/trash", &mut unlisted) {
                    push(format!("store/trash/{s}"), Kind::Immutable, Group::Trash);
                }
            }
            "tmp" => push("store/tmp".into(), Kind::Mutable, Group::Tmp),
            n if n.starts_with("store.db") || n == "LOCK" => {
                push(format!("store/{n}"), Kind::Always, Group::Db)
            }
            n => push(format!("store/{n}"), Kind::Mutable, Group::StoreOther),
        }
    }
    for (top, base) in [("bases", true), ("deps", false)] {
        for repo in names(root, top, &mut unlisted) {
            for n in names(root, &format!("{top}/{repo}"), &mut unlisted) {
                let kind = if n.starts_with('.') {
                    Kind::Mutable
                } else {
                    Kind::Immutable
                };
                let group = if base {
                    Group::Base(repo.clone())
                } else {
                    Group::Deps(repo.clone())
                };
                push(format!("{top}/{repo}/{n}"), kind, group);
            }
        }
    }
    for n in names(root, "sessions", &mut unlisted) {
        if n == ".repos" {
            for s in names(root, "sessions/.repos", &mut unlisted) {
                push(
                    format!("sessions/.repos/{s}"),
                    Kind::Mutable,
                    Group::Session(s.clone()),
                );
            }
        } else {
            push(
                format!("sessions/{n}"),
                Kind::Mutable,
                Group::Session(n.clone()),
            );
        }
    }
    Listing {
        units: out,
        unlisted,
    }
}

/// Inode and mtime (ns) of `p`, not following a symlink.
fn stat(p: &Path) -> std::io::Result<(u64, i64)> {
    let m = fs::symlink_metadata(p)?;
    Ok((
        m.ino(),
        m.mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(m.mtime_nsec()),
    ))
}

/// Trash entry names start with the retire time in ms (`<ms>-<hex32>`).
fn trash_age_ms(root: &Path, key: &str, now: u64) -> u64 {
    let name = key.rsplit('/').next().unwrap_or("");
    if let Some(ms) = name
        .split_once('-')
        .and_then(|(ms, _)| ms.parse::<u64>().ok())
    {
        return now.saturating_sub(ms);
    }
    match fs::symlink_metadata(root.join(key)) {
        Ok(m) => {
            let t = (m.mtime().max(m.ctime()).max(0) as u64) * 1000;
            now.saturating_sub(t)
        }
        Err(_) => 0,
    }
}

/// A sum of units with its freshness.
#[derive(Clone, Debug)]
struct Agg {
    fig: Figures,
    units: u64,
    stale: u64,
    pending: u64,
    oldest: Option<u64>,
}

impl Default for Agg {
    fn default() -> Agg {
        Agg {
            fig: Figures::zero(),
            units: 0,
            stale: 0,
            pending: 0,
            oldest: None,
        }
    }
}

impl Agg {
    fn add(&mut self, e: Option<&Entry>, s: Status) {
        self.units += 1;
        match s {
            Status::Pending => self.pending += 1,
            Status::Stale => self.stale += 1,
            Status::Fresh => {}
        }
        if let Some(e) = e {
            self.fig.add(&e.fig);
            self.oldest = Some(self.oldest.map_or(e.as_of_ms, |o| o.min(e.as_of_ms)));
        }
    }
    fn merge(&mut self, o: &Agg) {
        self.fig.add(&o.fig);
        self.units += o.units;
        self.stale += o.stale;
        self.pending += o.pending;
        self.oldest = match (self.oldest, o.oldest) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    fn json(&self) -> Value {
        let f = &self.fig;
        json!({
            "logical": f.logical,
            "allocated": f.allocated,
            "private": f.private,
            "shared": f.allocated.saturating_sub(f.private),
            "files": f.files,
            "exact": f.exact,
            "errors": f.errors,
            "units": self.units,
            "stale": self.stale,
            "pending": self.pending,
            "complete": self.pending == 0,
            "as_of": self.oldest.map(ts).unwrap_or(Value::Null),
        })
    }
}

#[derive(Debug, Default)]
struct ScanState {
    running: bool,
    started_ms: u64,
    queue: VecDeque<String>,
    queued: HashSet<String>,
    done: u64,
    failed: VecDeque<(String, String)>,
    full_started: Option<u64>,
    /// Sessions created by this daemon whose `new` record is still due,
    /// with the wall time of their `new` when it was measured.
    new_due: HashMap<String, Option<u64>>,
    last_save: Option<Instant>,
    /// Sessions held by a write (status-perf D8): their units are neither
    /// queued nor scanned while held. A count per session.
    holds: HashMap<String, usize>,
    /// The scans running now, each with its own cancel token.
    active: HashMap<String, Arc<AtomicBool>>,
    /// Units whose last scan failed: error and when (ms), one entry per
    /// unit, dropped when the unit is measured or confirmed gone (`ENOENT`,
    /// or absent from a listing that could read its directory). `failed` is
    /// the history of attempts instead.
    unresolved: BTreeMap<String, (String, u64)>,
}

/// The counters `daemon_status.scan` reads ([`Du::counts`]).
#[derive(Debug, Default)]
struct Counts {
    running: AtomicBool,
    active: AtomicU64,
    queued: AtomicU64,
    done: AtomicU64,
    full: AtomicBool,
    unresolved: AtomicU64,
}

impl Counts {
    fn copy(&self, st: &ScanState) {
        let n = |len: usize| len as u64;
        self.running.store(st.running, Ordering::Relaxed);
        self.active.store(n(st.active.len()), Ordering::Relaxed);
        self.queued.store(n(st.queue.len()), Ordering::Relaxed);
        self.done.store(st.done, Ordering::Relaxed);
        self.full
            .store(st.full_started.is_some(), Ordering::Relaxed);
        self.unresolved
            .store(n(st.unresolved.len()), Ordering::Relaxed);
    }
}

/// The scan state, locked ([`Du::scan_state`]). Dropping it copies the counters
/// into [`Du::counts`] while the lock is still held, so the copies follow
/// every change made under it, in order.
struct ScanGuard<'a> {
    counts: &'a Counts,
    st: MutexGuard<'a, ScanState>,
}

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.counts.copy(&self.st);
    }
}

impl std::ops::Deref for ScanGuard<'_> {
    type Target = ScanState;
    fn deref(&self) -> &ScanState {
        &self.st
    }
}

impl std::ops::DerefMut for ScanGuard<'_> {
    fn deref_mut(&mut self) -> &mut ScanState {
        &mut self.st
    }
}

impl ScanState {
    fn held(&self, key: &str) -> bool {
        session_of(key).is_some_and(|sid| self.holds.contains_key(sid))
    }

    fn scanning(&self, sid: &str) -> bool {
        self.active.keys().any(|k| session_of(k) == Some(sid))
    }
}

/// The session a unit key belongs to: the first path component after
/// `sessions/.repos/` or `sessions/` (`.repos` itself is no session).
/// The one decoder for keys → sessions (recording, removal, holds).
pub fn session_of(key: &str) -> Option<&str> {
    let rest = key
        .strip_prefix("sessions/.repos/")
        .or_else(|| key.strip_prefix("sessions/"))?;
    let sid = rest.split('/').next().unwrap_or("");
    (!sid.is_empty() && sid != ".repos").then_some(sid)
}

/// A write's hold on its sessions' du units (status-perf D8); released on
/// drop. Units become queueable again; nothing is queued by the release.
pub struct DuHold {
    du: Arc<Du>,
    sids: Vec<String>,
}

impl Drop for DuHold {
    fn drop(&mut self) {
        let mut st = lock(&self.du.scan);
        for sid in &self.sids {
            if let Some(n) = st.holds.get_mut(sid) {
                *n -= 1;
                if *n == 0 {
                    st.holds.remove(sid);
                }
            }
        }
    }
}

/// The du state of one daemon; shared with its scan thread.
/// Lock order (review P1-4): `recorded` → `scan` → `cache`. A path that
/// needs two of them takes them in that order or one at a time.
pub struct Du {
    root: PathBuf,
    dir: PathBuf,
    cache: Mutex<CacheFile>,
    scan: Mutex<ScanState>,
    /// Copies of `scan`'s counters, read without a lock.
    counts: Counts,
    /// Signalled (with `scan`) when a scan ends.
    scan_done: Condvar,
    recorded: Mutex<Option<HashSet<String>>>,
    stop: AtomicBool,
}

/// Owns the [`Du`]; stops its scan when dropped (the daemon going away).
pub struct DuHandle(pub Arc<Du>);

impl Drop for DuHandle {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::SeqCst);
    }
}

impl std::ops::Deref for DuHandle {
    type Target = Arc<Du>;
    fn deref(&self) -> &Arc<Du> {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub event: String,
    pub session: String,
    pub at_ms: u64,
    #[serde(default)]
    pub fig: Option<Figures>,
    #[serde(default)]
    pub as_of_ms: Option<u64>,
    /// `new` records only: how long the `new` handler took (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_ms: Option<u64>,
}

impl Du {
    /// `root` is the strata root; state goes to `<root>/daemon/du/`.
    pub fn open(root: &Path) -> DuHandle {
        let dir = root.join("daemon/du");
        let cache = fs::read(dir.join("cache.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or(CacheFile {
                version: CACHE_VERSION,
                ..CacheFile::default()
            });
        DuHandle(Arc::new(Du {
            root: root.to_path_buf(),
            dir,
            cache: Mutex::new(cache),
            scan: Mutex::new(ScanState::default()),
            counts: Counts::default(),
            scan_done: Condvar::new(),
            recorded: Mutex::new(None),
            stop: AtomicBool::new(false),
        }))
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Lock the scan state; the counters are copied when the guard drops.
    fn scan_state(&self) -> ScanGuard<'_> {
        ScanGuard {
            counts: &self.counts,
            st: lock(&self.scan),
        }
    }

    /// `daemon_status.scan`: `{running, active, queued, done, full,
    /// unresolved_n}` from the copied counters (no lock; the fields may be
    /// a moment apart).
    pub fn counts(&self) -> Value {
        let c = &self.counts;
        json!({
            "running": c.running.load(Ordering::Relaxed),
            "active": c.active.load(Ordering::Relaxed),
            "queued": c.queued.load(Ordering::Relaxed),
            "done": c.done.load(Ordering::Relaxed),
            "full": c.full.load(Ordering::Relaxed),
            "unresolved_n": c.unresolved.load(Ordering::Relaxed),
        })
    }

    pub fn cache_path(&self) -> PathBuf {
        self.dir.join("cache.json")
    }

    pub fn records_path(&self) -> PathBuf {
        self.dir.join("sessions.jsonl")
    }

    /// The `du` reply: cached figures now, missing or stale units queued
    /// (every unit when `refresh`).
    pub fn report(self: &Arc<Self>, refresh: bool) -> Value {
        let t0 = Instant::now();
        let now = now_ms();
        let listing = list_units(&self.root);
        let units = &listing.units;
        let mut due = Vec::new();
        let mut groups: BTreeMap<Group, Agg> = BTreeMap::new();
        let (mut eligible, mut young) = (Agg::default(), Agg::default());
        let (mut eligible_n, mut young_n) = (0u64, 0u64);
        let present: HashSet<&str> = units.iter().map(|u| u.key.as_str()).collect();
        // A unit missing from the listing is gone unless it is queued or
        // under a directory that could not be listed (status-bar §11.4).
        let keep = |k: &str, queued: &HashSet<String>| {
            present.contains(k) || queued.contains(k) || listing.unknown(k)
        };
        // Review P1-4: lock order is `recorded` → `scan` → `cache`; the
        // queue is read before the cache lock, never inside it. A unit gone
        // is no longer unresolved.
        let queued = {
            let mut st = self.scan_state();
            let ScanState {
                unresolved, queued, ..
            } = &mut *st;
            unresolved.retain(|k, _| keep(k, queued));
            queued.clone()
        };
        {
            let mut cache = lock(&self.cache);
            cache.units.retain(|k, _| keep(k, &queued));
            for u in units {
                let path = self.root.join(&u.key);
                let Ok((ino, mtime)) = stat(&path) else {
                    continue;
                };
                let (entry, st) = if u.kind == Kind::Always {
                    match usage_of(&path) {
                        Ok(us) => (
                            Some(Entry {
                                fig: us.into(),
                                ino,
                                mtime_ns: mtime,
                                as_of_ms: now,
                                took_ms: 0,
                            }),
                            Status::Fresh,
                        ),
                        Err(_) => (None, Status::Pending),
                    }
                } else {
                    let e = cache.units.get(&u.key).copied();
                    let s = status(e.as_ref(), u.kind, ino, mtime, now);
                    if refresh || s != Status::Fresh {
                        due.push(u.key.clone());
                    }
                    (e, s)
                };
                groups
                    .entry(u.group.clone())
                    .or_default()
                    .add(entry.as_ref(), st);
                if u.group == Group::Trash {
                    if trash_age_ms(&self.root, &u.key, now) >= TRASH_FLOOR.as_millis() as u64 {
                        eligible.add(entry.as_ref(), st);
                        eligible_n += 1;
                    } else {
                        young.add(entry.as_ref(), st);
                        young_n += 1;
                    }
                }
            }
        }
        self.enqueue(due, refresh);

        let get = |g: &Group| groups.get(g).cloned().unwrap_or_default();
        let mut store = Agg::default();
        for g in [
            Group::Db,
            Group::Blobs,
            Group::Tmp,
            Group::Trash,
            Group::StoreOther,
        ] {
            store.merge(&get(&g));
        }
        let mut total = store.clone();
        let mut bases = serde_json::Map::new();
        let mut deps = serde_json::Map::new();
        let mut sessions = Vec::new();
        let recs = self.records();
        let created = Self::create_times(&recs);
        let mut live = Agg::default();
        let mut bases_all = Agg::default();
        let mut deps_all = Agg::default();
        for (g, a) in &groups {
            match g {
                Group::Base(r) => {
                    bases.insert(r.clone(), a.json());
                    bases_all.merge(a);
                }
                Group::Deps(r) => {
                    deps.insert(r.clone(), a.json());
                    deps_all.merge(a);
                }
                Group::Session(s) => {
                    let mut j = a.json();
                    j["session"] = json!(s);
                    j["savings"] = json!(a.fig.logical.saturating_sub(a.fig.private));
                    j["create_ms"] = created.get(s.as_str()).map_or(Value::Null, |&ms| json!(ms));
                    sessions.push(j);
                    live.merge(a);
                }
                _ => {}
            }
        }
        total.merge(&bases_all);
        total.merge(&deps_all);
        total.merge(&live);
        let mut trash = get(&Group::Trash).json();
        trash["entries"] = json!(eligible_n + young_n);
        let mut el = eligible.json();
        el["entries"] = json!(eligible_n);
        el["reclaim_min"] = json!(eligible.fig.private);
        el["reclaim_max"] = json!(eligible.fig.allocated);
        let mut yo = young.json();
        yo["entries"] = json!(young_n);
        trash["eligible"] = el;
        trash["young"] = yo;
        trash["floor_secs"] = json!(TRASH_FLOOR.as_secs());
        trash["gc"] = json!(GC_COMMAND);

        json!({
            "root": self.root.to_string_lossy(),
            "now": ts(now),
            "total": total.json(),
            "store": {
                "all": store.json(),
                "db": get(&Group::Db).json(),
                "blobs": get(&Group::Blobs).json(),
                "tmp": get(&Group::Tmp).json(),
                "other": get(&Group::StoreOther).json(),
                "trash": trash,
            },
            "bases": { "all": bases_all.json(), "repos": bases },
            "deps": { "all": deps_all.json(), "repos": deps },
            "sessions": { "all": live.json(), "list": sessions },
            "savings": {
                "live": {
                    "logical": live.fig.logical,
                    "private": live.fig.private,
                    "savings": live.fig.logical.saturating_sub(live.fig.private),
                    "as_of": live.oldest.map(ts).unwrap_or(Value::Null),
                    "complete": live.pending == 0,
                },
                "cumulative": self.cumulative(),
            },
            "scan": self.scan_json(),
            "took_ms": t0.elapsed().as_millis() as u64,
        })
    }

    fn scan_json(&self) -> Value {
        let st = lock(&self.scan);
        let last_full = lock(&self.cache).last_full;
        json!({
            "running": st.running,
            "started": if st.running { ts(st.started_ms) } else { Value::Null },
            "done": st.done,
            "queued": st.queue.len(),
            "active": st.active.len(),
            "full": st.full_started.is_some(),
            "failed": st.failed.iter().map(|(k, e)| json!({ "unit": k, "error": e })).collect::<Vec<_>>(),
            "unresolved": st.unresolved.iter().map(|(k, (e, at))| json!({ "unit": k, "error": e, "at": ts(*at) })).collect::<Vec<_>>(),
            "last_full": last_full.map(|f| json!({ "started": ts(f.started_ms), "ended": ts(f.ended_ms) })).unwrap_or(Value::Null),
        })
    }

    /// `create_ms` per session, from its `new` record.
    fn create_times(recs: &[Record]) -> HashMap<&str, u64> {
        recs.iter()
            .filter_map(|r| Some((r.session.as_str(), r.create_ms?)))
            .collect()
    }

    /// Records, latest per session, summed.
    fn cumulative(&self) -> Value {
        let recs = self.records();
        let created = Self::create_times(&recs);
        let since = recs.iter().map(|r| r.at_ms).min();
        let mut latest: HashMap<&str, &Record> = HashMap::new();
        for r in &recs {
            if r.fig.is_some() {
                latest.insert(r.session.as_str(), r);
            } else {
                latest.entry(r.session.as_str()).or_insert(r);
            }
        }
        let (mut logical, mut private, mut known, mut unknown, mut removed) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        let rm: HashSet<&str> = recs
            .iter()
            .filter(|r| r.event == "rm")
            .map(|r| r.session.as_str())
            .collect();
        // Sessions with both a measured `new` and figures: the time and size of the same set.
        let (mut timed, mut create_ms, mut timed_logical, mut timed_files) =
            (0u64, 0u64, 0u64, 0u64);
        for (s, r) in &latest {
            if rm.contains(s) {
                removed += 1;
            }
            match &r.fig {
                Some(f) => {
                    known += 1;
                    logical += f.logical;
                    private += f.private;
                    if let Some(&ms) = created.get(s) {
                        timed += 1;
                        create_ms += ms;
                        timed_logical += f.logical;
                        timed_files += f.files;
                    }
                }
                None => unknown += 1,
            }
        }
        json!({
            "since": since.map(ts).unwrap_or(Value::Null),
            "sessions": latest.len(),
            "removed": removed,
            "sized": known,
            "unsized": unknown,
            "logical": logical,
            "private": private,
            "savings": logical.saturating_sub(private),
            "before_since": "unknown",
            "timed": timed,
            "create_ms": create_ms,
            "timed_logical": timed_logical,
            "timed_files": timed_files,
        })
    }

    pub fn records(&self) -> Vec<Record> {
        match fs::read_to_string(self.records_path()) {
            Ok(t) => t
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Append one record line. Callers hold `recorded` (so a session's
    /// first-sizing record is written once even with two units in flight).
    fn append(&self, set: &mut HashSet<String>, r: &Record) {
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.records_path())
        {
            let mut line = serde_json::to_vec(r).unwrap_or_default();
            line.push(b'\n');
            let _ = f.write_all(&line);
        }
        set.insert(r.session.clone());
    }

    fn recorded(&self) -> MutexGuard<'_, Option<HashSet<String>>> {
        let mut g = lock(&self.recorded);
        if g.is_none() {
            *g = Some(self.records().into_iter().map(|r| r.session).collect());
        }
        g
    }

    fn session_keys(sid: &str) -> [String; 2] {
        [format!("sessions/{sid}"), format!("sessions/.repos/{sid}")]
    }

    /// After `new`: size the session first; its `new` record follows.
    pub fn on_new(self: &Arc<Self>, sid: &str) {
        self.on_new_timed(sid, None);
    }

    /// [`Du::on_new`] with the wall time `new` took; the `new` record carries it as `create_ms`.
    pub fn on_new_timed(self: &Arc<Self>, sid: &str, took: Option<Duration>) {
        let ms = took.map(|d| d.as_millis().min(u64::MAX as u128) as u64);
        lock(&self.scan).new_due.insert(sid.to_owned(), ms);
        let keys = Self::session_keys(sid);
        {
            let mut st = self.scan_state();
            for k in keys.iter().rev() {
                if !st.held(k) && st.queued.insert(k.clone()) {
                    st.queue.push_front(k.clone());
                }
            }
        }
        self.kick();
    }

    /// After `rm`: one `rm` record with the last cached figures, then the
    /// session's entries leave the cache.
    pub fn on_remove(&self, sid: &str) {
        let keys = Self::session_keys(sid);
        let (fig, as_of) = {
            let mut cache = lock(&self.cache);
            let es: Vec<Entry> = keys.iter().filter_map(|k| cache.units.remove(k)).collect();
            if es.is_empty() {
                (None, None)
            } else {
                let mut f = Figures::zero();
                for e in &es {
                    f.add(&e.fig);
                }
                (Some(f), es.iter().map(|e| e.as_of_ms).min())
            }
        };
        {
            let mut st = self.scan_state();
            st.new_due.remove(sid);
            for k in &keys {
                st.unresolved.remove(k);
            }
        }
        let mut rec = self.recorded();
        let set = rec.get_or_insert_with(HashSet::new);
        self.append(
            set,
            &Record {
                event: "rm".into(),
                session: sid.to_owned(),
                at_ms: now_ms(),
                fig,
                as_of_ms: as_of,
                create_ms: None,
            },
        );
        drop(rec);
        self.save();
    }

    /// Hold `sids` against du (status-perf D8): their queued units leave the
    /// queue, no unit of theirs is queued or scanned while held, and a
    /// running scan of one is cancelled and waited for, up to `wait`. Past
    /// it: `Busy` (`du_cancel_timeout`), nothing held.
    pub fn hold_sessions(self: &Arc<Self>, sids: &[&str], wait: Duration) -> crate::Result<DuHold> {
        let end = Instant::now() + wait;
        let mut st = lock(&self.scan);
        for sid in sids {
            *st.holds.entry((*sid).to_owned()).or_default() += 1;
        }
        let hold = DuHold {
            du: Arc::clone(self),
            sids: sids.iter().map(|s| (*s).to_owned()).collect(),
        };
        let mine = |k: &str| session_of(k).is_some_and(|s| sids.contains(&s));
        st.queue.retain(|k| !mine(k));
        let active: HashSet<&String> = st.active.keys().collect();
        let gone: Vec<String> = st
            .queued
            .iter()
            .filter(|k| mine(k) && !active.contains(k))
            .cloned()
            .collect();
        for k in gone {
            st.queued.remove(&k);
        }
        for (k, t) in &st.active {
            if mine(k) {
                t.store(true, Ordering::SeqCst);
            }
        }
        // The raw lock (the wait below needs it): copy the counters by hand.
        self.counts.copy(&st);
        while sids.iter().any(|sid| st.scanning(sid)) {
            let now = Instant::now();
            if now >= end {
                drop(st);
                drop(hold);
                return Err(strata_types::StrataError::new(
                    strata_types::Code::Busy,
                    format!(
                        "the disk-usage scan of {} did not stop within {}s",
                        sids.join(", "),
                        wait.as_secs()
                    ),
                )
                .with_detail(json!({ "reason": "du_cancel_timeout", "sessions": sids })));
            }
            st = self
                .scan_done
                .wait_timeout(st, end - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        drop(st);
        Ok(hold)
    }

    /// Test-only: plant a session unit in `active` whose cancel token is never
    /// honoured by a real scan (the unit-test shape at the hold timeout). A
    /// later [`Self::hold_sessions`] wait past `wait` answers
    /// `Busy`/`du_cancel_timeout`. No effect on production builds.
    #[cfg(feature = "test-seams")]
    pub fn plant_stuck_scan_for_test(self: &Arc<Self>, sid: &str) -> Arc<AtomicBool> {
        let key = format!("sessions/{sid}");
        let token = Arc::new(AtomicBool::new(false));
        self.scan_state().active.insert(key, token.clone());
        token
    }

    /// Test-only: drop a plant from [`Self::plant_stuck_scan_for_test`] and wake
    /// any hold waiting on it.
    #[cfg(feature = "test-seams")]
    pub fn clear_stuck_scan_for_test(self: &Arc<Self>, sid: &str) {
        let key = format!("sessions/{sid}");
        self.scan_state().active.remove(&key);
        self.scan_done.notify_all();
    }

    fn enqueue(self: &Arc<Self>, keys: Vec<String>, full: bool) {
        {
            let mut st = self.scan_state();
            if full && st.full_started.is_none() {
                st.full_started = Some(now_ms());
            }
            for k in keys {
                if !st.held(&k) && st.queued.insert(k.clone()) {
                    st.queue.push_back(k);
                }
            }
            if st.queue.is_empty() && st.full_started.is_none() {
                return;
            }
        }
        self.kick();
    }

    /// Start the pass thread unless one runs.
    fn kick(self: &Arc<Self>) {
        {
            let mut st = self.scan_state();
            if st.running || self.stop.load(Ordering::SeqCst) {
                return;
            }
            st.running = true;
            st.started_ms = now_ms();
            st.done = 0;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("strata-du".into())
            .spawn(move || me.pass());
        if spawned.is_err() {
            self.scan_state().running = false;
        }
    }

    /// One pass: workers drain the queue; then save and stop (or go again
    /// if keys arrived meanwhile).
    fn pass(&self) {
        utility_io();
        loop {
            std::thread::scope(|s| {
                for _ in 0..WORKERS {
                    s.spawn(|| {
                        utility_io();
                        self.work();
                    });
                }
            });
            let mut st = self.scan_state();
            if st.queue.is_empty() || self.stop.load(Ordering::SeqCst) {
                if let Some(started) = st.full_started.take()
                    && !self.stop.load(Ordering::SeqCst)
                {
                    lock(&self.cache).last_full = Some(FullPass {
                        started_ms: started,
                        ended_ms: now_ms(),
                    });
                }
                st.running = false;
                drop(st);
                self.save();
                return;
            }
        }
    }

    fn work(&self) {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            // Dequeue and register as active in one `scan` critical
            // section: a hold sees either a queued key or an active scan.
            let (key, token) = {
                let mut st = self.scan_state();
                let key = loop {
                    match st.queue.pop_front() {
                        Some(k) if st.held(&k) => {
                            st.queued.remove(&k);
                        }
                        Some(k) => break k,
                        None => return,
                    }
                };
                let token = Arc::new(AtomicBool::new(false));
                st.active.insert(key.clone(), token.clone());
                (key, token)
            };
            self.scan_unit_with(&key, &token);
            let save = {
                let mut st = self.scan_state();
                st.active.remove(&key);
                st.queued.remove(&key);
                st.done += 1;
                self.scan_done.notify_all();
                let due = st.last_save.is_none_or(|t| t.elapsed() >= SAVE_EVERY);
                if due {
                    st.last_save = Some(Instant::now());
                }
                due
            };
            if save {
                self.save();
            }
        }
    }

    /// Size one unit now (the pass calls this; tests call it directly).
    pub fn scan_unit(&self, key: &str) {
        self.scan_unit_with(key, &AtomicBool::new(false));
    }

    /// [`Du::scan_unit`], given up when `cancel` is set (a hold). A
    /// cancelled scan writes nothing: the cache keeps its last figures.
    fn scan_unit_with(&self, key: &str, cancel: &AtomicBool) {
        self.scan_unit_using(key, cancel, &|p, stop| usage_with(p, stop));
    }

    /// [`Du::scan_unit_with`] with the sizing function given (tests reach
    /// the walk's `NoSuchPath` branch without racing a rename).
    fn scan_unit_using(&self, key: &str, cancel: &AtomicBool, size: &SizeFn) {
        let path = self.root.join(key);
        let stopped = || self.stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst);
        let (ino, mtime) = match stat(&path) {
            Ok(s) => s,
            // Only a confirmed disappearance forgets the unit (status-bar
            // §11.4); any other error leaves it failed, figures kept.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.forget_unless_cancelled(key, cancel);
                return;
            }
            Err(e) => {
                let e = strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &path);
                self.fail_unless_stopped(key, e.message, &stopped);
                return;
            }
        };
        let started = now_ms();
        let t = Instant::now();
        match size(&path, &stopped) {
            Ok(u) => {
                let e = Entry {
                    fig: u.into(),
                    ino,
                    mtime_ns: mtime,
                    as_of_ms: started,
                    took_ms: t.elapsed().as_millis() as u64,
                };
                // The last cancel check and the write are one `scan`
                // critical section, serialised with a hold's registration.
                let mut st = self.scan_state();
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                st.unresolved.remove(key);
                lock(&self.cache).units.insert(key.to_owned(), e);
            }
            Err(e) if e.code == strata_types::Code::NoSuchPath => {
                if !self.forget_unless_cancelled(key, cancel) {
                    return;
                }
            }
            Err(e) => {
                self.fail_unless_stopped(key, e.message, &stopped);
                return;
            }
        }
        if let Some(sid) = session_of(key) {
            self.maybe_record(sid);
        }
    }

    /// Record a failed measurement of `key`: one more `failed` entry and its
    /// `unresolved` error. The cache keeps its last figures. Nothing when
    /// the daemon stops or a hold cancelled the scan.
    fn fail_unless_stopped(&self, key: &str, error: String, stopped: &dyn Fn() -> bool) {
        if stopped() {
            return;
        }
        let mut st = self.scan_state();
        st.failed.push_back((key.to_owned(), error.clone()));
        while st.failed.len() > MAX_FAILED {
            st.failed.pop_front();
        }
        st.unresolved.insert(key.to_owned(), (error, now_ms()));
    }

    /// Drop `key`'s cached figures (and its unresolved failure) because its
    /// root is gone (`ENOENT`), unless a hold cancelled the scan: the check
    /// and the removal are one `scan` critical section, like the insert.
    /// False when cancelled.
    fn forget_unless_cancelled(&self, key: &str, cancel: &AtomicBool) -> bool {
        let mut st = self.scan_state();
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        st.unresolved.remove(key);
        lock(&self.cache).units.remove(key);
        true
    }

    /// First sizing of a session: `new` (created by this daemon) or `seen`.
    fn maybe_record(&self, sid: &str) {
        let mut rec = self.recorded();
        let set = rec.get_or_insert_with(HashSet::new);
        if set.contains(sid) {
            return;
        }
        let keys = Self::session_keys(sid);
        let (fig, as_of) = {
            let cache = lock(&self.cache);
            let mut f = Figures::zero();
            let mut as_of = u64::MAX;
            for k in &keys {
                match cache.units.get(k) {
                    Some(e) => {
                        f.add(&e.fig);
                        as_of = as_of.min(e.as_of_ms);
                    }
                    None if stat(&self.root.join(k)).is_ok() => return,
                    None => {}
                }
            }
            (f, as_of)
        };
        let new = lock(&self.scan).new_due.remove(sid);
        let event = if new.is_some() { "new" } else { "seen" };
        let create_ms = new.flatten();
        self.append(
            set,
            &Record {
                event: event.into(),
                session: sid.to_owned(),
                at_ms: now_ms(),
                fig: Some(fig),
                as_of_ms: Some(as_of),
                create_ms,
            },
        );
    }

    /// Write the cache atomically (tmp + rename).
    pub fn save(&self) {
        let bytes = {
            let c = lock(&self.cache);
            match serde_json::to_vec(&*c) {
                Ok(b) => b,
                Err(_) => return,
            }
        };
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let tmp = self
            .dir
            .join(format!(".cache.json.{}.tmp", std::process::id()));
        if fs::write(&tmp, &bytes).is_ok() && fs::rename(&tmp, self.cache_path()).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }

    /// Wait until no pass runs (tests and `--wait`); false on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if !lock(&self.scan).running {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

unsafe extern "C" {
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
}

/// This thread's disk I/O at utility priority (sys/resource.h:
/// IOPOL_TYPE_DISK 0, IOPOL_SCOPE_THREAD 1, IOPOL_UTILITY 4).
pub(crate) fn utility_io() {
    unsafe {
        setiopolicy_np(0, 1, 4);
    }
}

/// How a unit is sized: [`usage_with`] outside tests.
type SizeFn = dyn Fn(&Path, &dyn Fn() -> bool) -> crate::Result<Usage>;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::Builder::new()
            .prefix("strata-du")
            .tempdir()
            .unwrap();
        let p = fs::canonicalize(td.path()).unwrap();
        (td, p)
    }

    fn file(p: &Path, n: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; n]).unwrap();
    }

    fn layout(root: &Path) {
        file(&root.join("store/store.db"), 8192);
        file(&root.join("store/blobs/ab/abcd"), 5000);
        file(&root.join("store/tmp/x"), 10);
        let old = now_ms() - 2 * 3600 * 1000;
        file(
            &root.join(format!("store/trash/{old}-{}/a", "1".repeat(32))),
            40_000,
        );
        file(
            &root.join(format!("store/trash/{}-{}/a", now_ms(), "2".repeat(32))),
            20_000,
        );
        file(&root.join("bases/app/t1/d1/f"), 3000);
        file(&root.join("deps/app/s1/node_modules/m.js"), 1000);
        file(&root.join("sessions/s-aaaaaaaa/app/f"), 3000);
        file(&root.join("sessions/.repos/s-aaaaaaaa/app.git/HEAD"), 30);
    }

    #[test]
    fn units_cover_the_layout() {
        let (_td, root) = tmp();
        layout(&root);
        let keys: Vec<(String, Kind)> = list_units(&root)
            .units
            .into_iter()
            .map(|u| (u.key, u.kind))
            .collect();
        let k = |s: &str| keys.iter().find(|(k, _)| k == s).map(|(_, kind)| *kind);
        assert_eq!(k("store/store.db"), Some(Kind::Always));
        assert_eq!(k("store/blobs/ab"), Some(Kind::Immutable));
        assert_eq!(k("store/tmp"), Some(Kind::Mutable));
        assert_eq!(k("bases/app/t1"), Some(Kind::Immutable));
        assert_eq!(k("deps/app/s1"), Some(Kind::Immutable));
        assert_eq!(k("sessions/s-aaaaaaaa"), Some(Kind::Mutable));
        assert_eq!(k("sessions/.repos/s-aaaaaaaa"), Some(Kind::Mutable));
        assert_eq!(
            keys.iter()
                .filter(|(k, _)| k.starts_with("store/trash/"))
                .count(),
            2
        );
    }

    #[test]
    fn status_follows_inode_mtime_and_ttl() {
        let e = Entry {
            fig: Figures::zero(),
            ino: 5,
            mtime_ns: 100,
            as_of_ms: 1_000_000,
            took_ms: 0,
        };
        let t = 1_000_000;
        assert_eq!(status(None, Kind::Immutable, 5, 100, t), Status::Pending);
        assert_eq!(status(Some(&e), Kind::Immutable, 5, 100, t), Status::Fresh);
        assert_eq!(
            status(Some(&e), Kind::Immutable, 6, 100, t),
            Status::Stale,
            "new inode"
        );
        assert_eq!(
            status(Some(&e), Kind::Immutable, 5, 101, t),
            Status::Stale,
            "mtime moved"
        );
        let later = t + MUTABLE_TTL.as_millis() as u64 + 1;
        assert_eq!(
            status(Some(&e), Kind::Mutable, 5, 100, later),
            Status::Stale,
            "mutable TTL"
        );
        assert_eq!(
            status(Some(&e), Kind::Immutable, 5, 100, later),
            Status::Fresh
        );
        let much_later = t + IMMUTABLE_TTL.as_millis() as u64 + 1;
        assert_eq!(
            status(Some(&e), Kind::Immutable, 5, 100, much_later),
            Status::Stale,
            "immutable TTL"
        );
    }

    #[test]
    fn report_queues_scans_then_answers_from_the_cache() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let r = du.report(false);
        // the db is live; everything else is pending on the first call
        assert!(r["store"]["db"]["logical"].as_u64().unwrap() >= 8192);
        assert!(r["total"]["pending"].as_u64().unwrap() > 0);
        assert!(du.wait_idle(Duration::from_secs(30)));
        let r = du.report(false);
        assert_eq!(r["total"]["pending"], 0, "{r}");
        assert_eq!(r["total"]["stale"], 0, "{r}");
        assert!(r["total"]["as_of"].is_string());
        assert_eq!(r["bases"]["repos"]["app"]["logical"], 3000);
        assert_eq!(r["deps"]["repos"]["app"]["logical"], 1000);
        let trash = &r["store"]["trash"];
        assert_eq!(trash["entries"], 2);
        assert_eq!(trash["eligible"]["entries"], 1);
        assert_eq!(trash["eligible"]["logical"], 40_000);
        assert_eq!(trash["young"]["logical"], 20_000);
        assert!(trash["eligible"]["reclaim_max"].as_u64().unwrap() >= 40_000);
        assert_eq!(trash["gc"], GC_COMMAND);
        let s = &r["sessions"]["list"][0];
        assert_eq!(s["session"], "s-aaaaaaaa");
        assert_eq!(s["logical"], 3030);
        // Scan completion precedes the worker's final save. Persist the
        // complete cache explicitly before testing the disk representation.
        du.save();
        let c: CacheFile = serde_json::from_slice(&fs::read(du.cache_path()).unwrap()).unwrap();
        assert!(c.units.contains_key("bases/app/t1"));
        // older sessions get a `seen` record on first sizing
        let recs = du.records();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].event, "seen");
    }

    /// Review P1-4: `report` (cache, then scan) against `scan_json` and
    /// the pass end (scan, then cache) deadlocked. Hammer both.
    #[test]
    fn session_of_decodes_unit_keys() {
        assert_eq!(session_of("sessions/s-a"), Some("s-a"));
        assert_eq!(session_of("sessions/.repos/s-a"), Some("s-a"));
        assert_eq!(session_of("sessions/s-a/app"), Some("s-a"));
        assert_eq!(session_of("sessions/.repos/s-a/app.git"), Some("s-a"));
        assert_eq!(session_of("sessions/.repos"), None);
        assert_eq!(session_of("sessions/"), None);
        assert_eq!(session_of("store/tmp"), None);
        assert_eq!(session_of("bases/app/t1"), None);
    }

    #[test]
    fn a_hold_unqueues_and_blocks_its_session_until_released() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        // No pass runs: units are queued by hand.
        {
            let mut st = lock(&du.scan);
            st.running = true;
            for k in [
                "sessions/s-aaaaaaaa",
                "sessions/.repos/s-aaaaaaaa",
                "store/tmp",
            ] {
                st.queued.insert(k.to_owned());
                st.queue.push_back(k.to_owned());
            }
        }
        let hold = du
            .hold_sessions(&["s-aaaaaaaa"], Duration::from_millis(10))
            .unwrap();
        {
            let st = lock(&du.scan);
            assert_eq!(
                st.queue.iter().map(String::as_str).collect::<Vec<_>>(),
                ["store/tmp"]
            );
            assert!(!st.queued.contains("sessions/s-aaaaaaaa"));
        }
        du.on_new("s-aaaaaaaa");
        du.enqueue(
            vec!["sessions/s-aaaaaaaa".into(), "bases/app/t1".into()],
            false,
        );
        {
            let st = lock(&du.scan);
            assert!(
                !st.queued.contains("sessions/s-aaaaaaaa")
                    && !st.queued.contains("sessions/.repos/s-aaaaaaaa")
            );
            assert!(st.queued.contains("bases/app/t1"));
        }
        drop(hold);
        assert!(lock(&du.scan).holds.is_empty());
        du.enqueue(vec!["sessions/s-aaaaaaaa".into()], false);
        assert!(lock(&du.scan).queued.contains("sessions/s-aaaaaaaa"));
        lock(&du.scan).running = false;
    }

    #[test]
    fn a_hold_cancels_the_running_scan_and_waits_for_it() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let key = "sessions/s-aaaaaaaa".to_owned();
        let token = Arc::new(AtomicBool::new(false));
        lock(&du.scan).active.insert(key.clone(), token.clone());
        let other = Arc::new(AtomicBool::new(false));
        lock(&du.scan)
            .active
            .insert("store/tmp".into(), other.clone());
        // Nobody ends the scan: Busy, the token set, nothing left held.
        let e = du
            .hold_sessions(&["s-aaaaaaaa"], Duration::from_millis(50))
            .err()
            .unwrap();
        assert_eq!(e.code, strata_types::Code::Busy);
        assert_eq!(e.detail.as_ref().unwrap()["reason"], "du_cancel_timeout");
        assert!(token.load(Ordering::SeqCst));
        assert!(
            !other.load(Ordering::SeqCst),
            "another unit's scan is left alone"
        );
        assert!(lock(&du.scan).holds.is_empty());
        // The scan ends while the hold waits: the hold is granted.
        token.store(false, Ordering::SeqCst);
        let d2 = du.0.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let st = &mut lock(&d2.scan);
            st.active.remove("sessions/s-aaaaaaaa");
            d2.scan_done.notify_all();
        });
        let hold = du
            .hold_sessions(&["s-aaaaaaaa"], Duration::from_secs(5))
            .unwrap();
        assert!(token.load(Ordering::SeqCst));
        t.join().unwrap();
        // A new scan's token is not touched by the release.
        let fresh = Arc::new(AtomicBool::new(false));
        lock(&du.scan)
            .active
            .insert("sessions/.repos/s-bbbbbbbb".into(), fresh.clone());
        drop(hold);
        assert!(!fresh.load(Ordering::SeqCst));
    }

    #[test]
    fn a_cancelled_scan_keeps_the_last_figures() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let key = "sessions/s-aaaaaaaa";
        du.scan_unit(key);
        let before = lock(&du.cache).units.get(key).copied().unwrap();
        file(&root.join("sessions/s-aaaaaaaa/app/g"), 50_000);
        du.scan_unit_with(key, &AtomicBool::new(true));
        assert_eq!(
            lock(&du.cache).units.get(key).copied().unwrap().fig.logical,
            before.fig.logical
        );
        assert!(lock(&du.scan).failed.is_empty(), "a cancel is no failure");
        du.scan_unit(key);
        assert!(lock(&du.cache).units.get(key).copied().unwrap().fig.logical > before.fig.logical);
    }

    #[test]
    fn a_cancelled_scan_of_a_gone_root_keeps_the_last_figures() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let key = "sessions/s-aaaaaaaa";
        du.scan_unit(key);
        assert!(lock(&du.cache).units.contains_key(key));
        std::fs::rename(root.join(key), root.join("sessions/gone")).unwrap();
        du.scan_unit_with(key, &AtomicBool::new(true));
        assert!(
            lock(&du.cache).units.contains_key(key),
            "a cancelled scan removes nothing"
        );
        du.scan_unit(key);
        assert!(!lock(&du.cache).units.contains_key(key));
    }

    #[test]
    fn a_root_gone_mid_walk_is_forgotten_unless_cancelled() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let key = "sessions/s-aaaaaaaa";
        du.scan_unit(key);
        assert!(lock(&du.cache).units.contains_key(key));
        // The root stats, then the walk finds it gone (`NoSuchPath`).
        let gone = |_: &Path, _: &dyn Fn() -> bool| -> crate::Result<Usage> {
            Err(strata_types::StrataError::new(
                strata_types::Code::NoSuchPath,
                "gone mid walk",
            ))
        };
        du.scan_unit_using(key, &AtomicBool::new(true), &gone);
        assert!(
            lock(&du.cache).units.contains_key(key),
            "a cancelled scan removes nothing"
        );
        assert!(lock(&du.scan).failed.is_empty(), "NoSuchPath is no failure");
        du.scan_unit_using(key, &AtomicBool::new(false), &gone);
        assert!(
            !lock(&du.cache).units.contains_key(key),
            "not cancelled: forgotten"
        );
        assert!(lock(&du.scan).failed.is_empty());
    }

    fn units_of(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|u| u["unit"].as_str().unwrap().to_owned())
            .collect()
    }

    /// status-bar §11.4: one `unresolved` entry per failing unit, with its
    /// latest error; it goes when the unit is measured or disappears, while
    /// `failed` keeps every attempt.
    #[test]
    fn unresolved_is_one_entry_per_unit_until_measured_or_gone() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let fail = |_: &Path, _: &dyn Fn() -> bool| -> crate::Result<Usage> {
            Err(strata_types::StrataError::new(
                strata_types::Code::Unknown,
                "EIO",
            ))
        };
        let go = AtomicBool::new(false);
        let (base, ses, shadow) = (
            "bases/app/t1",
            "sessions/s-aaaaaaaa",
            "sessions/.repos/s-aaaaaaaa",
        );
        du.scan_unit_using(base, &go, &fail);
        du.scan_unit_using(base, &go, &fail);
        du.scan_unit_using(ses, &go, &fail);
        let scan = du.scan_json();
        assert_eq!(scan["failed"].as_array().unwrap().len(), 3, "{scan}");
        assert_eq!(units_of(&scan["unresolved"]), [base, ses], "{scan}");
        assert_eq!(scan["unresolved"][0]["error"], "EIO");
        assert!(
            strata_types::serde_rfc3339::parse(scan["unresolved"][0]["at"].as_str().unwrap())
                .is_ok()
        );
        assert_eq!(du.counts()["unresolved_n"], 2);
        // A cancelled attempt is no failure and resolves nothing.
        du.scan_unit_using(base, &AtomicBool::new(true), &fail);
        du.scan_unit_with(base, &AtomicBool::new(true));
        assert_eq!(units_of(&du.scan_json()["unresolved"]), [base, ses]);
        // Measured: resolved; the history stays.
        du.scan_unit(base);
        let scan = du.scan_json();
        assert_eq!(units_of(&scan["unresolved"]), [ses], "{scan}");
        assert_eq!(scan["failed"].as_array().unwrap().len(), 3);
        assert_eq!(du.counts()["unresolved_n"], 1);
        // Gone when its scan finds the root missing.
        du.scan_unit_using(base, &go, &fail);
        fs::remove_dir_all(root.join(base)).unwrap();
        du.scan_unit(base);
        assert_eq!(units_of(&du.scan_json()["unresolved"]), [ses]);
        // Gone when the listing no longer has it.
        fs::remove_dir_all(root.join(ses)).unwrap();
        du.report(false);
        assert!(units_of(&du.scan_json()["unresolved"]).is_empty());
        assert_eq!(du.counts()["unresolved_n"], 0);
        // (The pass `report` started measures the rest; let it end.)
        assert!(du.wait_idle(Duration::from_secs(30)));
        // Gone with its session's `rm`.
        du.scan_unit_using(shadow, &go, &fail);
        assert_eq!(units_of(&du.scan_json()["unresolved"]), [shadow]);
        du.on_remove("s-aaaaaaaa");
        assert!(units_of(&du.scan_json()["unresolved"]).is_empty());
        assert!(du.wait_idle(Duration::from_secs(30)));
    }

    /// `dir` at mode 000 until dropped: listing it and `lstat` through it
    /// fail with `EACCES`.
    struct Locked(PathBuf);

    impl Locked {
        fn new(dir: &Path) -> Locked {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).unwrap();
            Locked(dir.to_path_buf())
        }
    }

    impl Drop for Locked {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    /// status-bar §11.4: a unit root that cannot be `lstat`ed for a reason
    /// other than `ENOENT` is not gone. Its scan is a failure (history and
    /// `unresolved`, with that error), its cached figures stay, and only a
    /// later measurement or `ENOENT` resolves it.
    #[test]
    fn a_failed_lstat_keeps_the_unit_unresolved() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let fail = |_: &Path, _: &dyn Fn() -> bool| -> crate::Result<Usage> {
            Err(strata_types::StrataError::new(
                strata_types::Code::Unknown,
                "EIO",
            ))
        };
        let (ses, shadow) = ("sessions/s-aaaaaaaa", "sessions/.repos/s-aaaaaaaa");
        du.scan_unit(ses);
        du.scan_unit_using(ses, &AtomicBool::new(false), &fail);
        let figures = lock(&du.cache).units.get(ses).copied();
        assert!(figures.is_some());
        {
            let _locked = Locked::new(&root.join("sessions"));
            // A cancelled attempt records nothing.
            du.scan_unit_with(ses, &AtomicBool::new(true));
            assert_eq!(du.scan_json()["failed"].as_array().unwrap().len(), 1);
            du.scan_unit(ses);
            du.scan_unit(shadow);
        }
        let scan = du.scan_json();
        assert_eq!(units_of(&scan["unresolved"]), [shadow, ses], "{scan}");
        for u in scan["unresolved"].as_array().unwrap() {
            let error = u["error"].as_str().unwrap();
            assert!(error.contains("Permission denied"), "{error}");
        }
        assert_eq!(scan["failed"].as_array().unwrap().len(), 3, "{scan}");
        assert_eq!(du.counts()["unresolved_n"], 2);
        assert_eq!(lock(&du.cache).units.get(ses).copied(), figures);
        // Readable again: measured, resolved.
        du.scan_unit(ses);
        assert_eq!(units_of(&du.scan_json()["unresolved"]), [shadow]);
        // ENOENT: gone.
        fs::remove_dir_all(root.join(shadow)).unwrap();
        du.scan_unit(shadow);
        assert!(units_of(&du.scan_json()["unresolved"]).is_empty());
        assert_eq!(du.counts()["unresolved_n"], 0);
    }

    /// status-bar §11.4: a directory `report` cannot list hides its units
    /// without proving them gone, so their `unresolved` entries and cached
    /// figures stay. A listing that reads the directory, or finds it
    /// missing, drops them.
    #[test]
    fn an_unlistable_directory_keeps_its_units() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let fail = |_: &Path, _: &dyn Fn() -> bool| -> crate::Result<Usage> {
            Err(strata_types::StrataError::new(
                strata_types::Code::Unknown,
                "EIO",
            ))
        };
        let (ses, shadow) = ("sessions/s-aaaaaaaa", "sessions/.repos/s-aaaaaaaa");
        du.scan_unit(ses);
        du.scan_unit(shadow);
        du.scan_unit_using(ses, &AtomicBool::new(false), &fail);
        {
            let _locked = Locked::new(&root.join("sessions"));
            let listing = list_units(&root);
            assert_eq!(listing.unlisted, ["sessions/"]);
            assert!(
                listing
                    .units
                    .iter()
                    .all(|u| !u.key.starts_with("sessions/")),
                "{listing:?}"
            );
            du.report(false);
            assert!(du.wait_idle(Duration::from_secs(30)));
            assert_eq!(units_of(&du.scan_json()["unresolved"]), [ses]);
            let cache = lock(&du.cache);
            assert!(cache.units.contains_key(ses) && cache.units.contains_key(shadow));
        }
        assert!(list_units(&root).unlisted.is_empty());
        // Listed and missing: dropped.
        fs::remove_dir_all(root.join(ses)).unwrap();
        du.report(false);
        assert!(units_of(&du.scan_json()["unresolved"]).is_empty());
        assert!(!lock(&du.cache).units.contains_key(ses));
        assert!(du.wait_idle(Duration::from_secs(30)));
        // A missing directory lists nothing and proves its units gone.
        du.scan_unit_using(shadow, &AtomicBool::new(false), &fail);
        fs::remove_dir_all(root.join("sessions")).unwrap();
        assert!(list_units(&root).unlisted.is_empty());
        du.report(false);
        assert!(units_of(&du.scan_json()["unresolved"]).is_empty());
        assert!(!lock(&du.cache).units.contains_key(shadow));
        assert!(du.wait_idle(Duration::from_secs(30)));
    }

    /// status-bar §11.4: `counts` follows the scan state through an
    /// enqueue, a scan in flight, a hold that takes units off the queue and
    /// the workers' completion, and reads no lock.
    #[test]
    fn counts_mirror_the_scan_state() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let counts = |running: bool, active: u64, queued: u64, done: u64, full: bool| json!({ "running": running, "active": active, "queued": queued, "done": done, "full": full, "unresolved_n": 0 });
        assert_eq!(du.counts(), counts(false, 0, 0, 0, false));
        // No pass thread: the test runs the workers itself.
        du.scan_state().running = true;
        let keys = [
            "bases/app/t1",
            "store/tmp",
            "sessions/s-aaaaaaaa",
            "sessions/.repos/s-aaaaaaaa",
        ];
        du.enqueue(keys.iter().map(|k| (*k).to_owned()).collect(), true);
        assert_eq!(du.counts(), counts(true, 0, 4, 0, true));
        // `scan` locked elsewhere: `counts` still answers.
        {
            let _held = lock(&du.scan);
            assert_eq!(du.counts(), counts(true, 0, 4, 0, true));
        }
        du.scan_state()
            .active
            .insert("deps/app/s1".into(), Arc::new(AtomicBool::new(false)));
        assert_eq!(du.counts(), counts(true, 1, 4, 0, true));
        du.scan_state().active.remove("deps/app/s1");
        // A hold takes its session's units off the queue.
        let hold = du
            .hold_sessions(&["s-aaaaaaaa"], Duration::from_millis(10))
            .unwrap();
        assert_eq!(du.counts(), counts(true, 0, 2, 0, true));
        drop(hold);
        // The workers drain the queue; the pass ends.
        du.pass();
        assert_eq!(du.counts(), counts(false, 0, 0, 2, false));
        assert_eq!(du.scan_json()["active"], 0);
    }

    #[test]
    fn report_and_scan_status_do_not_deadlock() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        let (tx, rx) = std::sync::mpsc::channel();
        let d2 = du.0.clone();
        std::thread::spawn(move || {
            std::thread::scope(|s| {
                let a = &d2;
                s.spawn(move || {
                    for _ in 0..300 {
                        a.report(true);
                    }
                });
                s.spawn(move || {
                    for _ in 0..20_000 {
                        let _ = a.scan_json();
                    }
                });
            });
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(60)).is_ok(),
            "report and scan_json deadlocked"
        );
        assert!(du.wait_idle(Duration::from_secs(30)));
    }

    #[test]
    fn cache_invalidation_on_change_new_and_rm() {
        let (_td, root) = tmp();
        layout(&root);
        let du = Du::open(&root);
        du.report(false);
        assert!(du.wait_idle(Duration::from_secs(30)));
        let before = lock(&du.cache).units["bases/app/t1"];
        // a new generation under the tree moves the unit root's mtime
        std::thread::sleep(Duration::from_millis(20));
        file(&root.join("bases/app/t1/d2/g"), 500);
        let r = du.report(false);
        assert_eq!(r["bases"]["repos"]["app"]["stale"], 1, "{r}");
        assert_eq!(
            r["bases"]["repos"]["app"]["logical"], 3000,
            "stale figures are still shown"
        );
        assert!(du.wait_idle(Duration::from_secs(30)));
        let r = du.report(false);
        assert_eq!(r["bases"]["repos"]["app"]["logical"], 3500);
        assert_ne!(lock(&du.cache).units["bases/app/t1"], before);
        // a removed trash entry leaves the cache
        let gone = fs::read_dir(root.join("store/trash"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::remove_dir_all(&gone).unwrap();
        let r = du.report(false);
        assert_eq!(r["store"]["trash"]["entries"], 1);
        assert!(!lock(&du.cache).units.keys().any(|k| root.join(k) == gone));
        // new: sized at once, `new` record
        file(&root.join("sessions/s-bbbbbbbb/app/f"), 7000);
        du.on_new_timed("s-bbbbbbbb", Some(Duration::from_millis(1234)));
        assert!(du.wait_idle(Duration::from_secs(30)));
        let recs = du.records();
        let n = recs.iter().find(|r| r.session == "s-bbbbbbbb").unwrap();
        assert_eq!(n.event, "new");
        assert_eq!(n.fig.unwrap().logical, 7000);
        assert_eq!(n.create_ms, Some(1234));
        let r = du.report(false);
        let s = r["sessions"]["list"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["session"] == "s-bbbbbbbb")
            .unwrap()
            .clone();
        assert_eq!(s["create_ms"], 1234);
        let cum = &r["savings"]["cumulative"];
        assert_eq!(
            (
                cum["timed"].clone(),
                cum["create_ms"].clone(),
                cum["timed_logical"].clone()
            ),
            (json!(1), json!(1234), json!(7000))
        );
        // a `seen` session has no time
        assert!(
            recs.iter()
                .filter(|r| r.event == "seen")
                .all(|r| r.create_ms.is_none())
        );
        // rm: `rm` record with the cached figures; entries dropped
        fs::remove_dir_all(root.join("sessions/s-bbbbbbbb")).unwrap();
        du.on_remove("s-bbbbbbbb");
        assert!(!lock(&du.cache).units.contains_key("sessions/s-bbbbbbbb"));
        let recs = du.records();
        let rm = recs.iter().rfind(|r| r.session == "s-bbbbbbbb").unwrap();
        assert_eq!((rm.event.as_str(), rm.fig.unwrap().logical), ("rm", 7000));
        let r = du.report(false);
        let cum = &r["savings"]["cumulative"];
        assert_eq!(cum["sessions"], 2);
        assert_eq!(cum["removed"], 1);
        assert_eq!(cum["before_since"], "unknown");
        // refresh re-sizes everything and records the full pass
        du.report(true);
        assert!(du.wait_idle(Duration::from_secs(30)));
        assert!(lock(&du.cache).last_full.is_some());
        let r = du.report(false);
        assert!(r["scan"]["last_full"]["ended"].is_string());
    }

    #[test]
    fn a_reopened_du_reads_its_cache() {
        let (_td, root) = tmp();
        layout(&root);
        {
            let du = Du::open(&root);
            du.report(false);
            assert!(du.wait_idle(Duration::from_secs(30)));
            // wait_idle observes scan completion before the worker's final
            // save; this test needs a persisted cache before reopening.
            du.save();
        }
        let du = Du::open(&root);
        let r = du.report(false);
        assert_eq!(r["total"]["pending"], 0, "{r}");
    }
}

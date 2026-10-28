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

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn ts(ms: u64) -> Value {
    strata_types::serde_rfc3339::format(&(UNIX_EPOCH + Duration::from_millis(ms))).map(Value::String).unwrap_or(Value::Null)
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
        Figures { exact: true, ..Figures::default() }
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
        Figures { logical: u.logical, allocated: u.allocated, private: u.private, files: u.files, errors: u.errors, exact: u.private_exact }
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
    if e.ino != ino || e.mtime_ns != mtime_ns || now_ms.saturating_sub(e.as_of_ms) > ttl.as_millis() as u64 {
        Status::Stale
    } else {
        Status::Fresh
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = match fs::read_dir(dir) {
        Ok(d) => d.filter_map(|e| e.ok()).filter_map(|e| e.file_name().into_string().ok()).collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

/// Every unit under `root` (§5.12 table). Cheap: readdir of the top levels.
pub fn list_units(root: &Path) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut push = |key: String, kind: Kind, group: Group| out.push(Unit { key, kind, group });
    for n in names(&root.join("store")) {
        match n.as_str() {
            "blobs" => {
                for s in names(&root.join("store/blobs")) {
                    push(format!("store/blobs/{s}"), Kind::Immutable, Group::Blobs);
                }
            }
            "trash" => {
                for s in names(&root.join("store/trash")) {
                    push(format!("store/trash/{s}"), Kind::Immutable, Group::Trash);
                }
            }
            "tmp" => push("store/tmp".into(), Kind::Mutable, Group::Tmp),
            n if n.starts_with("store.db") || n == "LOCK" => push(format!("store/{n}"), Kind::Always, Group::Db),
            n => push(format!("store/{n}"), Kind::Mutable, Group::StoreOther),
        }
    }
    for (top, base) in [("bases", true), ("deps", false)] {
        for repo in names(&root.join(top)) {
            for n in names(&root.join(top).join(&repo)) {
                let kind = if n.starts_with('.') { Kind::Mutable } else { Kind::Immutable };
                let group = if base { Group::Base(repo.clone()) } else { Group::Deps(repo.clone()) };
                push(format!("{top}/{repo}/{n}"), kind, group);
            }
        }
    }
    for n in names(&root.join("sessions")) {
        if n == ".repos" {
            for s in names(&root.join("sessions/.repos")) {
                push(format!("sessions/.repos/{s}"), Kind::Mutable, Group::Session(s.clone()));
            }
        } else {
            push(format!("sessions/{n}"), Kind::Mutable, Group::Session(n.clone()));
        }
    }
    out
}

fn stat(p: &Path) -> Option<(u64, i64)> {
    let m = fs::symlink_metadata(p).ok()?;
    Some((m.ino(), m.mtime().saturating_mul(1_000_000_000).saturating_add(m.mtime_nsec())))
}

/// Trash entry names start with the retire time in ms (`<ms>-<hex32>`).
fn trash_age_ms(root: &Path, key: &str, now: u64) -> u64 {
    let name = key.rsplit('/').next().unwrap_or("");
    if let Some(ms) = name.split_once('-').and_then(|(ms, _)| ms.parse::<u64>().ok()) {
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
        Agg { fig: Figures::zero(), units: 0, stale: 0, pending: 0, oldest: None }
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
    let rest = key.strip_prefix("sessions/.repos/").or_else(|| key.strip_prefix("sessions/"))?;
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
            .unwrap_or(CacheFile { version: CACHE_VERSION, ..CacheFile::default() });
        DuHandle(Arc::new(Du {
            root: root.to_path_buf(),
            dir,
            cache: Mutex::new(cache),
            scan: Mutex::new(ScanState::default()),
            scan_done: Condvar::new(),
            recorded: Mutex::new(None),
            stop: AtomicBool::new(false),
        }))
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
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
        let units = list_units(&self.root);
        let mut due = Vec::new();
        let mut groups: BTreeMap<Group, Agg> = BTreeMap::new();
        let (mut eligible, mut young) = (Agg::default(), Agg::default());
        let (mut eligible_n, mut young_n) = (0u64, 0u64);
        // Review P1-4: lock order is `recorded` → `scan` → `cache`; the
        // queue is read before the cache lock, never inside it.
        let queued = lock(&self.scan).queued.clone();
        {
            let mut cache = lock(&self.cache);
            let present: HashSet<&str> = units.iter().map(|u| u.key.as_str()).collect();
            cache.units.retain(|k, _| present.contains(k.as_str()) || queued.contains(k));
            for u in &units {
                let path = self.root.join(&u.key);
                let Some((ino, mtime)) = stat(&path) else { continue };
                let (entry, st) = if u.kind == Kind::Always {
                    match usage_of(&path) {
                        Ok(us) => (Some(Entry { fig: us.into(), ino, mtime_ns: mtime, as_of_ms: now, took_ms: 0 }), Status::Fresh),
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
                groups.entry(u.group.clone()).or_default().add(entry.as_ref(), st);
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
        for g in [Group::Db, Group::Blobs, Group::Tmp, Group::Trash, Group::StoreOther] {
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
            "full": st.full_started.is_some(),
            "failed": st.failed.iter().map(|(k, e)| json!({ "unit": k, "error": e })).collect::<Vec<_>>(),
            "last_full": last_full.map(|f| json!({ "started": ts(f.started_ms), "ended": ts(f.ended_ms) })).unwrap_or(Value::Null),
        })
    }

    /// `create_ms` per session, from its `new` record.
    fn create_times(recs: &[Record]) -> HashMap<&str, u64> {
        recs.iter().filter_map(|r| Some((r.session.as_str(), r.create_ms?))).collect()
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
        let (mut logical, mut private, mut known, mut unknown, mut removed) = (0u64, 0u64, 0u64, 0u64, 0u64);
        let rm: HashSet<&str> = recs.iter().filter(|r| r.event == "rm").map(|r| r.session.as_str()).collect();
        // Sessions with both a measured `new` and figures: the time and size of the same set.
        let (mut timed, mut create_ms, mut timed_logical, mut timed_files) = (0u64, 0u64, 0u64, 0u64);
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
            Ok(t) => t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Append one record line. Callers hold `recorded` (so a session's
    /// first-sizing record is written once even with two units in flight).
    fn append(&self, set: &mut HashSet<String>, r: &Record) {
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(self.records_path()) {
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
            let mut st = lock(&self.scan);
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
        lock(&self.scan).new_due.remove(sid);
        let mut rec = self.recorded();
        let set = rec.get_or_insert_with(HashSet::new);
        self.append(set, &Record { event: "rm".into(), session: sid.to_owned(), at_ms: now_ms(), fig, as_of_ms: as_of, create_ms: None });
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
        let hold = DuHold { du: Arc::clone(self), sids: sids.iter().map(|s| (*s).to_owned()).collect() };
        let mine = |k: &str| session_of(k).is_some_and(|s| sids.contains(&s));
        st.queue.retain(|k| !mine(k));
        let active: HashSet<&String> = st.active.keys().collect();
        let gone: Vec<String> = st.queued.iter().filter(|k| mine(k) && !active.contains(k)).cloned().collect();
        for k in gone {
            st.queued.remove(&k);
        }
        for (k, t) in &st.active {
            if mine(k) {
                t.store(true, Ordering::SeqCst);
            }
        }
        while sids.iter().any(|sid| st.scanning(sid)) {
            let now = Instant::now();
            if now >= end {
                drop(st);
                drop(hold);
                return Err(strata_types::StrataError::new(
                    strata_types::Code::Busy,
                    format!("the disk-usage scan of {} did not stop within {}s", sids.join(", "), wait.as_secs()),
                )
                .with_detail(json!({ "reason": "du_cancel_timeout", "sessions": sids })));
            }
            st = self.scan_done.wait_timeout(st, end - now).unwrap_or_else(|p| p.into_inner()).0;
        }
        drop(st);
        Ok(hold)
    }

    fn enqueue(self: &Arc<Self>, keys: Vec<String>, full: bool) {
        {
            let mut st = lock(&self.scan);
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
            let mut st = lock(&self.scan);
            if st.running || self.stop.load(Ordering::SeqCst) {
                return;
            }
            st.running = true;
            st.started_ms = now_ms();
            st.done = 0;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("strata-du".into()).spawn(move || me.pass());
        if spawned.is_err() {
            lock(&self.scan).running = false;
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
            let mut st = lock(&self.scan);
            if st.queue.is_empty() || self.stop.load(Ordering::SeqCst) {
                if let Some(started) = st.full_started.take()
                    && !self.stop.load(Ordering::SeqCst)
                {
                    lock(&self.cache).last_full = Some(FullPass { started_ms: started, ended_ms: now_ms() });
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
                let mut st = lock(&self.scan);
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
                let mut st = lock(&self.scan);
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
        let path = self.root.join(key);
        let Some((ino, mtime)) = stat(&path) else {
            lock(&self.cache).units.remove(key);
            return;
        };
        let started = now_ms();
        let t = Instant::now();
        let stopped = || self.stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst);
        match usage_with(&path, &stopped) {
            Ok(u) => {
                let e = Entry { fig: u.into(), ino, mtime_ns: mtime, as_of_ms: started, took_ms: t.elapsed().as_millis() as u64 };
                // The last cancel check and the write are one `scan`
                // critical section, serialised with a hold's registration.
                let _st = lock(&self.scan);
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                lock(&self.cache).units.insert(key.to_owned(), e);
            }
            Err(e) if e.code == strata_types::Code::NoSuchPath => {
                lock(&self.cache).units.remove(key);
            }
            Err(e) => {
                if !stopped() {
                    let mut st = lock(&self.scan);
                    st.failed.push_back((key.to_owned(), e.message));
                    while st.failed.len() > MAX_FAILED {
                        st.failed.pop_front();
                    }
                }
                return;
            }
        }
        if let Some(sid) = session_of(key) {
            self.maybe_record(sid);
        }
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
                    None if stat(&self.root.join(k)).is_some() => return,
                    None => {}
                }
            }
            (f, as_of)
        };
        let new = lock(&self.scan).new_due.remove(sid);
        let event = if new.is_some() { "new" } else { "seen" };
        let create_ms = new.flatten();
        self.append(set, &Record { event: event.into(), session: sid.to_owned(), at_ms: now_ms(), fig: Some(fig), as_of_ms: Some(as_of), create_ms });
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
        let tmp = self.dir.join(format!(".cache.json.{}.tmp", std::process::id()));
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
fn utility_io() {
    unsafe {
        setiopolicy_np(0, 1, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::Builder::new().prefix("strata-du").tempdir().unwrap();
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
        file(&root.join(format!("store/trash/{old}-{}/a", "1".repeat(32))), 40_000);
        file(&root.join(format!("store/trash/{}-{}/a", now_ms(), "2".repeat(32))), 20_000);
        file(&root.join("bases/app/t1/d1/f"), 3000);
        file(&root.join("deps/app/s1/node_modules/m.js"), 1000);
        file(&root.join("sessions/s-aaaaaaaa/app/f"), 3000);
        file(&root.join("sessions/.repos/s-aaaaaaaa/app.git/HEAD"), 30);
    }

    #[test]
    fn units_cover_the_layout() {
        let (_td, root) = tmp();
        layout(&root);
        let keys: Vec<(String, Kind)> = list_units(&root).into_iter().map(|u| (u.key, u.kind)).collect();
        let k = |s: &str| keys.iter().find(|(k, _)| k == s).map(|(_, kind)| *kind);
        assert_eq!(k("store/store.db"), Some(Kind::Always));
        assert_eq!(k("store/blobs/ab"), Some(Kind::Immutable));
        assert_eq!(k("store/tmp"), Some(Kind::Mutable));
        assert_eq!(k("bases/app/t1"), Some(Kind::Immutable));
        assert_eq!(k("deps/app/s1"), Some(Kind::Immutable));
        assert_eq!(k("sessions/s-aaaaaaaa"), Some(Kind::Mutable));
        assert_eq!(k("sessions/.repos/s-aaaaaaaa"), Some(Kind::Mutable));
        assert_eq!(keys.iter().filter(|(k, _)| k.starts_with("store/trash/")).count(), 2);
    }

    #[test]
    fn status_follows_inode_mtime_and_ttl() {
        let e = Entry { fig: Figures::zero(), ino: 5, mtime_ns: 100, as_of_ms: 1_000_000, took_ms: 0 };
        let t = 1_000_000;
        assert_eq!(status(None, Kind::Immutable, 5, 100, t), Status::Pending);
        assert_eq!(status(Some(&e), Kind::Immutable, 5, 100, t), Status::Fresh);
        assert_eq!(status(Some(&e), Kind::Immutable, 6, 100, t), Status::Stale, "new inode");
        assert_eq!(status(Some(&e), Kind::Immutable, 5, 101, t), Status::Stale, "mtime moved");
        let later = t + MUTABLE_TTL.as_millis() as u64 + 1;
        assert_eq!(status(Some(&e), Kind::Mutable, 5, 100, later), Status::Stale, "mutable TTL");
        assert_eq!(status(Some(&e), Kind::Immutable, 5, 100, later), Status::Fresh);
        let much_later = t + IMMUTABLE_TTL.as_millis() as u64 + 1;
        assert_eq!(status(Some(&e), Kind::Immutable, 5, 100, much_later), Status::Stale, "immutable TTL");
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
        // the cache survived on disk
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
            for k in ["sessions/s-aaaaaaaa", "sessions/.repos/s-aaaaaaaa", "store/tmp"] {
                st.queued.insert(k.to_owned());
                st.queue.push_back(k.to_owned());
            }
        }
        let hold = du.hold_sessions(&["s-aaaaaaaa"], Duration::from_millis(10)).unwrap();
        {
            let st = lock(&du.scan);
            assert_eq!(st.queue.iter().map(String::as_str).collect::<Vec<_>>(), ["store/tmp"]);
            assert!(!st.queued.contains("sessions/s-aaaaaaaa"));
        }
        du.on_new("s-aaaaaaaa");
        du.enqueue(vec!["sessions/s-aaaaaaaa".into(), "bases/app/t1".into()], false);
        {
            let st = lock(&du.scan);
            assert!(!st.queued.contains("sessions/s-aaaaaaaa") && !st.queued.contains("sessions/.repos/s-aaaaaaaa"));
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
        lock(&du.scan).active.insert("store/tmp".into(), other.clone());
        // Nobody ends the scan: Busy, the token set, nothing left held.
        let e = du.hold_sessions(&["s-aaaaaaaa"], Duration::from_millis(50)).err().unwrap();
        assert_eq!(e.code, strata_types::Code::Busy);
        assert_eq!(e.detail.as_ref().unwrap()["reason"], "du_cancel_timeout");
        assert!(token.load(Ordering::SeqCst));
        assert!(!other.load(Ordering::SeqCst), "another unit's scan is left alone");
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
        let hold = du.hold_sessions(&["s-aaaaaaaa"], Duration::from_secs(5)).unwrap();
        assert!(token.load(Ordering::SeqCst));
        t.join().unwrap();
        // A new scan's token is not touched by the release.
        let fresh = Arc::new(AtomicBool::new(false));
        lock(&du.scan).active.insert("sessions/.repos/s-bbbbbbbb".into(), fresh.clone());
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
        assert_eq!(lock(&du.cache).units.get(key).copied().unwrap().fig.logical, before.fig.logical);
        assert!(lock(&du.scan).failed.is_empty(), "a cancel is no failure");
        du.scan_unit(key);
        assert!(lock(&du.cache).units.get(key).copied().unwrap().fig.logical > before.fig.logical);
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
        assert!(rx.recv_timeout(Duration::from_secs(60)).is_ok(), "report and scan_json deadlocked");
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
        assert_eq!(r["bases"]["repos"]["app"]["logical"], 3000, "stale figures are still shown");
        assert!(du.wait_idle(Duration::from_secs(30)));
        let r = du.report(false);
        assert_eq!(r["bases"]["repos"]["app"]["logical"], 3500);
        assert_ne!(lock(&du.cache).units["bases/app/t1"], before);
        // a removed trash entry leaves the cache
        let gone = fs::read_dir(root.join("store/trash")).unwrap().next().unwrap().unwrap().path();
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
        let s = r["sessions"]["list"].as_array().unwrap().iter().find(|s| s["session"] == "s-bbbbbbbb").unwrap().clone();
        assert_eq!(s["create_ms"], 1234);
        let cum = &r["savings"]["cumulative"];
        assert_eq!((cum["timed"].clone(), cum["create_ms"].clone(), cum["timed_logical"].clone()), (json!(1), json!(1234), json!(7000)));
        // a `seen` session has no time
        assert!(recs.iter().filter(|r| r.event == "seen").all(|r| r.create_ms.is_none()));
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
        }
        let du = Du::open(&root);
        let r = du.report(false);
        assert_eq!(r["total"]["pending"], 0, "{r}");
    }
}

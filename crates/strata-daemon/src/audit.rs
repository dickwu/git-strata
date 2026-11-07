//! Best-effort pin history, deliberately separate from the strict pins.json format,
//! and the sidecar of auto-close events beside it (p1-autoclose §7).
use crate::PinnedFlags;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use strata_types::RepoName;

pub const ROTATE_BYTES: u64 = 1024 * 1024;
/// Session events, so pin history is not rotated away by session traffic.
pub const SESSIONS_AUDIT_FILE: &str = "sessions-audit.jsonl";
#[derive(Clone, Debug, Default, Serialize)]
pub struct PinMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relaxed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub at: String,
    pub event: String,
    pub repo: RepoName,
    pub from: Option<PinnedFlags>,
    pub to: PinnedFlags,
    /// LocalAuthentication does not disclose Touch ID versus password.
    pub auth: String,
    pub pid: u32,
    pub version: String,
}
impl Entry {
    pub fn new(event: &str, repo: RepoName, from: Option<PinnedFlags>, to: PinnedFlags) -> Self {
        Self {
            at: strata_types::serde_rfc3339::format(&SystemTime::now()).unwrap_or_default(),
            event: event.into(),
            repo,
            from,
            to,
            auth: if event == "config_relax" {
                "local_authentication"
            } else {
                "startup"
            }
            .into(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}
pub fn apply(meta: &mut BTreeMap<RepoName, PinMeta>, e: &Entry) {
    if strata_types::serde_rfc3339::parse(&e.at).is_err() {
        return;
    }
    let m = meta.entry(e.repo.clone()).or_default();
    let slot = match e.event.as_str() {
        "config_relax" => &mut m.relaxed_at,
        "tighten" | "pin_new" => &mut m.pinned_at,
        _ => return,
    };
    if slot.as_ref().is_none_or(|at| {
        strata_types::serde_rfc3339::parse(at).ok()
            <= strata_types::serde_rfc3339::parse(&e.at).ok()
    }) {
        *slot = Some(e.at.clone());
    }
}
pub fn load(pins: &Path) -> BTreeMap<RepoName, PinMeta> {
    let mut meta = BTreeMap::new();
    for name in ["audit.jsonl.1", "audit.jsonl"] {
        let p = pins.with_file_name(name);
        let Ok(f) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&p)
        else {
            continue;
        };
        let Ok(metadata) = f.metadata() else { continue };
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > ROTATE_BYTES + 65536 {
            tracing::warn!(path = %p.display(), bytes = metadata.len(), "oversized pin audit sidecar ignored; timestamps from this file are unavailable");
            continue;
        }
        for line in BufReader::new(f).split(b'\n').map_while(Result::ok) {
            if let Ok(e) = serde_json::from_slice::<Entry>(&line) {
                apply(&mut meta, &e);
            }
        }
    }
    meta
}

/// The pins file. Its lock is `Pins.inner`, which every caller already holds.
pub fn append(pins: &Path, entry: &Entry) -> io::Result<()> {
    let path = pins.with_file_name("audit.jsonl");
    let rotated = pins.with_file_name("audit.jsonl.1");
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    append_to(&path, &rotated, ROTATE_BYTES, &line)
}

/// The size check, the rotation, the open, the delimiter, the write and the
/// fsyncs. It serialises nothing itself: every caller holds a lock for this file.
fn append_to(path: &Path, rotated: &Path, rotate_bytes: u64, line: &[u8]) -> io::Result<()> {
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_file() {
            return Err(io::Error::other("audit sidecar is not a regular file"));
        }
        if m.len() + line.len() as u64 > rotate_bytes {
            fs::rename(path, rotated)?;
            #[cfg(any(test, feature = "test-seams"))]
            after_rotate(path);
        }
    }
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    // A crash may leave a partial final JSON record. Preserve it for inspection
    // but delimit it so the next successful record is independently loadable.
    if f.metadata()?.len() > 0 {
        f.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    f.write_all(line)?;
    f.sync_all()?;
    File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
}

/// One sidecar with its own writer lock, held across the whole of
/// [`append_to`]. Two sessions' hooks run on two request threads, and two
/// unserialised appends can both rotate, which throws the retained history away.
pub struct AuditFile {
    path: PathBuf,
    rotated: PathBuf,
    rotate_bytes: u64,
    lock: Mutex<()>,
    last_error: Mutex<Option<String>>,
}

impl AuditFile {
    pub fn new(path: PathBuf, rotate_bytes: u64) -> Self {
        let mut rotated = path.clone().into_os_string();
        rotated.push(".1");
        Self {
            path,
            rotated: PathBuf::from(rotated),
            rotate_bytes,
            lock: Mutex::new(()),
            last_error: Mutex::new(None),
        }
    }

    /// The auto-close sidecar next to `pins`.
    pub fn sessions(pins: &Path) -> Self {
        Self::new(pins.with_file_name(SESSIONS_AUDIT_FILE), ROTATE_BYTES)
    }

    /// Appends one line, with its newline. A panic elsewhere that held the lock
    /// must not silence every later line: it protects the order, not data.
    pub fn append_line(&self, line: &[u8]) -> io::Result<()> {
        let mut framed = line.to_vec();
        if framed.last() != Some(&b'\n') {
            framed.push(b'\n');
        }
        let _order = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        append_to(&self.path, &self.rotated, self.rotate_bytes, &framed)
    }

    /// Best effort: a failure is logged and kept for `daemon_status`, and never
    /// changes the reply. A later success clears it.
    pub fn record(&self, value: &serde_json::Value) {
        let written = serde_json::to_vec(value)
            .map_err(io::Error::other)
            .and_then(|line| self.append_line(&line));
        let mut last = self.last_error.lock().unwrap_or_else(|p| p.into_inner());
        match written {
            Ok(()) => *last = None,
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "audit_write_failed");
                *last = Some(e.to_string());
            }
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// Runs between a rotation's rename and its open, keyed by the sidecar's path.
#[cfg(any(test, feature = "test-seams"))]
pub type AfterRotate = std::sync::Arc<dyn Fn() + Send + Sync>;

#[cfg(any(test, feature = "test-seams"))]
static AFTER_ROTATE: Mutex<Vec<(PathBuf, AfterRotate)>> = Mutex::new(Vec::new());

/// `audit.after_rotate`: `None` removes the hook. Keyed so that parallel tests
/// are unaffected.
#[cfg(any(test, feature = "test-seams"))]
pub fn set_after_rotate(path: &Path, hook: Option<AfterRotate>) {
    let mut v = AFTER_ROTATE.lock().unwrap_or_else(|p| p.into_inner());
    v.retain(|(p, _)| p != path);
    if let Some(h) = hook {
        v.push((path.to_path_buf(), h));
    }
}

#[cfg(any(test, feature = "test-seams"))]
fn after_rotate(path: &Path) {
    let hook = AFTER_ROTATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, h)| h.clone());
    if let Some(h) = hook {
        h();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier, mpsc};

    fn lines_of(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn rotates_reads_both_files_and_ignores_corrupt_lines() {
        let d = tempfile::tempdir().unwrap();
        let pins = d.path().join("pins.json");
        let flags: PinnedFlags = serde_json::from_value(
            serde_json::json!({"confirm":true,"push":false,"deploy_on_push":false}),
        )
        .unwrap();
        let repo = RepoName::try_from("app".to_string()).unwrap();
        append(
            &pins,
            &Entry::new("pin_new", repo.clone(), None, flags.clone()),
        )
        .unwrap();
        let path = pins.with_file_name("audit.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&vec![b' '; ROTATE_BYTES as usize]).unwrap();
        drop(f);
        append(
            &pins,
            &Entry::new("config_relax", repo.clone(), Some(flags.clone()), flags),
        )
        .unwrap();
        assert!(pins.with_file_name("audit.jsonl.1").exists());
        // Rotation can include the padding line; both valid history lines remain usable.
        let meta = load(&pins);
        assert!(meta[&repo].relaxed_at.is_some());
        assert!(meta[&repo].pinned_at.is_some());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn sessions_audit_is_a_separate_file_and_rotates_independently() {
        let d = tempfile::tempdir().unwrap();
        let pins = d.path().join("pins.json");
        let flags: PinnedFlags = serde_json::from_value(
            json!({"confirm":true,"push":false,"deploy_on_push":false}),
        )
        .unwrap();
        let repo = RepoName::try_from("app".to_string()).unwrap();
        append(&pins, &Entry::new("pin_new", repo, None, flags)).unwrap();
        let file = AuditFile::sessions(&pins);
        let session_path = pins.with_file_name(SESSIONS_AUDIT_FILE);
        file.append_line(br#"{"event":"auto_close","seq":1}"#).unwrap();
        let mut padded = OpenOptions::new().append(true).open(&session_path).unwrap();
        padded.write_all(&vec![b' '; ROTATE_BYTES as usize]).unwrap();
        drop(padded);
        file.append_line(br#"{"event":"auto_close","seq":2}"#).unwrap();
        assert!(pins.with_file_name("sessions-audit.jsonl.1").exists());
        assert_eq!(lines_of(&session_path)[0]["seq"], 2);
        assert!(!pins.with_file_name("audit.jsonl.1").exists(), "pin history is not rotated by session traffic");
        assert_eq!(lines_of(&pins.with_file_name("audit.jsonl")).len(), 1);
    }

    #[test]
    fn pins_loader_ignores_the_sessions_file() {
        let d = tempfile::tempdir().unwrap();
        let pins = d.path().join("pins.json");
        AuditFile::sessions(&pins)
            .append_line(br#"{"at":"2026-11-05T00:00:00Z","event":"auto_close","session":"s-a"}"#)
            .unwrap();
        // The same line in the pins file is skipped too: it is not an `Entry`.
        fs::write(
            pins.with_file_name("audit.jsonl"),
            br#"{"at":"2026-11-05T00:00:00Z","event":"auto_close","session":"s-a"}
"#,
        )
        .unwrap();
        assert!(load(&pins).is_empty());
    }

    #[test]
    fn audit_file_concurrent_appends_lose_nothing_across_a_rotation() {
        const THREADS: usize = 8;
        const LINES: usize = 10;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.jsonl");
        let rotated = d.path().join("s.jsonl.1");
        let file = Arc::new(AuditFile::new(path.clone(), 6 * 1024));
        let start = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let (file, start) = (file.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    for seq in 0..LINES {
                        let line = json!({ "thread": t, "seq": seq, "pad": "x".repeat(90) });
                        file.append_line(line.to_string().as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let old = lines_of(&rotated);
        let new = lines_of(&path);
        assert!(!old.is_empty(), "the burst crosses the threshold once");
        assert_eq!(old.len() + new.len(), THREADS * LINES);
        let pairs: BTreeSet<(u64, u64)> = old
            .iter()
            .chain(&new)
            .map(|v| (v["thread"].as_u64().unwrap(), v["seq"].as_u64().unwrap()))
            .collect();
        assert_eq!(pairs.len(), THREADS * LINES, "every (thread, seq) appears once");
        for t in 0..THREADS as u64 {
            let last_old = old.iter().filter(|v| v["thread"] == t).map(|v| v["seq"].as_u64().unwrap()).max();
            let first_new = new.iter().filter(|v| v["thread"] == t).map(|v| v["seq"].as_u64().unwrap()).min();
            if let (Some(a), Some(b)) = (last_old, first_new) {
                assert!(a < b, "thread {t}: a line in the rotated file follows one in the main file");
            }
        }
        for p in [&path, &rotated] {
            assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn a_rotation_in_progress_blocks_the_other_writers() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("parked.jsonl");
        let rotated = d.path().join("parked.jsonl.1");
        let file = Arc::new(AuditFile::new(path.clone(), 64));
        file.append_line(&vec![b'a'; 60]).unwrap();
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let parked_tx = Mutex::new(parked_tx);
        let go_rx = Mutex::new(go_rx);
        set_after_rotate(
            &path,
            Some(Arc::new(move || {
                parked_tx.lock().unwrap().send(()).unwrap();
                go_rx.lock().unwrap().recv().unwrap();
            })),
        );
        let rotating = {
            let file = file.clone();
            std::thread::spawn(move || file.append_line(br#"{"who":"rotator"}"#).unwrap())
        };
        parked_rx.recv().unwrap();
        set_after_rotate(&path, None);
        let other = {
            let file = file.clone();
            std::thread::spawn(move || file.append_line(br#"{"who":"other"}"#).unwrap())
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!path.exists() || fs::metadata(&path).unwrap().len() == 0, "the other writer waits");
        assert!(!fs::read_to_string(&rotated).unwrap().contains("other"), "no other line reaches the rotated file");
        go_tx.send(()).unwrap();
        rotating.join().unwrap();
        other.join().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("rotator") && text.contains("other"));
    }

    #[test]
    fn audit_file_survives_a_poisoned_lock() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.jsonl");
        let file = Arc::new(AuditFile::new(path.clone(), ROTATE_BYTES));
        let holder = file.clone();
        let panicked = std::thread::spawn(move || {
            let _held = holder.lock.lock().unwrap();
            panic!("a panic while holding the audit lock");
        })
        .join();
        assert!(panicked.is_err());
        file.append_line(br#"{"seq":1}"#).unwrap();
        assert_eq!(lines_of(&path)[0]["seq"], 1);
    }

    #[test]
    fn a_failed_append_is_kept_for_status_and_cleared_by_the_next_success() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.jsonl");
        fs::create_dir(&path).unwrap();
        let file = AuditFile::new(path.clone(), ROTATE_BYTES);
        file.record(&json!({ "seq": 1 }));
        assert!(file.last_error().unwrap().contains("not a regular file"));
        fs::remove_dir(&path).unwrap();
        file.record(&json!({ "seq": 2 }));
        assert_eq!(file.last_error(), None);
        assert_eq!(lines_of(&path)[0]["seq"], 2);
    }
}

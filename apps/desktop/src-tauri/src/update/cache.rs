//! Update-check cache.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::source::UpdateCandidate;

const APP_SUPPORT: &str = "Library/Application Support/io.github.dickwu.git-strata";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn path(home: &Path) -> PathBuf {
    home.join(APP_SUPPORT).join("update.json")
}

#[derive(Debug)]
pub(crate) struct CheckRecord {
    pub automatic: bool,
    pub last_check_s: Option<u64>,
    pub rate_limit_s: u64,
    pub last_result: Option<CachedCheck>,
}

impl Default for CheckRecord {
    fn default() -> Self {
        Self {
            automatic: true,
            last_check_s: None,
            rate_limit_s: 0,
            last_result: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CachedCheck {
    pub checked_for: String,
    pub candidate: Option<UpdateCandidate>,
}

pub(crate) fn read_check(path: &Path) -> io::Result<CheckRecord> {
    let document = read_document(path)?;
    let Some(check) = document.get("check") else {
        return Ok(CheckRecord::default());
    };
    let check = check.as_object().ok_or_else(invalid_check)?;
    Ok(CheckRecord {
        // An absent preference uses the default. An unreadable saved choice opts out.
        automatic: check
            .get("automatic")
            .is_none_or(|value| value.as_bool() == Some(true)),
        last_check_s: check.get("last_check_s").and_then(Value::as_u64),
        rate_limit_s: check
            .get("rate_limit_s")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        last_result: check
            .get("last_result")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
    })
}

pub(crate) fn write_check(
    path: &Path,
    last_check_s: u64,
    rate_limit_s: u64,
    last_result: Option<&CachedCheck>,
) -> io::Result<()> {
    let last_result = last_result
        .map(serde_json::to_value)
        .transpose()
        .map_err(io::Error::other)?;
    edit_check(path, move |check| {
        check.insert("last_check_s".into(), last_check_s.into());
        check.insert("rate_limit_s".into(), rate_limit_s.into());
        if let Some(result) = last_result {
            check.insert("last_result".into(), result);
        }
    })
}

pub(crate) fn write_automatic(path: &Path, automatic: bool) -> io::Result<()> {
    edit_check(path, |check| {
        check.insert("automatic".into(), automatic.into());
    })
}

pub(crate) fn normalize_last_check(
    path: &Path,
    observed_s: u64,
    normalized_s: u64,
) -> io::Result<()> {
    edit_check(path, |check| {
        // Another checker may have saved a new timestamp since startup read it.
        if let Some(timestamp) = check.get_mut("last_check_s")
            && timestamp.as_u64() == Some(observed_s)
        {
            *timestamp = normalized_s.into();
        }
    })
}

fn edit_check(path: &Path, edit: impl FnOnce(&mut Map<String, Value>)) -> io::Result<()> {
    rewrite_document(path, |document| {
        let check = document
            .entry("check")
            .or_insert_with(|| Value::Object(Map::new()));
        edit(check.as_object_mut().ok_or_else(invalid_check)?);
        Ok(())
    })
}

fn invalid_check() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "update check cache is not an object",
    )
}

pub fn read_document(path: &Path) -> io::Result<Map<String, Value>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    match options.open(path) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error),
    }
}

/// Use this for update.json writes so every writer takes the same lock.
pub fn edit_document(path: &Path, edit: impl FnOnce(&mut Map<String, Value>)) -> io::Result<()> {
    rewrite_document(path, |document| {
        edit(document);
        Ok(())
    })
}

fn rewrite_document(
    path: &Path,
    edit: impl FnOnce(&mut Map<String, Value>) -> io::Result<()>,
) -> io::Result<()> {
    let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(directory)?;
    // Lock the stable sibling inode. The document inode changes on every rename.
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let lock = options.open(path.with_extension("lock"))?;
    lock.lock()?;

    let mut document = read_document(path)?;
    edit(&mut document)?;

    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".update-{}-{sequence}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, &document).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_writes_preserve_transaction_move_and_future_fields() {
        let home = tempfile::tempdir().unwrap();
        let path = path(home.path());
        assert!(
            path.ends_with("Library/Application Support/io.github.dickwu.git-strata/update.json")
        );
        assert!(read_check(&path).unwrap().automatic);
        let txn = json!({ "phase": "a_future_phase", "from_daemon": null, "additional": 17 });
        let move_state = json!({ "phase": "bootout" });
        edit_document(&path, |doc| {
            doc.insert("txn".into(), txn.clone());
            doc.insert("move".into(), move_state.clone());
            doc.insert("future_root".into(), json!(3));
            doc.insert("check".into(), json!({ "future_check": true }));
        })
        .unwrap();
        write_automatic(&path, false).unwrap();
        write_check(&path, 123, 43_200, None).unwrap();
        normalize_last_check(&path, 123, 100).unwrap();
        normalize_last_check(&path, 123, 99).unwrap();
        let doc = read_document(&path).unwrap();
        assert_eq!(doc["txn"], txn);
        assert_eq!(doc["move"], move_state);
        assert_eq!(doc["future_root"], 3);
        assert_eq!(doc["check"]["future_check"], true);
        let stored = read_check(&path).unwrap();
        assert!(!stored.automatic);
        assert_eq!(stored.last_check_s, Some(100));
        assert_eq!(stored.rate_limit_s, 43_200);
    }

    #[test]
    fn separate_writers_preserve_the_transaction_and_check() {
        use std::{sync::mpsc, thread};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        thread::scope(|scope| {
            let transaction = scope.spawn(|| {
                edit_document(&path, move |document| {
                    document.insert(
                        "txn".into(),
                        json!({ "phase": "future", "from_daemon": null }),
                    );
                    entered_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                })
                .unwrap();
            });
            entered_rx.recv().unwrap();
            // An independent open competes for the OS lock, just like another app process.
            let blocked = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.with_extension("lock"))
                .map(|lock| lock.try_lock());
            let check = scope.spawn(|| write_check(&path, 456, 0, None).unwrap());
            finish_tx.send(()).unwrap();
            transaction.join().unwrap();
            assert!(matches!(
                blocked,
                Ok(Err(std::fs::TryLockError::WouldBlock))
            ));
            check.join().unwrap();
        });
        let document = read_document(&path).unwrap();
        assert_eq!(
            document["txn"],
            json!({ "phase": "future", "from_daemon": null })
        );
        assert_eq!(document["check"]["last_check_s"], 456);
    }

    #[test]
    fn result_parse_errors_cannot_override_a_saved_preference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        edit_document(&path, |document| {
            document.insert(
                "check".into(),
                json!({ "automatic": false, "last_result": "future format", "last_check_s": 42, "rate_limit_s": 43200 }),
            );
        })
        .unwrap();
        let check = read_check(&path).unwrap();
        assert!(!check.automatic);
        assert!(check.last_result.is_none());
        assert_eq!(check.last_check_s, Some(42));
        assert_eq!(check.rate_limit_s, 43_200);
        write_check(&path, 456, 0, None).unwrap();
        let check = &read_document(&path).unwrap()["check"];
        assert_eq!(check["automatic"], false);
        assert_eq!(check["last_result"], "future format");
    }

    #[test]
    fn damaged_cache_is_left_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        fs::write(&path, b"{\"txn\":").unwrap();
        assert!(write_check(&path, 456, 0, None).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{\"txn\":");

        fs::write(&path, br#"{"check": []}"#).unwrap();
        assert!(read_check(&path).is_err());
        assert!(write_check(&path, 456, 0, None).is_err());
        assert!(write_automatic(&path, true).is_err());
        assert_eq!(fs::read(path).unwrap(), br#"{"check": []}"#);
    }

    #[cfg(unix)]
    #[test]
    fn cache_and_lock_cannot_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("other.json");
        fs::write(&target, b"{}").unwrap();
        let path = dir.path().join("update.json");
        symlink(&target, &path).unwrap();
        assert!(read_document(&path).is_err());
        assert!(write_automatic(&path, false).is_err());
        fs::remove_file(&path).unwrap();

        let lock = path.with_extension("lock");
        fs::remove_file(&lock).unwrap();
        symlink(&target, &lock).unwrap();
        assert!(write_automatic(&path, false).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read(target).unwrap(), b"{}");
    }
}

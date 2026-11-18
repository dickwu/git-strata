//! On-disk update transaction (version-upgrade design §4.2).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_types::{Code, serde_rfc3339};

use crate::error::GuiError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Txn {
    /// Persisted phase name, including recovery phases such as `cancel_after_stop`.
    pub phase: String,
    pub from: String,
    pub to: String,
    pub from_schema: u32,
    pub to_schema: u32,
    pub to_floor: String,
    /// Required key. Null when the daemon was not serving at the start of the update.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub from_daemon: Option<DaemonIdentity>,
    pub program: PathBuf,
    pub rollback_zip: PathBuf,
    /// The label's original enabled state, recorded before taking the fence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_enabled: Option<bool>,
}

/// Extra keys within `update.json.txn` owned by the install coordinator.
/// Other update operations can continue to construct the base `Txn`.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct InstallDetails {
    /// Wall-clock anchor for step 11 across the app relaunch, in Unix milliseconds.
    pub(super) start_requested_ms: Option<u64>,
    /// Budget already spent when macOS paused the wait. The wall time
    /// spent on permissions stays out of the budget across app restarts.
    pub(super) permission_wait_elapsed_ms: Option<u64>,
    pub(super) cancel_not_serving: bool,
    /// Exact path returned by the plugin after a failed swap.
    pub(super) kept_bundle: Option<PathBuf>,
    /// Plugin destinations that were vacant before the swap began.
    pub(super) kept_candidates: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonIdentity {
    pub pid: u32,
    #[serde(with = "serde_rfc3339")]
    pub started: SystemTime,
    pub version: String,
}

/// The App's `update.json`. All writers of this shared document should use
/// `edit_document` so a cache refresh cannot overwrite a transaction phase.
pub struct TxnStore {
    path: PathBuf,
}

impl TxnStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<Option<Txn>, GuiError> {
        read_txn(&self.read_document()?)
    }

    pub(super) fn require_idle(&self) -> Result<(), GuiError> {
        require_idle(&self.read_document()?)
    }

    /// Takes ownership of the rollback zip. An unresolved or unrecognized
    /// transaction must survive another attempt, along with its zip.
    pub fn begin(&self, txn: &Txn) -> Result<(), GuiError> {
        self.edit_document(|document| {
            require_idle(document)?;
            document.insert("txn".into(), serde_json::to_value(txn).map_err(invalid)?);
            Ok(())
        })
    }

    pub fn save(&self, txn: &Txn) -> Result<(), GuiError> {
        self.edit_txn(txn, |saved| {
            let fields = serde_json::to_value(txn).map_err(invalid)?;
            let Value::Object(fields) = fields else {
                return Err(invalid("transaction must be an object"));
            };
            saved.extend(fields);
            if txn.was_enabled.is_none() {
                saved.remove("was_enabled");
            }
            Ok(())
        })
    }

    pub(super) fn install_details(&self, txn: &Txn) -> Result<InstallDetails, GuiError> {
        let document = self.read_document()?;
        check_owner(&document, txn)?;
        serde_json::from_value(document["txn"].clone()).map_err(invalid)
    }

    pub(super) fn edit_install(
        &self,
        txn: &Txn,
        phase: Option<&str>,
        edit: impl FnOnce(&mut InstallDetails),
    ) -> Result<(), GuiError> {
        self.edit_txn(txn, |saved| {
            let mut details =
                serde_json::from_value(Value::Object(saved.clone())).map_err(invalid)?;
            edit(&mut details);
            let Value::Object(fields) = serde_json::to_value(details).map_err(invalid)? else {
                return Err(invalid("install details must be an object"));
            };
            saved.extend(fields);
            if let Some(phase) = phase {
                saved.insert("phase".into(), json!(phase));
            }
            Ok(())
        })
    }

    fn edit_txn(
        &self,
        txn: &Txn,
        edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), GuiError>,
    ) -> Result<(), GuiError> {
        self.edit_document(|document| {
            check_owner(document, txn)?;
            let saved = document
                .get_mut("txn")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| invalid("transaction must be an object"))?;
            edit(saved)
        })
    }

    /// Call only after verifying the end of the transaction.
    pub fn clear(&self, txn: &Txn) -> Result<(), GuiError> {
        self.edit_document(|document| {
            check_owner(document, txn)?;
            document.remove("txn");
            Ok(())
        })
    }

    pub fn edit_document(
        &self,
        edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), GuiError>,
    ) -> Result<(), GuiError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| invalid("update.json has no parent"))?;
        fs::create_dir_all(dir).map_err(io_error)?;
        // Lock a stable inode. Renaming update.json cannot break this lock.
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path.with_extension("lock"))
            .map_err(io_error)?;
        lock.lock().map_err(io_error)?;
        let mut document = self.read_document()?;
        edit(&mut document)?;
        write_document(dir, &self.path, &document)
    }

    fn read_document(&self) -> Result<Map<String, Value>, GuiError> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(libc::O_NOFOLLOW);
        match options.open(&self.path) {
            Ok(file) => serde_json::from_reader(file).map_err(invalid),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(io_error(e)),
        }
    }
}

fn write_document(dir: &Path, path: &Path, document: &Map<String, Value>) -> Result<(), GuiError> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let (temp, mut file) = loop {
        let seq = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".update-{}-{seq}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(e)),
        }
    };
    let result = (|| {
        serde_json::to_writer(&mut file, document).map_err(invalid)?;
        file.write_all(b"\n").map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        fs::rename(&temp, path).map_err(io_error)?;
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn read_txn(document: &Map<String, Value>) -> Result<Option<Txn>, GuiError> {
    match document.get("txn") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(invalid),
    }
}

fn require_idle(document: &Map<String, Value>) -> Result<(), GuiError> {
    if read_txn(document)?.is_some() {
        return Err(pending());
    }
    if document.get("move").is_some_and(|value| !value.is_null()) {
        return Err(GuiError::new(
            Code::Busy,
            "Finish moving the daemon before installing an update",
            json!({"reason": "update_move_pending"}),
        ));
    }
    Ok(())
}

fn check_owner(document: &Map<String, Value>, txn: &Txn) -> Result<(), GuiError> {
    match read_txn(document)? {
        Some(current)
            if current.from == txn.from
                && current.to == txn.to
                && current.program == txn.program
                && current.rollback_zip == txn.rollback_zip =>
        {
            Ok(())
        }
        _ => Err(invalid("the transaction changed on disk")),
    }
}

fn invalid(error: impl std::fmt::Display) -> GuiError {
    GuiError::new(
        Code::InvalidInput,
        format!("Cannot read the update transaction: {error}"),
        json!({"reason": "update_txn_invalid"}),
    )
}

fn io_error(error: io::Error) -> GuiError {
    GuiError::new(
        Code::Unknown,
        format!("Cannot access the update transaction: {error}"),
        json!({"reason": "update_txn_io", "errno": error.raw_os_error()}),
    )
}

pub(super) fn pending() -> GuiError {
    GuiError::new(
        Code::Busy,
        "Finish or roll back the previous update first",
        json!({"reason": "update_txn_pending"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn transaction() -> Value {
        json!({
            "phase": "stopping",
            "from": "0.3.0",
            "to": "0.3.1",
            "from_schema": 7,
            "to_schema": 7,
            "to_floor": "0.3.0",
            "from_daemon": null,
            "program": "/test/GIT STRATA.app/Contents/MacOS/git-stratad",
            "rollback_zip": "/test/previous/GIT-STRATA-0.3.0.zip"
        })
    }

    #[test]
    fn from_daemon_round_trips_an_identity() {
        let mut input = transaction();
        input["from_daemon"] = json!({
            "pid": 42,
            "started": "1970-01-01T00:00:00Z",
            "version": "0.3.0"
        });
        let txn: Txn = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(
            txn.from_daemon,
            Some(DaemonIdentity {
                pid: 42,
                started: SystemTime::UNIX_EPOCH,
                version: "0.3.0".into(),
            })
        );
        assert_eq!(serde_json::to_value(txn).unwrap(), input);
    }

    #[test]
    fn from_daemon_round_trips_explicit_null() {
        let input = transaction();
        let txn: Txn = serde_json::from_value(input.clone()).unwrap();
        assert!(txn.from_daemon.is_none());
        assert_eq!(serde_json::to_value(txn).unwrap(), input);
    }

    #[test]
    fn from_daemon_key_cannot_be_omitted() {
        let mut input = transaction();
        input.as_object_mut().unwrap().remove("from_daemon");
        let error = serde_json::from_value::<Txn>(input).unwrap_err();
        assert_eq!(error.to_string(), "missing field `from_daemon`");
    }

    #[test]
    fn phase_updates_and_clear_preserve_the_rest_of_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        let mut input = transaction();
        input["future_field"] = json!(37);
        fs::write(
            &path,
            json!({"txn": input, "move": {"phase": "done"}, "check": 9}).to_string(),
        )
        .unwrap();
        let store = TxnStore::new(path.clone());
        let mut txn = store.load().unwrap().unwrap();
        txn.phase = "bootout".into();
        txn.was_enabled = Some(true);
        store
            .edit_install(&txn, None, |details| {
                details.start_requested_ms = Some(10_000);
            })
            .unwrap();
        store.save(&txn).unwrap();
        assert_eq!(store.load().unwrap(), Some(txn.clone()));
        assert!(dir.path().join("update.lock").is_file());
        let document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["txn"]["future_field"], 37);
        assert_eq!(document["txn"]["start_requested_ms"], 10_000);
        store.clear(&txn).unwrap();
        assert_eq!(store.load().unwrap(), None);
        let document: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(document, json!({"move": {"phase": "done"}, "check": 9}));
    }

    #[test]
    fn begin_keeps_unrecognized_and_corrupt_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        let store = TxnStore::new(path.clone());
        let mut txn: Txn = serde_json::from_value(transaction()).unwrap();
        txn.phase = "from_a_newer_app".into();
        store.begin(&txn).unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            store.begin(&txn).unwrap_err().reason(),
            Some("update_txn_pending")
        );
        assert_eq!(fs::read(&path).unwrap(), before);

        fs::write(&path, b"{unfinished").unwrap();
        assert_eq!(
            store.begin(&txn).unwrap_err().reason(),
            Some("update_txn_invalid")
        );
        assert_eq!(fs::read(&path).unwrap(), b"{unfinished");
    }

    #[test]
    fn begin_leaves_an_unfinished_move_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        let document = br#"{"move":{"phase":"bootout"}}"#;
        let store = TxnStore::new(path.clone());
        let txn: Txn = serde_json::from_value(transaction()).unwrap();
        assert!(store.require_idle().is_ok());
        fs::write(&path, document).unwrap();

        let error = store.begin(&txn).unwrap_err();
        assert_eq!(error.reason(), Some("update_move_pending"));
        assert_eq!(fs::read(path).unwrap(), document);
    }
}

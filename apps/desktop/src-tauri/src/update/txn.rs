//! On-disk update transaction (version-upgrade design §4.2).

use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_types::{Code, serde_rfc3339};

use crate::error::GuiError;

use super::bundle::ExchangeEvidence;
use super::state::{RollbackPhase, TxnPhase};

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
    pub(super) stop_requested_ms: Option<u64>,
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RollbackDisposition {
    Continue,
    RetainTarget,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RollbackSubject {
    Serving(DaemonIdentity),
    NoPid,
}

impl RollbackSubject {
    pub(super) fn daemon(&self) -> Option<&DaemonIdentity> {
        match self {
            Self::Serving(daemon) => Some(daemon),
            Self::NoPid => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RollbackAction {
    Acquire,
    Renew,
    Seal,
    Release,
    Disable,
    Enable,
    Bootout,
    Exchange,
    Kickstart,
    Bootstrap,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct Admission {
    pub(super) action: RollbackAction,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TargetRecovery {
    Cancelled,
    NotStarting,
    DisableRefused,
    BootoutFailed,
    ExchangeRefused,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct RollbackScope {
    pub(super) plist: PathBuf,
    pub(super) config_dir: PathBuf,
    pub(super) data_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct RollbackAttempt {
    parent_id: String,
    pub(super) id: String,
    revision: u64,
    pub(super) disposition: RollbackDisposition,
    pub(super) phase: RollbackPhase,
    pub(super) subject: RollbackSubject,
    pub(super) scope: RollbackScope,
    pub(super) pending: Option<Admission>,
    pub(super) was_enabled: bool,
    pub(super) owns_fence: bool,
    pub(super) stop_requested_ms: Option<u64>,
    pub(super) start_requested_ms: Option<u64>,
    pub(super) seal_accepted: bool,
    pub(super) stopped: bool,
    pub(super) bootout_admitted: bool,
    pub(super) unloaded: bool,
    pub(super) exchange: Option<ExchangeEvidence>,
    pub(super) target_recovery: Option<TargetRecovery>,
}

impl RollbackAttempt {
    fn new(parent_id: String, subject: RollbackSubject, scope: RollbackScope) -> Self {
        let phase = match subject {
            RollbackSubject::Serving(_) => RollbackPhase::Draining,
            RollbackSubject::NoPid => RollbackPhase::Fencing,
        };
        Self {
            parent_id,
            id: super::state::new_id(),
            revision: 0,
            disposition: RollbackDisposition::Continue,
            phase,
            subject,
            scope,
            pending: None,
            was_enabled: false,
            owns_fence: false,
            stop_requested_ms: None,
            start_requested_ms: None,
            seal_accepted: false,
            stopped: false,
            bootout_admitted: false,
            unloaded: false,
            exchange: None,
            target_recovery: None,
        }
    }

    pub(super) fn withdrawn(&self) -> bool {
        self.disposition == RollbackDisposition::RetainTarget
    }

    pub(super) fn admit(&mut self, action: RollbackAction) {
        self.pending = Some(Admission {
            action,
            revision: self.revision,
        });
    }

    pub(super) fn action(&self) -> Option<RollbackAction> {
        self.pending.map(|admission| admission.action)
    }
}

/// The App's `update.json`. All writers of this shared document should use
/// `edit_document` so a cache refresh cannot overwrite a transaction phase.
pub struct TxnStore {
    path: PathBuf,
    #[cfg(test)]
    fail_cancel_sync: std::sync::atomic::AtomicBool,
}

impl TxnStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            #[cfg(test)]
            fail_cancel_sync: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn load(&self) -> Result<Option<Txn>, GuiError> {
        read_txn(&self.read_document()?)
    }

    pub(crate) fn require_idle(&self) -> Result<(), GuiError> {
        require_idle(&self.read_document()?)
    }

    /// Takes ownership of the rollback zip. An unresolved or unrecognized
    /// transaction must survive another attempt, along with its zip.
    pub fn begin(&self, txn: &Txn) -> Result<(), GuiError> {
        self.begin_with_id(txn, None)
    }

    pub(super) fn begin_with_id(&self, txn: &Txn, id: Option<String>) -> Result<(), GuiError> {
        self.edit_document(|document| {
            require_idle(document)?;
            let mut saved = serde_json::to_value(txn).map_err(invalid)?;
            saved["id"] = json!(id.unwrap_or_else(super::state::new_id));
            document.insert("txn".into(), saved);
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

    pub(super) fn id(&self, txn: &Txn) -> Result<String, GuiError> {
        let document = self.read_document()?;
        check_parent(&document, txn)?;
        Ok(txn_id(&document["txn"])?.to_owned())
    }

    pub(super) fn rollback(&self, txn: &Txn) -> Result<Option<RollbackAttempt>, GuiError> {
        let document = self.read_document()?;
        check_parent(&document, txn)?;
        read_rollback(&document["txn"])
    }

    pub(super) fn begin_rollback(
        &self,
        txn: &Txn,
        parent_id: &str,
        subject: RollbackSubject,
        scope: RollbackScope,
    ) -> Result<(), GuiError> {
        self.edit_document(|document| {
            check_parent(document, txn)?;
            let saved = document.get_mut("txn").ok_or_else(stale_rollback)?;
            if txn_id(saved)? != parent_id
                || read_rollback(saved)?.is_some()
                || !matches!(
                    TxnPhase::parse(&txn.phase),
                    Some(
                        TxnPhase::StartingDaemon
                            | TxnPhase::Relaunching
                            | TxnPhase::WaitingDaemon
                            | TxnPhase::WaitingPermission
                    )
                )
            {
                return Err(stale_rollback());
            }
            let attempt = RollbackAttempt::new(parent_id.into(), subject, scope);
            saved["rollback"] = serde_json::to_value(attempt).map_err(invalid)?;
            Ok(())
        })
    }

    /// Compare the parent and the complete attempt under the same lock used by
    /// Cancel. The returned snapshot is usable only after the synced write.
    pub(super) fn advance_rollback(
        &self,
        txn: &Txn,
        expected: &RollbackAttempt,
        edit: impl FnOnce(&mut RollbackAttempt),
    ) -> Result<Option<RollbackAttempt>, GuiError> {
        let mut changed = None;
        let result = self.edit_document(|document| {
            let saved = rollback_parent(document, txn, expected)?;
            let mut current = read_rollback(saved)?.ok_or_else(stale_rollback)?;
            if &current != expected {
                return Err(stale_rollback());
            }
            current.revision = current.revision.checked_add(1).ok_or_else(stale_rollback)?;
            edit(&mut current);
            saved["rollback"] = serde_json::to_value(&current).map_err(invalid)?;
            changed = Some(current);
            Ok(())
        });
        match result {
            Ok(()) => Ok(changed),
            Err(error) if error.reason() == Some("update_rollback_changed") => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The Cancel cutoff is the durable bootout admission. An earlier Cancel
    /// saves withdrawal and its cleanup continuation before acknowledging it.
    pub(super) fn withdraw_rollback(&self) -> Result<bool, GuiError> {
        let document = self.read_document()?;
        let Some(txn) = read_txn(&document)? else {
            return Ok(false);
        };
        let Some(expected) = read_rollback(&document["txn"])? else {
            return Ok(false);
        };
        self.edit_document(|document| {
            let saved = rollback_parent(document, &txn, &expected)?;
            let mut attempt = read_rollback(saved)?.ok_or_else(stale_rollback)?;
            if attempt.id != expected.id {
                return Err(stale_rollback());
            }
            if attempt.bootout_admitted {
                return Err(not_cancellable());
            }
            if attempt.withdrawn() {
                return Ok(());
            }
            attempt.revision = attempt.revision.checked_add(1).ok_or_else(stale_rollback)?;
            attempt.disposition = RollbackDisposition::RetainTarget;
            if attempt.phase == RollbackPhase::Stopping {
                attempt.phase = RollbackPhase::CancelAfterStop;
            }
            if attempt.target_recovery.is_some() {
                attempt.target_recovery = Some(TargetRecovery::Cancelled);
            }
            saved["rollback"] = serde_json::to_value(attempt).map_err(invalid)?;
            Ok(())
        })?;
        #[cfg(test)]
        if self
            .fail_cancel_sync
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(io_error(io::Error::other(
                "injected directory sync failure after rename",
            )));
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(super) fn fail_next_cancel_sync(&self) {
        self.fail_cancel_sync
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn finish_rollback(
        &self,
        txn: &Txn,
        expected: &RollbackAttempt,
        consume_parent: bool,
    ) -> Result<bool, GuiError> {
        let result = self.edit_document(|document| {
            let saved = rollback_parent(document, txn, expected)?;
            if read_rollback(saved)?.as_ref() != Some(expected) {
                return Err(stale_rollback());
            }
            if consume_parent {
                document.remove("txn");
            } else {
                saved
                    .as_object_mut()
                    .ok_or_else(stale_rollback)?
                    .remove("rollback");
            }
            Ok(())
        });
        match result {
            Ok(()) => Ok(true),
            Err(error) if error.reason() == Some("update_rollback_changed") => Ok(false),
            Err(error) => Err(error),
        }
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
        super::cache::rewrite_document(&self.path, |document| {
            edit(document).map_err(io::Error::other)
        })
        .map_err(|error| {
            error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<GuiError>())
                .cloned()
                .unwrap_or_else(|| map_document_error(error))
        })
    }

    fn read_document(&self) -> Result<Map<String, Value>, GuiError> {
        super::cache::read_document(&self.path).map_err(map_document_error)
    }
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

fn txn_id(saved: &Value) -> Result<&str, GuiError> {
    saved["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("the transaction has no ID"))
}

fn read_rollback(saved: &Value) -> Result<Option<RollbackAttempt>, GuiError> {
    let Some(value) = saved.get("rollback") else {
        return Ok(None);
    };
    let attempt: RollbackAttempt = serde_json::from_value(value.clone()).map_err(invalid)?;
    if attempt.parent_id != txn_id(saved)? {
        return Err(invalid(
            "the rollback attempt belongs to a different transaction",
        ));
    }
    Ok(Some(attempt))
}

fn check_parent(document: &Map<String, Value>, txn: &Txn) -> Result<(), GuiError> {
    if read_txn(document)?.as_ref() == Some(txn) {
        Ok(())
    } else {
        Err(stale_rollback())
    }
}

fn rollback_parent<'a>(
    document: &'a mut Map<String, Value>,
    txn: &Txn,
    attempt: &RollbackAttempt,
) -> Result<&'a mut Value, GuiError> {
    check_parent(document, txn)?;
    let saved = document.get_mut("txn").ok_or_else(stale_rollback)?;
    if txn_id(saved)? != attempt.parent_id {
        return Err(stale_rollback());
    }
    Ok(saved)
}

pub(super) fn stale_rollback() -> GuiError {
    GuiError::new(
        Code::Busy,
        "The rollback attempt changed",
        json!({"reason": "update_rollback_changed"}),
    )
}

pub(super) fn not_cancellable() -> GuiError {
    GuiError::new(
        Code::Busy,
        "The update is past its cancellation point",
        json!({"reason": "update_not_cancellable"}),
    )
}

fn map_document_error(error: io::Error) -> GuiError {
    if error.kind() == io::ErrorKind::InvalidData {
        invalid(error)
    } else {
        io_error(error)
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
    use std::fs;

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

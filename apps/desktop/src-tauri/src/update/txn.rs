//! On-disk update transaction (version-upgrade design §4.2).

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use strata_types::serde_rfc3339;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonIdentity {
    pub pid: u32,
    #[serde(with = "serde_rfc3339")]
    pub started: SystemTime,
    pub version: String,
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
}

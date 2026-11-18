//! Bundle rollback eligibility and recovery.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::json;
use strata_types::{Code, serde_rfc3339};

use super::{
    bundle::{
        BundleOps, RestoreDecision, probe_bundle, release_version, restore_previous,
        verify_predecessor_schema, verify_target_info,
    },
    txn::Txn,
};
use crate::{error::GuiError, service::scope::anchored};

const MANUAL_BACKUPS: &str =
    "See the release notes for manual recovery and verified store backups in store/backups/.";

/// Obtain fresh evidence from the daemon identity verified by the lifecycle
/// flow. A timeout, failed `print`, or uncertain migration is `Unknown`.
pub enum StoreEvidence<'a> {
    Live(Option<u32>),
    Down {
        config_dir: &'a Path,
        data_root: &'a Path,
    },
    Unknown,
}

/// Design §5.3 preflight. The fenced file step rechecks the store from disk.
pub fn check(txn: &Txn, evidence: StoreEvidence<'_>) -> Result<(), GuiError> {
    let versions = release_version(&txn.from).zip(release_version(&txn.to_floor));
    match versions {
        Some((from, floor)) if from >= floor => (),
        Some(_) => {
            return Err(disabled(
                "rollback_floor",
                format!(
                    "Version {} is below the downgrade floor {}.",
                    txn.from, txn.to_floor
                ),
            ));
        }
        None => {
            return Err(disabled(
                "rollback_version_unknown",
                "The rollback version or downgrade floor could not be verified.",
            ));
        }
    }

    let schema = match evidence {
        StoreEvidence::Live(schema) => schema,
        StoreEvidence::Down {
            config_dir,
            data_root,
        } => read_store_schema(config_dir, data_root),
        StoreEvidence::Unknown => None,
    };
    match schema {
        Some(schema) if schema > 0 && txn.from_schema > 0 => {
            if schema > txn.from_schema {
                return Err(disabled(
                    "rollback_store_migrated",
                    format!(
                        "The store is at schema {schema}. Version {} supports schema {}.",
                        txn.from, txn.from_schema
                    ),
                ));
            }
            Ok(())
        }
        _ => Err(disabled(
            "rollback_store_unknown",
            "The store's schema could not be verified.",
        )),
    }
}

/// The explicit rollback's file step. Call only after §6.1 confirms that the
/// job is unloaded and disabled. Recheck the store and app after preparing the
/// zip, immediately before replacing the recorded target.
pub fn restore_fenced(
    ops: &dyn BundleOps,
    bundle: &Path,
    txn: &Txn,
    config_dir: &Path,
    data_root: &Path,
) -> Result<(), GuiError> {
    let check_store = || {
        check(
            txn,
            StoreEvidence::Down {
                config_dir,
                data_root,
            },
        )
    };
    let decide = || -> Result<RestoreDecision, GuiError> {
        let app = probe_bundle(ops, bundle)
            .map_err(|error| disabled("rollback_bundle_unverified", error.into_error()))?;
        if app.info.version == txn.from {
            verify_predecessor_schema(&app.info, txn)?;
            return Ok(RestoreDecision::AlreadyRestored);
        }
        verify_target_info(&app.info, txn)?;
        Ok(RestoreDecision::Swap {
            expected: Some(app.identity),
        })
    };
    check_store()?;
    if matches!(decide()?, RestoreDecision::AlreadyRestored) {
        return check_store();
    }
    restore_previous(ops, bundle, txn, &|| {
        let decision = decide()?;
        check_store()?;
        Ok(decision)
    })
}

fn disabled(reason: &str, cause: impl std::fmt::Display) -> GuiError {
    GuiError::new(
        Code::Unsupported,
        format!("Rollback is unavailable. {cause} {MANUAL_BACKUPS}"),
        json!({ "reason": reason }),
    )
}

#[derive(Deserialize)]
struct StoreState {
    user_version: u32,
    root: PathBuf,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    migrated_from: Option<u32>,
    #[serde(rename = "at", with = "serde_rfc3339")]
    _written_at: SystemTime,
    #[serde(flatten)]
    additional: serde_json::Map<String, serde_json::Value>,
}

fn read_store_schema(config_dir: &Path, data_root: &Path) -> Option<u32> {
    for path in [config_dir, data_root] {
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return None;
        }
    }
    if anchored(config_dir).starts_with(anchored(data_root)) {
        return None;
    }
    let path = config_dir.join("store_state.json");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    const MAX_STATE_BYTES: u64 = 64 * 1024;
    let mut contents = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut contents)
        .ok()?;
    if contents.len() as u64 > MAX_STATE_BYTES {
        return None;
    }
    let state: StoreState = serde_json::from_slice(&contents).ok()?;
    (state.root == data_root
        && !state.additional.contains_key("migrating_to")
        && state
            .migrated_from
            .is_none_or(|from| from < state.user_version))
    .then_some(state.user_version)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    fn txn() -> Txn {
        serde_json::from_value(json!({
            "phase": "daemon_failed",
            "from": "0.3.10",
            "to": "0.3.11",
            "from_schema": 7,
            "to_schema": 8,
            "to_floor": "0.3.9",
            "from_daemon": null,
            "program": "/test/GIT STRATA.app/Contents/MacOS/git-stratad",
            "rollback_zip": "/test/previous/GIT-STRATA-0.3.10.zip",
            "was_enabled": true,
        }))
        .unwrap()
    }

    fn assert_disabled(result: Result<(), GuiError>, reason: &str) {
        let error = result.unwrap_err();
        assert_eq!(error.reason(), Some(reason));
        assert!(error.message.contains("store/backups/"));
    }

    #[test]
    fn requires_version_at_or_above_floor() {
        let mut txn = txn();
        let evidence = || StoreEvidence::Live(Some(7));
        assert!(check(&txn, evidence()).is_ok());
        txn.to_floor = txn.from.clone();
        assert!(check(&txn, evidence()).is_ok());
        txn.to_floor = "0.3.11".into();
        assert_disabled(check(&txn, evidence()), "rollback_floor");
        txn.to_floor = "0.03.0".into();
        assert_disabled(check(&txn, evidence()), "rollback_version_unknown");
    }

    #[test]
    fn live_schema_must_be_known_and_at_most_the_old_schema() {
        let txn = txn();
        for schema in [6, 7] {
            assert!(check(&txn, StoreEvidence::Live(Some(schema))).is_ok());
        }
        assert_disabled(
            check(&txn, StoreEvidence::Live(Some(8))),
            "rollback_store_migrated",
        );
        for evidence in [
            StoreEvidence::Live(None),
            StoreEvidence::Live(Some(0)),
            StoreEvidence::Unknown,
        ] {
            assert_disabled(check(&txn, evidence), "rollback_store_unknown");
        }
    }

    #[test]
    fn confirmed_down_uses_only_a_completed_snapshot_for_the_configured_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("data");
        let config = temp.path().join("config");
        fs::create_dir(&store).unwrap();
        fs::create_dir(&config).unwrap();
        let snapshot = config.join("store_state.json");
        let evidence = || StoreEvidence::Down {
            config_dir: &config,
            data_root: &store,
        };
        let txn = txn();

        assert_disabled(check(&txn, evidence()), "rollback_store_unknown");
        fs::write(&snapshot, "{").unwrap();
        assert_disabled(check(&txn, evidence()), "rollback_store_unknown");

        let mut state = json!({ "root": &store, "user_version": 7, "migrated_from": null, "at": "2026-11-17T20:00:00Z" });
        for key in ["at", "migrated_from"] {
            let mut incomplete = state.clone();
            incomplete.as_object_mut().unwrap().remove(key);
            fs::write(&snapshot, incomplete.to_string()).unwrap();
            assert_disabled(check(&txn, evidence()), "rollback_store_unknown");
        }
        for invalid in [
            json!({ "root": &store, "migrating_to": 8 }),
            json!({ "root": &store, "user_version": 7, "migrated_from": 8, "at": "2026-11-17T20:00:00Z" }),
            json!({ "root": &store, "user_version": 7, "migrated_from": null, "at": "unknown" }),
        ] {
            fs::write(&snapshot, invalid.to_string()).unwrap();
            assert_disabled(check(&txn, evidence()), "rollback_store_unknown");
        }

        fs::write(&snapshot, state.to_string()).unwrap();
        assert!(check(&txn, evidence()).is_ok());
        assert_disabled(
            check(&txn, StoreEvidence::Live(None)),
            "rollback_store_unknown",
        );
        assert_disabled(
            check(&txn, StoreEvidence::Unknown),
            "rollback_store_unknown",
        );

        state["migrating_to"] = json!(8);
        fs::write(&snapshot, state.to_string()).unwrap();
        assert_disabled(check(&txn, evidence()), "rollback_store_unknown");
        state["migrating_to"] = json!(null);
        fs::write(&snapshot, state.to_string()).unwrap();
        assert_disabled(check(&txn, evidence()), "rollback_store_unknown");
        state.as_object_mut().unwrap().remove("migrating_to");
        state["user_version"] = json!(8);
        state["migrated_from"] = json!(7);
        fs::write(&snapshot, state.to_string()).unwrap();
        assert_disabled(check(&txn, evidence()), "rollback_store_migrated");
        assert!(check(&txn, StoreEvidence::Live(Some(7))).is_ok());

        state["root"] = json!("/another-store");
        state["user_version"] = json!(7);
        state["migrated_from"] = json!(null);
        fs::write(&snapshot, state.to_string()).unwrap();
        assert_disabled(check(&txn, evidence()), "rollback_store_unknown");
    }

    #[test]
    fn skips_store_root_and_symlinked_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("data");
        let config = store.join("config");
        fs::create_dir_all(&config).unwrap();
        let snapshot = config.join("store_state.json");
        fs::write(
            &snapshot,
            json!({ "root": &store, "user_version": 7, "migrated_from": null, "at": "2026-11-17T20:00:00Z" }).to_string(),
        )
        .unwrap();
        let txn = txn();
        assert_disabled(
            check(
                &txn,
                StoreEvidence::Down {
                    config_dir: &config,
                    data_root: &store,
                },
            ),
            "rollback_store_unknown",
        );
        symlink(&snapshot, temp.path().join("store_state.json")).unwrap();
        assert_disabled(
            check(
                &txn,
                StoreEvidence::Down {
                    config_dir: temp.path(),
                    data_root: &store,
                },
            ),
            "rollback_store_unknown",
        );
    }
}

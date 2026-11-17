//! Update progress sent to the main window (version-upgrade design §4.3).

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strata_types::serde_rfc3339;

use super::manifest::UpdateTarget;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available {
        target: UpdateTarget,
    },
    CheckFailed,
    Confirming,
    Downloading {
        /// Percent when the total download size is known.
        pct: Option<u8>,
    },
    VerifyingBundle,
    Draining {
        /// The daemon's `inflight` entries.
        inflight: Vec<Value>,
        /// Background entries from the daemon's `holds` list.
        background: Vec<Value>,
    },
    Stopping,
    Installing,
    StartingDaemon,
    Relaunching,
    Reconciling,
    WaitingDaemon {
        #[serde(with = "serde_rfc3339")]
        since: SystemTime,
    },
    Done,
    DaemonFailed {
        reason: String,
    },
    WaitingPermission,
    Cancelled,
    NotStarting,
    InstallFailedUnchanged,
    Restored {
        from: String,
    },
    RestoreFailed,
    CancelBlocked,
    BootoutFailed,
    FenceUncertain,
}

#[cfg(test)]
mod tests {
    use super::UpdateState;
    use serde_json::json;

    #[test]
    fn old_app_end_states_match_the_wire_contract() {
        for (state, wire) in [
            (UpdateState::Cancelled, json!({"phase": "cancelled"})),
            (UpdateState::NotStarting, json!({"phase": "not_starting"})),
            (
                UpdateState::InstallFailedUnchanged,
                json!({"phase": "install_failed_unchanged"}),
            ),
            (
                UpdateState::Restored {
                    from: "0.3.0".into(),
                },
                json!({"phase": "restored", "from": "0.3.0"}),
            ),
            (
                UpdateState::RestoreFailed,
                json!({"phase": "restore_failed"}),
            ),
            (
                UpdateState::CancelBlocked,
                json!({"phase": "cancel_blocked"}),
            ),
            (
                UpdateState::BootoutFailed,
                json!({"phase": "bootout_failed"}),
            ),
            (
                UpdateState::FenceUncertain,
                json!({"phase": "fence_uncertain"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&state).unwrap(), wire);
            assert_eq!(serde_json::from_value::<UpdateState>(wire).unwrap(), state);
        }
    }
}

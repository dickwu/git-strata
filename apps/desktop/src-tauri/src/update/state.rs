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
    StopDetail {
        slow: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        close_error: Option<Value>,
    },
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

/// Durable intents are written before the service or bundle operation they
/// protect. Keep their names stable across app versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TxnPhase {
    Draining,
    Stopping,
    CancelAfterStop,
    Fencing,
    Disabling,
    Bootout,
    Installing,
    Restoring,
    StartingDaemon,
    Relaunching,
    WaitingDaemon,
    WaitingPermission,
    Interrupted,
    Unchanged,
    Restored,
    BootoutFailed,
}

impl TxnPhase {
    const ALL: [Self; 16] = [
        Self::Draining,
        Self::Stopping,
        Self::CancelAfterStop,
        Self::Fencing,
        Self::Disabling,
        Self::Bootout,
        Self::Installing,
        Self::Restoring,
        Self::StartingDaemon,
        Self::Relaunching,
        Self::WaitingDaemon,
        Self::WaitingPermission,
        Self::Interrupted,
        Self::Unchanged,
        Self::Restored,
        Self::BootoutFailed,
    ];

    pub(super) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.name() == name)
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Draining => "draining",
            Self::Stopping => "stopping",
            Self::CancelAfterStop => "cancel_after_stop",
            Self::Fencing => "fencing",
            Self::Disabling => "disabling",
            Self::Bootout => "bootout",
            Self::Installing => "installing",
            Self::Restoring => "restoring",
            Self::StartingDaemon => "starting_daemon",
            Self::Relaunching => "relaunching",
            Self::WaitingDaemon => "waiting_daemon",
            Self::WaitingPermission => "waiting_permission",
            Self::Interrupted => "interrupted",
            Self::Unchanged => "install_failed_unchanged",
            Self::Restored => "restored",
            Self::BootoutFailed => "bootout_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UpdateState;
    use serde_json::json;

    #[test]
    fn stopping_details_are_additive() {
        let state = UpdateState::Stopping;
        let wire = json!({"phase": "stopping"});
        assert_eq!(serde_json::to_value(&state).unwrap(), wire);
        assert_eq!(serde_json::from_value::<UpdateState>(wire).unwrap(), state);
        let detailed = UpdateState::StopDetail {
            slow: true,
            close_error: Some(json!("close failed")),
        };
        assert_eq!(
            serde_json::to_value(detailed).unwrap(),
            json!({
                "phase": "stop_detail", "slow": true, "close_error": "close failed"
            })
        );
    }

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

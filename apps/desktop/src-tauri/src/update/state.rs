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

/// Rollback leaves the parent update's recovery phase and timing intact.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RollbackPhase {
    Draining,
    Stopping,
    CancelAfterStop,
    Fencing,
    Disabling,
    Fenced,
    RollbackEnabling,
    Bootout,
    Restoring,
    StartingTarget,
    StartingPrevious,
    Relaunching,
}

#[cfg(test)]
mod tests {
    use super::UpdateState;
    use serde_json::json;

    #[test]
    fn a_persisted_restart_window_expires_on_the_original_budget() {
        let view = super::Presentation::default();
        assert!(view.begin());
        view.restore_timing(super::now_ms().saturating_sub(100_000), None);
        view.progress(UpdateState::Reconciling);
        view.progress(UpdateState::StartingDaemon);
        assert!(view.expected_restart());
        view.0.lock().unwrap().timing.as_mut().unwrap().deadline = std::time::Instant::now();
        assert!(!view.expected_restart());

        view.restore_timing(super::now_ms().saturating_sub(200_000), Some(100_000));
        assert!(view.expected_restart());
        view.finish(None);
        assert!(!view.expected_restart());
    }

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

use crate::error::GuiError;
use serde_json::json;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

pub(super) fn new_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{}-{}-{}",
        std::process::id(),
        now.as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Default)]
pub(crate) struct Presentation(Mutex<View>);

#[derive(Default)]
struct View {
    id: Option<String>,
    phase: Option<UpdateState>,
    active: bool,
    error: Option<GuiError>,
    consent: Option<Value>,
    timing: Option<RestartTiming>,
}

#[derive(Clone, Copy)]
struct RestartTiming {
    start_ms: u64,
    deadline: std::time::Instant,
    paused_elapsed: Option<u64>,
}

impl RestartTiming {
    fn new(start_ms: u64, paused_elapsed: Option<u64>) -> Self {
        let elapsed = paused_elapsed.unwrap_or_else(|| now_ms().saturating_sub(start_ms));
        Self {
            start_ms,
            deadline: std::time::Instant::now()
                + Duration::from_millis(150_000u64.saturating_sub(elapsed)),
            paused_elapsed,
        }
    }

    fn expected(self) -> bool {
        self.paused_elapsed.map_or_else(
            || std::time::Instant::now() < self.deadline,
            |elapsed| elapsed < 150_000,
        )
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Presentation {
    pub(crate) fn begin(&self) -> bool {
        let mut view = self.0.lock().unwrap();
        if view.active {
            return false;
        }
        *view = View {
            id: Some(new_id()),
            phase: Some(UpdateState::Confirming),
            active: true,
            ..View::default()
        };
        true
    }

    pub(crate) fn transaction_id(&self) -> Option<String> {
        self.0.lock().unwrap().id.clone()
    }

    pub(crate) fn restore_id(&self, id: &str) {
        self.0.lock().unwrap().id = Some(id.to_owned());
    }

    pub(crate) fn wire_progress(&self, phase: UpdateState) -> Value {
        self.progress(phase.clone());
        let mut value = serde_json::to_value(phase).expect("serialize update progress");
        if let Some(id) = self.transaction_id() {
            value["transaction_id"] = json!(id);
        }
        value
    }

    pub(crate) fn set_consent(&self, value: Option<Value>) {
        self.0.lock().unwrap().consent = value;
    }

    pub(crate) fn progress(&self, phase: UpdateState) {
        let mut view = self.0.lock().unwrap();
        match &phase {
            UpdateState::StopDetail { .. } => return,
            UpdateState::Stopping
            | UpdateState::Installing
            | UpdateState::Relaunching
            | UpdateState::Reconciling => {
                if view.timing.is_none() && phase == UpdateState::Stopping {
                    view.timing = Some(RestartTiming::new(now_ms(), None));
                }
            }
            UpdateState::StartingDaemon
                if !matches!(view.phase, Some(UpdateState::Reconciling))
                    || view.timing.is_none() =>
            {
                view.timing = Some(RestartTiming::new(now_ms(), None));
            }
            UpdateState::StartingDaemon => {}
            UpdateState::WaitingPermission => {
                if let Some(window) = &mut view.timing {
                    let remaining = window
                        .deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis() as u64;
                    window
                        .paused_elapsed
                        .get_or_insert(150_000u64.saturating_sub(remaining));
                }
            }
            UpdateState::WaitingDaemon { since } => {
                let start = since
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let timing = view.timing;
                if let Some(elapsed) = timing.and_then(|t| t.paused_elapsed) {
                    view.timing = Some(RestartTiming::new(now_ms().saturating_sub(elapsed), None));
                } else if timing.is_none() {
                    view.timing = Some(RestartTiming::new(start, None));
                }
            }
            _ => view.timing = None,
        }
        view.phase = Some(phase);
    }

    pub(crate) fn restore_timing(&self, start_ms: u64, paused: Option<u64>) {
        self.0.lock().unwrap().timing = Some(RestartTiming::new(start_ms, paused));
    }

    pub(crate) fn finish(&self, error: Option<GuiError>) {
        let mut view = self.0.lock().unwrap();
        view.active = false;
        view.timing = None;
        view.consent = None;
        if let Some(error) = error {
            if matches!(
                view.phase,
                None | Some(
                    UpdateState::Reconciling
                        | UpdateState::StartingDaemon
                        | UpdateState::WaitingDaemon { .. }
                        | UpdateState::WaitingPermission
                )
            ) {
                view.phase = Some(UpdateState::DaemonFailed {
                    reason: error.reason().unwrap_or("update_failed").into(),
                });
            }
            view.error = Some(error);
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.0.lock().unwrap().active
    }
    pub(crate) fn expected_restart(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .timing
            .is_some_and(RestartTiming::expected)
    }
    pub(crate) fn can_update_skill(&self) -> bool {
        let view = self.0.lock().unwrap();
        !view.active && view.phase == Some(UpdateState::Done) && view.error.is_none()
    }
    pub(crate) fn clear_completed(&self) {
        let mut view = self.0.lock().unwrap();
        if !view.active {
            *view = View::default();
        }
    }

    pub(crate) fn snapshot(&self, ready: UpdateState, pending_id: Option<&str>) -> Value {
        let view = self.0.lock().unwrap();
        let phase = view.phase.as_ref().unwrap_or(&ready);
        let mut snapshot = serde_json::to_value(phase).expect("serialize update phase");
        if let Some(id) = pending_id.or(view.id.as_deref()) {
            snapshot["transaction_id"] = json!(id);
        }
        if let Some(error) = &view.error {
            snapshot["operation_error"] = json!(error);
        }
        if let Some(consent) = &view.consent {
            snapshot["schema_consent"] = consent.clone();
        }
        if let Some(timing) = view.timing {
            snapshot["start_requested_ms"] = json!(timing.start_ms);
            snapshot["permission_wait_elapsed_ms"] = json!(timing.paused_elapsed);
        }
        snapshot
    }
}

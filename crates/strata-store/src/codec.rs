//! Column conventions (store.md §4.4): ns timestamps, snake_case enum text,
//! UTF-8 paths, sequence-number bounds.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use strata_types::StrataError;

use crate::errors::{bad_row, invalid_field};
use crate::Result;

/// `SystemTime` → signed ns since the epoch; out of i64 range → `InvalidInput`.
pub(crate) fn ns(t: SystemTime, field: &str) -> Result<i64> {
    let out = || invalid_field(field, format!("{field} is outside the representable range (1677..2262)"));
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).map_err(|_| out()),
        Err(e) => {
            let n = i128::try_from(e.duration().as_nanos()).map_err(|_| out())?;
            i64::try_from(-n).map_err(|_| out())
        }
    }
}

pub(crate) fn opt_ns(t: Option<SystemTime>, field: &str) -> Result<Option<i64>> {
    t.map(|t| ns(t, field)).transpose()
}

pub(crate) fn time(n: i64) -> SystemTime {
    if n >= 0 {
        UNIX_EPOCH + Duration::from_nanos(n as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos(n.unsigned_abs())
    }
}

pub(crate) fn opt_time(n: Option<i64>) -> Option<SystemTime> {
    n.map(time)
}

pub(crate) fn now() -> SystemTime {
    SystemTime::now()
}

/// A path as UTF-8 text; anything else → `InvalidInput`.
pub(crate) fn path_text<'a>(p: &'a Path, field: &str) -> Result<&'a str> {
    p.to_str().ok_or_else(|| invalid_field(field, format!("{field} is not UTF-8: {}", p.display())))
}

/// A sequence number read back from a row: must be ≥ 1.
pub(crate) fn seq(v: i64, what: &str) -> Result<u64> {
    if v < 1 {
        return Err(bad_row(format!("{what} {v} is below 1")));
    }
    Ok(v as u64)
}

/// A u64 going into an INTEGER column.
pub(crate) fn int(v: u64, field: &str) -> Result<i64> {
    i64::try_from(v).map_err(|_| invalid_field(field, format!("{field} {v} exceeds i64::MAX")))
}

pub(crate) fn nonneg(v: i64, what: &str) -> Result<u64> {
    u64::try_from(v).map_err(|_| bad_row(format!("{what} {v} is negative")))
}

/// Re-validate an id read back from a row.
pub(crate) fn parse<T: TryFrom<String>>(s: String, what: &str) -> Result<T> {
    let shown = s.clone();
    T::try_from(s).map_err(|_| bad_row(format!("bad {what} in a row: {shown:?}")))
}

pub(crate) fn opt_parse<T: TryFrom<String>>(s: Option<String>, what: &str) -> Result<Option<T>> {
    s.map(|s| parse(s, what)).transpose()
}

pub(crate) fn pathbuf(s: String) -> PathBuf {
    PathBuf::from(s)
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T, field: &str) -> Result<String> {
    serde_json::to_string(v).map_err(|e| invalid_field(field, format!("{field} does not serialize: {e}")))
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned>(s: &str, what: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| bad_row(format!("bad JSON in {what}: {e}")))
}

/// Fixed enum ↔ text table.
macro_rules! text_enum {
    ($t:ty, $to:ident, $from:ident, { $($v:path => $s:literal),* $(,)? }) => {
        pub(crate) fn $to(v: $t) -> &'static str {
            match v { $($v => $s),* }
        }
        pub(crate) fn $from(s: &str) -> Result<$t> {
            match s { $($s => Ok($v),)* other => Err(bad_row(format!("unknown {} value {other:?}", stringify!($t)))) }
        }
    };
}

use crate::{
    AttemptKind, AttemptState, DepsOrigin, Health, Lifecycle, MoveState, ProposalStatus, PublishPhase,
    RefOp, RefOutcome,
};

text_enum!(Lifecycle, lifecycle_text, lifecycle_of, {
    Lifecycle::Creating => "creating", Lifecycle::Active => "active", Lifecycle::Removing => "removing",
    Lifecycle::Removed => "removed", Lifecycle::Unknown => "unknown",
});
text_enum!(Health, health_text, health_of, { Health::Ok => "ok", Health::Unknown => "unknown" });
text_enum!(MoveState, move_state_text, move_state_of, {
    MoveState::Preparing => "preparing", MoveState::Swapping => "swapping", MoveState::Done => "done",
    MoveState::Refused => "refused", MoveState::Abandoned => "abandoned", MoveState::Unknown => "unknown",
});
text_enum!(AttemptKind, attempt_kind_text, attempt_kind_of, { AttemptKind::Adopt => "adopt", AttemptKind::Build => "build" });
text_enum!(AttemptState, attempt_state_text, attempt_state_of, {
    AttemptState::Running => "running", AttemptState::Sealed => "sealed", AttemptState::Failed => "failed",
    AttemptState::Refused => "refused",
});
text_enum!(DepsOrigin, origin_text, origin_of, { DepsOrigin::Adopt => "adopt", DepsOrigin::Build => "build", DepsOrigin::Empty => "empty" });
text_enum!(ProposalStatus, proposal_status_text, proposal_status_of, {
    ProposalStatus::AwaitingPublish => "awaiting_publish", ProposalStatus::Pending => "pending",
    ProposalStatus::Applied => "applied", ProposalStatus::Superseded => "superseded",
});
text_enum!(PublishPhase, publish_phase_text, publish_phase_of, {
    PublishPhase::Running => "running", PublishPhase::Done => "done", PublishPhase::Unknown => "unknown",
});
text_enum!(RefOp, ref_op_text, ref_op_of, { RefOp::UpdateRef => "update_ref", RefOp::Push => "push" });
text_enum!(RefOutcome, ref_outcome_text, ref_outcome_of, {
    RefOutcome::Applied => "applied", RefOutcome::HeadMoved => "head_moved", RefOutcome::Pushed => "pushed",
    RefOutcome::Refused => "refused", RefOutcome::Unknown => "unknown",
});

pub(crate) fn _unused(_: StrataError) {}

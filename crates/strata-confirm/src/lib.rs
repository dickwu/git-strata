//! Human confirmation for git-strata (confirm.md).
//!
//! The daemon owns one [`Confirmer`]. [`Confirmer::require`] shows one LocalAuthentication
//! prompt (Touch ID, or the login password) for an [`Action`] and, when the human passes it,
//! mints a single-use [`ConfirmToken`] that lives only in this process.
//! [`Confirmer::validate`] redeems it. confirm reads no environment, no file and no TTY.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use strata_types::{Action, StrataError};

mod reason;

pub use reason::prompt_reason;

/// types.md §8 `Result<T>`.
pub type Result<T> = std::result::Result<T, StrataError>;

/// How long a token is valid, from the moment authentication passed to `validate`.
/// Age is the larger of the `Instant` age and the `CLOCK_MONOTONIC` age (§5.7).
pub const TOKEN_TTL: Duration = Duration::from_secs(30);

/// How long one prompt may stay open, on a clock that keeps running during sleep.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// After invalidating an expired prompt, how long to wait for the framework's late reply.
pub const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// How long the registry keeps a record after minting, so spent and expired tokens
/// report `used` and `expired` rather than `unknown` (§4.1).
pub const RECORD_RETENTION: Duration = Duration::from_secs(600);

/// Whether this crate was built with test seams. git-stratad refuses to start when true (C8).
pub const TEST_SEAMS: bool = cfg!(feature = "test-seams");

/// Something that can show a prompt (crate-private; production is LocalAuthentication).
pub(crate) trait Authenticator: Send + Sync {}

/// The two clocks of §5.7.
pub(crate) trait Clock: Send + Sync {}

#[allow(dead_code)]
pub(crate) struct Record {
    pub(crate) action: Action,
    pub(crate) issued: Instant,
    pub(crate) issued_mono_ns: u64,
    pub(crate) spent: bool,
}

/// The daemon's single issuer of confirm tokens. `Send + Sync`.
#[allow(dead_code)]
pub struct Confirmer {
    auth: Box<dyn Authenticator>,
    clock: Box<dyn Clock>,
    registry: Mutex<HashMap<u128, Record>>,
    gate: Mutex<()>,
    prompt_timeout: Duration,
    cancel_grace: Duration,
}

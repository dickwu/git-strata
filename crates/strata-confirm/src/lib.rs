//! Human confirmation for git-strata (confirm.md).
//!
//! The daemon owns one [`Confirmer`]. [`Confirmer::require`] shows one LocalAuthentication
//! prompt (Touch ID, or the login password) for an [`Action`](strata_types::Action) and, when
//! the human passes it, mints a single-use [`ConfirmToken`](strata_types::ConfirmToken) that
//! lives only in this process. [`Confirmer::validate`] redeems it. confirm reads no
//! environment, no file and no TTY (C4).

use std::time::Duration;

use strata_types::StrataError;

mod clock;
mod confirmer;
mod outcome;
mod reason;
mod registry;

#[cfg(feature = "test-seams")]
pub mod seams;

pub use confirmer::Confirmer;
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

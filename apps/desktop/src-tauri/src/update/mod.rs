//! Contracts for the desktop updater and bundled daemon lifecycle.

pub mod bundle;
pub mod cache;
pub mod check;
pub mod cli_link;
pub mod exec;
pub mod install;
pub mod manifest;
pub mod move_daemon;
pub mod restart;
pub mod rollback;
pub mod setup;
pub mod source;
pub mod state;
pub mod txn;

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime};

pub type UpdateFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Clock: Send + Sync {
    /// Wall clock for persisted timestamps and check scheduling.
    fn now(&self) -> SystemTime;
    /// Monotonic clock for active operation deadlines.
    fn monotonic(&self) -> Instant;
    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()>;
}

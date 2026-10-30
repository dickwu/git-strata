//! gui.md §3.2: the commands the front end invokes. P1 has reads only,
//! over existing daemon methods (§8.1); every one returns the daemon's
//! `data` as is (typed replies arrive with P2's summary methods). S0 adds
//! `legacy_agent` and `daemon_start` (service.rs), the only commands that
//! run a process. gui-approve adds `deps_approve` (deps.rs), the one
//! daemon write; the daemon itself asks the human (Touch ID / password).

pub mod deps;
pub mod health;
pub mod service;
pub mod sessions;
pub mod storage;

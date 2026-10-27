//! gui.md §3.2: the commands the front end invokes. P1 has reads only,
//! over existing daemon methods (§8.1); every one returns the daemon's
//! `data` as is (typed replies arrive with P2's summary methods).

pub mod deps;
pub mod health;
pub mod sessions;
pub mod storage;

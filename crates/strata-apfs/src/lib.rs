//! APFS primitives for git-strata (docs/modules/apfs.md).
//!
//! macOS only. Everything here talks to the kernel directly through libc:
//! `clonefile(2)` family, `openat` chains that never follow symlinks under a
//! root, `getattrlistbulk(2)`, and FSEvents. There is no copy fallback
//! anywhere in this crate (invariant A2).

use strata_types::StrataError;

#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod sys;
mod volume;

pub use error::{IoSide, classify_io_error};
pub use volume::{VolumeCaps, nearest_existing_ancestor, same_volume, volume_caps};

pub type Result<T> = std::result::Result<T, StrataError>;

pub const NAME_MAX_UTF16_UNITS: usize = 255;
pub const PATH_MAX_BYTES: usize = 1024;
pub const MAX_WALK_DEPTH: u32 = 128;
pub const DEFAULT_BULK_BUFFER_BYTES: usize = 64 * 1024;
pub const MIN_BULK_BUFFER_BYTES: usize = 1024;
pub const MAX_BULK_BUFFER_BYTES: usize = 4 * 1024 * 1024;
pub const WATCH_LATENCY_SECS: f64 = 0.05;
pub const GEN_COUNT_UNAVAILABLE: u64 = 0;
pub const MAX_REPORTED_STUCK: usize = 1000;

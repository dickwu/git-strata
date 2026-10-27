//! APFS primitives for git-strata (docs/modules/apfs.md).
//!
//! macOS only. Everything here talks to the kernel directly through libc:
//! `clonefile(2)` family, `openat` chains that never follow symlinks under a
//! root, `getattrlistbulk(2)`, and FSEvents. There is no copy fallback
//! anywhere in this crate (invariant A2).

use strata_types::StrataError;

mod attrs;
mod bulk;
mod clone;
mod clone_from;
mod remove_tree;
mod rooted;
mod hooks;
mod meta;
mod rooted_remove;
mod rooted_rename;
mod rooted_write;
mod sync;
#[allow(dead_code)]
mod error;
mod eintr;
#[allow(dead_code)]
mod sys;
mod volume;
mod walk;
mod usage;
mod casefold;
mod casefold_table;
mod watch;

pub use attrs::{Attrs, FileKind, attrs_of};
pub use clone::{clone_file, clone_tree};
pub use clone_from::{CloneKind, clone_from_rooted};
pub use remove_tree::{RemoveReport, Stuck, StuckReason, remove_tree_rooted};
pub use meta::{FileMeta, make_removable, set_file_meta};
pub use rooted::{open_root, open_rooted, open_rooted_file};
pub use rooted_remove::{RemoveKind, remove_rooted};
pub use rooted_rename::{RenameMode, rename_rooted};
pub use rooted_write::{clone_file_rooted, clone_tree_rooted, create_file_rooted, mkdir_rooted, symlink_rooted};
pub use sync::full_fsync;

pub(crate) type StrataErrorAlias = StrataError;

/// Thread-local test hooks (§3). Only with `feature = "test-seams"`.
#[cfg(feature = "test-seams")]
pub mod seams {
    pub use crate::hooks::{MetaChange, set_after_bulk_window, set_before_meta, set_before_open};
}
pub use error::{IoSide, classify_io_error};
pub use eintr::{EINTR_ATTEMPTS, retry_eintr, retry_eintr_n};
pub use casefold::{case_fold_eq, case_fold_key};
pub use watch::{WatchEvent, WatchOptions, Watcher, watch, watch_with};
pub use usage::{Usage, usage_of, usage_with};
pub use walk::{Walk, WalkOptions, list_dir, walk, walk_with};
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

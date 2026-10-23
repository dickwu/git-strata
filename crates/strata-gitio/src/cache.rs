//! In-process caches (§4.3). Dropping any of them only costs speed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::fsx::FileKey;
use crate::index::IndexInfo;

#[derive(Default)]
pub(crate) struct Cache {
    /// Index snapshots by index path, keyed by the stat of the configs read
    /// and of the index.
    pub index: HashMap<PathBuf, (Vec<Option<FileKey>>, IndexInfo)>,
    /// Shadow repositories known to belong to this repository.
    pub shadows: HashSet<PathBuf>,
}

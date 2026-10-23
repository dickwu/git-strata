//! In-process caches (§4.3). Dropping any of them only costs speed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::fsx::FileKey;
use crate::ignore::Rules;
use crate::index::IndexInfo;

#[derive(Default)]
pub(crate) struct Cache {
    /// Index snapshots by index path, keyed by the stat of the configs read
    /// and of the index.
    pub index: HashMap<PathBuf, (Vec<Option<FileKey>>, IndexInfo)>,
    /// Shadow repositories known to belong to this repository.
    pub shadows: HashSet<PathBuf>,
    /// Parsed ignore-rule files by path, keyed by their stat.
    pub rules: HashMap<PathBuf, (FileKey, Arc<Rules>)>,
    /// The global excludes file per common dir (`None` = there is none).
    pub global_excludes: HashMap<PathBuf, Option<PathBuf>>,
}

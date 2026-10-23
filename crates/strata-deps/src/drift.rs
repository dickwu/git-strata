//! §5.8: drift as an install-input hash comparison.

use std::path::Path;

use serde::{Deserialize, Serialize};
use strata_gitio::Git;
use strata_types::{DepsSet, DepsSetId, RepoConfig};

use crate::Result;
use crate::inputs::inputs_hash;

/// Drift (design §4): the install-input hash found in a directory differs
/// from the one its pinned deps set records. It is data, not an error; the
/// `Drift` code is only for refusals (§6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    /// The pinned deps set.
    pub pinned: DepsSetId,
    /// The install-input hash the pinned set records.
    pub pinned_inputs: String,
    /// The install-input hash found in the workspace (or main tip export).
    pub found_inputs: String,
}

/// §5.8: compare install inputs only; deps dir contents, toolchain, platform
/// and the deps configuration never count.
pub fn drift(cfg: &RepoConfig, git: &Git, ws: &Path, pinned: &DepsSet) -> Result<Option<Drift>> {
    let found = inputs_hash(cfg, git, ws)?;
    if found == pinned.inputs_hash {
        return Ok(None);
    }
    Ok(Some(Drift {
        pinned: pinned.id.clone(),
        pinned_inputs: pinned.inputs_hash.clone(),
        found_inputs: found,
    }))
}

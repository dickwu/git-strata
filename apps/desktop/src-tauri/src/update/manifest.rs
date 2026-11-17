//! Release information shown before download (version-upgrade design §3.1).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTarget {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    /// Advisory schema for the first confirmation dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_schema: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_updatable_from: Option<String>,
}

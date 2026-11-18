//! Release information shown before download (version-upgrade design §3.1).

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_types::Code;
use tauri_plugin_updater::{RemoteRelease, RemoteReleaseInner};

use super::source::UpdateCandidate;
use crate::error::GuiError;

pub const PLATFORM: &str = "darwin-aarch64";

pub(crate) fn dmg_url(version: &str) -> String {
    format!(
        "https://github.com/dickwu/git-strata/releases/download/v{version}/Git-Strata_{version}_arm64.dmg"
    )
}

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
    /// The running App is below the manifest's minimum for in-app updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_download_url: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Manifest {
    UpToDate,
    NoPlatform,
    Available(UpdateTarget),
}

#[derive(Default, Deserialize)]
struct AppMetadata {
    notes_url: Option<String>,
    store_schema: Option<u32>,
    min_updatable_from: Option<String>,
}

pub(crate) fn malformed_manifest() -> GuiError {
    GuiError::new(
        Code::InvalidInput,
        "The update manifest is malformed",
        json!({ "reason": "malformed_update_manifest" }),
    )
}

pub(crate) fn read(candidate: &UpdateCandidate, installed: &str) -> Result<Manifest, GuiError> {
    let release = static_release(&candidate.raw_json)?;
    let installed = installed.parse().map_err(|_| malformed_manifest())?;
    if release.version.to_string() != candidate.version {
        return Err(malformed_manifest());
    }
    if !release.version.pre.is_empty() || release.version.cmp_precedence(&installed).is_le() {
        return Ok(Manifest::UpToDate);
    }
    let RemoteReleaseInner::Static { platforms } = release.data else {
        return Err(malformed_manifest());
    };
    let Some(platform) = platforms.get(PLATFORM) else {
        return Ok(Manifest::NoPlatform);
    };
    if platform.signature.trim().is_empty() || platform.url.scheme() != "https" {
        return Err(malformed_manifest());
    }

    let metadata: AppMetadata = candidate
        .raw_json
        .get("git_strata")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| malformed_manifest())?
        .unwrap_or_default();
    let manual_download_url = match &metadata.min_updatable_from {
        Some(floor) => {
            let minimum = floor.parse().map_err(|_| malformed_manifest())?;
            installed
                .cmp_precedence(&minimum)
                .is_lt()
                .then(|| dmg_url(&candidate.version))
        }
        None => None,
    };
    let notes_url = metadata
        .notes_url
        .filter(|url| tauri::Url::parse(url).is_ok_and(|parsed| parsed.scheme() == "https"));
    Ok(Manifest::Available(UpdateTarget {
        version: candidate.version.clone(),
        notes: candidate.notes.clone(),
        notes_url,
        store_schema: metadata.store_schema,
        min_updatable_from: metadata.min_updatable_from,
        manual_download_url,
    }))
}

pub(crate) fn static_release(raw: &Value) -> Result<RemoteRelease, GuiError> {
    serde_json::from_value(raw.clone()).map_err(|_| malformed_manifest())
}

pub(crate) fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    // Reuse the updater's semver parser without adding a second dependency.
    let parsed = static_release(&json!({ "version": left, "platforms": {} })).ok()?;
    Some(parsed.version.cmp_precedence(&right.parse().ok()?))
}

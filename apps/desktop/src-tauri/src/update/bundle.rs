//! Bundle inspection, installation and rollback file operations.

use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::Code;
use tauri_plugin_updater::Error::PreviousAppNotRestored;

use super::source::VerifiedUpdate;
use super::txn::Txn;
use crate::error::GuiError;

/// Release metadata read from the downloaded bundle's Info.plist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleInfo {
    pub version: String,
    pub store_schema: u32,
    pub downgrade_floor: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleIdentity {
    device: u64,
    inode: u64,
}

pub enum RestoreDecision {
    AlreadyRestored,
    /// None means that the guard observed an absent destination.
    Swap {
        expected: Option<BundleIdentity>,
    },
}

pub trait BundleOps: Send + Sync {
    /// Extract into the caller's same-volume staging directory and return the app path.
    fn extract(&self, bytes: &[u8], staging_dir: &Path) -> Result<PathBuf, GuiError>;
    /// Use NoSuchPath for confirmed absence and Corrupt for invalid bundle metadata.
    fn read_info_plist(&self, bundle: &Path) -> Result<BundleInfo, GuiError>;
    /// Identify the bundle's directory entry without following a leaf symlink.
    fn bundle_identity(&self, _bundle: &Path) -> Result<Option<BundleIdentity>, GuiError> {
        Err(GuiError::internal("Bundle identity is unavailable"))
    }
    fn zip_backup(&self, bundle: &Path, zip: &Path) -> Result<(), GuiError>;
    /// Check the installer's own staging location before draining the daemon.
    fn check_install_staging(&self) -> Result<(), GuiError> {
        Ok(())
    }
    /// Swap the installed app. Preserve the plugin's PreviousAppNotRestored path for recovery.
    fn install(&self, update: VerifiedUpdate) -> tauri_plugin_updater::Result<()>;
    /// Verify codesign and the version encoded in the zip name before and after putting it back.
    fn restore_zip(&self, zip: &Path, bundle: &Path) -> Result<(), GuiError>;
    /// Pass the verified staged bundle's metadata to the guard before the swap.
    fn restore_zip_if(
        &self,
        _zip: &Path,
        _bundle: &Path,
        _decide: &dyn Fn(&BundleInfo) -> Result<RestoreDecision, GuiError>,
    ) -> Result<(), GuiError> {
        Err(GuiError::internal(
            "Guarded bundle restoration is unavailable",
        ))
    }
    /// Return CheckFailed with reason bad_codesign only for a conclusive rejection.
    fn codesign_verify(&self, bundle: &Path) -> Result<(), GuiError>;

    /// Verify and put back the plugin's kept copy while the daemon is fenced.
    /// Implementations without this operation let recovery use the zip.
    fn restore_kept(&self, _kept: &Path, _bundle: &Path, _txn: &Txn) -> Result<(), GuiError> {
        Err(bundle_error(
            "kept_unavailable",
            "The updater's kept copy is unavailable",
        ))
    }
}

/// The release channel and CFBundleShortVersionString use numeric X.Y.Z versions.
pub(super) fn release_version(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let mut parsed = [0; 3];
    for part in &mut parsed {
        let digits = parts.next()?;
        if !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return None;
        }
        *part = digits.parse().ok()?;
    }
    parts.next().is_none().then_some(parsed)
}

fn backup_name(from: &str) -> Result<String, GuiError> {
    release_version(from)
        .ok_or_else(|| bundle_error("bundle_version", "Invalid rollback version"))?;
    Ok(format!("GIT-STRATA-{from}.zip"))
}

fn zip_version(zip: &Path) -> Option<&str> {
    let version = zip
        .file_name()?
        .to_str()?
        .strip_prefix("GIT-STRATA-")?
        .strip_suffix(".zip")?;
    release_version(version)?;
    Some(version)
}

fn bundle_error(reason: &str, message: impl Into<String>) -> GuiError {
    GuiError::new(Code::InvalidInput, message, json!({ "reason": reason }))
}

pub(super) enum ProbeError {
    MissingOrInvalid {
        error: GuiError,
        identity: Option<BundleIdentity>,
    },
    Uncertain(GuiError),
}

impl ProbeError {
    pub(super) fn into_error(self) -> GuiError {
        match self {
            Self::MissingOrInvalid { error, .. } => error,
            Self::Uncertain(error) => GuiError::new(
                Code::Unknown,
                "Could not verify the app bundle.",
                json!({ "reason": "bundle_verification_uncertain", "cause": error }),
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct VerifiedBundle {
    pub(super) info: BundleInfo,
    pub(super) identity: BundleIdentity,
}

fn bundle_changed() -> ProbeError {
    ProbeError::Uncertain(bundle_error(
        "bundle_changed",
        "The app changed while verifying the bundle",
    ))
}

fn confirm_identity(
    ops: &dyn BundleOps,
    bundle: &Path,
    expected: Option<BundleIdentity>,
) -> Result<(), ProbeError> {
    let current = ops.bundle_identity(bundle).map_err(ProbeError::Uncertain)?;
    if current != expected {
        return Err(bundle_changed());
    }
    Ok(())
}

pub(super) fn probe_bundle(
    ops: &dyn BundleOps,
    bundle: &Path,
) -> Result<VerifiedBundle, ProbeError> {
    let identity = ops.bundle_identity(bundle).map_err(ProbeError::Uncertain)?;
    let info = match ops.read_info_plist(bundle) {
        Ok(info) => info,
        Err(error)
            if [Code::NoSuchPath.as_str(), Code::Corrupt.as_str()]
                .contains(&error.code.as_str()) =>
        {
            confirm_identity(ops, bundle, identity)?;
            return Err(ProbeError::MissingOrInvalid { error, identity });
        }
        Err(error) => return Err(ProbeError::Uncertain(error)),
    };
    let identity = identity.ok_or_else(bundle_changed)?;
    let signature = ops.codesign_verify(bundle);
    // The verifier resolves the path separately. A replacement during the check
    // can have an identical plist, so compare both identity and release metadata.
    let current = ops.read_info_plist(bundle).map_err(ProbeError::Uncertain)?;
    confirm_identity(ops, bundle, Some(identity))?;
    if current != info {
        return Err(bundle_changed());
    }
    signature.map_err(|error| {
        if error.code == Code::CheckFailed.as_str() && error.reason() == Some("bad_codesign") {
            ProbeError::MissingOrInvalid {
                error,
                identity: Some(identity),
            }
        } else {
            ProbeError::Uncertain(error)
        }
    })?;
    Ok(VerifiedBundle { info, identity })
}

fn verify_version(
    ops: &dyn BundleOps,
    bundle: &Path,
    expected: &str,
) -> Result<VerifiedBundle, GuiError> {
    let observed = probe_bundle(ops, bundle).map_err(ProbeError::into_error)?;
    let version = &observed.info.version;
    if version != expected {
        return Err(bundle_error(
            "bundle_version",
            format!("Expected bundle version {expected}, found {version}"),
        ));
    }
    Ok(observed)
}

/// Inspect exactly the archive that the update source verified. The returned
/// schema and floor are authoritative. Only a newer, matching bundle advances.
pub fn inspect_download(
    ops: &dyn BundleOps,
    update: &VerifiedUpdate,
    staging: &Path,
    manifest_version: &str,
    running_version: &str,
) -> Result<BundleInfo, GuiError> {
    ops.check_install_staging()?;
    let bundle = ops.extract(&update.bytes, staging)?;
    let info = ops.read_info_plist(&bundle)?;
    let target = release_version(&info.version);
    let running = release_version(running_version);
    if info.version != manifest_version || target.zip(running).is_none_or(|(new, old)| new <= old) {
        return Err(bundle_error(
            "bundle_version",
            format!(
                "Bundle version {} must match manifest {manifest_version} and be newer than {running_version}",
                info.version
            ),
        ));
    }
    ops.codesign_verify(&bundle)?;
    Ok(info)
}

#[derive(Debug)]
pub enum InstallOutcome {
    Installed,
    Unchanged,
    Restored,
    /// Bundle identity or recovery could not be verified. Keep the fence and transaction.
    RestoreFailed(GuiError),
}

#[derive(Debug)]
pub struct InstallReport {
    pub outcome: InstallOutcome,
    pub installer_error: Option<tauri_plugin_updater::Error>,
    /// Record in diagnostics or the transaction if the plugin's path remains.
    pub retained_app: Option<PathBuf>,
}

/// Design §4.2 step 8. The caller must keep the job unloaded and disabled
/// through installation and recovery. Only a verified outcome permits a start.
pub fn install_and_recover(
    ops: &dyn BundleOps,
    update: VerifiedUpdate,
    bundle: &Path,
    txn: &Txn,
) -> InstallReport {
    // The drain can outlast a replacement by another installer. Recheck under
    // the fence before handing the destination to the updater plugin.
    let predecessor = probe_bundle(ops, bundle)
        .map_err(ProbeError::into_error)
        .and_then(|app| {
            if app.info.version != txn.from {
                return Err(unexpected_version(&app.info.version));
            }
            verify_predecessor_schema(&app.info, txn)
        });
    if let Err(error) = predecessor {
        return InstallReport {
            outcome: InstallOutcome::RestoreFailed(error),
            installer_error: None,
            retained_app: None,
        };
    }
    let installer_error = ops.install(update).err();
    let kept = match &installer_error {
        Some(PreviousAppNotRestored(path)) => Some(path.as_path()),
        _ => None,
    };
    let outcome = match probe_bundle(ops, bundle) {
        Ok(app) if app.info.version == txn.from => {
            match verify_predecessor_schema(&app.info, txn) {
                Ok(()) => InstallOutcome::Unchanged,
                Err(error) => InstallOutcome::RestoreFailed(error),
            }
        }
        Ok(app) => match verify_target_info(&app.info, txn) {
            Ok(()) => InstallOutcome::Installed,
            Err(error) => InstallOutcome::RestoreFailed(error),
        },
        Err(error @ ProbeError::Uncertain(_)) => InstallOutcome::RestoreFailed(error.into_error()),
        Err(ProbeError::MissingOrInvalid { .. }) => match recover_install(ops, bundle, txn, kept) {
            Ok(()) => InstallOutcome::Restored,
            Err(error) => InstallOutcome::RestoreFailed(error),
        },
    };
    let retained_app = kept
        .filter(|path| {
            !std::fs::symlink_metadata(path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        })
        .map(Path::to_path_buf);
    InstallReport {
        outcome,
        installer_error,
        retained_app,
    }
}

pub(super) fn unexpected_version(version: &str) -> GuiError {
    bundle_error(
        "unexpected_bundle_version",
        format!("The installed bundle is an unexpected version ({version})."),
    )
}

pub(super) fn verify_target_info(info: &BundleInfo, txn: &Txn) -> Result<(), GuiError> {
    if info.version != txn.to {
        return Err(unexpected_version(&info.version));
    }
    if info.store_schema != txn.to_schema || info.downgrade_floor != txn.to_floor {
        return Err(bundle_error(
            "unexpected_bundle_metadata",
            format!(
                "Version {} has schema {} and downgrade floor {}. Expected schema {} and floor {}.",
                info.version, info.store_schema, info.downgrade_floor, txn.to_schema, txn.to_floor
            ),
        ));
    }
    Ok(())
}

pub(super) fn verify_predecessor_schema(info: &BundleInfo, txn: &Txn) -> Result<(), GuiError> {
    if info.store_schema != txn.from_schema {
        return Err(bundle_error(
            "unexpected_bundle_metadata",
            format!(
                "Version {} has schema {}. Expected schema {}.",
                info.version, info.store_schema, txn.from_schema,
            ),
        ));
    }
    Ok(())
}

fn recovery_decision(
    ops: &dyn BundleOps,
    bundle: &Path,
    txn: &Txn,
) -> Result<RestoreDecision, GuiError> {
    match probe_bundle(ops, bundle) {
        Ok(app) if app.info.version == txn.from => {
            verify_predecessor_schema(&app.info, txn)?;
            Ok(RestoreDecision::AlreadyRestored)
        }
        Ok(app) => Err(unexpected_version(&app.info.version)),
        Err(ProbeError::MissingOrInvalid { identity, .. }) => {
            Ok(RestoreDecision::Swap { expected: identity })
        }
        Err(error) => Err(error.into_error()),
    }
}

fn recover_install(
    ops: &dyn BundleOps,
    bundle: &Path,
    txn: &Txn,
    kept: Option<&Path>,
) -> Result<(), GuiError> {
    let kept_error = match kept {
        Some(path) => match ops.restore_kept(path, bundle, txn) {
            Ok(()) => return Ok(()),
            Err(error) => match error.reason() {
                Some(
                    "bundle_verification_uncertain"
                    | "unexpected_bundle_version"
                    | "unexpected_bundle_metadata"
                    | "restore_verification_failed",
                ) => {
                    return Err(error);
                }
                _ => Some(error),
            },
        },
        None => None,
    };
    restore_previous(ops, bundle, txn, &|| recovery_decision(ops, bundle, txn))
    .map_err(|zip_error| match kept_error {
        Some(kept_error) => GuiError::new(
            Code::Unknown,
            format!(
                "Could not restore the kept app ({kept_error}) or the rollback zip ({zip_error})."
            ),
            json!({ "reason": "restore_failed", "kept": kept_error, "zip": zip_error }),
        ),
        None => zip_error,
    })
}

/// The same restore used by recovery and explicit rollback.
pub(super) fn restore_previous(
    ops: &dyn BundleOps,
    bundle: &Path,
    txn: &Txn,
    decide: &dyn Fn() -> Result<RestoreDecision, GuiError>,
) -> Result<(), GuiError> {
    if zip_version(&txn.rollback_zip) != Some(txn.from.as_str()) {
        return Err(bundle_error(
            "rollback_zip_version",
            "The rollback zip does not match the recorded previous version",
        ));
    }
    ops.restore_zip_if(&txn.rollback_zip, bundle, &|staged| {
        verify_predecessor_schema(staged, txn)?;
        decide()
    })
}

#[cfg(target_os = "macos")]
pub use macos::MacBundleOps;

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
    use std::fs::{self, File};
    use std::io::{self, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Component;
    use std::ptr::{self, NonNull};

    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSData, NSString};

    use super::*;
    use crate::service::scope::anchored;
    use crate::update::exec::Operation;

    /// All filesystem locations are injected. `run` executes only the fixed
    /// operations from `update::exec`, using the verified login home.
    pub struct MacBundleOps<R> {
        installed_bundle: PathBuf,
        previous_dir: PathBuf,
        protected_roots: Vec<PathBuf>,
        temp_root: PathBuf,
        #[cfg(test)]
        pub(super) plugin_temp_root: Option<PathBuf>,
        run: R,
    }

    impl<R> MacBundleOps<R>
    where
        R: for<'a> Fn(Operation<'a>) -> Result<(), GuiError> + Send + Sync,
    {
        pub fn new(
            installed_bundle: PathBuf,
            app_support: &Path,
            data_root: PathBuf,
            managed_checkouts: Vec<PathBuf>,
            temp_root: PathBuf,
            run: R,
        ) -> Result<Self, GuiError> {
            let protected_roots: Vec<_> = std::iter::once(data_root)
                .chain(managed_checkouts)
                .collect();
            if protected_roots.iter().any(|root| !absolute_path(root)) {
                return Err(bundle_error(
                    "bundle_path",
                    "The configured data root and checkout paths must be absolute and contain no parent components",
                ));
            }
            let ops = Self {
                installed_bundle,
                previous_dir: app_support.join("previous"),
                protected_roots,
                temp_root,
                #[cfg(test)]
                plugin_temp_root: None,
                run,
            };
            if ops.installed_bundle.extension() != Some(OsStr::new("app")) {
                return Err(bundle_error(
                    "bundle_path",
                    "The installed path must name an app bundle",
                ));
            }
            ops.outside_protected_roots(&ops.installed_bundle)?;
            ops.outside_protected_roots(&ops.previous_dir)?;
            Ok(ops)
        }

        pub fn inspect_download(
            &self,
            update: &VerifiedUpdate,
            manifest_version: &str,
            running_version: &str,
        ) -> Result<BundleInfo, GuiError> {
            let staging = self.stage_on_install_volume()?;
            super::inspect_download(self, update, &staging.0, manifest_version, running_version)
        }

        /// Call while serializing updates, before persisting a new transaction.
        pub fn save_previous(
            &self,
            from: &str,
            pending: Option<&Txn>,
        ) -> Result<PathBuf, GuiError> {
            if pending.is_some() {
                return Err(GuiError::new(
                    Code::Busy,
                    "An unresolved update owns the rollback zip",
                    json!({ "reason": "rollback_zip_owned" }),
                ));
            }
            let zip = self.previous_dir.join(backup_name(from)?);
            self.zip_backup(&self.installed_bundle, &zip)?;
            Ok(zip)
        }

        fn outside_protected_roots(&self, path: &Path) -> Result<(), GuiError> {
            let protected = |path: &Path| {
                let resolved = anchored(path);
                self.protected_roots
                    .iter()
                    .any(|root| path.starts_with(root) || resolved.starts_with(anchored(root)))
            };
            if !absolute_path(path) || protected(path) {
                return Err(bundle_error(
                    "bundle_path",
                    format!(
                        "Bundle operation requires a path outside the data root and managed checkouts: {}",
                        path.display()
                    ),
                ));
            }
            Ok(())
        }

        fn staging_parent(&self, path: &Path) -> Result<PathBuf, GuiError> {
            self.outside_protected_roots(path)?;
            let resolved =
                fs::canonicalize(path).map_err(|e| fs_error("bundle_staging", path, e))?;
            self.outside_protected_roots(&resolved)?;
            require_dir(&resolved)?;
            Ok(resolved)
        }

        fn install_parent(&self) -> Result<&Path, GuiError> {
            self.outside_protected_roots(&self.installed_bundle)?;
            let parent = self.installed_bundle.parent().ok_or_else(|| {
                bundle_error(
                    "bundle_path",
                    "The installed bundle has no parent directory",
                )
            })?;
            self.outside_protected_roots(parent)?;
            Ok(parent)
        }

        fn install_device(&self) -> Result<u64, GuiError> {
            let parent = self.install_parent()?;
            let dev = fs::metadata(parent)
                .map_err(|e| fs_error("bundle_path", parent, e))?
                .dev();
            match fs::symlink_metadata(&self.installed_bundle) {
                Ok(meta) if meta.dev() == dev => Ok(dev),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(dev),
                Err(error) => Err(fs_error("bundle_path", &self.installed_bundle, error)),
                Ok(_) => Err(bundle_error(
                    "bundle_volume",
                    "The app and its parent are on different volumes",
                )),
            }
        }

        fn stage_on_install_volume(&self) -> Result<Scratch, GuiError> {
            let dev = self.install_device()?;
            let mut failure = None;
            for parent in [self.temp_root.as_path(), self.install_parent()?] {
                if let Ok(parent) = self.staging_parent(parent)
                    && fs::metadata(&parent).is_ok_and(|m| m.dev() == dev)
                {
                    match Scratch::new(&parent) {
                        Ok(staging) => return Ok(staging),
                        Err(error) => failure = Some(fs_error("bundle_staging", &parent, error)),
                    }
                }
            }
            Err(failure.unwrap_or_else(|| {
                bundle_error(
                    "bundle_volume",
                    "No staging directory on the app's volume outside the data root and managed checkouts",
                )
            }))
        }

        fn plugin_temp_root(&self) -> PathBuf {
            #[cfg(test)]
            if let Some(path) = &self.plugin_temp_root {
                return path.clone();
            }
            std::env::temp_dir()
        }

        fn check_plugin_temp_root(&self, system_temp: &Path) -> Result<(), GuiError> {
            let dev = self.install_device()?;
            // Match the pinned updater's selection order. Its backup and extraction
            // both use the first matching device, even when that path is protected.
            let parent = [system_temp, self.install_parent()?]
                .into_iter()
                .find(|dir| fs::metadata(dir).is_ok_and(|m| m.dev() == dev))
                .ok_or_else(|| {
                    bundle_error(
                        "bundle_volume",
                        "The updater has no temp directory on the app's volume",
                    )
                })?;
            self.staging_parent(parent)?;
            Ok(())
        }

        fn backup_version<'a>(&self, zip: &'a Path) -> Result<&'a str, GuiError> {
            self.outside_protected_roots(zip)?;
            if zip.parent() != Some(self.previous_dir.as_path()) {
                return Err(bundle_error(
                    "rollback_zip_path",
                    "The rollback zip must be in the app's previous directory",
                ));
            }
            zip_version(zip)
                .ok_or_else(|| bundle_error("rollback_zip_path", "Invalid rollback zip name"))
        }

        fn check_destination(&self, bundle: &Path) -> Result<(), GuiError> {
            if bundle != self.installed_bundle {
                return Err(bundle_error(
                    "bundle_path",
                    "The bundle operation must target the installed app",
                ));
            }
            self.outside_protected_roots(bundle)
        }

        fn unpack_backup(&self, zip: &Path, staging: &Path) -> Result<PathBuf, GuiError> {
            (self.run)(Operation::UnzipBackup { zip, staging })?;
            let name = self
                .installed_bundle
                .file_name()
                .ok_or_else(|| bundle_error("bundle_path", "The installed bundle has no name"))?;
            Ok(staging.join(name))
        }
    }

    impl<R> BundleOps for MacBundleOps<R>
    where
        R: for<'a> Fn(Operation<'a>) -> Result<(), GuiError> + Send + Sync,
    {
        fn extract(&self, bytes: &[u8], staging_dir: &Path) -> Result<PathBuf, GuiError> {
            let staging = self.staging_parent(staging_dir)?;
            let meta =
                fs::metadata(&staging).map_err(|e| fs_error("bundle_staging", &staging, e))?;
            if meta.dev() != self.install_device()? {
                return Err(bundle_error(
                    "bundle_volume",
                    "The extraction directory must be on the app's volume",
                ));
            }
            let mut entries =
                fs::read_dir(&staging).map_err(|e| fs_error("bundle_staging", &staging, e))?;
            if entries.next().is_some() {
                return Err(bundle_error(
                    "bundle_staging",
                    "The extraction directory must be empty",
                ));
            }
            let name = self
                .installed_bundle
                .file_name()
                .ok_or_else(|| bundle_error("bundle_path", "The installed bundle has no name"))?;
            extract_tar_gz(bytes, &staging, name)?;
            let bundle = staging.join(name);
            require_dir(&bundle)?;
            Ok(bundle)
        }

        fn read_info_plist(&self, bundle: &Path) -> Result<BundleInfo, GuiError> {
            self.outside_protected_roots(bundle)?;
            let contents = bundle.join("Contents");
            require_dir(bundle)?;
            require_dir(&contents)?;
            let path = contents.join("Info.plist");
            let file = regular_file(&path, "bundle_info_plist")?;
            let mut bytes = Vec::new();
            const MAX_PLIST_BYTES: u64 = 1024 * 1024;
            file.take(MAX_PLIST_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| fs_error("bundle_info_plist", &path, e))?;
            if bytes.len() as u64 > MAX_PLIST_BYTES {
                return Err(corrupt_plist("Info.plist exceeds 1 MiB"));
            }
            autoreleasepool(|_| Plist::parse(&bytes)?.bundle_info())
        }

        fn bundle_identity(&self, bundle: &Path) -> Result<Option<BundleIdentity>, GuiError> {
            self.outside_protected_roots(bundle)?;
            match fs::symlink_metadata(bundle) {
                Ok(meta) => Ok(Some(BundleIdentity {
                    device: meta.dev(),
                    inode: meta.ino(),
                })),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(fs_error("bundle_path", bundle, error)),
            }
        }

        fn zip_backup(&self, bundle: &Path, zip: &Path) -> Result<(), GuiError> {
            self.check_destination(bundle)?;
            let version = self.backup_version(zip)?;
            let reason = "rollback_backup";
            verify_version(self, bundle, version)?;
            fs::create_dir_all(&self.previous_dir)
                .map_err(|e| fs_error(reason, &self.previous_dir, e))?;
            let scratch = Scratch::new(&self.staging_parent(&self.previous_dir)?)
                .map_err(|e| fs_error(reason, &self.previous_dir, e))?;
            let staged = scratch.0.join("backup.zip");
            (self.run)(Operation::ZipBackup {
                bundle,
                zip: &staged,
            })?;
            let zip_file = regular_file(&staged, reason)?;
            let verify_dir = scratch.0.join("verify");
            fs::create_dir(&verify_dir).map_err(|e| fs_error(reason, &verify_dir, e))?;
            let archived_app = self.unpack_backup(&staged, &verify_dir)?;
            verify_version(self, &archived_app, version)?;
            sync_backup(&zip_file).map_err(|e| fs_error(reason, &staged, e))?;
            fs::rename(&staged, zip).map_err(|e| fs_error(reason, zip, e))?;

            // Discard stale app backups only after the replacement is ready.
            for entry in fs::read_dir(&self.previous_dir)
                .map_err(|e| fs_error(reason, &self.previous_dir, e))?
            {
                let path = entry
                    .map_err(|e| fs_error(reason, &self.previous_dir, e))?
                    .path();
                if path != zip && zip_version(&path).is_some() {
                    fs::remove_file(&path).map_err(|e| fs_error(reason, &path, e))?;
                }
            }
            File::open(&self.previous_dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|e| fs_error(reason, &self.previous_dir, e))?;
            Ok(())
        }

        fn check_install_staging(&self) -> Result<(), GuiError> {
            self.check_plugin_temp_root(&self.plugin_temp_root())
        }

        fn install(&self, update: VerifiedUpdate) -> tauri_plugin_updater::Result<()> {
            self.check_install_staging().map_err(io::Error::other)?;
            (update.installer)(&update.bytes)
        }

        fn restore_zip(&self, zip: &Path, bundle: &Path) -> Result<(), GuiError> {
            self.restore_zip_if(zip, bundle, &|_| {
                Ok(RestoreDecision::Swap {
                    expected: self.bundle_identity(bundle)?,
                })
            })
        }

        fn restore_zip_if(
            &self,
            zip: &Path,
            bundle: &Path,
            decide: &dyn Fn(&BundleInfo) -> Result<RestoreDecision, GuiError>,
        ) -> Result<(), GuiError> {
            self.check_destination(bundle)?;
            let expected = self.backup_version(zip)?;
            regular_file(zip, "rollback_zip_path")?;
            let stage = self.stage_on_install_volume()?;
            let staged = self.unpack_backup(zip, &stage.0)?;
            let verified = verify_version(self, &staged, expected)?;
            let RestoreDecision::Swap { expected } = decide(&verified.info)? else {
                return Ok(());
            };
            activate_bundle(&staged, bundle, expected)?;
            if let Err(error) = verify_swap(self, &staged, bundle, &verified, expected) {
                let kept = stage.keep();
                return Err(GuiError::new(
                    Code::Unknown,
                    format!("The restored app could not be verified: {error}"),
                    json!({ "reason": "restore_failed", "staging": kept, "cause": error }),
                ));
            }
            Ok(())
        }

        fn restore_kept(&self, kept: &Path, bundle: &Path, txn: &Txn) -> Result<(), GuiError> {
            self.check_destination(bundle)?;
            let verified = verify_version(self, kept, &txn.from)
                .and_then(|app| {
                    verify_predecessor_schema(&app.info, txn)?;
                    Ok(app)
                })
                .map_err(|error| {
                    GuiError::new(
                        Code::CheckFailed,
                        format!("Could not verify the updater's kept copy: {error}"),
                        json!({ "reason": "kept_unavailable", "cause": error }),
                    )
                })?;
            let RestoreDecision::Swap { expected } = recovery_decision(self, bundle, txn)? else {
                return Ok(());
            };
            activate_bundle(kept, bundle, expected)?;
            verify_swap(self, kept, bundle, &verified, expected).map_err(|error| {
                GuiError::new(
                    Code::Unknown,
                    format!("Could not verify the app restored from the kept copy: {error}"),
                    json!({ "reason": "restore_verification_failed", "kept": kept, "cause": error }),
                )
            })
        }

        fn codesign_verify(&self, bundle: &Path) -> Result<(), GuiError> {
            self.outside_protected_roots(bundle)?;
            require_dir(bundle)?;
            (self.run)(Operation::VerifyCodesign { bundle })
        }
    }

    fn absolute_path(path: &Path) -> bool {
        path.is_absolute() && !path.components().any(|c| c == Component::ParentDir)
    }

    fn fs_error(reason: &str, path: &Path, err: io::Error) -> GuiError {
        let code = match err.kind() {
            io::ErrorKind::NotFound => Code::NoSuchPath,
            _ => Code::Unknown,
        };
        GuiError::new(
            code,
            format!("{}: {err}", path.display()),
            json!({ "reason": reason }),
        )
    }

    fn sync_backup(file: &File) -> io::Result<()> {
        loop {
            // SAFETY: the file owns a live descriptor and F_FULLFSYNC takes no argument.
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return match error.raw_os_error() {
                Some(libc::ENOTSUP | libc::EOPNOTSUPP) => file.sync_all(),
                _ => Err(error),
            };
        }
    }

    fn require_dir(path: &Path) -> Result<(), GuiError> {
        let meta = fs::symlink_metadata(path).map_err(|e| fs_error("bundle_path", path, e))?;
        if !meta.is_dir() {
            return Err(GuiError::new(
                Code::Corrupt,
                format!("Expected a real directory at {}", path.display()),
                json!({ "reason": "bundle_path" }),
            ));
        }
        Ok(())
    }

    fn regular_file(path: &Path, reason: &str) -> Result<File, GuiError> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| fs_error(reason, path, e))?;
        if !file
            .metadata()
            .map_err(|e| fs_error(reason, path, e))?
            .is_file()
        {
            return Err(GuiError::new(
                Code::Corrupt,
                format!("Expected a regular file at {}", path.display()),
                json!({ "reason": reason }),
            ));
        }
        Ok(file)
    }

    fn activate_bundle(
        staged: &Path,
        bundle: &Path,
        expected: Option<BundleIdentity>,
    ) -> Result<(), GuiError> {
        let flags = if expected.is_some() {
            libc::RENAME_SWAP
        } else {
            libc::RENAME_EXCL
        };
        let source = c_path(staged).map_err(|e| fs_error("restore_failed", staged, e))?;
        let dest = c_path(bundle).map_err(|e| fs_error("restore_failed", bundle, e))?;
        // SAFETY: both C strings remain live, and renamex_np uses pathname entries without following leaf symlinks.
        if unsafe { libc::renamex_np(source.as_ptr(), dest.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(fs_error(
                "restore_failed",
                bundle,
                io::Error::last_os_error(),
            ))
        }
    }

    fn verify_swap(
        ops: &dyn BundleOps,
        staged: &Path,
        bundle: &Path,
        verified: &VerifiedBundle,
        expected_displaced: Option<BundleIdentity>,
    ) -> Result<(), GuiError> {
        // renamex_np cannot compare the old entry as it swaps. Retain any
        // unexpected bundle it displaced and report an uncertain restore.
        if ops.bundle_identity(staged)? != expected_displaced {
            return Err(bundle_error(
                "restore_destination_changed",
                "The restore displaced a different app than the one observed before the swap",
            ));
        }
        let restored = verify_version(ops, bundle, &verified.info.version)?;
        if restored != *verified {
            return Err(bundle_changed().into_error());
        }
        Ok(())
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(parent: &Path) -> io::Result<Self> {
            let path = parent.join(".git-strata-bundle.XXXXXX");
            let mut bytes = c_path(&path)?.into_bytes_with_nul();
            // SAFETY: the writable, NUL-terminated buffer ends in six Xs. mkdtemp replaces them in place.
            if unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null() {
                return Err(io::Error::last_os_error());
            }
            bytes.pop();
            Ok(Self(std::ffi::OsString::from_vec(bytes).into()))
        }

        fn keep(mut self) -> PathBuf {
            std::mem::take(&mut self.0)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if !self.0.as_os_str().is_empty() {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        Ok(CString::new(path.as_os_str().as_bytes())?)
    }

    fn corrupt_plist(message: impl Into<String>) -> GuiError {
        GuiError::new(
            Code::Corrupt,
            message,
            json!({ "reason": "bundle_info_plist" }),
        )
    }

    // Foundation decodes both XML and binary plists. Read the file directly so
    // no bundle cache can return the old metadata after an atomic swap.
    struct Plist(Retained<AnyObject>);

    impl Plist {
        fn parse(bytes: &[u8]) -> Result<Self, GuiError> {
            let data = NSData::with_bytes(bytes);
            // SAFETY: NSData is live, the selector has the Foundation signature,
            // and nil output pointers request neither the format nor the error.
            let root: Option<Retained<AnyObject>> = unsafe {
                msg_send![class!(NSPropertyListSerialization), propertyListWithData: &*data,
                    options: 0usize, format: ptr::null_mut::<usize>(), error: ptr::null_mut::<*mut AnyObject>()]
            };
            // SAFETY: Foundation property list objects implement NSObject's class check.
            root.filter(|root| unsafe { msg_send![&**root, isKindOfClass: class!(NSDictionary)] })
                .map(Self)
                .ok_or_else(|| corrupt_plist("Info.plist must be a property list dictionary"))
        }

        fn value(&self, key: &str) -> Option<Retained<AnyObject>> {
            let key = NSString::from_str(key);
            // SAFETY: parse checked the receiver is an NSDictionary, and the
            // NSString and dictionary are live for the lookup.
            unsafe { msg_send![&*self.0, objectForKey: &*key] }
        }

        fn version(&self, key: &str) -> Result<(String, [u64; 3]), GuiError> {
            let parsed = self.value(key).and_then(|value| {
                let string = value.downcast_ref::<NSString>()?.to_string();
                let release = release_version(&string)?;
                Some((string, release))
            });
            parsed
                .ok_or_else(|| corrupt_plist(format!("Info.plist {key} must be a release version")))
        }

        fn bundle_info(&self) -> Result<BundleInfo, GuiError> {
            let (version, release) = self.version("CFBundleShortVersionString")?;
            let (downgrade_floor, floor) = self.version("GitStrataDowngradeFloor")?;
            if floor > release {
                return Err(corrupt_plist(
                    "Info.plist downgrade floor exceeds the bundle version",
                ));
            }
            let store_schema = self
                .value("GitStrataStoreSchema")
                .and_then(|value| positive_schema(&value))
                .ok_or_else(|| {
                    corrupt_plist("Info.plist GitStrataStoreSchema must be a positive u32 integer")
                })?;
            Ok(BundleInfo {
                version,
                store_schema,
                downgrade_floor,
            })
        }
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberIsFloatType(value: *const c_void) -> u8;
        fn CFNumberGetValue(value: *const c_void, kind: isize, output: *mut c_void) -> u8;
    }

    fn positive_schema(value: &AnyObject) -> Option<u32> {
        const CF_NUMBER_SINT64_TYPE: isize = 4;
        let value = ptr::from_ref(value).cast();
        let mut number = 0i64;
        // SAFETY: PropertyList values are toll-free bridged. Test the CF type
        // before the CFNumber calls (a plist boolean has a different type ID).
        // The selected CFNumber type writes an i64 to our live buffer.
        let valid = unsafe {
            CFGetTypeID(value) == CFNumberGetTypeID()
                && CFNumberIsFloatType(value) == 0
                && CFNumberGetValue(
                    value,
                    CF_NUMBER_SINT64_TYPE,
                    ptr::from_mut(&mut number).cast(),
                ) != 0
        };
        let schema = u32::try_from(number).ok()?;
        (valid && schema > 0).then_some(schema)
    }

    // macOS ships libarchive. Use its tar and gzip readers without another
    // process or a second Cargo archive stack.
    #[link(name = "archive")]
    unsafe extern "C" {
        fn archive_read_new() -> *mut c_void;
        fn archive_read_free(archive: *mut c_void) -> c_int;
        fn archive_read_support_filter_gzip(archive: *mut c_void) -> c_int;
        fn archive_read_support_format_tar(archive: *mut c_void) -> c_int;
        fn archive_read_open_memory(archive: *mut c_void, data: *const c_void, len: usize)
        -> c_int;
        fn archive_read_next_header(archive: *mut c_void, entry: *mut *mut c_void) -> c_int;
        fn archive_read_extract(archive: *mut c_void, entry: *mut c_void, flags: c_int) -> c_int;
        fn archive_read_close(archive: *mut c_void) -> c_int;
        fn archive_error_string(archive: *mut c_void) -> *const c_char;
        fn archive_entry_pathname(entry: *mut c_void) -> *const c_char;
        fn archive_entry_filetype(entry: *mut c_void) -> libc::mode_t;
        fn archive_entry_hardlink(entry: *mut c_void) -> *const c_char;
        fn archive_entry_symlink(entry: *mut c_void) -> *const c_char;
        fn archive_entry_set_pathname(entry: *mut c_void, path: *const c_char);
        fn archive_entry_set_hardlink(entry: *mut c_void, path: *const c_char);
    }

    struct Archive(NonNull<c_void>);

    impl Archive {
        fn check(&self, code: c_int) -> Result<(), GuiError> {
            const ARCHIVE_OK: c_int = 0;
            if code == ARCHIVE_OK {
                return Ok(());
            }
            // SAFETY: libarchive owns this string until the next operation.
            let message = unsafe {
                let raw = archive_error_string(self.0.as_ptr());
                (!raw.is_null()).then(|| CStr::from_ptr(raw).to_string_lossy().into_owned())
            };
            Err(bundle_error(
                "bundle_archive",
                message.unwrap_or_else(|| format!("Could not extract update archive ({code})")),
            ))
        }
    }

    impl Drop for Archive {
        fn drop(&mut self) {
            // SAFETY: this guard owns the archive for its full lifetime.
            unsafe { archive_read_free(self.0.as_ptr()) };
        }
    }

    fn extract_tar_gz(bytes: &[u8], staging: &Path, name: &OsStr) -> Result<(), GuiError> {
        const ARCHIVE_EOF: c_int = 1;
        const ARCHIVE_EXTRACT_SECURE_SYMLINKS: c_int = 0x0100;
        const ARCHIVE_EXTRACT_SECURE_NODOTDOT: c_int = 0x0200;
        if !bytes.starts_with(&[0x1f, 0x8b]) {
            return Err(bundle_error(
                "bundle_archive",
                "The update archive must be gzip-compressed tar",
            ));
        }
        // SAFETY: the guard keeps this archive live, `bytes` stays live until it
        // is freed, and each entry belongs to the current successfully read header.
        unsafe {
            let archive = Archive(NonNull::new(archive_read_new()).ok_or_else(|| {
                bundle_error("bundle_archive", "Unable to allocate an archive reader")
            })?);
            let handle = archive.0.as_ptr();
            archive.check(archive_read_support_filter_gzip(handle))?;
            archive.check(archive_read_support_format_tar(handle))?;
            archive.check(archive_read_open_memory(
                handle,
                bytes.as_ptr().cast(),
                bytes.len(),
            ))?;
            loop {
                let mut entry = ptr::null_mut();
                let result = archive_read_next_header(handle, &mut entry);
                if result == ARCHIVE_EOF {
                    break;
                }
                archive.check(result)?;

                let stage_path = |raw: *const c_char| -> Result<CString, GuiError> {
                    if raw.is_null() {
                        return Err(bundle_error("bundle_archive", "Missing archive entry path"));
                    }
                    let relative = Path::new(OsStr::from_bytes(CStr::from_ptr(raw).to_bytes()));
                    if !in_bundle(relative, name) {
                        return Err(bundle_error(
                            "bundle_archive",
                            "The archive must contain only the expected app bundle",
                        ));
                    }
                    c_path(&staging.join(relative))
                        .map_err(|e| fs_error("bundle_archive", relative, e))
                };
                let dest = stage_path(archive_entry_pathname(entry))?;
                let kind = archive_entry_filetype(entry);
                if !matches!(kind, libc::S_IFDIR | libc::S_IFREG | libc::S_IFLNK) {
                    return Err(bundle_error(
                        "bundle_archive",
                        "Unsupported bundle archive entry type",
                    ));
                }
                let symlink = archive_entry_symlink(entry);
                if !symlink.is_null() {
                    let relative = Path::new(OsStr::from_bytes(
                        CStr::from_ptr(archive_entry_pathname(entry)).to_bytes(),
                    ));
                    let target = Path::new(OsStr::from_bytes(CStr::from_ptr(symlink).to_bytes()));
                    if !contained_link(relative, target) {
                        return Err(bundle_error(
                            "bundle_archive",
                            "An archive link leaves the app bundle",
                        ));
                    }
                }
                let hardlink = archive_entry_hardlink(entry);
                if !hardlink.is_null() {
                    let target = stage_path(hardlink)?;
                    archive_entry_set_hardlink(entry, target.as_ptr());
                }
                archive_entry_set_pathname(entry, dest.as_ptr());
                archive.check(archive_read_extract(
                    handle,
                    entry,
                    ARCHIVE_EXTRACT_SECURE_SYMLINKS | ARCHIVE_EXTRACT_SECURE_NODOTDOT,
                ))?;
            }
            archive.check(archive_read_close(handle))
        }
    }

    fn in_bundle(path: &Path, name: &OsStr) -> bool {
        let mut parts = path.components();
        parts.next() == Some(Component::Normal(name))
            && parts.all(|part| matches!(part, Component::Normal(_)))
    }

    fn contained_link(entry: &Path, target: &Path) -> bool {
        let Some(mut depth) = entry.components().count().checked_sub(2) else {
            return false;
        };
        for part in target.components() {
            match part {
                Component::Normal(_) => depth += 1,
                Component::CurDir => (),
                Component::ParentDir if depth > 0 => depth -= 1,
                _ => return false,
            }
        }
        true
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::update::{
        exec::Operation,
        rollback::{StoreEvidence, check, restore_fenced},
    };

    const NAME: &str = "GIT STRATA.app";
    const FROM: &str = "0.3.9";
    const TO: &str = "0.3.10";
    const PLIST: &str = include_str!("../../tests/fixtures/update/t11/Info.plist");
    const ARCHIVE: &[u8] = include_bytes!("../../tests/fixtures/update/t11/download.app.tar.gz");

    struct Layout {
        _temp: tempfile::TempDir,
        app: PathBuf,
        support: PathBuf,
        config: PathBuf,
        store: PathBuf,
        checkout: PathBuf,
        staging: PathBuf,
    }

    impl Layout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path();
            let app = path.join("Applications").join(NAME);
            let support = path.join("App Support");
            let config = path.join("Daemon Config");
            let store = path.join("data");
            let checkout = path.join("Checkout");
            let staging = path.join("temp");
            for dir in [
                app.parent().unwrap(),
                &support,
                &config,
                &store,
                &checkout,
                &staging,
            ] {
                fs::create_dir(dir).unwrap();
            }
            Self {
                _temp: temp,
                app,
                support,
                config,
                store,
                checkout,
                staging,
            }
        }

        fn ops<R>(&self, run: R) -> MacBundleOps<R>
        where
            R: for<'a> Fn(Operation<'a>) -> Result<(), GuiError> + Send + Sync,
        {
            let mut ops = MacBundleOps::new(
                self.app.clone(),
                &self.support,
                self.store.clone(),
                vec![self.checkout.clone()],
                self.staging.clone(),
                run,
            )
            .unwrap();
            ops.plugin_temp_root = Some(self.staging.clone());
            ops
        }

        fn txn(&self) -> Txn {
            serde_json::from_value(json!({
                "phase": "installing",
                "from": FROM,
                "to": TO,
                "from_schema": 7,
                "to_schema": 8,
                "to_floor": "0.3.4",
                "from_daemon": null,
                "program": self.app.join("Contents/MacOS/git-stratad"),
                "rollback_zip": self.support.join("previous").join(backup_name(FROM).unwrap()),
                "was_enabled": true,
            }))
            .unwrap()
        }

        fn previous_app(&self) -> PathBuf {
            self.app.with_file_name("GIT STRATA (previous version).app")
        }

        fn write_store_state(&self, user_version: u32) {
            fs::write(
                self.config.join("store_state.json"),
                json!({
                    "root": self.store,
                    "user_version": user_version,
                    "at": "2026-11-17T20:00:00Z",
                    "migrated_from": null
                })
                .to_string(),
            )
            .unwrap();
        }
    }

    fn fake_plist(bundle: &Path, plist: &str) {
        let contents = bundle.join("Contents");
        fs::create_dir_all(&contents).unwrap();
        fs::write(contents.join("Info.plist"), plist).unwrap();
    }

    fn fake_app(bundle: &Path, version: &str) {
        let plist = PLIST.replace(TO, version);
        let plist = if version == FROM {
            plist.replace("<integer>8</integer>", "<integer>7</integer>")
        } else {
            plist
        };
        fake_plist(bundle, &plist);
    }

    fn fake_predecessor_with_schema(bundle: &Path, schema: u32) {
        let plist = PLIST.replace(TO, FROM).replace(
            "<integer>8</integer>",
            &format!("<integer>{schema}</integer>"),
        );
        fake_plist(bundle, &plist);
    }

    fn fake_zip(txn: &Txn, version: &str) {
        fs::create_dir_all(txn.rollback_zip.parent().unwrap()).unwrap();
        fs::write(&txn.rollback_zip, version).unwrap();
    }

    fn fake_run(op: Operation<'_>) -> Result<(), GuiError> {
        match op {
            Operation::ZipBackup { bundle, zip } => {
                assert_eq!(bundle.file_name().unwrap(), NAME);
                fs::write(zip, FROM).unwrap();
            }
            Operation::UnzipBackup { zip, staging } => {
                let version = fs::read_to_string(zip).unwrap();
                fake_app(&staging.join(NAME), &version);
            }
            Operation::VerifyCodesign { bundle } => {
                if bundle.join("Contents/invalid-signature").exists() {
                    return Err(GuiError::new(
                        Code::CheckFailed,
                        "Fake signature check failed",
                        json!({ "reason": "bad_codesign" }),
                    ));
                }
            }
            _ => panic!("unexpected updater operation"),
        }
        Ok(())
    }

    fn update(
        install: impl FnOnce(&[u8]) -> tauri_plugin_updater::Result<()> + Send + 'static,
    ) -> VerifiedUpdate {
        VerifiedUpdate {
            bytes: ARCHIVE.to_vec(),
            installer: Box::new(install),
        }
    }

    #[test]
    fn extracts_expected_app_and_reads_signed_release_metadata() {
        let layout = Layout::new();
        let ops = layout.ops(|op| {
            let Operation::VerifyCodesign { bundle } = op else {
                panic!("unexpected updater operation");
            };
            assert!(bundle.join("Contents/bundle-fixture.txt").is_file());
            Ok(())
        });
        let update = update(|_| panic!("inspection must not install"));
        assert_eq!(
            ops.inspect_download(&update, TO, FROM).unwrap(),
            BundleInfo {
                version: TO.into(),
                store_schema: 8,
                downgrade_floor: "0.3.4".into()
            }
        );
        assert_eq!(fs::read_dir(&layout.staging).unwrap().count(), 0);
        assert_eq!(
            ops.extract(ARCHIVE, &layout.store).unwrap_err().reason(),
            Some("bundle_path")
        );
        assert_eq!(
            ops.extract(b"not a tarball", &layout.staging)
                .unwrap_err()
                .reason(),
            Some("bundle_archive")
        );
    }

    #[test]
    fn rejects_manifest_mismatch_equal_and_older_bundle_versions() {
        let layout = Layout::new();
        let ops = layout.ops(|_| panic!("a version mismatch must fail before codesign"));
        let update = update(|_| unreachable!());
        for (manifest, current) in [("0.3.11", FROM), (TO, TO), (TO, "0.3.11"), (TO, "invalid")] {
            assert_eq!(
                ops.inspect_download(&update, manifest, current)
                    .unwrap_err()
                    .reason(),
                Some("bundle_version")
            );
        }
    }

    #[test]
    fn rejects_a_matching_download_when_codesign_fails() {
        let layout = Layout::new();
        let ops = layout.ops(|op| {
            assert!(matches!(op, Operation::VerifyCodesign { .. }));
            Err(bundle_error("bad_codesign", "Fake signature check failed"))
        });
        let update = update(|_| unreachable!());
        let err = ops.inspect_download(&update, TO, FROM).unwrap_err();
        assert_eq!(err.reason(), Some("bad_codesign"));
    }

    #[test]
    fn refuses_the_plugins_same_volume_temp_parent_inside_the_store() {
        let layout = Layout::new();
        fake_app(&layout.app, FROM);
        let protected = layout.store.join("plugin-tmp");
        fs::create_dir(&protected).unwrap();
        let alias = layout.staging.join("plugin-tmp");
        std::os::unix::fs::symlink(&protected, &alias).unwrap();

        let mut ops = layout.ops(|_| panic!("the staging check must precede bundle inspection"));
        ops.plugin_temp_root = Some(alias);
        let update = update(|_| panic!("the plugin must never run with the protected temp parent"));
        let error = ops.inspect_download(&update, TO, FROM).unwrap_err();
        assert_eq!(error.reason(), Some("bundle_path"));
        let Err(tauri_plugin_updater::Error::Io(error)) = ops.install(update) else {
            panic!("expected installer staging refusal");
        };
        let cause = error.get_ref().unwrap().downcast_ref::<GuiError>().unwrap();
        assert_eq!(cause.reason(), Some("bundle_path"));
        assert_eq!(fs::read_dir(&protected).unwrap().count(), 0);
    }

    #[test]
    fn neither_temp_selection_stages_inside_a_managed_checkout() {
        let layout = Layout::new();
        fake_app(&layout.app, FROM);
        let protected = layout.checkout.join("staging");
        fs::create_dir(&protected).unwrap();
        let alias = layout.staging.join("checkout-temp");
        std::os::unix::fs::symlink(&protected, &alias).unwrap();

        let app_parent = fs::canonicalize(layout.app.parent().unwrap()).unwrap();
        let mut ops = MacBundleOps::new(
            layout.app.clone(),
            &layout.support,
            layout.store.clone(),
            vec![layout.checkout.clone()],
            alias.clone(),
            |operation| {
                let Operation::VerifyCodesign { bundle } = operation else {
                    panic!("unexpected operation during inspection");
                };
                assert!(bundle.starts_with(&app_parent));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            ops.extract(ARCHIVE, &protected).unwrap_err().reason(),
            Some("bundle_path")
        );
        let blocked_update =
            update(|_| panic!("the plugin must not use the checkout as a temp parent"));
        ops.plugin_temp_root = Some(alias);
        assert_eq!(
            ops.inspect_download(&blocked_update, TO, FROM)
                .unwrap_err()
                .reason(),
            Some("bundle_path")
        );
        let Err(tauri_plugin_updater::Error::Io(error)) = ops.install(blocked_update) else {
            panic!("expected installer staging refusal");
        };
        assert_eq!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<GuiError>()
                .unwrap()
                .reason(),
            Some("bundle_path")
        );

        // With no usable system temp, the plugin and inspection can use the app's parent.
        ops.plugin_temp_root = Some(layout.staging.join("absent"));
        let inspected = ops
            .inspect_download(&update(|_| unreachable!()), TO, FROM)
            .unwrap();
        assert_eq!(inspected.version, TO);
        assert_eq!(fs::read_dir(&protected).unwrap().count(), 0);
        assert_eq!(fs::read_dir(app_parent).unwrap().count(), 1);
    }

    #[test]
    fn rejects_missing_or_malformed_plist_fields() {
        let layout = Layout::new();
        let ops = layout.ops(fake_run);
        for (key, malformed) in [
            ("CFBundleShortVersionString", PLIST.replace(TO, "0.03.10")),
            (
                "CFBundleShortVersionString",
                PLIST.replace("CFBundleShortVersionString", "IrrelevantKey"),
            ),
            (
                "GitStrataStoreSchema",
                PLIST.replace("GitStrataStoreSchema", "IrrelevantKey"),
            ),
            (
                "GitStrataStoreSchema",
                PLIST.replace("<integer>8</integer>", "<string>8</string>"),
            ),
            (
                "GitStrataStoreSchema",
                PLIST.replace("<integer>8</integer>", "<true/>"),
            ),
            (
                "GitStrataStoreSchema",
                PLIST.replace("<integer>8</integer>", "<real>8.0</real>"),
            ),
            (
                "GitStrataStoreSchema",
                PLIST.replace("<integer>8</integer>", "<integer>4294967296</integer>"),
            ),
            (
                "GitStrataStoreSchema",
                PLIST.replace("<integer>8</integer>", "<integer>0</integer>"),
            ),
            (
                "GitStrataDowngradeFloor",
                PLIST.replace("GitStrataDowngradeFloor", "IrrelevantKey"),
            ),
            ("downgrade floor", PLIST.replace("0.3.4", "0.3.11")),
        ] {
            fake_plist(&layout.app, &malformed);
            let err = ops.read_info_plist(&layout.app).unwrap_err();
            assert_eq!(err.reason(), Some("bundle_info_plist"));
            assert!(err.message.contains(key), "{err}");
        }
    }

    #[test]
    fn reads_binary_info_plist() {
        let layout = Layout::new();
        let contents = layout.app.join("Contents");
        fs::create_dir_all(&contents).unwrap();
        fs::write(
            contents.join("Info.plist"),
            include_bytes!("../../tests/fixtures/update/t11/Info.binary.plist"),
        )
        .unwrap();
        assert_eq!(
            layout.ops(fake_run).read_info_plist(&layout.app).unwrap(),
            BundleInfo {
                version: TO.into(),
                store_schema: 8,
                downgrade_floor: "0.3.4".into(),
            }
        );
    }

    #[test]
    fn backup_is_replaced_only_after_success_and_never_while_owned() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        fake_zip(&txn, "keep me until the replacement is ready");
        let previous = txn.rollback_zip.parent().unwrap();
        let stale = previous.join("GIT-STRATA-0.3.8.zip");
        let unrelated = previous.join("notes.txt");
        fs::write(&stale, "previous release").unwrap();
        fs::write(&unrelated, "unrelated").unwrap();

        let ops = layout.ops(|operation| match operation {
            Operation::ZipBackup { .. } => Err(bundle_error("ditto_failed", "Fake backup error")),
            other => fake_run(other),
        });
        assert_eq!(
            ops.save_previous(FROM, Some(&txn)).unwrap_err().reason(),
            Some("rollback_zip_owned")
        );
        assert_eq!(
            ops.save_previous(FROM, None).unwrap_err().reason(),
            Some("ditto_failed")
        );
        assert_eq!(
            fs::read_to_string(&txn.rollback_zip).unwrap(),
            "keep me until the replacement is ready"
        );
        assert!(stale.exists());

        let ops = layout.ops(fake_run);
        assert_eq!(ops.save_previous(FROM, None).unwrap(), txn.rollback_zip);
        assert_eq!(fs::read_to_string(&txn.rollback_zip).unwrap(), FROM);
        assert!(!stale.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn keeps_existing_backups_when_ditto_archives_a_concurrent_replacement() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        fake_zip(&txn, FROM);
        let stale = txn.rollback_zip.with_file_name("GIT-STRATA-0.3.8.zip");
        fs::write(&stale, "0.3.8").unwrap();

        let ops = layout.ops(|op| match op {
            Operation::ZipBackup { bundle, zip } => {
                fake_app(bundle, "0.3.11");
                fs::write(zip, "0.3.11").unwrap();
                Ok(())
            }
            other => fake_run(other),
        });

        let error = ops.save_previous(FROM, None).unwrap_err();
        assert_eq!(error.reason(), Some("bundle_version"));
        assert_eq!(fs::read_to_string(&txn.rollback_zip).unwrap(), FROM);
        assert_eq!(fs::read_to_string(&stale).unwrap(), "0.3.8");
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, "0.3.11");
    }

    #[test]
    fn classifies_verified_installed_bundle_even_after_plugin_error() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        let app = layout.app.clone();
        let update = update(move |bytes| {
            assert_eq!(bytes, ARCHIVE);
            fake_app(&app, TO);
            Err(tauri_plugin_updater::Error::InvalidUpdaterFormat)
        });
        let got = install_and_recover(&layout.ops(fake_run), update, &layout.app, &txn);
        assert!(matches!(got.outcome, InstallOutcome::Installed));
        assert!(got.installer_error.is_some());
    }

    #[test]
    fn classifies_verified_old_bundle_as_unchanged() {
        let layout = Layout::new();
        fake_app(&layout.app, FROM);
        let update = update(|_| Err(tauri_plugin_updater::Error::InvalidUpdaterFormat));
        let got = install_and_recover(&layout.ops(fake_run), update, &layout.app, &layout.txn());
        assert!(matches!(got.outcome, InstallOutcome::Unchanged));
    }

    #[test]
    fn install_cannot_report_unchanged_on_a_predecessor_schema_mismatch() {
        let layout = Layout::new();
        let txn = layout.txn();
        let ops = layout.ops(fake_run);
        fake_app(&layout.app, FROM);
        let app = layout.app.clone();
        let got = install_and_recover(
            &ops,
            update(move |_| {
                fake_predecessor_with_schema(&app, 6);
                Err(tauri_plugin_updater::Error::InvalidUpdaterFormat)
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
        let got = install_and_recover(
            &ops,
            update(|_| panic!("the updater must not run with a mismatched predecessor schema")),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
        assert!(got.installer_error.is_none());
    }

    #[test]
    fn refuses_an_installed_target_with_different_schema_or_floor() {
        let layout = Layout::new();
        let txn = layout.txn();
        let ops = layout.ops(fake_run);
        for plist in [
            PLIST.replace("0.3.4", TO),
            PLIST.replace("<integer>8</integer>", "<integer>9</integer>"),
        ] {
            fake_app(&layout.app, FROM);
            let app = layout.app.clone();
            let got = install_and_recover(
                &ops,
                update(move |_| {
                    fake_plist(&app, &plist);
                    Ok(())
                }),
                &layout.app,
                &txn,
            );
            let InstallOutcome::RestoreFailed(error) = got.outcome else {
                panic!("{got:?}");
            };
            assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
            assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, TO);
        }
    }

    #[test]
    fn inconclusive_installed_codesign_retains_the_app_and_skips_both_restores() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        let kept = layout.previous_app();
        fake_app(&kept, FROM);
        fake_zip(&txn, FROM);
        let app = layout.app.clone();
        let update = update(move |_| {
            fake_app(&app, TO);
            fs::write(app.join("Contents/verifier-failed"), "test").unwrap();
            Err(PreviousAppNotRestored(kept))
        });
        let ops = layout.ops(|op| match op {
            Operation::VerifyCodesign { bundle }
                if bundle == layout.app && bundle.join("Contents/verifier-failed").exists() =>
            {
                Err(GuiError::internal("Could not start codesign"))
            }
            Operation::VerifyCodesign { bundle } if bundle == layout.app => fake_run(op),
            _ => panic!("a failed verifier must not launch either restore path"),
        });
        let got = install_and_recover(&ops, update, &layout.app, &txn);
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("bundle_verification_uncertain"));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, TO);
        assert_eq!(got.retained_app, Some(layout.previous_app()));
        assert_eq!(fs::read_to_string(&txn.rollback_zip).unwrap(), FROM);
    }

    #[test]
    fn prefers_the_verified_plugin_copy_to_the_zip() {
        let layout = Layout::new();
        let kept = layout.previous_app();
        fake_app(&layout.app, FROM);
        let app = layout.app.clone();
        let update = update(move |_| {
            fs::rename(&app, &kept).unwrap();
            Err(PreviousAppNotRestored(kept))
        });
        let ops = layout.ops(|op| {
            assert!(matches!(op, Operation::VerifyCodesign { .. }));
            fake_run(op)
        });
        let got = install_and_recover(&ops, update, &layout.app, &layout.txn());
        assert!(matches!(got.outcome, InstallOutcome::Restored));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, FROM);
        assert!(got.retained_app.is_none());
        assert!(!layout.previous_app().exists());
    }

    #[test]
    fn falls_back_to_the_verified_zip_and_reports_the_kept_path() {
        let layout = Layout::new();
        let txn = layout.txn();
        let kept = layout.previous_app();
        fake_app(&kept, TO);
        fake_app(&layout.app, FROM);
        fake_zip(&txn, FROM);
        let app = layout.app.clone();
        let update = update(move |_| {
            fake_app(&app, TO);
            fs::write(app.join("Contents/invalid-signature"), "test").unwrap();
            Err(PreviousAppNotRestored(kept))
        });

        let ops = layout.ops(fake_run);
        let got = install_and_recover(&ops, update, &layout.app, &txn);
        assert!(matches!(got.outcome, InstallOutcome::Restored));
        assert_eq!(got.retained_app, Some(layout.previous_app()));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, FROM);
        assert!(!layout.app.join("Contents/invalid-signature").exists());
        assert!(txn.rollback_zip.exists());
    }

    #[test]
    fn install_recovery_refuses_a_kept_predecessor_schema_mismatch() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        let app = layout.app.clone();
        let kept = layout.previous_app();
        let got = install_and_recover(
            &layout.ops(fake_run),
            update(move |_| {
                fs::rename(&app, &kept).unwrap();
                fake_predecessor_with_schema(&kept, 6);
                Err(PreviousAppNotRestored(kept))
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("restore_failed"));
        assert_eq!(
            error.detail["kept"]["detail"]["cause"]["detail"]["reason"],
            "unexpected_bundle_metadata"
        );
        assert_eq!(got.retained_app, Some(layout.previous_app()));
        assert!(layout.previous_app().is_dir());
        assert!(!layout.app.exists());
    }

    #[test]
    fn install_recovery_refuses_a_predecessor_schema_mismatch_at_the_destination() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        let kept = layout.previous_app();
        let ops = layout.ops(|op| {
            let Operation::VerifyCodesign { bundle } = op else {
                panic!("a mismatched app at the destination must stop recovery");
            };
            if bundle == kept {
                fake_predecessor_with_schema(&layout.app, 6);
            }
            fake_run(op)
        });
        let app = layout.app.clone();
        let kept_for_install = kept.clone();
        let got = install_and_recover(
            &ops,
            update(move |_| {
                fs::rename(&app, &kept_for_install).unwrap();
                Err(PreviousAppNotRestored(kept_for_install))
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
        assert_eq!(got.retained_app.as_deref(), Some(kept.as_path()));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().store_schema, 6);
        assert_eq!(
            ops.read_info_plist(&kept).unwrap().store_schema,
            txn.from_schema
        );
    }

    #[test]
    fn reports_restore_failed_when_recovery_zip_cannot_be_verified() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        fake_zip(&txn, TO);
        let ops = layout.ops(fake_run);
        let app = layout.app.clone();
        let got = install_and_recover(
            &ops,
            update(move |_| {
                fs::remove_dir_all(app).unwrap();
                Ok(())
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("bundle_version"));
        assert!(!layout.app.exists());
        assert!(txn.rollback_zip.exists());
    }

    #[test]
    fn reports_both_failed_recovery_sources() {
        let layout = Layout::new();
        let txn = layout.txn();
        let kept = layout.previous_app();
        fake_app(&layout.app, FROM);
        fake_app(&kept, TO);
        fake_zip(&txn, "0.3.11");
        let app = layout.app.clone();
        let got = install_and_recover(
            &layout.ops(fake_run),
            update(move |_| {
                fs::remove_dir_all(app).unwrap();
                Err(PreviousAppNotRestored(kept))
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("restore_failed"));
        assert!(
            error.detail["kept"]["message"]
                .as_str()
                .unwrap()
                .contains(TO)
        );
        assert!(
            error.detail["zip"]["message"]
                .as_str()
                .unwrap()
                .contains("0.3.11")
        );
        assert_eq!(got.retained_app, Some(layout.previous_app()));
    }

    #[test]
    fn retains_zip_staging_if_a_same_version_app_replaces_the_restore_during_codesign() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        fake_zip(&txn, FROM);

        let zip_prepared = AtomicBool::new(false);
        let restored = layout.app.with_file_name("verified-staged-copy.app");
        let ops = layout.ops(|op| match &op {
            Operation::UnzipBackup { .. } => {
                zip_prepared.store(true, Ordering::Relaxed);
                fake_run(op)
            }
            Operation::VerifyCodesign { bundle }
                if *bundle == layout.app
                    && zip_prepared.load(Ordering::Relaxed)
                    && !bundle.join("Contents/invalid-signature").exists() =>
            {
                fs::rename(bundle, &restored).unwrap();
                fake_app(bundle, FROM);
                fake_run(op)
            }
            _ => fake_run(op),
        });
        let app = layout.app.clone();
        let got = install_and_recover(
            &ops,
            update(move |_| {
                fake_app(&app, TO);
                fs::write(app.join("Contents/invalid-signature"), "test").unwrap();
                Ok(())
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("restore_failed"));
        assert_eq!(
            error.detail["cause"]["detail"]["cause"]["detail"]["reason"],
            "bundle_changed"
        );
        let stage = Path::new(error.detail["staging"].as_str().unwrap());
        assert!(
            stage
                .join(NAME)
                .join("Contents/invalid-signature")
                .is_file()
        );
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, FROM);
        assert!(restored.is_dir());
    }

    #[test]
    fn refuses_kept_copy_restore_if_its_installed_metadata_changes_during_codesign() {
        let layout = Layout::new();
        let txn = layout.txn();
        let kept = layout.previous_app();
        let marker = "Contents/invalid-signature";
        fake_app(&layout.app, FROM);
        fake_zip(&txn, FROM);
        let ops = layout.ops(|op| {
            let Operation::VerifyCodesign { bundle } = op else {
                panic!("verification after the kept swap must not fall back to the zip");
            };
            if bundle == layout.app && kept.join(marker).is_file() {
                fake_app(bundle, "0.3.11");
            }
            fake_run(op)
        });
        let app = layout.app.clone();
        let kept_for_install = kept.clone();
        let got = install_and_recover(
            &ops,
            update(move |_| {
                fs::rename(&app, &kept_for_install).unwrap();
                fake_app(&app, TO);
                fs::write(app.join(marker), "test").unwrap();
                Err(PreviousAppNotRestored(kept_for_install))
            }),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("restore_verification_failed"));
        assert_eq!(got.retained_app.as_deref(), Some(kept.as_path()));
        assert!(kept.join(marker).is_file());
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, "0.3.11");
    }

    #[test]
    fn zip_swap_retains_a_concurrent_replacement_displaced_after_the_guard() {
        let layout = Layout::new();
        let txn = layout.txn();
        let ops = layout.ops(fake_run);
        fake_app(&layout.app, TO);
        fake_zip(&txn, FROM);
        let replacement = layout.app.with_file_name("concurrent.app");
        fake_app(&replacement, "0.3.11");
        let replacement_id = ops.bundle_identity(&replacement).unwrap();

        let error = ops
            .restore_zip_if(&txn.rollback_zip, &layout.app, &|_| {
                let expected = ops.bundle_identity(&layout.app)?;
                fs::rename(&layout.app, layout.app.with_file_name("observed.app")).unwrap();
                fs::rename(&replacement, &layout.app).unwrap();
                Ok(RestoreDecision::Swap { expected })
            })
            .unwrap_err();
        assert_eq!(error.reason(), Some("restore_failed"));
        assert_eq!(
            error.detail["cause"]["detail"]["reason"],
            "restore_destination_changed"
        );
        let displaced = Path::new(error.detail["staging"].as_str().unwrap()).join(NAME);
        assert_eq!(ops.bundle_identity(&displaced).unwrap(), replacement_id);
        assert_eq!(ops.read_info_plist(&displaced).unwrap().version, "0.3.11");
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, FROM);
        assert!(txn.rollback_zip.is_file());
    }

    #[test]
    fn zip_restore_of_observed_absence_leaves_a_concurrent_arrival_in_place() {
        let layout = Layout::new();
        let txn = layout.txn();
        let ops = layout.ops(fake_run);
        fake_zip(&txn, FROM);

        let error = ops
            .restore_zip_if(&txn.rollback_zip, &layout.app, &|_| {
                let expected = ops.bundle_identity(&layout.app)?;
                assert!(expected.is_none());
                fake_app(&layout.app, "0.3.11");
                Ok(RestoreDecision::Swap { expected })
            })
            .unwrap_err();
        assert_eq!(error.reason(), Some("restore_failed"));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, "0.3.11");
        assert_eq!(fs::read_dir(&layout.staging).unwrap().count(), 0);
    }

    #[test]
    fn preserves_an_unexpected_verified_version_for_reconciliation() {
        let layout = Layout::new();
        fake_app(&layout.app, "0.3.11");
        let txn = layout.txn();
        fake_zip(&txn, FROM);
        let ops = layout.ops(fake_run);
        let got = install_and_recover(
            &ops,
            update(|_| panic!("the updater must not replace an unexpected version")),
            &layout.app,
            &txn,
        );
        let InstallOutcome::RestoreFailed(error) = got.outcome else {
            panic!("{got:?}");
        };
        assert_eq!(error.reason(), Some("unexpected_bundle_version"));
        assert!(got.installer_error.is_none());
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, "0.3.11");
        assert_eq!(fs::read_to_string(&txn.rollback_zip).unwrap(), FROM);
    }

    #[test]
    fn fenced_rollback_requires_a_complete_snapshot_and_rechecks_it_before_swapping() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, TO);
        fake_zip(&txn, FROM);
        assert!(check(&txn, StoreEvidence::Live(Some(7))).is_ok());
        fs::write(
            layout.config.join("store_state.json"),
            json!({ "root": layout.store, "user_version": 7 }).to_string(),
        )
        .unwrap();
        let ops = layout.ops(|_| panic!("an incomplete snapshot cannot reach bundle operations"));
        let err =
            restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
        assert_eq!(err.reason(), Some("rollback_store_unknown"));
        assert!(err.message.contains("store/backups/"));

        layout.write_store_state(7);
        let ops = layout.ops(|op| {
            let unzip = matches!(op, Operation::UnzipBackup { .. });
            fake_run(op)?;
            if unzip {
                layout.write_store_state(8);
            }
            Ok(())
        });
        let err =
            restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
        assert_eq!(err.reason(), Some("rollback_store_migrated"));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, TO);

        layout.write_store_state(7);
        let ops = layout.ops(fake_run);
        restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap();
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, FROM);
    }

    #[test]
    fn fenced_rollback_preserves_a_new_release_installed_during_zip_preparation() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, TO);
        fake_zip(&txn, FROM);
        layout.write_store_state(7);
        let ops = layout.ops(|op| {
            let unzip = matches!(op, Operation::UnzipBackup { .. });
            fake_run(op)?;
            if unzip {
                fake_plist(
                    &layout.app,
                    &PLIST.replace(TO, "0.3.11").replace("0.3.4", TO),
                );
            }
            Ok(())
        });
        let error =
            restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
        assert_eq!(error.reason(), Some("unexpected_bundle_version"));
        assert!(error.message.contains("0.3.11"));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, "0.3.11");
        assert_eq!(fs::read_to_string(txn.rollback_zip).unwrap(), FROM);
    }

    #[test]
    fn fenced_rollback_requires_the_recorded_target_schema_and_floor_at_both_checks() {
        let layout = Layout::new();
        let txn = layout.txn();
        layout.write_store_state(7);
        fake_zip(&txn, FROM);
        for plist in [
            PLIST.replace("0.3.4", TO),
            PLIST.replace("<integer>8</integer>", "<integer>9</integer>"),
        ] {
            fake_plist(&layout.app, &plist);
            let ops = layout.ops(|op| {
                assert!(matches!(op, Operation::VerifyCodesign { .. }));
                fake_run(op)
            });
            let error =
                restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
            assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));

            fake_app(&layout.app, TO);
            let ops = layout.ops(|op| {
                if matches!(op, Operation::UnzipBackup { .. }) {
                    fake_plist(&layout.app, &plist);
                }
                fake_run(op)
            });
            let error =
                restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
            assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
            assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, TO);
        }
    }

    #[test]
    fn fenced_rollback_refuses_a_replacement_during_the_final_codesign_check() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, TO);
        fake_zip(&txn, FROM);
        layout.write_store_state(7);
        let zip_prepared = AtomicBool::new(false);
        let ops = layout.ops(|op| {
            match &op {
                Operation::UnzipBackup { .. } => zip_prepared.store(true, Ordering::Relaxed),
                Operation::VerifyCodesign { bundle }
                    if *bundle == layout.app && zip_prepared.load(Ordering::Relaxed) =>
                {
                    fake_app(&layout.app, "0.3.11");
                }
                _ => {}
            }
            fake_run(op)
        });
        let error =
            restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
        assert_eq!(error.reason(), Some("rollback_bundle_unverified"));
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, "0.3.11");
    }

    #[test]
    fn fenced_rollback_accepts_an_already_restored_and_verified_bundle() {
        let layout = Layout::new();
        let txn = layout.txn();
        fake_app(&layout.app, FROM);
        layout.write_store_state(7);
        let ops = layout.ops(|op| {
            assert!(matches!(op, Operation::VerifyCodesign { .. }));
            fake_run(op)
        });
        restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap();
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, FROM);
    }

    #[test]
    fn fenced_rollback_refuses_an_installed_predecessor_schema_mismatch() {
        let layout = Layout::new();
        let txn = layout.txn();
        layout.write_store_state(txn.from_schema);
        let ops = layout.ops(|op| {
            assert!(matches!(op, Operation::VerifyCodesign { .. }));
            fake_run(op)
        });
        for schema in [6, 8] {
            fake_predecessor_with_schema(&layout.app, schema);
            let error =
                restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
            assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
            assert_eq!(
                ops.read_info_plist(&layout.app).unwrap().store_schema,
                schema
            );
        }
    }

    #[test]
    fn fenced_rollback_refuses_a_staged_predecessor_schema_mismatch() {
        let layout = Layout::new();
        let txn = layout.txn();
        layout.write_store_state(txn.from_schema);
        fake_app(&layout.app, TO);
        fake_zip(&txn, FROM);
        let ops = layout.ops(|op| match op {
            Operation::UnzipBackup { staging, .. } => {
                fake_predecessor_with_schema(&staging.join(NAME), 6);
                Ok(())
            }
            other => fake_run(other),
        });
        let installed = ops.bundle_identity(&layout.app).unwrap();
        let error =
            restore_fenced(&ops, &layout.app, &txn, &layout.config, &layout.store).unwrap_err();
        assert_eq!(error.reason(), Some("unexpected_bundle_metadata"));
        assert_eq!(ops.bundle_identity(&layout.app).unwrap(), installed);
        assert_eq!(ops.read_info_plist(&layout.app).unwrap().version, TO);
        assert_eq!(fs::read_to_string(&txn.rollback_zip).unwrap(), FROM);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    use super::*;

    /// Records install requests and returns a result supplied by the test.
    pub(crate) struct FakeBundleOps {
        pub(crate) install_requests: Mutex<Vec<Vec<u8>>>,
        install_result: Box<dyn Fn() -> tauri_plugin_updater::Result<()> + Send + Sync>,
    }

    impl FakeBundleOps {
        pub(crate) fn new(
            install_result: impl Fn() -> tauri_plugin_updater::Result<()> + Send + Sync + 'static,
        ) -> Self {
            Self {
                install_requests: Mutex::new(Vec::new()),
                install_result: Box::new(install_result),
            }
        }
    }

    impl BundleOps for FakeBundleOps {
        fn extract(&self, _bytes: &[u8], _staging_dir: &Path) -> Result<PathBuf, GuiError> {
            Err(GuiError::internal("unexpected extract"))
        }

        fn read_info_plist(&self, _bundle: &Path) -> Result<BundleInfo, GuiError> {
            Err(GuiError::internal("unexpected read_info_plist"))
        }

        fn zip_backup(&self, _bundle: &Path, _zip: &Path) -> Result<(), GuiError> {
            Err(GuiError::internal("unexpected zip_backup"))
        }

        fn install(&self, update: VerifiedUpdate) -> tauri_plugin_updater::Result<()> {
            self.install_requests.lock().unwrap().push(update.bytes);
            (self.install_result)()
        }

        fn restore_zip(&self, _zip: &Path, _bundle: &Path) -> Result<(), GuiError> {
            Err(GuiError::internal("unexpected restore_zip"))
        }

        fn codesign_verify(&self, _bundle: &Path) -> Result<(), GuiError> {
            Err(GuiError::internal("unexpected codesign_verify"))
        }
    }

    #[test]
    fn fake_install_records_bytes_without_invoking_the_embedded_installer() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use tauri_plugin_updater::Error::PreviousAppNotRestored;

        let kept = PathBuf::from("/test/GIT STRATA (previous version).app");
        let kept_by_fake = kept.clone();
        let fake = FakeBundleOps::new(move || Err(PreviousAppNotRestored(kept_by_fake.clone())));
        let installer_called = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&installer_called);
        let update = VerifiedUpdate {
            bytes: vec![1, 2, 3],
            installer: Box::new(move |_| {
                called.store(true, Ordering::Relaxed);
                Ok(())
            }),
        };
        let ops: &dyn BundleOps = &fake;
        let result = ops.install(update);

        assert!(!installer_called.load(Ordering::Relaxed));
        assert_eq!(*fake.install_requests.lock().unwrap(), [vec![1, 2, 3]]);
        assert!(matches!(result, Err(PreviousAppNotRestored(path)) if path == kept));
    }
}

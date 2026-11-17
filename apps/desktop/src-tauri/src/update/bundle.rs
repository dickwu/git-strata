//! Bundle inspection, installation and rollback file operations.

use std::path::{Path, PathBuf};

use super::source::VerifiedUpdate;
use crate::error::GuiError;

/// Release metadata read from the downloaded bundle's Info.plist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleInfo {
    pub version: String,
    pub store_schema: u32,
    pub downgrade_floor: String,
}

pub trait BundleOps: Send + Sync {
    /// Extract into the caller's same-volume staging directory and return the app path.
    fn extract(&self, bytes: &[u8], staging_dir: &Path) -> Result<PathBuf, GuiError>;
    fn read_info_plist(&self, bundle: &Path) -> Result<BundleInfo, GuiError>;
    fn zip_backup(&self, bundle: &Path, zip: &Path) -> Result<(), GuiError>;
    /// Swap the installed app. Preserve the plugin's PreviousAppNotRestored path for recovery.
    fn install(&self, update: VerifiedUpdate) -> tauri_plugin_updater::Result<()>;
    fn restore_zip(&self, zip: &Path, bundle: &Path) -> Result<(), GuiError>;
    fn codesign_verify(&self, bundle: &Path) -> Result<(), GuiError>;
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

//! Contracts for the desktop updater and bundled daemon lifecycle.

pub mod bundle;
pub mod cache;
pub mod check;
pub mod cli_link;
pub mod exec;
pub mod install;
pub mod manifest;
pub mod move_daemon;
pub mod restart;
pub mod rollback;
pub mod setup;
pub mod source;
pub mod state;
pub mod txn;

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime};

pub type UpdateFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Clock: Send + Sync {
    /// Wall clock for persisted timestamps and check scheduling.
    fn now(&self) -> SystemTime;
    /// Monotonic clock for active operation deadlines.
    fn monotonic(&self) -> Instant;
    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()>;
}

#[cfg(target_os = "macos")]
use {
    crate::{
        error::GuiError,
        service::{AGENT_LABEL, ServiceManager, launchctl::Launchctl, plist_for_home, scope},
    },
    serde_json::{Value, json},
    std::{
        fs::{self, OpenOptions},
        io::{self, Read},
        os::unix::fs::OpenOptionsExt,
        path::{Path, PathBuf},
        sync::Arc,
    },
    strata_types::{Code, Config},
    tauri::{AppHandle, Emitter, Listener, Manager},
};

#[cfg(target_os = "macos")]
pub(crate) type Lifecycle = Arc<tokio::sync::Mutex<()>>;

#[cfg(target_os = "macos")]
pub(crate) fn lock_lifecycle(
    window: &tauri::WebviewWindow,
) -> Result<tokio::sync::OwnedMutexGuard<()>, GuiError> {
    window
        .state::<Lifecycle>()
        .inner()
        .clone()
        .try_lock_owned()
        .map_err(|_| {
            GuiError::new(
                Code::Busy,
                "Another daemon lifecycle operation is in progress",
                json!({"reason":"update_in_flight"}),
            )
        })
}

#[cfg(target_os = "macos")]
pub(crate) struct Installed {
    pub(crate) home: PathBuf,
    pub(crate) bundle: PathBuf,
    pub(crate) version: String,
    socket: PathBuf,
    app: AppHandle,
}

#[cfg(target_os = "macos")]
impl Installed {
    pub(crate) fn support(&self) -> PathBuf {
        cache::path(&self.home)
            .parent()
            .expect("update cache parent")
            .to_path_buf()
    }
    fn config_dir(&self) -> &Path {
        self.socket.parent().expect("daemon config directory")
    }
    fn config(&self) -> Result<Option<Config>, GuiError> {
        let path = self.config_dir().join("config.toml");
        let bytes = match read_regular(&path, 1 << 20) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(GuiError::internal(format!(
                    "Cannot read the daemon config: {e}"
                )));
            }
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| GuiError::internal("Daemon config is not UTF-8"))?;
        let config = Config::parse(text)
            .map_err(|_| GuiError::internal("Cannot verify the daemon config"))?;
        config
            .validate()
            .map_err(|_| GuiError::internal("Cannot verify the daemon config"))?;
        Ok(Some(config))
    }
    pub(crate) fn data_root(&self) -> Result<PathBuf, GuiError> {
        self.config()?
            .map(|config| config.paths.root)
            .ok_or_else(|| {
                GuiError::new(
                    Code::InvalidInput,
                    "The daemon config is missing",
                    json!({"reason":"update_config_missing"}),
                )
            })
    }

    fn bundles(
        &self,
    ) -> Result<
        bundle::MacBundleOps<impl for<'a> Fn(exec::Operation<'a>) -> Result<(), GuiError> + use<>>,
        GuiError,
    > {
        let (data_root, checkouts) = self.config()?.map_or_else(
            || (self.config_dir().to_path_buf(), Vec::new()),
            |config| {
                (
                    config.paths.root,
                    config.repos.into_values().map(|repo| repo.path).collect(),
                )
            },
        );
        let home = self.home.clone();
        bundle::MacBundleOps::new(
            self.bundle.clone(),
            &self.support(),
            data_root,
            checkouts,
            std::env::temp_dir(),
            move |op| exec::checked(&home, op).map(|_| ()),
        )
    }

    async fn status(&self) -> Result<Value, GuiError> {
        status_at(&self.socket).await
    }

    pub(crate) fn retained_schema(&self) -> Option<u32> {
        let root = self.data_root().ok()?;
        rollback::read_store_schema(self.config_dir(), &root)
    }

    pub(crate) fn verify_scope(&self) -> Result<(), GuiError> {
        scope::lifecycle_scope(&self.home, scope::login_home().as_deref())?;
        if cfg!(debug_assertions) || scope::anchored(&self.bundle) != self.bundle {
            return Err(GuiError::new(
                Code::Unsupported,
                "Open GIT STRATA in Applications",
                json!({"reason":"update_bundle_location"}),
            ));
        }
        Ok(())
    }

    pub(crate) fn daemon_version_output(&self) -> Result<String, GuiError> {
        let bytes = exec::checked(&self.home, exec::Operation::DaemonVersion)?;
        String::from_utf8(bytes).map_err(|_| GuiError::internal("The daemon version is not UTF-8"))
    }

    async fn ask_schema(
        &self,
        current: Option<u32>,
        target: &bundle::BundleInfo,
    ) -> Result<bool, GuiError> {
        #[derive(serde::Deserialize)]
        struct Answer {
            request_id: String,
            acknowledgement: Option<u32>,
        }
        let window = self
            .app
            .get_webview_window("main")
            .ok_or_else(|| GuiError::internal("Open the main window to confirm the schema"))?;
        let request_id = state::new_id();
        let schema = target.store_schema;
        let (send, receive) = tokio::sync::oneshot::channel();
        let send = std::sync::Mutex::new(Some(send));
        let listener_id = request_id.clone();
        let listener = window.listen("git-strata-schema-answer", move |event| {
            if let Ok(answer) = serde_json::from_str::<Answer>(event.payload())
                && answer.request_id == listener_id
                && (answer.acknowledgement.is_none() || answer.acknowledgement == Some(schema))
                && let Some(send) = send.lock().unwrap().take()
            {
                let _ = send.send(answer.acknowledgement.is_some());
            }
        });
        let presentation = self.app.state::<state::Presentation>();
        presentation.set_consent(Some(json!({"request_id":request_id,"from_schema":current,"to_schema":schema,"version":target.version})));
        let _ = window.emit("strata://schema-pending", ());
        let result = tokio::time::timeout(Duration::from_secs(600), receive).await;
        window.unlisten(listener);
        presentation.set_consent(None);
        let _ = window.emit("strata://schema-pending", ());
        Ok(matches!(result, Ok(Ok(true))))
    }
}

#[cfg(target_os = "macos")]
const RPC_BUDGET: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
pub(super) async fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, GuiError> + Send + 'static,
) -> Result<T, GuiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| GuiError::internal(format!("Updater worker failed: {e}")))?
}

#[cfg(target_os = "macos")]
async fn rpc_at<T: Send + 'static>(
    socket: &Path,
    call: impl FnOnce(&Path) -> Result<T, GuiError> + Send + 'static,
) -> Result<T, GuiError> {
    let socket = socket.to_owned();
    background(move || call(&socket)).await
}

#[cfg(target_os = "macos")]
async fn status_at(socket: &Path) -> Result<Value, GuiError> {
    rpc_at(socket, |socket| {
        strata_client::call_within(
            socket,
            "daemon_status",
            json!({"verify": false}),
            strata_client::Kind::Read,
            RPC_BUDGET,
        )
        .map_err(Into::into)
    })
    .await
}

#[cfg(target_os = "macos")]
async fn acquire_at(socket: &Path, expected: txn::DaemonIdentity) -> Result<String, GuiError> {
    rpc_at(socket, move |socket| {
        strata_client::drain_acquire(socket, RPC_BUDGET, |status| {
            strata_client::require_feature(status, "daemon.drain").is_ok()
                && status["pid"] == expected.pid
                && status["version"] == expected.version
                && status["started"]
                    .as_str()
                    .and_then(|s| strata_types::serde_rfc3339::parse(s).ok())
                    == Some(expected.started)
        })
        .map_err(Into::into)
    })
    .await
}

#[cfg(target_os = "macos")]
async fn drain_at(
    socket: &Path,
    action: install::DrainAction<'_>,
) -> Result<Option<String>, GuiError> {
    use install::DrainAction;
    let (lease_id, renew, seal) = match action {
        DrainAction::Acquire { expected, .. } => {
            return acquire_at(socket, expected.clone()).await.map(Some);
        }
        DrainAction::Renew { lease_id, .. } => (lease_id, true, false),
        DrainAction::Release { lease_id } => (lease_id, false, false),
        DrainAction::SealStop { lease_id } => (lease_id, false, true),
    };
    let id = lease_id.to_owned();
    rpc_at(socket, move |socket| {
        if renew {
            return strata_client::drain_on(socket, Some(&id), RPC_BUDGET)
                .map(Some)
                .map_err(Into::into);
        }
        if seal {
            strata_client::drain_seal(socket, &id, "stop", RPC_BUDGET)
        } else {
            strata_client::drain_cancel(socket, &id, RPC_BUDGET)
        }
        .map(|()| None)
        .map_err(Into::into)
    })
    .await
}

#[cfg(target_os = "macos")]
fn read_regular(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Expected a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "File is too large",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
impl bundle::BundleOps for Installed {
    fn extract(&self, bytes: &[u8], staging: &Path) -> Result<PathBuf, GuiError> {
        self.bundles()?.extract(bytes, staging)
    }
    fn read_info_plist(&self, bundle: &Path) -> Result<bundle::BundleInfo, GuiError> {
        self.bundles()?.read_info_plist(bundle)
    }
    fn bundle_identity(&self, bundle: &Path) -> Result<Option<bundle::BundleIdentity>, GuiError> {
        self.bundles()?.bundle_identity(bundle)
    }
    fn zip_backup(&self, bundle: &Path, zip: &Path) -> Result<(), GuiError> {
        self.bundles()?.zip_backup(bundle, zip)
    }
    fn check_install_staging(&self) -> Result<(), GuiError> {
        self.bundles()?.check_install_staging()
    }
    fn install(&self, update: source::VerifiedUpdate) -> tauri_plugin_updater::Result<()> {
        self.bundles().map_err(io::Error::other)?.install(update)
    }
    fn restore_zip(&self, zip: &Path, bundle: &Path) -> Result<(), GuiError> {
        self.bundles()?.restore_zip(zip, bundle)
    }
    fn restore_zip_if(
        &self,
        zip: &Path,
        bundle: &Path,
        decide: &dyn Fn(&bundle::BundleInfo) -> Result<bundle::RestoreDecision, GuiError>,
    ) -> Result<(), GuiError> {
        self.bundles()?.restore_zip_if(zip, bundle, decide)
    }
    fn exchange_previous(
        &self,
        zip: &Path,
        path: &Path,
        guard: &dyn Fn(&bundle::VerifiedBundle) -> Result<bundle::VerifiedBundle, GuiError>,
        admit: &dyn Fn(&bundle::ExchangeEvidence) -> Result<(), GuiError>,
    ) -> Result<(), bundle::ExchangeError> {
        self.bundles()
            .map_err(bundle::ExchangeError::NotActivated)?
            .exchange_previous(zip, path, guard, admit)
    }
    fn discard_exchange(&self, path: &Path, evidence: &bundle::ExchangeEvidence, swapped: bool) {
        if let Ok(ops) = self.bundles() {
            ops.discard_exchange(path, evidence, swapped);
        }
    }
    fn verify_previous_archive(&self, path: &Path, txn: &txn::Txn) -> Result<(), GuiError> {
        self.bundles()?.verify_previous_archive(path, txn)
    }
    fn restore_kept(&self, kept: &Path, bundle: &Path, txn: &txn::Txn) -> Result<(), GuiError> {
        self.bundles()?.restore_kept(kept, bundle, txn)
    }
    fn codesign_verify(&self, bundle: &Path) -> Result<(), GuiError> {
        self.bundles()?.codesign_verify(bundle)
    }
}

#[cfg(target_os = "macos")]
impl install::InstallHost for Installed {
    fn validate(&self, bundle: &Path, plist: &Path, program: &Path) -> Result<(), GuiError> {
        self.verify_scope()?;
        self.data_root()?;
        if bundle != self.bundle
            || plist != plist_for_home(&self.home)
            || program != bundle.join("Contents/MacOS/git-stratad")
        {
            return Err(GuiError::internal(
                "The update does not target this login home's app and LaunchAgent",
            ));
        }
        let bytes = read_regular(plist, 1 << 20)
            .map_err(|e| GuiError::internal(format!("Cannot read the LaunchAgent: {e}")))?;
        move_daemon::validate_template(&bundle::decode_move_plist(&bytes)?, program)
    }
    fn prepare<'a>(
        &'a self,
        target: &'a str,
        acknowledgement: Option<u32>,
        live: Option<u32>,
        rollback_zip: &'a Path,
        progress: &'a mut install::Progress<'_>,
    ) -> UpdateFuture<'a, Result<install::PreparedUpdate, GuiError>> {
        Box::pin(async move {
            let source = check::checked_source().ok_or_else(|| {
                GuiError::new(
                    Code::Unsupported,
                    "Check for updates first",
                    json!({"reason":"update_source_unavailable"}),
                )
            })?;
            progress(state::UpdateState::Downloading { pct: None });
            let mut downloaded = 0u64;
            let update = source
                .download(target, &mut |chunk, total| {
                    downloaded = downloaded.saturating_add(chunk as u64);
                    let pct = total
                        .filter(|n| *n > 0)
                        .map(|n| downloaded.saturating_mul(100).saturating_div(n).min(100) as u8);
                    progress(state::UpdateState::Downloading { pct });
                })
                .await?;
            progress(state::UpdateState::VerifyingBundle);
            let installed = self.app.state::<Arc<Installed>>().inner().clone();
            let target = target.to_owned();
            let rollback_zip = rollback_zip.to_owned();
            background(move || {
                let ops = installed.bundles()?;
                let info = ops.inspect_download(&update, &target, &installed.version)?;
                install::require_schema_ack(live, &info, acknowledgement)?;
                let pending = txn::TxnStore::new(cache::path(&installed.home)).load()?;
                if ops.save_previous(&installed.version, pending.as_ref())? != rollback_zip {
                    return Err(GuiError::internal("The rollback archive path changed"));
                }
                Ok(install::PreparedUpdate { update, info })
            })
            .await
        })
    }
    fn check_preconditions(&self, running: Option<&str>, bundled: &str) -> Result<(), GuiError> {
        let state = running.map_or(check::DaemonVersion::NotServing, |version| {
            check::DaemonVersion::Running(Some(version))
        });
        check::install_preconditions(state, Some(bundled))
    }
    fn daemon_version(&self) -> Result<String, GuiError> {
        use bundle::BundleOps as _;
        self.bundles()?.codesign_verify(&self.bundle)?;
        let output = self.daemon_version_output()?;
        let line = output.trim();
        line.strip_prefix("git-stratad ")
            .filter(|version| bundle::release_version(version).is_some())
            .map(str::to_owned)
            .ok_or_else(|| GuiError::internal("Cannot verify the bundled daemon version"))
    }
    fn confirm_schema<'a>(
        &'a self,
        live: Option<u32>,
        target: &'a bundle::BundleInfo,
    ) -> UpdateFuture<'a, Result<bool, GuiError>> {
        Box::pin(self.ask_schema(live, target))
    }
    fn restore_kept(&self, kept: &Path, bundle: &Path, txn: &txn::Txn) -> Result<(), GuiError> {
        bundle::BundleOps::restore_kept(self, kept, bundle, txn)
    }
    fn transaction_id(&self) -> Option<String> {
        self.app.state::<state::Presentation>().transaction_id()
    }
    fn rollback_scope(&self) -> Result<(PathBuf, PathBuf), GuiError> {
        Ok((self.config_dir().to_path_buf(), self.data_root()?))
    }
    fn verify_loaded_policy(&self, dump: &str) -> Result<(), GuiError> {
        if restart::has_restart_policy(dump) {
            return Ok(());
        }
        Err(GuiError::new(
            Code::Unsupported,
            "The loaded LaunchAgent needs the app's conditional restart policy",
            json!({"reason":"agent_keepalive_mismatch"}),
        ))
    }
}

#[cfg(target_os = "macos")]
impl install::InstallDaemon for Installed {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(self.status())
    }
    fn drain<'a>(
        &'a self,
        action: install::DrainAction<'a>,
    ) -> UpdateFuture<'a, Result<Option<String>, GuiError>> {
        Box::pin(drain_at(&self.socket, action))
    }
    fn quiescent(&self) -> Result<bool, GuiError> {
        install::socket_refusing_and_lock_free(&self.socket, &self.config_dir().join("daemon.lock"))
    }
    fn close_error(&self, after: SystemTime) -> Option<Value> {
        let bytes = read_regular(&self.config_dir().join("last_shutdown.json"), 65536).ok()?;
        let report: Value = serde_json::from_slice(&bytes).ok()?;
        let at = strata_types::serde_rfc3339::parse(report["at"].as_str()?).ok()?;
        (at >= after && report["mode"] == "stop" && !report["close_error"].is_null())
            .then(|| report["close_error"].clone())
    }
    fn retained_store_schema(&self) -> Option<u32> {
        self.retained_schema()
    }
}

#[cfg(target_os = "macos")]
impl move_daemon::MoveDrain for Installed {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(self.status())
    }
    fn acquire(&self, expected: &txn::DaemonIdentity) -> UpdateFuture<'_, Result<Value, GuiError>> {
        let expected = expected.clone();
        Box::pin(async move {
            acquire_at(&self.socket, expected)
                .await
                .map(|id| json!({"lease_id":id}))
        })
    }
    fn drain(&self, params: Value) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(async move {
            let id = params["lease_id"]
                .as_str()
                .ok_or_else(|| GuiError::internal("Move has no drain lease ID"))?
                .to_owned();
            rpc_at(&self.socket, move |socket| {
                if params["seal"] == true {
                    strata_client::drain_seal(socket, &id, "hold", RPC_BUDGET).map(|()| json!({}))
                } else if params["on"] == true {
                    strata_client::drain_on(socket, Some(&id), RPC_BUDGET)
                        .map(|id| json!({"lease_id":id}))
                } else {
                    strata_client::drain_cancel(socket, &id, RPC_BUDGET).map(|()| json!({}))
                }
                .map_err(Into::into)
            })
            .await
        })
    }
}

#[cfg(target_os = "macos")]
impl move_daemon::MovePlatform for Installed {
    fn bundle_version(&self) -> Result<String, GuiError> {
        install::InstallHost::daemon_version(self)
    }
    fn render_plist(&self, program: &Path) -> Result<Vec<u8>, GuiError> {
        exec::checked(&self.home, exec::Operation::PrintDaemonPlist { program })
    }
    fn lint_plist(&self, plist: &Path) -> Result<(), GuiError> {
        exec::checked(&self.home, exec::Operation::LintPlist { plist }).map(|_| ())
    }
    fn decode_plist(&self, bytes: &[u8]) -> Result<move_daemon::Plist, GuiError> {
        bundle::decode_move_plist(bytes)
    }
    fn encode_plist(&self, plist: &move_daemon::Plist) -> Result<Vec<u8>, GuiError> {
        bundle::encode_move_plist(plist)
    }
    fn store_schema(&self) -> Option<u32> {
        self.retained_schema()
    }
    fn socket_refusing_and_lock_free(&self) -> Result<bool, GuiError> {
        install::InstallDaemon::quiescent(self)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn start(app: &AppHandle) {
    let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
    app.manage(lifecycle.clone());
    app.manage(crate::service::Starter::from_env().with_lifecycle(lifecycle.clone()));
    app.manage(state::Presentation::default());
    let Ok(paths) = setup::SetupPaths::installed() else {
        return;
    };
    let socket = crate::socket::socket_for_home(&paths.home);
    let installed = Arc::new(Installed {
        home: paths.home,
        bundle: paths.bundle,
        version: app.package_info().version.to_string(),
        socket,
        app: app.clone(),
    });
    app.manage(installed.clone());
    let service = Arc::new(Launchctl::new());
    let clock = Arc::new(restart::SystemClock);
    let installer = install::InstallCoordinator::new(
        install::InstallPaths {
            bundle: installed.bundle.clone(),
            plist: plist_for_home(&installed.home),
            app_support: installed.support(),
            app_version: installed.version.clone(),
        },
        install::InstallAdapters {
            service: service.clone(),
            daemon: installed.clone(),
            bundles: installed.clone(),
            host: installed.clone(),
            clock: clock.clone(),
        },
        lifecycle,
    );
    app.manage(installer);
    if let Ok(mover) = move_daemon::MoveDaemon::new(
        &installed.home,
        scope::login_home().as_deref(),
        &installed.bundle,
        installed.version.clone(),
        move_daemon::MoveAdapters {
            service,
            platform: installed.clone(),
            daemon: installed.clone(),
            bundle: installed.clone(),
            clock,
        },
    ) {
        app.manage(mover);
    }
    app.manage(setup::SetupRuntime {
        exec: Arc::new(exec::FixedRunner),
        bundles: installed.clone(),
    });
    app.manage(cli_link::AdminCliLinks(Arc::new(exec::FixedRunner)));
    let host = installed.clone();
    app.manage(restart::RestartController::new(move |home| {
        use bundle::BundleOps as _;
        if home != host.home {
            return Err(GuiError::internal("The restart login home changed"));
        }
        host.codesign_verify(&host.bundle)?;
        let info = host.read_info_plist(&host.bundle)?;
        Ok(restart::RestartBundle {
            daemon_version_output: host.daemon_version_output()?,
            store_schema: info.store_schema,
        })
    }));
    if let Some(window) = app.get_webview_window("main") {
        let handle = app.clone();
        window.listen("git-strata-update-skill", move |event| {
            let Ok(id) = serde_json::from_str::<u32>(event.payload()) else {
                return;
            };
            let app = handle.clone();
            tauri::async_runtime::spawn(async move {
                if !app.state::<state::Presentation>().can_update_skill() {
                    return;
                }
                let home = app.state::<Arc<Installed>>().home.clone();
                let result = background(move || {
                    exec::checked(&home, exec::Operation::UpdateSkills)
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                })
                .await;
                let _ = app.emit_to(
                    "main",
                    "strata://update-skill-result",
                    json!({"id": id, "result": result}),
                );
            });
        });
    }
    if app
        .state::<install::InstallCoordinator>()
        .pending()
        .is_ok_and(|txn| txn.is_none())
    {
        return;
    }
    // Mark reconciliation synchronously so the first WebView read cannot offer an install.
    let app_handle = app.clone();
    let presentation = app.state::<state::Presentation>();
    presentation.begin();
    if let Ok(doc) = cache::read_document(&cache::path(&installed.home))
        && let Some(txn) = doc.get("txn")
    {
        if let Some(id) = txn["id"].as_str() {
            presentation.restore_id(id);
        }
        if let Some(rollback) = txn.get("rollback") {
            if let Some(start) = rollback["start_requested_ms"]
                .as_u64()
                .or_else(|| rollback["stop_requested_ms"].as_u64())
            {
                presentation.restore_timing(start, None);
            }
        } else if let Some(start) = txn["start_requested_ms"].as_u64() {
            presentation.restore_timing(start, txn["permission_wait_elapsed_ms"].as_u64());
        }
    }
    presentation.progress(state::UpdateState::Reconciling);
    tauri::async_runtime::spawn(async move {
        let installer = app_handle.state::<install::InstallCoordinator>();
        let view = app_handle.state::<state::Presentation>();
        let result = installer.reconcile(&mut |state| view.progress(state)).await;
        view.finish(result.as_ref().err().cloned());
        if matches!(result, Ok(Some(install::InstallOutcome::Restored)))
            && installer.rollback_relaunch_pending().unwrap_or(false)
            && let Some(window) = app_handle.get_webview_window("main")
        {
            let handle = app_handle.clone();
            let _ = window.run_on_main_thread(move || handle.restart());
        }
    });
}

#[cfg(target_os = "macos")]
impl Installed {
    pub(crate) async fn versions(self: &Arc<Self>, move_pending: bool) -> Result<Value, GuiError> {
        let installed = self.clone();
        background(move || {
            let launchd = Launchctl::new();
            let plist = plist_for_home(&installed.home);
            let job = launchd.print(AGENT_LABEL)?;
            let mut versions = json!({"cli": cli_link::LinkPaths::installed(installed.bundle.clone()).labels().unwrap_or_default()});
            if let Some(dump) = job {
                scope::owned(&dump, &plist)?;
                let program = crate::service::launchctl::printed_program(&dump)
                    .filter(|program| Path::new(program).is_absolute())
                    .ok_or_else(|| GuiError::internal("Cannot verify the loaded LaunchAgent program"))?;
                versions["program"] = json!(program);
                if move_pending {
                    versions["move_mode"] = json!("resume");
                } else if !launchd.print_disabled(AGENT_LABEL)? {
                    let bundled = installed.bundle.join("Contents/MacOS/git-stratad");
                    if scope::anchored(Path::new(program)) == scope::anchored(&bundled) {
                        versions["can_link_cli"] = json!(true);
                    } else if crate::service::launchctl::printed_pid(&dump) == Some(None)
                        && fs::metadata(program).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                    {
                        versions["move_mode"] = json!("program_missing");
                    } else if move_daemon::formula_program(Path::new(program)) {
                        versions["move_mode"] = json!("formula");
                    }
                }
            } else if move_pending {
                versions["move_mode"] = json!("resume");
            } else if fs::symlink_metadata(&plist).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
                versions["can_setup"] = json!(true);
            }
            Ok(versions)
        }).await
    }

    pub(crate) async fn rollback_scope(self: &Arc<Self>) -> Result<(PathBuf, PathBuf), GuiError> {
        let installed = self.clone();
        background(move || Ok((installed.config_dir().to_path_buf(), installed.data_root()?))).await
    }
}

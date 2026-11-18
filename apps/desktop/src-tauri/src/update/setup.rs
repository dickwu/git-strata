//! First-time setup of the bundled daemon (version-upgrade design §6.3).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};
use strata_types::{Code, Config, RepoName};
use tauri::{Manager, WebviewWindow, ipc::Channel};

use super::{bundle::BundleOps, exec::Operation, state::UpdateState};
use crate::daemon::Read;
use crate::error::GuiError;
use crate::service::{AGENT_LABEL, ServiceManager, launchctl::Launchctl, plist_for_home, scope};
use crate::socket::socket_for_home;
use crate::state::AppState;

pub(crate) const APP_BUNDLE: &str = "/Applications/GIT STRATA.app";
const SETUP_BUDGET: Duration = Duration::from_secs(150);
const PROBE_INTERVAL: Duration = Duration::from_millis(500);
static SETUP: tokio::sync::Mutex<Option<PendingSetup>> = tokio::sync::Mutex::const_new(None);
static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);
static NEXT_REVIEW: AtomicUsize = AtomicUsize::new(1);

/// The runner in update/exec.rs supplies stdout for the fixed operations and
/// returns an error on nonzero exit. It must set the verified HOME with
/// exec::environment after clearing the inherited environment.
pub trait SetupExecutor: Send + Sync {
    fn run(&self, home: &Path, operation: Operation<'_>) -> Result<Vec<u8>, GuiError>;
}

/// Managed by the updater integration when the production runner and bundle
/// reader are available.
#[derive(Clone)]
pub struct SetupRuntime {
    pub exec: Arc<dyn SetupExecutor>,
    pub bundles: Arc<dyn BundleOps>,
}

pub(crate) struct SetupPaths {
    pub home: PathBuf,
    pub bundle: PathBuf,
}

impl SetupPaths {
    pub(crate) fn installed() -> Result<Self, GuiError> {
        let bundle = PathBuf::from(APP_BUNDLE);
        let desktop = bundle.join("Contents/MacOS/git-strata-desktop");
        if cfg!(debug_assertions) || std::env::current_exe().ok().as_ref() != Some(&desktop) {
            return Err(failure(
                Code::Unsupported,
                "setup_bundle_location",
                "Move GIT STRATA to Applications, then open it again.",
            ));
        }
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(GuiError::no_home)?;
        let home = scope::lifecycle_scope(&home, scope::login_home().as_deref())?;
        Ok(Self { home, bundle })
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join("Library/Application Support/git-strata")
    }

    fn config(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    fn program(&self) -> PathBuf {
        self.bundle.join("Contents/MacOS/git-stratad")
    }
}

#[derive(Clone)]
struct FolderPicks {
    root: PathBuf,
    repos: BTreeMap<String, PathBuf>,
}

struct PendingSetup {
    id: String,
    picks: Option<FolderPicks>,
    config: String,
    target: u32,
}

impl PendingSetup {
    fn issue(
        pending: &mut Option<Self>,
        picks: Option<FolderPicks>,
        config: String,
        target: u32,
    ) -> GuiError {
        let id = NEXT_REVIEW.fetch_add(1, Ordering::Relaxed).to_string();
        let error = GuiError::new(
            Code::NotConfirmed,
            "Acknowledge the store migration warning to set up the daemon.",
            json!({
                "reason": "setup_schema_acknowledgement",
                "from_schema": null,
                "target_schema": target,
                "review_id": id,
            }),
        );
        *pending = Some(Self {
            id,
            picks,
            config,
            target,
        });
        error
    }

    fn take_acknowledged(
        pending: &mut Option<Self>,
        target: Option<u32>,
        id: Option<&str>,
    ) -> Result<Option<Self>, GuiError> {
        if target.is_none() && id.is_none() {
            *pending = None;
            return Ok(None);
        }
        if pending
            .as_ref()
            .is_some_and(|review| target == Some(review.target) && id == Some(review.id.as_str()))
        {
            return Ok(pending.take());
        }
        Err(stale_review())
    }
}

fn stale_review() -> GuiError {
    failure(
        Code::NotConfirmed,
        "setup_review_stale",
        "This setup review expired. Start setup again to review the selected folders and store.",
    )
}

fn failure(code: Code, reason: &str, message: impl Into<String>) -> GuiError {
    GuiError::new(code, message, json!({ "reason": reason }))
}

fn file_error(path: &Path, error: io::Error) -> GuiError {
    failure(
        Code::InvalidInput,
        "setup_file",
        format!("{}: {error}", path.display()),
    )
}

/// Treat symlinks as existing entries, even if their target is missing.
fn entry_exists(path: &Path) -> Result<bool, GuiError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(file_error(path, e)),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, GuiError> {
    if entry_exists(path)? {
        fs::read_to_string(path)
            .map(Some)
            .map_err(|e| file_error(path, e))
    } else {
        Ok(None)
    }
}

/// Carry the template's header comments only. Repository sample blocks can
/// contain install commands, push settings and placeholder paths.
fn render_config(template: &str, picks: &FolderPicks) -> Result<String, GuiError> {
    fn toml_path(path: &Path) -> Result<String, GuiError> {
        let value = path.to_str().ok_or_else(|| {
            failure(
                Code::InvalidInput,
                "setup_path_encoding",
                "Folder paths must be valid UTF-8.",
            )
        })?;
        Ok(json!(value).to_string().replace('\u{7f}', "\\u007F"))
    }

    let mut text = String::new();
    for line in template
        .lines()
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
    {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&format!("[paths]\nroot = {}\n", toml_path(&picks.root)?));
    for (name, path) in &picks.repos {
        let name = RepoName::try_from(name.clone())?;
        text.push_str(&format!(
            "\n[repo.{}]\npath = {}\ndefault_branch = \"main\"\ndeps_dirs = []\npublish = {{ branch = \"main\", push = false, confirm = true }}\n",
            json!(name.as_str()), toml_path(path)?,
        ));
    }
    Config::parse(&text)?;
    Ok(text)
}

/// A complete scratch file in the destination directory. Validation and launchd
/// preflight can fail before the final name becomes visible.
struct StagedFile(PathBuf);

impl StagedFile {
    fn create(path: &Path, bytes: &[u8], mode: u32) -> Result<Self, GuiError> {
        let dir = path
            .parent()
            .ok_or_else(|| GuiError::internal("missing setup directory"))?;
        fs::create_dir_all(dir).map_err(|e| file_error(dir, e))?;
        let temp = dir.join(format!(
            ".git-strata-setup-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(|e| file_error(&temp, e))?;
        let staged = Self(temp);
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| file_error(&staged.0, e))?;
        Ok(staged)
    }

    fn publish(&self, path: &Path) -> Result<(), GuiError> {
        fs::hard_link(&self.0, path).map_err(|e| file_error(path, e))
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Deserialize)]
struct StoreState {
    user_version: u32,
    root: PathBuf,
}

fn require_valid_config(output: &[u8]) -> Result<(), GuiError> {
    let reply: Value = serde_json::from_slice(output)
        .map_err(|e| GuiError::internal(format!("invalid check-config reply: {e}")))?;
    if reply["ok"].as_bool() != Some(true) {
        return Err(serde_json::from_value(reply["error"].clone())
            .unwrap_or_else(|_| GuiError::internal("check-config did not confirm validation")));
    }
    Ok(())
}

fn schema_confirmation_needed(
    paths: &SetupPaths,
    picks: Option<&FolderPicks>,
    target: u32,
) -> Result<bool, GuiError> {
    let Some(picks) = picks else {
        return Ok(true);
    };
    // The checker validates the config only. Inspect the selected root's
    // store directory entry without following it or opening any store files.
    if matches!(
        fs::symlink_metadata(picks.root.join("store")),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    ) {
        return Ok(false);
    }
    // A same-path restore can leave an old marker. Use it only to refuse
    // an apparent downgrade. A retained store always needs confirmation.
    let recorded = read_optional(&paths.config_dir().join("store_state.json"))
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<StoreState>(&text).ok())
        .filter(|state| state.root == picks.root)
        .map(|state| state.user_version);
    if let Some(schema) = recorded.filter(|version| *version > target) {
        return Err(GuiError::new(
            Code::Unsupported,
            format!(
                "The last recorded store version for this folder is v{schema}. Update GIT STRATA before starting its daemon."
            ),
            json!({ "reason": "setup_schema_downgrade", "from_schema": schema, "target_schema": target }),
        ));
    }
    Ok(true)
}

fn require_socket_free(home: &Path) -> Result<(), GuiError> {
    let socket = socket_for_home(home);
    match UnixStream::connect(&socket) {
        Ok(_) => Err(failure(
            Code::Exists,
            "setup_daemon_running",
            "A daemon is listening on the login user's socket. Use the Daemon panel to manage it.",
        )),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(file_error(&socket, e)),
    }
}

/// An unmanaged daemon takes this flock before it binds the socket.
fn lock_daemon_instance(paths: &SetupPaths) -> Result<File, GuiError> {
    let path = paths.config_dir().join("daemon.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|e| file_error(&path, e))?;
    if !file.metadata().map_err(|e| file_error(&path, e))?.is_file() {
        return Err(file_error(
            &path,
            io::Error::new(io::ErrorKind::InvalidInput, "expected a daemon lock file"),
        ));
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(failure(
            Code::Exists,
            "setup_daemon_running",
            "Another daemon is holding the login user's daemon lock.",
        )),
        Err(TryLockError::Error(e)) => Err(file_error(&path, e)),
    }
}

fn config_changed() -> GuiError {
    failure(
        Code::Busy,
        "setup_config_changed",
        "The config changed during setup. Review it and try again.",
    )
}

fn require_fresh(paths: &SetupPaths, service: &dyn ServiceManager) -> Result<(), GuiError> {
    let journal = paths
        .home
        .join("Library/Application Support/io.github.dickwu.git-strata/update.json");
    if let Some(text) = read_optional(&journal)? {
        let cache: Value = serde_json::from_str(&text)
            .map_err(|e| file_error(&journal, io::Error::new(io::ErrorKind::InvalidData, e)))?;
        if !cache.is_object() || !cache["txn"].is_null() || !cache["move"].is_null() {
            return Err(failure(
                Code::Busy,
                "setup_update_in_progress",
                "Finish the daemon update or move before setting up the daemon.",
            ));
        }
    }
    let plist = plist_for_home(&paths.home);
    if let Some(job) = service.print(AGENT_LABEL)? {
        scope::owned(&job, &plist)?;
        return Err(failure(
            Code::Exists,
            "setup_agent_loaded",
            "The daemon is already registered.",
        ));
    }
    if entry_exists(&plist)? {
        return Err(failure(
            Code::Exists,
            "setup_plist_exists",
            "A LaunchAgent plist already exists. Use Start daemon or Move the daemon into the app.",
        ));
    }
    Ok(())
}

struct Setup<'a> {
    paths: &'a SetupPaths,
    service: &'a dyn ServiceManager,
    exec: &'a dyn SetupExecutor,
}

impl Setup<'_> {
    fn bootstrap(
        &self,
        picks: Option<FolderPicks>,
        target_schema: u32,
        pending: &mut Option<PendingSetup>,
        socket_free: impl FnOnce() -> Result<(), GuiError>,
        starting: impl FnOnce(),
    ) -> Result<(), GuiError> {
        require_fresh(self.paths, self.service)?;
        let config_path = self.paths.config();
        let (text, staged_config) = match (read_optional(&config_path)?, &picks) {
            (Some(text), None) => (text, None),
            (None, Some(picks)) => {
                let template = self.paths.bundle.join("Contents/Resources/config.toml");
                let template =
                    fs::read_to_string(&template).map_err(|e| file_error(&template, e))?;
                let text = render_config(&template, picks)?;
                let config_dir = self.paths.config_dir();
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&config_dir)
                    .map_err(|e| file_error(&config_dir, e))?;
                let staged = StagedFile::create(&config_path, text.as_bytes(), 0o600)?;
                (text, Some(staged))
            }
            _ => return Err(config_changed()),
        };

        let home = &self.paths.home;
        let checked_path = staged_config
            .as_ref()
            .map_or(config_path.as_path(), |staged| &staged.0);
        let report = self.exec.run(
            home,
            Operation::CheckDaemonConfig {
                config: checked_path,
            },
        )?;
        require_valid_config(&report)?;
        let reviewed = pending.take();
        if reviewed.as_ref().is_some_and(|previous| {
            previous.config != text
                || previous.picks.is_some() != picks.is_some()
                || previous.target != target_schema
        }) {
            return Err(stale_review());
        }
        let mut check_schema = || {
            if schema_confirmation_needed(self.paths, picks.as_ref(), target_schema)?
                && reviewed.is_none()
            {
                return Err(PendingSetup::issue(
                    pending,
                    picks.clone(),
                    text.clone(),
                    target_schema,
                ));
            }
            Ok(())
        };
        check_schema()?;

        let program = self.paths.program();
        let plist = self
            .exec
            .run(home, Operation::PrintDaemonPlist { program: &program })?;
        let plist_path = plist_for_home(home);
        let staged_plist = StagedFile::create(&plist_path, &plist, 0o644)?;
        require_fresh(self.paths, self.service)?;
        if self.service.print_disabled(AGENT_LABEL)? {
            // Keep the exact rendered plist private until enable succeeds.
            // An interrupted enable leaves setup eligible to retry.
            self.service.enable(AGENT_LABEL)?;
            require_fresh(self.paths, self.service)?;
        }
        if read_optional(checked_path)?.as_ref() != Some(&text) {
            return Err(config_changed());
        }
        let instance = lock_daemon_instance(self.paths)?;
        socket_free()?;
        // A store may have appeared while the plist was being prepared.
        check_schema()?;
        if let Some(staged) = staged_config {
            staged.publish(&config_path)?;
        }
        staged_plist.publish(&plist_path)?;
        drop(instance);
        starting();
        self.service.bootstrap(&plist_path)
    }
}

pub(crate) async fn run(
    window: &WebviewWindow,
    acknowledgement: Option<u32>,
    review_id: Option<&str>,
    progress: Channel<UpdateState>,
) -> Result<(), GuiError> {
    let mut setup = SETUP.try_lock().map_err(|_| {
        failure(
            Code::Busy,
            "setup_in_progress",
            "Daemon setup is already in progress.",
        )
    })?;
    let paths = SetupPaths::installed()?;
    let runtime = window
        .try_state::<SetupRuntime>()
        .ok_or_else(|| {
            failure(
                Code::Unsupported,
                "setup_runtime_unavailable",
                "This build cannot run bundled daemon setup.",
            )
        })?
        .inner()
        .clone();
    let mut pending = PendingSetup::take_acknowledged(&mut setup, acknowledgement, review_id)?;
    let service = Arc::new(Launchctl::new());
    let (paths, kept) = tokio::task::spawn_blocking({
        let service = service.clone();
        move || {
            require_fresh(&paths, service.as_ref())?;
            require_socket_free(&paths.home)?;
            entry_exists(&paths.config()).map(|kept| (paths, kept))
        }
    })
    .await
    .map_err(|e| GuiError::internal(e.to_string()))??;
    let picks = match &pending {
        Some(entry) if entry.picks.is_none() == kept => entry.picks.clone(),
        Some(_) => return Err(config_changed()),
        None if kept => None,
        None => Some(pick_folders(window).await?),
    };

    let version = window.package_info().version.to_string();
    let (paths, version, result, confirmation) = tokio::task::spawn_blocking({
        let service = service.clone();
        let progress = progress.clone();
        move || {
            runtime.bundles.codesign_verify(&paths.bundle)?;
            let bundle = runtime.bundles.read_info_plist(&paths.bundle)?;
            let daemon_version = runtime.exec.run(&paths.home, Operation::DaemonVersion)?;
            if bundle.version != version
                || String::from_utf8_lossy(&daemon_version).trim()
                    != format!("git-stratad {version}")
            {
                return Err(failure(
                    Code::InvalidInput,
                    "setup_bundle_version",
                    "The bundled daemon version does not match this copy of GIT STRATA.",
                ));
            }
            let result = Setup {
                paths: &paths,
                service: service.as_ref(),
                exec: runtime.exec.as_ref(),
            }
            .bootstrap(
                picks,
                bundle.store_schema,
                &mut pending,
                || require_socket_free(&paths.home),
                || {
                    let _ = progress.send(UpdateState::StartingDaemon);
                },
            );
            Ok::<_, GuiError>((paths, version, result, pending))
        }
    })
    .await
    .map_err(|e| GuiError::internal(e.to_string()))??;
    *setup = confirmation;
    result?;

    let state = window.state::<AppState>();
    let result = wait_for_daemon(state.inner(), service, &paths, &version, &progress).await;
    let _ = progress.send(match &result {
        Ok(()) => UpdateState::Done,
        Err(e) => UpdateState::DaemonFailed {
            reason: e.reason().unwrap_or("setup_verify").into(),
        },
    });
    result
}

fn verified_daemon(status: &Value, job: &str, version: &str, program: &Path) -> bool {
    let mut depth = 0;
    let mut job_program = None;
    let mut job_pid = None;
    for line in job.lines().map(str::trim) {
        if line.starts_with('}') {
            depth -= 1;
        } else if line.ends_with('{') {
            depth += 1;
        } else if depth == 1 {
            if let Some(path) = line.strip_prefix("program = ") {
                job_program = Some(Path::new(path));
            } else if let Some(pid) = line.strip_prefix("pid = ") {
                job_pid = pid.parse::<u64>().ok().filter(|pid| *pid > 0);
            }
        }
    }
    status["version"].as_str() == Some(version)
        && job_program == Some(program)
        && job_pid.is_some()
        && status["pid"].as_u64() == job_pid
}

fn verification_timeout(last_error: Option<String>) -> GuiError {
    failure(
        Code::Busy,
        "setup_verify_timeout",
        last_error.unwrap_or_else(|| {
            "No bundled daemon answered within the setup verification window.".into()
        }),
    )
}

async fn wait_for_daemon(
    state: &AppState,
    service: Arc<dyn ServiceManager>,
    paths: &SetupPaths,
    version: &str,
    progress: &Channel<UpdateState>,
) -> Result<(), GuiError> {
    let waiting = UpdateState::WaitingDaemon {
        since: SystemTime::now(),
    };
    let _ = progress.send(waiting.clone());
    let mut waiting_for_permission = false;
    let mut remaining = SETUP_BUDGET;
    let program = paths.program();
    let plist = plist_for_home(&paths.home);
    let mut last_error = None;
    loop {
        let started = Instant::now();
        let probe = state.read(Read::DaemonStatus);
        let status = if waiting_for_permission {
            probe.await
        } else {
            tokio::time::timeout(remaining, probe)
                .await
                .map_err(|_| verification_timeout(last_error.take()))?
        };
        let permission = status.as_ref().err().is_some_and(|e| {
            e.reason() == Some("store_unavailable")
                && matches!(
                    e.detail["last_error"]["detail"]["errno"].as_i64(),
                    Some(1 | 4 | 13)
                )
        });
        if waiting_for_permission != permission {
            let _ = progress.send(if permission {
                UpdateState::WaitingPermission
            } else {
                waiting.clone()
            });
            waiting_for_permission = permission;
        }
        match status {
            Ok(reply) => {
                let service = service.clone();
                let job = tokio::task::spawn_blocking(move || service.print(AGENT_LABEL));
                let budget = remaining.saturating_sub(started.elapsed());
                match tokio::time::timeout(budget, job)
                    .await
                    .map_err(|_| verification_timeout(last_error.take()))?
                    .map_err(|e| GuiError::internal(e.to_string()))?
                {
                    Ok(Some(job)) => {
                        scope::owned(&job, &plist)?;
                        if verified_daemon(&reply, &job, version, &program) {
                            return Ok(());
                        }
                        last_error = Some(
                            "The answering daemon does not match the bundled launch agent."
                                .to_owned(),
                        );
                    }
                    Err(e) => last_error = Some(e.message),
                    Ok(None) => {}
                }
            }
            Err(e) if e.reason() == Some("store_unavailable") => last_error = Some(e.message),
            Err(_) => {}
        }
        if !permission && remaining <= started.elapsed() {
            return Err(verification_timeout(last_error));
        }
        let pause = if permission {
            PROBE_INTERVAL
        } else {
            PROBE_INTERVAL.min(remaining.saturating_sub(started.elapsed()))
        };
        tokio::time::sleep(pause).await;
        if !permission {
            remaining = remaining.saturating_sub(started.elapsed());
        }
    }
}

async fn pick_folders(window: &WebviewWindow) -> Result<FolderPicks, GuiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = tx.send(native_picks());
        })
        .map_err(|e| GuiError::internal(e.to_string()))?;
    rx.await.map_err(|e| GuiError::internal(e.to_string()))?
}

#[cfg(target_os = "macos")]
fn native_picks() -> Result<FolderPicks, GuiError> {
    use objc2::{
        MainThreadMarker, class, msg_send,
        rc::Retained,
        runtime::{AnyObject, Bool},
    };
    use objc2_foundation::{NSArray, NSString};

    fn choose(message: &str, repos: bool) -> Result<Vec<PathBuf>, GuiError> {
        let _main = MainThreadMarker::new()
            .ok_or_else(|| GuiError::internal("folder picker needs the main thread"))?;
        let message = NSString::from_str(message);
        // SAFETY: NSOpenPanel and its URLs are used on the AppKit main
        // thread. These are AppKit/Foundation selectors with their SDK types.
        unsafe {
            let panel: Retained<AnyObject> = msg_send![class!(NSOpenPanel), openPanel];
            let _: () = msg_send![&*panel, setMessage: &*message];
            let _: () = msg_send![&*panel, setCanChooseDirectories: Bool::YES];
            let _: () = msg_send![&*panel, setCanChooseFiles: Bool::NO];
            let _: () = msg_send![&*panel, setAllowsMultipleSelection: Bool::new(repos)];
            let _: () = msg_send![&*panel, setCanCreateDirectories: Bool::new(!repos)];
            let result: isize = msg_send![&*panel, runModal];
            if result == 0 {
                return Err(failure(
                    Code::NotConfirmed,
                    "setup_cancelled",
                    "Daemon setup was cancelled.",
                ));
            }
            if result != 1 {
                return Err(failure(
                    Code::Unknown,
                    "setup_folder_picker",
                    "macOS could not open the folder picker.",
                ));
            }
            let urls: Retained<NSArray<AnyObject>> = msg_send![&*panel, URLs];
            urls.to_vec()
                .into_iter()
                .map(|url| {
                    let path: Option<Retained<NSString>> = msg_send![&*url, path];
                    path.map(|path| PathBuf::from(path.to_string()))
                        .ok_or_else(|| {
                            failure(
                                Code::InvalidInput,
                                "setup_folder_picker",
                                "Select a folder on disk.",
                            )
                        })
                })
                .collect()
        }
    }

    let root = choose(
        "Choose a new or empty data folder, such as .strata on the same volume as your repositories, or select an existing GIT STRATA data folder.",
        false,
    )?
        .into_iter()
        .next()
        .ok_or_else(|| GuiError::internal("no data folder was selected"))?;
    let repos = choose(
        "Choose one or more Git repository folders on the same volume.",
        true,
    )?;
    let root = root.canonicalize().map_err(|e| file_error(&root, e))?;
    let mut named = BTreeMap::new();
    for repo in repos {
        let path = repo.canonicalize().map_err(|e| file_error(&repo, e))?;
        let name = path
            .file_name()
            .and_then(|part| part.to_str())
            .ok_or_else(|| {
                failure(
                    Code::InvalidInput,
                    "setup_repo_name",
                    "Select a named repository folder.",
                )
            })?
            .to_owned();
        if named.insert(name, path).is_some() {
            return Err(failure(
                Code::InvalidInput,
                "setup_repo_name",
                "Choose repository folders with distinct names.",
            ));
        }
    }
    Ok(FolderPicks { root, repos: named })
}

#[cfg(not(target_os = "macos"))]
fn native_picks() -> Result<FolderPicks, GuiError> {
    Err(failure(
        Code::Unsupported,
        "setup_folder_picker",
        "Daemon setup requires macOS.",
    ))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use super::*;

    const TEMPLATE: &str = "# Example config\n\n[paths]\nroot = \"/Users/you/code/.strata\"\n[repo.web]\ninstall = \"bun install\"\n";

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<&'static str>>,
        check_fails: bool,
        disabled: bool,
        fail_enable_once: AtomicBool,
        change_config_after_check: bool,
    }

    impl SetupExecutor for Fake {
        fn run(&self, home: &Path, operation: Operation<'_>) -> Result<Vec<u8>, GuiError> {
            let (name, args) = operation.argv();
            assert!(name.ends_with("/git-stratad"));
            match args.as_slice() {
                [flag, config] if *flag == "--check-config" => {
                    let config = Path::new(config);
                    let dir = home.join("Library/Application Support/git-strata");
                    assert_eq!(config.parent(), Some(dir.as_path()));
                    if !dir.join("config.toml").exists() {
                        assert_ne!(config.file_name().unwrap(), "config.toml");
                    }
                    Config::parse(&fs::read_to_string(config).unwrap()).unwrap();
                    self.calls.lock().unwrap().push("check-config");
                    if self.check_fails {
                        return Err(failure(Code::InvalidInput, "check_config", "Bad folder"));
                    }
                    // This is the bundled checker's actual success envelope.
                    Ok(b"{\"ok\":true,\"error\":null}\n".to_vec())
                }
                [flag, program] if *flag == "--print-plist" => {
                    assert!(Path::new(program).ends_with("Contents/MacOS/git-stratad"));
                    self.calls.lock().unwrap().push("print-plist");
                    if self.change_config_after_check {
                        fs::write(
                            home.join("Library/Application Support/git-strata/config.toml"),
                            "changed",
                        )
                        .unwrap();
                    }
                    Ok(b"fake plist".to_vec())
                }
                _ => panic!("unexpected daemon invocation"),
            }
        }
    }

    impl ServiceManager for Fake {
        fn print(&self, label: &str) -> Result<Option<String>, GuiError> {
            assert_eq!(label, AGENT_LABEL);
            Ok(None)
        }

        fn kickstart(&self, _label: &str) -> Result<(), GuiError> {
            panic!("fresh setup must bootstrap")
        }

        fn print_disabled(&self, label: &str) -> Result<bool, GuiError> {
            assert_eq!(label, AGENT_LABEL);
            self.calls.lock().unwrap().push("print-disabled");
            Ok(self.disabled)
        }

        fn enable(&self, label: &str) -> Result<(), GuiError> {
            assert_eq!(label, AGENT_LABEL);
            self.calls.lock().unwrap().push("enable");
            if self.fail_enable_once.swap(false, Ordering::Relaxed) {
                return Err(failure(
                    Code::Busy,
                    "enable_failed",
                    "Could not enable label",
                ));
            }
            Ok(())
        }

        fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
            assert_eq!(fs::read(plist).unwrap(), b"fake plist");
            self.calls.lock().unwrap().push("bootstrap");
            Ok(())
        }
    }

    fn fixture(root: &Path) -> (SetupPaths, FolderPicks) {
        let paths = SetupPaths {
            home: root.join("home"),
            bundle: root.join("GIT STRATA.app"),
        };
        let resources = paths.bundle.join("Contents/Resources");
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("config.toml"), TEMPLATE).unwrap();
        let picks = FolderPicks {
            root: root.join("Strata \"data\""),
            repos: BTreeMap::from([("team.web".into(), root.join("team.web"))]),
        };
        fs::create_dir(&picks.root).unwrap();
        (paths, picks)
    }

    #[test]
    fn renders_only_picked_paths_and_conservative_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let (_, picks) = fixture(temp.path());
        let text = render_config(TEMPLATE, &picks).unwrap();
        let config = Config::parse(&text).unwrap();
        let repo = config.repos.values().next().unwrap();
        assert_eq!(config.paths.root, picks.root);
        assert_eq!(repo.path, picks.repos["team.web"]);
        assert!(!repo.publish.push);
        assert!(repo.publish.confirm);
        assert!(repo.deps_dirs.is_empty());
        assert!(text.starts_with("# Example config\n"));
        assert!(!text.contains("/Users/you"));
        assert!(!text.contains("bun install"));
    }

    #[test]
    fn failed_check_config_never_renders_or_bootstraps_a_plist() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let fake = Fake {
            check_fails: true,
            disabled: true,
            ..Fake::default()
        };
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let error = setup
            .bootstrap(
                Some(picks.clone()),
                8,
                &mut None,
                || Ok(()),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("check_config"));
        assert_eq!(*fake.calls.lock().unwrap(), ["check-config"]);
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
        assert!(!entry_exists(&paths.config()).unwrap());
        assert_eq!(fs::read_dir(paths.config_dir()).unwrap().count(), 0);

        let retry = Fake::default();
        let corrected = FolderPicks {
            root: temp.path().join("corrected"),
            ..picks
        };
        Setup {
            paths: &paths,
            service: &retry,
            exec: &retry,
        }
        .bootstrap(Some(corrected.clone()), 8, &mut None, || Ok(()), || {})
        .unwrap();
        let config = Config::parse(&fs::read_to_string(paths.config()).unwrap()).unwrap();
        assert_eq!(config.paths.root, corrected.root);
    }

    #[test]
    fn fresh_data_needs_no_acknowledgement_and_enables_before_bootstrap() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let fake = Fake {
            disabled: true,
            ..Fake::default()
        };
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        setup
            .bootstrap(Some(picks), 8, &mut None, || Ok(()), || {})
            .unwrap();
        assert_eq!(
            fs::metadata(paths.config_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                "check-config",
                "print-plist",
                "print-disabled",
                "enable",
                "bootstrap"
            ]
        );
    }

    #[test]
    fn failed_enable_leaves_no_plist_and_retries_while_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let fake = Fake {
            disabled: true,
            fail_enable_once: AtomicBool::new(true),
            ..Fake::default()
        };
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let error = setup
            .bootstrap(
                Some(picks.clone()),
                8,
                &mut None,
                || panic!("checked socket after failed enable"),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("enable_failed"));
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
        assert!(!entry_exists(&paths.config()).unwrap());
        assert!(!fake.calls.lock().unwrap().contains(&"bootstrap"));

        setup
            .bootstrap(Some(picks), 8, &mut None, || Ok(()), || {})
            .unwrap();
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|&&call| call == "enable").count(), 2);
        assert_eq!(calls.last(), Some(&"bootstrap"));
    }

    #[test]
    fn daemon_that_starts_during_setup_blocks_plist_publication() {
        let temp = tempfile::Builder::new()
            .prefix("t09")
            .tempdir_in("/tmp")
            .unwrap();
        let (paths, picks) = fixture(temp.path());
        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        require_socket_free(&paths.home).unwrap();
        let error = setup
            .bootstrap(
                Some(picks),
                8,
                &mut None,
                || {
                    let _daemon = UnixListener::bind(socket_for_home(&paths.home)).unwrap();
                    require_socket_free(&paths.home)
                },
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_daemon_running"));
        assert!(!entry_exists(&paths.config()).unwrap());
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
        assert!(!fake.calls.lock().unwrap().contains(&"bootstrap"));
        require_socket_free(&paths.home).unwrap(); // A stale socket alone is OK.
    }

    #[test]
    fn held_daemon_lock_refuses_setup_even_without_a_listener() {
        let temp = tempfile::Builder::new()
            .prefix("t09")
            .tempdir_in("/tmp")
            .unwrap();
        let (paths, picks) = fixture(temp.path());
        fs::create_dir_all(paths.config_dir()).unwrap();
        let daemon_lock = File::create(paths.config_dir().join("daemon.lock")).unwrap();
        daemon_lock.try_lock().unwrap();
        require_socket_free(&paths.home).unwrap();

        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let error = setup
            .bootstrap(
                Some(picks.clone()),
                8,
                &mut None,
                || require_socket_free(&paths.home),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_daemon_running"));
        assert!(!entry_exists(&paths.config()).unwrap());
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
        assert!(!fake.calls.lock().unwrap().contains(&"bootstrap"));

        drop(daemon_lock);
        setup
            .bootstrap(
                Some(picks),
                8,
                &mut None,
                || require_socket_free(&paths.home),
                || {},
            )
            .unwrap();
    }

    #[test]
    fn fresh_setup_locks_daemon_before_publishing() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let lock_path = paths.config_dir().join("daemon.lock");
        let config_path = paths.config();
        let plist_path = plist_for_home(&paths.home);
        assert!(!entry_exists(&lock_path).unwrap());

        let fake = Fake::default();
        Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        }
        .bootstrap(
            Some(picks),
            8,
            &mut None,
            || {
                assert!(!entry_exists(&config_path).unwrap());
                assert!(!entry_exists(&plist_path).unwrap());
                let contender = File::open(&lock_path).expect("setup must create daemon.lock");
                assert_eq!(
                    contender.metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert!(matches!(
                    contender.try_lock(),
                    Err(TryLockError::WouldBlock)
                ));
                Ok(())
            },
            || {
                assert!(config_path.is_file());
                assert!(plist_path.is_file());
                File::open(&lock_path).unwrap().try_lock().unwrap();
            },
        )
        .unwrap();
    }

    fn keep_config(paths: &SetupPaths, picks: &FolderPicks) -> String {
        let text = render_config(TEMPLATE, picks).unwrap();
        fs::create_dir_all(paths.config_dir()).unwrap();
        fs::write(paths.config(), &text).unwrap();
        text
    }

    #[test]
    fn kept_config_ignores_a_stale_newer_schema_and_requires_unknown_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let text = keep_config(&paths, &picks);
        let state = paths.config_dir().join("store_state.json");
        fs::write(
            &state,
            json!({"root": picks.root, "user_version": 9}).to_string(),
        )
        .unwrap();
        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let mut pending = None;
        let error = setup
            .bootstrap(
                None,
                8,
                &mut pending,
                || Ok(()),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_schema_acknowledgement"));
        assert_eq!(error.detail["from_schema"], Value::Null);
        assert_eq!(error.detail["target_schema"], 8);
        assert_eq!(*fake.calls.lock().unwrap(), ["check-config"]);
        setup
            .bootstrap(None, 8, &mut pending, || Ok(()), || {})
            .unwrap();
        assert_eq!(fs::read_to_string(paths.config()).unwrap(), text);
        assert!(!fake.calls.lock().unwrap().contains(&"enable"));
    }

    #[test]
    fn setup_with_kept_config_confirms_then_links_the_bundled_cli() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let text = keep_config(&paths, &picks);
        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let mut pending = None;
        let error = setup
            .bootstrap(
                None,
                7,
                &mut pending,
                || Ok(()),
                || panic!("setup requires consent"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_schema_acknowledgement"));
        setup
            .bootstrap(None, 7, &mut pending, || Ok(()), || {})
            .unwrap();
        assert_eq!(fs::read_to_string(paths.config()).unwrap(), text);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                "check-config",
                "check-config",
                "print-plist",
                "print-disabled",
                "bootstrap"
            ]
        );
        let links = super::super::cli_link::LinkPaths {
            bundle: paths.bundle.clone(),
            homebrew: temp.path().join("opt/homebrew"),
            local: temp.path().join("usr/local"),
        };
        fs::create_dir_all(links.homebrew.join("bin")).unwrap();
        let bin = paths.bundle.join("Contents/MacOS");
        fs::create_dir_all(&bin).unwrap();
        for tool in ["git-strata", "git-stratad"] {
            fs::write(bin.join(tool), b"fixture").unwrap();
        }
        links.install_without_elevation().unwrap();
        assert_eq!(links.labels().unwrap()[0].label, "this app");
        for tool in ["git-strata", "git-stratad"] {
            assert_eq!(
                fs::read_link(links.homebrew.join("bin").join(tool)).unwrap(),
                bin.join(tool)
            );
        }
    }

    #[test]
    fn restored_store_with_a_same_path_marker_requires_unknown_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let store = picks.root.join("store");
        fs::create_dir(&store).unwrap();
        fs::write(store.join("store.db"), b"restored store fixture").unwrap();
        fs::create_dir_all(paths.config_dir()).unwrap();
        let state = paths.config_dir().join("store_state.json");
        fs::write(
            &state,
            json!({"root": picks.root, "user_version": 8}).to_string(),
        )
        .unwrap();

        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let downgrade = setup
            .bootstrap(
                Some(picks.clone()),
                7,
                &mut None,
                || Ok(()),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(downgrade.reason(), Some("setup_schema_downgrade"));
        let error = setup
            .bootstrap(
                Some(picks),
                8,
                &mut None,
                || Ok(()),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_schema_acknowledgement"));
        assert_eq!(error.detail["from_schema"], Value::Null);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["check-config", "check-config"]
        );
        assert!(!entry_exists(&paths.config()).unwrap());
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
    }

    #[test]
    fn store_appearing_before_publication_requires_review() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let store = picks.root.join("store");
        let error = setup
            .bootstrap(
                Some(picks),
                8,
                &mut None,
                || {
                    fs::create_dir(&store).unwrap();
                    Ok(())
                },
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_schema_acknowledgement"));
        assert!(!entry_exists(&paths.config()).unwrap());
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
    }

    #[test]
    fn schema_acknowledgement_applies_only_to_the_reviewed_picks() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        let fake = Fake::default();
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let mut pending = None;
        let mut selected = picks;
        let mut reviews = Vec::new();
        for root in ["first", "second"] {
            PendingSetup::take_acknowledged(&mut pending, None, None).unwrap();
            selected.root = temp.path().join(root);
            fs::create_dir_all(selected.root.join("store")).unwrap();
            let error = setup
                .bootstrap(
                    Some(selected.clone()),
                    8,
                    &mut pending,
                    || Ok(()),
                    || panic!("launch started"),
                )
                .unwrap_err();
            assert_eq!(error.reason(), Some("setup_schema_acknowledgement"));
            reviews.push(error.detail["review_id"].as_str().unwrap().to_owned());
            assert!(!entry_exists(&paths.config()).unwrap());
        }
        assert_ne!(reviews[0], reviews[1]);
        for id in [None, Some(reviews[0].as_str())] {
            let error = PendingSetup::take_acknowledged(&mut pending, Some(8), id)
                .err()
                .unwrap();
            assert_eq!(error.reason(), Some("setup_review_stale"));
            assert!(!entry_exists(&paths.config()).unwrap());
        }
        let mut reviewed =
            PendingSetup::take_acknowledged(&mut pending, Some(8), Some(&reviews[1])).unwrap();
        let reviewed_picks = reviewed.as_ref().unwrap().picks.clone().unwrap();
        assert_eq!(reviewed_picks.root, selected.root);
        setup
            .bootstrap(Some(reviewed_picks), 8, &mut reviewed, || Ok(()), || {})
            .unwrap();
        assert!(reviewed.is_none());
        let config = Config::parse(&fs::read_to_string(paths.config()).unwrap()).unwrap();
        assert_eq!(config.paths.root, selected.root);
    }

    #[test]
    fn config_changed_after_validation_blocks_plist_publication() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, picks) = fixture(temp.path());
        keep_config(&paths, &picks);
        let fake = Fake {
            change_config_after_check: true,
            ..Fake::default()
        };
        let setup = Setup {
            paths: &paths,
            service: &fake,
            exec: &fake,
        };
        let mut pending = None;
        setup
            .bootstrap(
                None,
                8,
                &mut pending,
                || Ok(()),
                || panic!("launch started"),
            )
            .unwrap_err();
        let error = setup
            .bootstrap(
                None,
                8,
                &mut pending,
                || Ok(()),
                || panic!("launch started"),
            )
            .unwrap_err();
        assert_eq!(error.reason(), Some("setup_config_changed"));
        assert_eq!(fs::read_to_string(paths.config()).unwrap(), "changed");
        assert!(!entry_exists(&plist_for_home(&paths.home)).unwrap());
    }

    #[test]
    fn atomic_write_preserves_an_entry_created_by_someone_else() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(&path, "keep").unwrap();
        let staged = StagedFile::create(&path, b"new", 0o600).unwrap();
        assert!(staged.publish(&path).is_err());
        drop(staged);
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn unresolved_update_transaction_blocks_setup() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, _) = fixture(temp.path());
        let journal = paths
            .home
            .join("Library/Application Support/io.github.dickwu.git-strata/update.json");
        let staged =
            StagedFile::create(&journal, br#"{"txn":{"phase":"stopping"}}"#, 0o600).unwrap();
        staged.publish(&journal).unwrap();
        let fake = Fake {
            disabled: true,
            ..Fake::default()
        };
        let err = require_fresh(&paths, &fake).unwrap_err();
        assert_eq!(err.reason(), Some("setup_update_in_progress"));
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn an_existing_plist_is_left_for_the_existing_daemon_actions() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, _) = fixture(temp.path());
        let plist = plist_for_home(&paths.home);
        let staged = StagedFile::create(&plist, b"another agent", 0o644).unwrap();
        staged.publish(&plist).unwrap();
        let fake = Fake {
            disabled: true,
            ..Fake::default()
        };
        assert_eq!(
            require_fresh(&paths, &fake).unwrap_err().reason(),
            Some("setup_plist_exists")
        );
        assert!(fake.calls.lock().unwrap().is_empty());
        assert_eq!(fs::read(&plist).unwrap(), b"another agent");
    }

    #[test]
    fn verifies_program_version_and_the_answering_pid() {
        let program = Path::new("/test/GIT STRATA.app/Contents/MacOS/git-stratad");
        let job = format!(
            "gui/501/{AGENT_LABEL} = {{\n\tprogram = {}\n\tpid = 42\n\tproperties = {{\n\t\tpid = 9\n\t}}\n}}",
            program.display()
        );
        assert!(verified_daemon(
            &json!({"version": "0.3.1", "pid": 42}),
            &job,
            "0.3.1",
            program
        ));
        assert!(!verified_daemon(
            &json!({"version": "0.3.1", "pid": 9}),
            &job,
            "0.3.1",
            program
        ));
        assert!(!verified_daemon(
            &json!({"version": "0.3.0", "pid": 42}),
            &job,
            "0.3.1",
            program
        ));
        assert!(!verified_daemon(
            &json!({"version": "0.3.1", "pid": 42}),
            &job,
            "0.3.1",
            Path::new("/other")
        ));
    }
}

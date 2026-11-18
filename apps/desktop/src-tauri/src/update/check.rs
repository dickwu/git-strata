//! Scheduled and manual update checks.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use strata_types::Code;
use tauri::{AppHandle, Emitter, Listener, Manager, Runtime};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc};

use super::cache::{self, CachedCheck, CheckRecord};
use super::manifest::{self, Manifest};
use super::source::{PluginUpdateSource, UpdateSource};
use super::state::UpdateState;
use super::{Clock, UpdateFuture};
use crate::error::GuiError;

const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_INTERVAL: u64 = 6 * 60 * 60;
const MAX_BACKOFF: u64 = 24 * 60 * 60;
const APPLICATIONS: &str = "/Applications";
const INSTALL_BUNDLE: &str = "/Applications/GIT STRATA.app";

const STATE_EVENT: &str = "strata://update-state";
pub(crate) const MANUAL_RESULT_EVENT: &str = "strata://update-check-result";
const SETTINGS_GET: &str = "strata://update-settings-get";
const SETTINGS_SET: &str = "strata://update-settings-set";
const SETTINGS_EVENT: &str = "strata://update-settings";

// T00 registered the IPC commands with no arguments. Initialize their shared
// backend when the app installs its tray, including hidden launches.
static CHECKER: OnceLock<Arc<UpdateChecker>> = OnceLock::new();

struct SystemClock {
    #[cfg(target_os = "macos")]
    anchor: (Instant, u64),
}

impl SystemClock {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            anchor: (Instant::now(), continuous_nanos()),
        }
    }
}

#[cfg(target_os = "macos")]
fn continuous_nanos() -> u64 {
    unsafe extern "C" {
        fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
    }

    // SAFETY: CLOCK_MONOTONIC_RAW is valid on all supported macOS versions.
    // It counts sleep and is unaffected by adjustments to the system clock.
    unsafe { clock_gettime_nsec_np(libc::CLOCK_MONOTONIC_RAW) }
}

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn monotonic(&self) -> Instant {
        #[cfg(target_os = "macos")]
        {
            let (instant, nanos) = self.anchor;
            instant + Duration::from_nanos(continuous_nanos().saturating_sub(nanos))
        }
        #[cfg(not(target_os = "macos"))]
        Instant::now()
    }

    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Launch,
    Scheduled,
    Manual,
}

enum SettingsRequest {
    Get,
    Set(Option<bool>),
}

struct CheckState {
    automatic: bool,
    next_check: Instant,
    backoff_until: Instant,
    rate_limit_s: u64,
    ready: UpdateState,
    checking: bool,
    completed_checks: u64,
    last_manual_result: Result<UpdateState, GuiError>,
}

impl CheckState {
    fn automatic_wait(&self, now: Instant) -> Option<Duration> {
        self.automatic.then(|| {
            self.next_check
                .max(self.backoff_until)
                .saturating_duration_since(now)
        })
    }
}

pub(crate) struct UpdateChecker {
    source: Result<Box<dyn UpdateSource>, GuiError>,
    installed: String,
    cache: Option<PathBuf>,
    clock: Box<dyn Clock>,
    state: Mutex<CheckState>,
    flight: AsyncMutex<()>,
    changed: Notify,
    on_state: Option<Box<dyn Fn(UpdateState) + Send + Sync>>,
}

impl UpdateChecker {
    fn new(
        source: Result<Box<dyn UpdateSource>, GuiError>,
        installed: String,
        cache: Option<PathBuf>,
        clock: Box<dyn Clock>,
        record: CheckRecord,
    ) -> Self {
        let ready = record
            .last_result
            .as_ref()
            .filter(|cached| source.is_ok() && cached.checked_for == installed)
            .and_then(|cached| classify(cached.candidate.as_ref(), &installed).ok())
            .unwrap_or(UpdateState::Idle);
        let wall_now = unix_seconds(clock.now());
        // Persist the clamp so later launches continue the same backoff.
        if let (Some(path), Some(checked)) = (cache.as_deref(), record.last_check_s)
            && checked > wall_now
            && let Err(error) = cache::normalize_last_check(path, checked, wall_now)
        {
            eprintln!("update cache: {error}");
        }
        let elapsed = record
            .last_check_s
            .map_or(u64::MAX, |checked| wall_now.saturating_sub(checked));
        let now = clock.monotonic();
        let rate_limit_s = record.rate_limit_s.min(MAX_BACKOFF);
        Self {
            source,
            installed,
            cache,
            clock,
            state: Mutex::new(CheckState {
                automatic: record.automatic,
                next_check: now + Duration::from_secs(CHECK_INTERVAL.saturating_sub(elapsed)),
                backoff_until: now + Duration::from_secs(rate_limit_s.saturating_sub(elapsed)),
                rate_limit_s,
                ready,
                checking: false,
                completed_checks: 0,
                last_manual_result: Ok(UpdateState::Idle),
            }),
            flight: AsyncMutex::new(()),
            changed: Notify::new(),
            on_state: None,
        }
    }

    fn snapshot(&self) -> std::sync::MutexGuard<'_, CheckState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn state(&self) -> UpdateState {
        let state = self.snapshot();
        if state.checking {
            UpdateState::Checking
        } else {
            state.ready.clone()
        }
    }

    pub(crate) fn source(&self) -> Result<&dyn UpdateSource, GuiError> {
        self.source.as_deref().map_err(Clone::clone)
    }

    async fn refresh_automatic(&self) -> bool {
        let Some(path) = self.cache.clone() else {
            return self.snapshot().automatic;
        };
        let automatic =
            tokio::task::spawn_blocking(move || load_check_record(Some(&path)).automatic)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("update cache: {error}");
                    false
                });
        self.snapshot().automatic = automatic;
        automatic
    }

    async fn set_automatic(&self, automatic: bool) -> Result<bool, GuiError> {
        self.source()?;
        let save_error = || unavailable("update_settings_write", "Couldn't save update settings");
        let path = self.cache.clone().ok_or_else(save_error)?;
        tokio::task::spawn_blocking(move || cache::write_automatic(&path, automatic))
            .await
            .map_err(std::io::Error::other)
            .and_then(|result| result)
            .map_err(|error| {
                eprintln!("update settings: {error}");
                if error.kind() == std::io::ErrorKind::InvalidData {
                    unavailable(
                        "update_settings_write",
                        "Couldn't save update settings because update.json is damaged",
                    )
                } else {
                    save_error()
                }
            })?;
        self.snapshot().automatic = automatic;
        self.changed.notify_one();
        Ok(automatic)
    }

    async fn serve_settings(
        &self,
        mut requests: mpsc::UnboundedReceiver<SettingsRequest>,
        mut reply: impl FnMut(Result<bool, GuiError>),
    ) {
        while let Some(request) = requests.recv().await {
            let result = match request {
                SettingsRequest::Get => match self.source() {
                    Ok(_) => Ok(self.refresh_automatic().await),
                    Err(error) => Err(error),
                },
                SettingsRequest::Set(Some(automatic)) => self.set_automatic(automatic).await,
                SettingsRequest::Set(None) => Err(GuiError::new(
                    Code::InvalidInput,
                    "Expected a boolean update preference",
                    json!({ "reason": "invalid_update_settings" }),
                )),
            };
            reply(result);
        }
    }

    /// None means a scheduled check was suppressed or failed silently.
    async fn check(&self, trigger: Trigger) -> Result<Option<UpdateState>, GuiError> {
        let source = self.source()?;
        let manual = trigger == Trigger::Manual;
        let completed = self.snapshot().completed_checks;
        let _flight = self.flight.lock().await;
        if !manual && !self.refresh_automatic().await {
            return Ok(None);
        }

        {
            let mut state = self.snapshot();
            // A manual action during an active request shares its terminal
            // result, including errors that an automatic check would hide.
            if manual && state.completed_checks != completed {
                return state.last_manual_result.clone().map(Some);
            }
            let now = self.clock.monotonic();
            let backoff = state.backoff_until.saturating_duration_since(now);
            if !backoff.is_zero() {
                return if manual {
                    Err(rate_limited(self.clock.now(), backoff))
                } else {
                    Ok(None)
                };
            }
            if !manual {
                let Some(wait) = state.automatic_wait(now) else {
                    return Ok(None);
                };
                if (trigger != Trigger::Launch || state.completed_checks > 0) && !wait.is_zero() {
                    return Ok(None);
                }
            }
            state.checking = true;
        }

        let result = source.check().await.and_then(|candidate| {
            let ready = classify(candidate.as_ref(), &self.installed)?;
            Ok((candidate, ready))
        });

        let wall_now = self.clock.now();
        let now = self.clock.monotonic();
        let (cached, backoff, reply) = match result {
            Ok((candidate, ready)) => (
                Some(CachedCheck {
                    checked_for: self.installed.clone(),
                    candidate,
                }),
                0,
                Ok(ready),
            ),
            Err(error) => {
                eprintln!("update check: {}", error.message);
                match error.reason() {
                    Some("update_rate_limited") => {
                        let previous = self.snapshot().rate_limit_s;
                        let delay = (previous.max(CHECK_INTERVAL) * 2).min(MAX_BACKOFF);
                        (
                            None,
                            delay,
                            Err(rate_limited(wall_now, Duration::from_secs(delay))),
                        )
                    }
                    Some("no_update_for_this_mac") => (None, 0, Err(error)),
                    _ => (None, 0, Ok(UpdateState::CheckFailed)),
                }
            }
        };
        let succeeded = cached.is_some();
        if let Some(path) = self.cache.clone() {
            let saved = tokio::task::spawn_blocking(move || {
                cache::write_check(&path, unix_seconds(wall_now), backoff, cached.as_ref())
            })
            .await
            .map_err(std::io::Error::other)
            .and_then(|result| result);
            if let Err(error) = saved {
                eprintln!("update cache: {error}");
            }
        }
        {
            let mut state = self.snapshot();
            state.checking = false;
            state.completed_checks += 1;
            state.next_check = now + Duration::from_secs(CHECK_INTERVAL);
            state.backoff_until = now + Duration::from_secs(backoff);
            state.rate_limit_s = backoff;
            if succeeded && let Ok(ready) = &reply {
                state.ready = ready.clone();
            }
            state.last_manual_result = reply.clone();
        }
        if manual {
            self.changed.notify_one();
        }
        if manual || succeeded {
            reply.map(Some)
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn manual(&self) -> Result<UpdateState, GuiError> {
        let result = self.check(Trigger::Manual).await;
        self.publish_state(self.state());
        result.map(|state| state.unwrap_or(UpdateState::CheckFailed))
    }

    fn publish_state(&self, state: UpdateState) {
        if let Some(publish) = &self.on_state {
            publish(state);
        }
    }

    fn did_wake(&self) {
        // The async timer may have paused during sleep. Recheck against the continuous clock.
        self.changed.notify_one();
    }

    async fn schedule(&self, mut publish: impl FnMut(UpdateState)) {
        self.clock.sleep(FIRST_CHECK).await;
        let mut trigger = Trigger::Launch;
        loop {
            if let Ok(Some(state)) = self.check(trigger).await {
                publish(state);
            }
            let wait = self
                .snapshot()
                .automatic_wait(self.clock.monotonic())
                // Pick up a preference change made by another app instance.
                .unwrap_or(Duration::from_secs(CHECK_INTERVAL));
            let _ = tokio::time::timeout(wait, self.changed.notified()).await;
            trigger = Trigger::Scheduled;
        }
    }
}

fn classify(
    candidate: Option<&super::source::UpdateCandidate>,
    installed: &str,
) -> Result<UpdateState, GuiError> {
    match candidate
        .map(|candidate| manifest::read(candidate, installed))
        .transpose()?
    {
        Some(Manifest::Available(target)) => Ok(UpdateState::Available { target }),
        Some(Manifest::NoPlatform) => Err(no_update_for_mac()),
        Some(Manifest::UpToDate) | None => Ok(UpdateState::UpToDate),
    }
}

pub(crate) fn checker() -> Option<&'static UpdateChecker> {
    CHECKER.get().map(Arc::as_ref)
}

/// Uses the exact checked target and the configured plugin for download.
pub fn checked_source() -> Option<&'static dyn UpdateSource> {
    checker()?.source().ok()
}

pub(crate) async fn manual() -> Result<UpdateState, GuiError> {
    let checker = checker()
        .ok_or_else(|| unavailable("update_source_unavailable", "Couldn't check for updates"))?;
    checker.manual().await
}

fn load_check_record(path: Option<&Path>) -> CheckRecord {
    if let Some(path) = path {
        match cache::read_check(path) {
            Ok(record) => return record,
            Err(error) => eprintln!("update cache: {error}"),
        }
    }
    // A failed read cannot establish whether automatic checks are enabled.
    CheckRecord {
        automatic: false,
        ..CheckRecord::default()
    }
}

pub(crate) fn start<R: Runtime>(app: &AppHandle<R>) {
    let installed = app.package_info().version.to_string();
    let bundle = running_bundle();
    let cache = bundle
        .as_ref()
        .ok()
        .and_then(|_| app.path().home_dir().ok())
        .map(|home| cache::path(&home));
    let mut record = load_check_record(cache.as_deref());
    let source = bundle.and_then(|_| {
        let source = PluginUpdateSource::new(app, installed.clone())?;
        if let Some(cached) = &record.last_result
            && cached.checked_for == installed
            && let Some(candidate) = &cached.candidate
            && let Err(error) = source.restore(candidate.raw_json.clone())
        {
            eprintln!("update cache: {}", error.message);
            record.last_result = None;
        }
        Ok(Box::new(source) as Box<dyn UpdateSource>)
    });
    let mut checker = UpdateChecker::new(
        source,
        installed,
        cache,
        Box::new(SystemClock::new()),
        record,
    );
    let handle = app.clone();
    checker.on_state = Some(Box::new(move |state| {
        let _ = handle.emit_to("main", STATE_EVENT, state);
    }));
    let checker = Arc::new(checker);
    if CHECKER.set(Arc::clone(&checker)).is_err() {
        return;
    }
    // Apply Settings requests in the order the callbacks enqueue them.
    let (requests, receiver) = mpsc::unbounded_channel();
    let reads = requests.clone();
    app.listen_any(SETTINGS_GET, move |_| {
        let _ = reads.send(SettingsRequest::Get);
    });
    app.listen_any(SETTINGS_SET, move |event| {
        let automatic = serde_json::from_str::<bool>(event.payload()).ok();
        let _ = requests.send(SettingsRequest::Set(automatic));
    });
    let handle = app.clone();
    let settings = Arc::clone(&checker);
    tauri::async_runtime::spawn(async move {
        settings
            .serve_settings(receiver, |result| {
                let _ = handle.emit_to("main", SETTINGS_EVENT, result);
            })
            .await;
    });
    if checker.source().is_ok() {
        #[cfg(target_os = "macos")]
        listen_for_wake(&checker);
        tauri::async_runtime::spawn(async move {
            checker.schedule(|state| checker.publish_state(state)).await;
        });
    }
}

#[cfg(target_os = "macos")]
fn listen_for_wake(checker: &Arc<UpdateChecker>) {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSWorkspaceDidWakeNotification: &'static NSString;
    }

    let checker = Arc::downgrade(checker);
    let callback = RcBlock::new(move |_: *mut AnyObject| {
        if let Some(checker) = checker.upgrade() {
            checker.did_wake();
        }
    });
    // SAFETY: startup runs on the main thread. The workspace notification
    // center retains the observer and copies its thread-safe block until exit.
    unsafe {
        let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let center: Retained<AnyObject> = msg_send![&*workspace, notificationCenter];
        let _: Retained<AnyObject> = msg_send![&*center, addObserverForName: NSWorkspaceDidWakeNotification, object: &*workspace, queue: std::ptr::null::<AnyObject>(), usingBlock: &*callback];
    }
}

fn unavailable(reason: &str, message: &str) -> GuiError {
    GuiError::new(Code::Unsupported, message, json!({ "reason": reason }))
}

fn no_update_for_mac() -> GuiError {
    unavailable("no_update_for_this_mac", "No update for this Mac")
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn rate_limited(now: SystemTime, remaining: Duration) -> GuiError {
    let delay_s = remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0);
    let retry_at_s = unix_seconds(now).saturating_add(delay_s);
    GuiError::new(
        Code::Busy,
        "Couldn't check for updates",
        json!({ "reason": "update_rate_limited", "retry_at_s": retry_at_s }),
    )
}

fn running_bundle() -> Result<PathBuf, GuiError> {
    let development = tauri::is_dev() || cfg!(feature = "dev-connector");
    let disabled = cfg!(debug_assertions) || development;
    if disabled {
        return Err(unavailable(
            "update_disabled",
            "Updates are disabled in development builds",
        ));
    }
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(no_update_for_mac());
    }
    let executable = std::env::current_exe().unwrap_or_default();
    let resolved = executable.canonicalize().unwrap_or_default();
    eligible_bundle(&executable, &resolved, cfg!(debug_assertions), development)
}

fn eligible_bundle(
    executable: &Path,
    resolved: &Path,
    debug: bool,
    development: bool,
) -> Result<PathBuf, GuiError> {
    if debug || development {
        return Err(unavailable(
            "update_disabled",
            "Updates are disabled in development builds",
        ));
    }
    for path in [executable, resolved] {
        if path
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
        {
            return Err(unavailable(
                "update_translocated",
                "Move GIT STRATA into Applications to check for updates",
            ));
        }
    }
    fn bundle(path: &Path) -> Option<&Path> {
        let macos = path.parent()?;
        let contents = macos.parent()?;
        let bundle = contents.parent()?;
        (macos.ends_with("MacOS")
            && contents.ends_with("Contents")
            && bundle.extension().is_some_and(|ext| ext == "app")
            && bundle.starts_with(APPLICATIONS))
        .then_some(bundle)
    }
    match (bundle(executable), bundle(resolved)) {
        (Some(_), Some(resolved_bundle)) => Ok(resolved_bundle.to_path_buf()),
        _ => Err(unavailable(
            "update_bundle_location",
            "Move GIT STRATA into Applications to check for updates",
        )),
    }
}

pub enum DaemonVersion<'a> {
    /// The service preflight verified that the bundle's job is not serving.
    NotServing,
    Running(Option<&'a str>),
}

/// Items 3 and 4 of §4.1. Pass fresh evidence from the service preflight
/// and the bundled `git-stratad --version` runner.
pub fn install_preconditions(
    running: DaemonVersion<'_>,
    bundled: Option<&str>,
) -> Result<(), GuiError> {
    let bundle = running_bundle()?;
    let writable = bundle.parent().is_some_and(user_can_write) && user_can_write(&bundle);
    validate_install(&bundle, writable, running, bundled)
}

#[cfg(unix)]
fn user_can_write(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    CString::new(path.as_os_str().as_bytes()).is_ok_and(|path| {
        // SAFETY: the path is a NUL-terminated string that lives through access.
        unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
    })
}

#[cfg(not(unix))]
fn user_can_write(_: &Path) -> bool {
    false
}

pub(crate) fn validate_install(
    bundle: &Path,
    writable: bool,
    running: DaemonVersion<'_>,
    bundled: Option<&str>,
) -> Result<(), GuiError> {
    if bundle != Path::new(INSTALL_BUNDLE) || !writable {
        return Err(unavailable(
            "update_bundle_not_writable",
            "Install a writable copy of GIT STRATA in Applications first",
        ));
    }
    let DaemonVersion::Running(running) = running else {
        return Ok(());
    };
    let running = running.unwrap_or_default();
    let bundled = bundled.unwrap_or_default();
    match manifest::compare_versions(bundled, running) {
        Some(Ordering::Equal) if bundled == running => Ok(()),
        Some(Ordering::Less) => Err(GuiError::new(
            Code::Unsupported,
            "Install a matching or newer GIT STRATA first",
            json!({ "reason": "bundled_daemon_older", "url": manifest::dmg_url(running) }),
        )),
        Some(Ordering::Greater) => Err(unavailable(
            "restart_daemon_first",
            "Restart the daemon first",
        )),
        _ => Err(unavailable(
            "daemon_version_unknown",
            "Couldn't verify the daemon version",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::source::{UpdateCandidate, fake::FakeUpdateSource};
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    #[derive(Clone)]
    struct FakeClock {
        wall: Arc<AtomicU64>,
        elapsed: Arc<AtomicU64>,
        origin: Instant,
    }

    impl Default for FakeClock {
        fn default() -> Self {
            Self {
                wall: Arc::default(),
                elapsed: Arc::default(),
                origin: Instant::now(),
            }
        }
    }

    impl FakeClock {
        fn advance(&self, seconds: u64) {
            self.wall.fetch_add(seconds, AtomicOrdering::Relaxed);
            self.elapsed.fetch_add(seconds, AtomicOrdering::Relaxed);
        }

        fn set_wall(&self, seconds: u64) {
            self.wall.store(seconds, AtomicOrdering::Relaxed);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(self.wall.load(AtomicOrdering::Relaxed))
        }

        fn monotonic(&self) -> Instant {
            self.origin + Duration::from_secs(self.elapsed.load(AtomicOrdering::Relaxed))
        }

        fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
            Box::pin(async move { self.advance(duration.as_secs()) })
        }
    }

    fn candidate(version: &str) -> UpdateCandidate {
        UpdateCandidate {
            version: version.into(),
            notes: Some("Release notes".into()),
            raw_json: json!({
                "version": version,
                "notes": "Release notes",
                "platforms": {
                    "darwin-aarch64": {
                        "url": "https://example.com/test.app.tar.gz",
                        "signature": "test fixture"
                    }
                },
                "git_strata": { "store_schema": 7, "min_updatable_from": "0.3.0" }
            }),
        }
    }

    fn no_platform_candidate() -> UpdateCandidate {
        let mut candidate = candidate("0.4.0");
        candidate.raw_json["platforms"] = json!({});
        candidate
    }

    fn setup(
        replies: impl IntoIterator<Item = Result<Option<UpdateCandidate>, GuiError>>,
        cache: Option<PathBuf>,
        record: CheckRecord,
    ) -> (UpdateChecker, FakeClock) {
        let source = FakeUpdateSource {
            responses: Mutex::new(replies.into_iter().collect()),
            ..FakeUpdateSource::default()
        };
        let clock = FakeClock::default();
        (
            UpdateChecker::new(
                Ok(Box::new(source)),
                "0.3.0".into(),
                cache,
                Box::new(clock.clone()),
                record,
            ),
            clock,
        )
    }

    #[tokio::test]
    async fn fake_manifest_matrix() {
        let mut malformed = candidate("unknown");
        let no_platform = no_platform_candidate();
        let mut schema_up = candidate("0.4.0");
        schema_up.raw_json["git_strata"]["store_schema"] = json!(8);
        let mut below_minimum = candidate("0.4.0");
        below_minimum.raw_json["git_strata"]["min_updatable_from"] = json!("0.3.1");
        malformed.raw_json["version"] = json!("broken");
        let (checker, _) = setup(
            [
                candidate("0.3.1"),
                candidate("0.3.0"),
                candidate("0.2.9"),
                candidate("0.4.0-beta.1"),
                malformed,
                no_platform,
                schema_up,
                below_minimum,
            ]
            .into_iter()
            .map(|candidate| Ok(Some(candidate))),
            None,
            CheckRecord::default(),
        );

        assert!(
            matches!(checker.manual().await.unwrap(), UpdateState::Available { target } if target.version == "0.3.1" && target.manual_download_url.is_none())
        );
        for _ in 0..3 {
            assert_eq!(checker.manual().await.unwrap(), UpdateState::UpToDate);
        }
        assert_eq!(checker.manual().await.unwrap(), UpdateState::CheckFailed);
        assert_eq!(
            checker.manual().await.unwrap_err().reason(),
            Some("no_update_for_this_mac")
        );
        assert!(
            matches!(checker.manual().await.unwrap(), UpdateState::Available { target } if target.store_schema == Some(8) && target.manual_download_url.is_none())
        );
        assert!(
            matches!(checker.manual().await.unwrap(), UpdateState::Available { target }
            if target.manual_download_url.as_deref() == Some("https://github.com/dickwu/git-strata/releases/download/v0.4.0/Git-Strata_0.4.0_arm64.dmg"))
        );
    }

    #[tokio::test]
    async fn missing_platform_preserves_last_success_across_relaunch() {
        let temp = tempfile::tempdir().unwrap();
        let path = cache::path(temp.path());
        let previous = candidate("0.3.1");
        let (checker, clock) = setup(
            [
                previous.clone(),
                no_platform_candidate(),
                no_platform_candidate(),
            ]
            .into_iter()
            .map(|candidate| Ok(Some(candidate))),
            Some(path.clone()),
            load_check_record(Some(&path)),
        );
        let available = checker.check(Trigger::Launch).await.unwrap().unwrap();
        assert!(matches!(available, UpdateState::Available { .. }));
        clock.advance(CHECK_INTERVAL + 1);
        assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
        assert_eq!(checker.state(), available);
        assert_eq!(
            checker.manual().await.unwrap_err().reason(),
            Some("no_update_for_this_mac")
        );
        assert_eq!(checker.state(), available);

        let persisted = load_check_record(Some(&path));
        assert_eq!(
            persisted.last_check_s,
            Some(
                clock
                    .now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            )
        );
        assert_eq!(
            persisted.last_result.as_ref().unwrap().candidate,
            Some(previous)
        );
        let (restarted, _) = setup([], Some(path), persisted);
        assert_eq!(restarted.state(), available);
    }

    #[tokio::test]
    async fn signature_failure_never_returns_an_installer() {
        let source = FakeUpdateSource {
            reject_signature: true,
            ..FakeUpdateSource::default()
        };
        source
            .responses
            .lock()
            .unwrap()
            .push_back(Ok(Some(candidate("0.3.1"))));
        let checked = source.check().await.unwrap().unwrap();
        let result = source.download(&checked.version, &mut |_, _| {}).await;
        assert_eq!(
            result.err().unwrap().reason(),
            Some("invalid_update_signature")
        );
    }

    #[tokio::test]
    async fn manual_checks_join_a_scheduled_request_and_get_its_error() {
        let (resume, pause) = tokio::sync::oneshot::channel();
        let source = FakeUpdateSource {
            responses: Mutex::new([Err(manifest::malformed_manifest())].into()),
            pause_check: Mutex::new(Some(pause)),
            ..FakeUpdateSource::default()
        };
        let checker = UpdateChecker::new(
            Ok(Box::new(source)),
            "0.3.0".into(),
            None,
            Box::new(FakeClock::default()),
            CheckRecord::default(),
        );
        let automatic = checker.check(Trigger::Launch);
        tokio::pin!(automatic);
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut automatic)
                .await
                .is_err()
        );
        assert_eq!(checker.state(), UpdateState::Checking);
        let manual = checker.manual();
        tokio::pin!(manual);
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut manual)
                .await
                .is_err()
        );
        resume.send(()).unwrap();
        assert_eq!(automatic.await.unwrap(), None);
        assert_eq!(manual.await.unwrap(), UpdateState::CheckFailed);
        assert_eq!(checker.state(), UpdateState::Idle);
    }

    #[tokio::test]
    async fn manual_check_broadcasts_the_ready_state() {
        let (mut checker, _) = setup(
            [
                Ok(Some(candidate("0.3.1"))),
                Err(manifest::malformed_manifest()),
            ],
            None,
            CheckRecord::default(),
        );
        let (publish, mut published) = mpsc::unbounded_channel();
        checker.on_state = Some(Box::new(move |state| publish.send(state).unwrap()));
        let available = checker.manual().await.unwrap();
        assert!(matches!(available, UpdateState::Available { .. }));
        assert_eq!(published.try_recv().unwrap(), available);
        assert_eq!(checker.manual().await.unwrap(), UpdateState::CheckFailed);
        assert_eq!(published.try_recv().unwrap(), available);
        assert!(matches!(
            published.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn rate_limits_double_to_one_day_without_losing_the_last_success() {
        let (checker, clock) = setup(
            [
                Ok(Some(candidate("0.3.1"))),
                Err(http_rate_limit(403)),
                Err(http_rate_limit(429)),
                Err(http_rate_limit(429)),
                Ok(None),
            ],
            None,
            CheckRecord::default(),
        );
        clock.sleep(FIRST_CHECK).await;
        assert_eq!(unix_seconds(clock.now()), 15);
        let available = checker.check(Trigger::Launch).await.unwrap().unwrap();
        assert!(matches!(available, UpdateState::Available { .. }));
        assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
        clock.advance(CHECK_INTERVAL - 1);
        assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
        clock.advance(1);
        for backoff in [2 * CHECK_INTERVAL, MAX_BACKOFF, MAX_BACKOFF] {
            assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
            assert_eq!(checker.state(), available);
            assert_eq!(checker.snapshot().rate_limit_s, backoff);
            let error = checker.manual().await.unwrap_err();
            assert_eq!(error.code, Code::Busy.as_str());
            assert_eq!(error.reason(), Some("update_rate_limited"));
            assert_eq!(
                error.detail["retry_at_s"],
                unix_seconds(clock.now()) + backoff
            );
            clock.advance(backoff - 1);
            assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
            clock.advance(1);
        }
        assert_eq!(
            checker.check(Trigger::Scheduled).await.unwrap(),
            Some(UpdateState::UpToDate)
        );
        assert_eq!(checker.snapshot().rate_limit_s, 0);
    }

    fn http_rate_limit(status: u16) -> GuiError {
        GuiError::new(
            Code::Busy,
            "rate limited",
            json!({ "reason": "update_rate_limited", "status": status }),
        )
    }

    #[tokio::test]
    async fn clock_changes_do_not_extend_or_shorten_an_active_backoff() {
        let (checker, clock) = setup(
            [
                Err(http_rate_limit(403)),
                Err(http_rate_limit(429)),
                Ok(None),
            ],
            None,
            CheckRecord::default(),
        );
        clock.set_wall(3 * MAX_BACKOFF);
        assert_eq!(checker.check(Trigger::Launch).await.unwrap(), None);
        clock.advance(2 * CHECK_INTERVAL);
        let limited_at = unix_seconds(clock.now());
        let error = checker.manual().await.unwrap_err();
        assert_eq!(error.detail["retry_at_s"], limited_at + MAX_BACKOFF);

        clock.set_wall(limited_at + 2 * MAX_BACKOFF);
        assert_eq!(
            checker.manual().await.unwrap_err().reason(),
            Some("update_rate_limited")
        );
        clock.set_wall(limited_at - 2 * MAX_BACKOFF);
        clock.advance(MAX_BACKOFF - 1);
        assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
        let error = checker.manual().await.unwrap_err();
        assert_eq!(error.detail["retry_at_s"], unix_seconds(clock.now()) + 1);
        clock.advance(1);
        assert_eq!(
            checker.check(Trigger::Scheduled).await.unwrap(),
            Some(UpdateState::UpToDate)
        );
    }

    #[tokio::test]
    async fn future_cached_check_can_delay_relaunch_by_at_most_one_day() {
        let temp = tempfile::tempdir().unwrap();
        let path = cache::path(temp.path());
        cache::write_check(&path, 2 * MAX_BACKOFF, 2 * MAX_BACKOFF, None).unwrap();
        let (checker, clock) = setup([], Some(path.clone()), load_check_record(Some(&path)));

        assert_eq!(checker.check(Trigger::Launch).await.unwrap(), None);
        assert_eq!(
            checker.manual().await.unwrap_err().detail["retry_at_s"],
            MAX_BACKOFF
        );
        drop(checker);

        let hour = 60 * 60;
        clock.advance(MAX_BACKOFF - hour);
        let source = FakeUpdateSource {
            responses: Mutex::new([Ok(None)].into()),
            ..FakeUpdateSource::default()
        };
        let relaunched = UpdateChecker::new(
            Ok(Box::new(source)),
            "0.3.0".into(),
            Some(path.clone()),
            Box::new(clock.clone()),
            load_check_record(Some(&path)),
        );
        let error = relaunched.manual().await.unwrap_err();
        assert_eq!(error.reason(), Some("update_rate_limited"));
        assert_eq!(error.detail["retry_at_s"], unix_seconds(clock.now()) + hour);

        clock.advance(hour - 1);
        assert_eq!(relaunched.check(Trigger::Scheduled).await.unwrap(), None);
        assert_eq!(
            relaunched.manual().await.unwrap_err().detail["retry_at_s"],
            MAX_BACKOFF
        );
        clock.advance(1);
        assert_eq!(
            relaunched.check(Trigger::Scheduled).await.unwrap(),
            Some(UpdateState::UpToDate)
        );
    }

    #[tokio::test]
    async fn schedule_recomputes_after_wake_and_only_publishes_success() {
        let (checker, clock) = setup(
            [
                Ok(Some(candidate("0.3.1"))),
                Ok(Some(no_platform_candidate())),
                Ok(None),
            ],
            None,
            CheckRecord::default(),
        );
        let (publish, mut published) = mpsc::unbounded_channel();
        let schedule = checker.schedule(|state| publish.send(state).unwrap());
        tokio::pin!(schedule);
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut schedule)
                .await
                .is_err()
        );
        assert_eq!(unix_seconds(clock.now()), 15);
        let available = published.try_recv().unwrap();
        assert!(matches!(available, UpdateState::Available { .. }));

        clock.advance(CHECK_INTERVAL - 1);
        checker.did_wake();
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut schedule)
                .await
                .is_err()
        );
        assert_eq!(checker.snapshot().completed_checks, 1);
        assert!(matches!(
            published.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        clock.advance(2);
        checker.did_wake();
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut schedule)
                .await
                .is_err()
        );
        assert_eq!(checker.snapshot().completed_checks, 2);
        assert_eq!(checker.state(), available);
        assert!(matches!(
            published.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        clock.advance(CHECK_INTERVAL);
        checker.did_wake();
        assert!(
            tokio::time::timeout(Duration::ZERO, &mut schedule)
                .await
                .is_err()
        );
        assert_eq!(published.try_recv().unwrap(), UpdateState::UpToDate);
    }

    #[tokio::test]
    async fn queued_settings_apply_in_order_and_last_opt_out_survives_relaunch() {
        let temp = tempfile::tempdir().unwrap();
        let path = cache::path(temp.path());
        let record = load_check_record(Some(&path));
        let (checker, _) = setup([], Some(path.clone()), record);
        let (send, requests) = mpsc::unbounded_channel();
        for request in [
            SettingsRequest::Get,
            SettingsRequest::Set(Some(false)),
            SettingsRequest::Set(Some(true)),
            SettingsRequest::Set(None),
            SettingsRequest::Set(Some(false)),
            SettingsRequest::Get,
        ] {
            send.send(request).unwrap();
        }
        drop(send);
        let mut replies = Vec::new();
        checker
            .serve_settings(requests, |reply| replies.push(reply))
            .await;
        assert_eq!(&replies[..3], &[Ok(true), Ok(false), Ok(true)]);
        assert_eq!(
            replies[3].as_ref().unwrap_err().code,
            Code::InvalidInput.as_str()
        );
        assert_eq!(&replies[4..], &[Ok(false), Ok(false)]);

        let from_disk = load_check_record(Some(&path));
        let (restarted, _) = setup([Ok(None)], Some(path), from_disk);
        assert_eq!(restarted.check(Trigger::Launch).await.unwrap(), None);
        assert_eq!(restarted.check(Trigger::Scheduled).await.unwrap(), None);
        assert_eq!(restarted.manual().await.unwrap(), UpdateState::UpToDate);
    }

    #[tokio::test]
    async fn two_checkers_honor_preference_changes_in_the_same_cache() {
        let temp = tempfile::tempdir().unwrap();
        let path = cache::path(temp.path());
        let (checker, clock) = setup(
            [Ok(Some(candidate("0.3.1"))), Ok(None)],
            Some(path.clone()),
            load_check_record(Some(&path)),
        );
        let (settings, _) = setup([], Some(path.clone()), load_check_record(Some(&path)));

        settings.set_automatic(false).await.unwrap();
        assert_eq!(checker.check(Trigger::Launch).await.unwrap(), None);
        assert!(matches!(
            checker.manual().await.unwrap(),
            UpdateState::Available { .. }
        ));
        settings.set_automatic(true).await.unwrap();
        clock.advance(CHECK_INTERVAL);
        assert_eq!(
            checker.check(Trigger::Scheduled).await.unwrap(),
            Some(UpdateState::UpToDate)
        );
        settings.set_automatic(false).await.unwrap();
        clock.advance(CHECK_INTERVAL);
        assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
    }

    #[tokio::test]
    async fn damaged_cache_disables_automatic_checks_without_disabling_manual() {
        for contents in [
            r#"{"check":{"automatic":false,"last_result":"future format"}}"#,
            r#"{"check":{"automatic":"unreadable"}}"#,
            r#"{"check":[]}"#,
            r#"{"txn":"#,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("update.json");
            std::fs::write(&path, contents).unwrap();
            let record = load_check_record(Some(&path));
            assert!(!record.automatic);
            let (checker, _) = setup([Ok(None)], Some(path.clone()), record);
            assert_eq!(checker.check(Trigger::Launch).await.unwrap(), None);
            assert_eq!(checker.check(Trigger::Scheduled).await.unwrap(), None);
            assert_eq!(checker.manual().await.unwrap(), UpdateState::UpToDate);
            assert!(!load_check_record(Some(&path)).automatic);
        }
    }

    #[tokio::test]
    async fn saving_settings_reports_a_damaged_cache() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("update.json");
        std::fs::write(&path, r#"{"txn":"#).unwrap();
        let (checker, _) = setup([], Some(path.clone()), load_check_record(Some(&path)));
        let error = checker.set_automatic(true).await.unwrap_err();
        assert_eq!(error.reason(), Some("update_settings_write"));
        assert!(error.message.ends_with("update.json is damaged"));
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"txn":"#);
    }

    #[test]
    fn launch_checks_require_a_release_running_inside_applications() {
        let valid = Path::new("/Applications/GIT STRATA.app/Contents/MacOS/git-strata-desktop");
        assert_eq!(
            eligible_bundle(valid, valid, false, false).unwrap(),
            Path::new(INSTALL_BUNDLE)
        );
        for (debug, dev) in [(true, false), (false, true)] {
            assert_eq!(
                eligible_bundle(valid, valid, debug, dev)
                    .unwrap_err()
                    .reason(),
                Some("update_disabled")
            );
        }
        let moved = Path::new(
            "/private/var/folders/xx/AppTranslocation/abc/d/GIT STRATA.app/Contents/MacOS/git-strata-desktop",
        );
        assert_eq!(
            eligible_bundle(moved, moved, false, false)
                .unwrap_err()
                .reason(),
            Some("update_translocated")
        );
        let external = Path::new("/Volumes/Other/GIT STRATA.app/Contents/MacOS/git-strata-desktop");
        assert_eq!(
            eligible_bundle(valid, external, false, false)
                .unwrap_err()
                .reason(),
            Some("update_bundle_location")
        );
        assert_eq!(
            eligible_bundle(external, external, false, false)
                .unwrap_err()
                .reason(),
            Some("update_bundle_location")
        );
    }

    #[test]
    fn install_requires_a_writable_canonical_bundle_and_matching_daemon() {
        use DaemonVersion::{NotServing, Running};

        let bundle = Path::new(INSTALL_BUNDLE);
        assert!(validate_install(bundle, true, NotServing, None).is_ok());
        assert!(validate_install(bundle, true, Running(Some("0.3.1")), Some("0.3.1")).is_ok());
        assert_eq!(
            validate_install(bundle, true, Running(None), Some("0.3.1"))
                .unwrap_err()
                .reason(),
            Some("daemon_version_unknown")
        );
        let older =
            validate_install(bundle, true, Running(Some("0.3.2")), Some("0.3.0")).unwrap_err();
        assert_eq!(older.reason(), Some("bundled_daemon_older"));
        assert_eq!(
            older.detail["url"],
            "https://github.com/dickwu/git-strata/releases/download/v0.3.2/Git-Strata_0.3.2_arm64.dmg"
        );
        for (bundled, reason) in [
            (Some("0.3.10"), "restart_daemon_first"),
            (Some("invalid"), "daemon_version_unknown"),
            (None, "daemon_version_unknown"),
        ] {
            assert_eq!(
                validate_install(bundle, true, Running(Some("0.3.2")), bundled)
                    .unwrap_err()
                    .reason(),
                Some(reason)
            );
        }
        for (bundle, writable) in [(bundle, false), (Path::new("/Applications/Test.app"), true)] {
            assert_eq!(
                validate_install(bundle, writable, NotServing, None)
                    .unwrap_err()
                    .reason(),
                Some("update_bundle_not_writable")
            );
        }
    }

    #[tokio::test]
    async fn disabled_builds_deny_manual_checks_and_setting_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = cache::path(temp.path());
        let checker = UpdateChecker::new(
            Err(unavailable(
                "update_disabled",
                "Updates are disabled in development builds",
            )),
            "0.3.0".into(),
            Some(path.clone()),
            Box::new(FakeClock::default()),
            CheckRecord::default(),
        );
        assert_eq!(checker.state(), UpdateState::Idle);
        assert_eq!(
            checker.manual().await.unwrap_err().reason(),
            Some("update_disabled")
        );
        assert_eq!(
            checker.set_automatic(false).await.unwrap_err().reason(),
            Some("update_disabled")
        );
        assert!(!path.exists());
    }
}

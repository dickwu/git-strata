use super::*;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

const FROM: &str = "0.3.0";
const TO: &str = "0.3.1";
const PID: u32 = 42;

#[derive(Clone, Copy)]
enum Stop {
    Clean(u64),
    Respawn,
    RespawnExitsClean,
    Loop,
}

#[derive(Clone, Copy)]
enum Swap {
    Replace,
    Unchanged,
    Missing,
    Kept,
}

#[derive(Clone, Copy)]
enum Disable {
    Ready,
    Timeout(u64),
    JournalWriteFails,
    PreflightFails,
}

#[derive(Clone, Copy)]
enum Bootout {
    Unload,
    Failed,
    Uncertain,
    PrintFailed,
    PreflightFails,
}

#[derive(Clone, Copy)]
enum Boot {
    Ready,
    Codesign(u64),
    Permission(u64),
    Never,
}

struct FakeState {
    bundle: Option<&'static str>,
    daemon_version: &'static str,
    loaded: bool,
    disabled: bool,
    pid: Option<u32>,
    answering_pid: Option<u32>,
    answering_version: Option<&'static str>,
    feature: bool,
    lock_busy: bool,
    lock_busy_after_unload: bool,
    pending_on_disable: bool,
    disable: Disable,
    disable_at: Option<u64>,
    last_exit: i32,
    exit_report: Option<&'static str>,
    exit_timeout: u32,
    exit_unreadable: bool,
    started: u64,
    lease: bool,
    drain_busy_until: u64,
    seal_race: Option<&'static str>,
    seals: u32,
    seal_at: Option<u64>,
    transient_print_failures: u8,
    transient_fence_failures: u8,
    transient_enabled_failures: u8,
    stop: Stop,
    bootout: Bootout,
    boot: Boot,
    boot_at: Option<u64>,
    swap: Swap,
    bundle_probe_error: Option<&'static str>,
    bundle_probe_error_on_bootout: bool,
    restore_ok: bool,
    stop_during_prepare: bool,
    retained_schema: Option<u32>,
    confirmed_schemas: Vec<Option<u32>>,
    consent: bool,
    replace_during_confirmation: Option<&'static str>,
    close_error: Option<Value>,
    calls: Vec<(&'static str, u64)>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            bundle: Some(FROM),
            daemon_version: FROM,
            loaded: true,
            disabled: false,
            pid: Some(PID),
            answering_pid: None,
            answering_version: None,
            feature: true,
            lock_busy: false,
            lock_busy_after_unload: false,
            pending_on_disable: false,
            disable: Disable::Ready,
            disable_at: None,
            last_exit: 0,
            exit_report: None,
            exit_timeout: 3,
            exit_unreadable: false,
            started: 0,
            lease: false,
            drain_busy_until: 0,
            seal_race: None,
            seals: 0,
            seal_at: None,
            transient_print_failures: 0,
            transient_fence_failures: 0,
            transient_enabled_failures: 0,
            stop: Stop::Clean(1),
            bootout: Bootout::Unload,
            boot: Boot::Ready,
            boot_at: None,
            swap: Swap::Replace,
            bundle_probe_error: None,
            bundle_probe_error_on_bootout: false,
            restore_ok: true,
            stop_during_prepare: false,
            retained_schema: Some(7),
            confirmed_schemas: Vec::new(),
            consent: true,
            replace_during_confirmation: None,
            close_error: None,
            calls: Vec::new(),
        }
    }
}

struct Fake {
    epoch: Instant,
    elapsed: AtomicU64,
    bundle: PathBuf,
    plist: PathBuf,
    program: PathBuf,
    update_lock: PathBuf,
    state: Mutex<FakeState>,
}

impl Fake {
    fn seconds(&self) -> u64 {
        self.elapsed.load(AtomicOrdering::SeqCst)
    }

    fn data(&self) -> MutexGuard<'_, FakeState> {
        let now = self.seconds();
        let mut state = self.state.lock().unwrap();
        if state.disable_at.is_some_and(|at| now >= at) {
            state.disable_at = None;
            state.disabled = true;
            self.record(&mut state, "disable_effect");
        }
        if let Some(sealed_at) = state.seal_at {
            match state.stop {
                Stop::Clean(delay) if now >= sealed_at + delay => {
                    state.pid = None;
                    state.lease = false;
                    state.seal_at = None;
                    state.last_exit = 0;
                }
                Stop::Respawn if now >= sealed_at + 2 => {
                    state.pid = Some(PID + 1);
                    state.started = now;
                    state.lease = false;
                    state.seal_at = None;
                    state.stop = Stop::Clean(1);
                    state.last_exit = 1;
                }
                Stop::RespawnExitsClean if now > sealed_at => {
                    state.pid = (now == sealed_at + 1).then_some(PID + 1);
                    state.lease = false;
                    state.last_exit = i32::from(state.pid.is_some());
                    if state.pid.is_none() {
                        state.seal_at = None;
                    }
                }
                Stop::Loop if now > sealed_at => {
                    state.pid = (!state.disabled && (now - sealed_at) % 10 == 5).then_some(PID + 1);
                    state.lease = false;
                    state.last_exit = 1;
                }
                _ => {}
            }
        }
        if let Some(boot_at) = state.boot_at {
            let ready = match state.boot {
                Boot::Ready => true,
                Boot::Codesign(delay) | Boot::Permission(delay) => now >= boot_at + delay,
                Boot::Never => false,
            };
            if ready {
                state.pid = Some(PID + 10 + state.seals);
                state.started = now + 1;
                state.boot_at = None;
            }
        }
        state
    }

    fn trace(&self) -> Vec<&'static str> {
        self.data()
            .calls
            .iter()
            .map(|(op, _)| *op)
            .filter(|op| !matches!(*op, "print" | "loop_probe"))
            .collect()
    }

    fn record(&self, state: &mut FakeState, op: &'static str) {
        state.calls.push((op, self.seconds()));
    }

    fn boot(&self, state: &mut FakeState, op: &'static str) {
        assert!(!state.disabled);
        self.record(state, op);
        state.loaded = true;
        state.lease = false;
        state.last_exit = i32::from(matches!(state.boot, Boot::Codesign(_)));
        state.pid = matches!(state.boot, Boot::Permission(_)).then_some(PID + 10);
        state.boot_at = Some(self.seconds());
    }

    fn kept(&self) -> PathBuf {
        self.bundle
            .with_file_name("GIT STRATA (previous version).app")
    }

    fn block_journal_writes(&self) {
        // Keep the durable txn readable while making its next write fail.
        fs::remove_file(&self.update_lock).unwrap();
        fs::create_dir(&self.update_lock).unwrap();
    }

    fn info(version: &str) -> BundleInfo {
        BundleInfo {
            version: version.into(),
            store_schema: if version == FROM { 7 } else { 8 },
            downgrade_floor: FROM.into(),
        }
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    fake: Arc<Fake>,
    coordinator: InstallCoordinator,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths {
            bundle: dir.path().join("Applications/GIT STRATA.app"),
            plist: dir.path().join("Library/LaunchAgents/daemon.plist"),
            app_support: dir.path().join("support"),
            app_version: FROM.into(),
        };
        fs::create_dir_all(&paths.bundle).unwrap();
        let fake = Arc::new(Fake {
            epoch: Instant::now(),
            elapsed: AtomicU64::new(0),
            bundle: paths.bundle.clone(),
            plist: paths.plist.clone(),
            program: paths.bundle.join("Contents/MacOS/git-stratad"),
            update_lock: paths.app_support.join("update.lock"),
            state: Mutex::new(FakeState::default()),
        });
        let adapters = InstallAdapters {
            service: fake.clone(),
            daemon: fake.clone(),
            bundles: fake.clone(),
            host: fake.clone(),
            clock: fake.clone(),
        };
        let coordinator =
            InstallCoordinator::new(paths, adapters, Arc::new(tokio::sync::Mutex::new(())));
        Self {
            _dir: dir,
            fake,
            coordinator,
        }
    }

    fn record(&self, phase: Phase, predecessor: bool, was_enabled: Option<bool>) -> Txn {
        let txn = Txn {
            phase: phase.name().into(),
            from: FROM.into(),
            to: TO.into(),
            from_schema: 7,
            to_schema: 8,
            to_floor: FROM.into(),
            from_daemon: predecessor.then_some(DaemonIdentity {
                pid: PID,
                started: self.fake.now(),
                version: FROM.into(),
            }),
            program: self.fake.program.clone(),
            rollback_zip: self.coordinator.rollback_zip(FROM),
            was_enabled,
        };
        self.coordinator.store.begin(&txn).unwrap();
        txn
    }

    async fn install(&self) -> InstallOutcome {
        self.coordinator
            .install(TO, Some(8), &mut |_| {})
            .await
            .unwrap()
    }

    fn app(&self, version: &str) -> InstallCoordinator {
        InstallCoordinator::new(
            InstallPaths {
                bundle: self.fake.bundle.clone(),
                plist: self.fake.plist.clone(),
                app_support: self.coordinator.paths.app_support.clone(),
                app_version: version.into(),
            },
            InstallAdapters {
                service: self.fake.clone(),
                daemon: self.fake.clone(),
                bundles: self.fake.clone(),
                host: self.fake.clone(),
                clock: self.fake.clone(),
            },
            Arc::new(tokio::sync::Mutex::new(())),
        )
    }

    async fn reconcile(&self) -> InstallOutcome {
        let app = self.app(self.fake.data().bundle.unwrap_or(FROM));
        app.reconcile(&mut |_| {}).await.unwrap().unwrap()
    }

    fn txn(&self) -> Option<Txn> {
        self.coordinator.store.load().unwrap()
    }
}

impl Clock for Fake {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + self.seconds())
    }

    fn monotonic(&self) -> Instant {
        self.epoch + Duration::from_secs(self.seconds())
    }

    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
        Box::pin(async move {
            let at = self
                .elapsed
                .fetch_add(duration.as_secs(), AtomicOrdering::SeqCst);
            assert!(at < 1_000, "fake clock exceeded the test horizon");
        })
    }
}

impl ServiceManager for Fake {
    fn print(&self, label: &str) -> Result<Option<String>, GuiError> {
        assert_eq!(label, AGENT_LABEL);
        let mut state = self.data();
        self.record(&mut state, "print");
        if state.exit_unreadable {
            return Err(GuiError::launchctl_failed(
                "print",
                Some(1),
                "can't inspect",
            ));
        }
        if state.transient_print_failures > 0 {
            state.transient_print_failures -= 1;
            return Err(GuiError::launchctl_failed(
                "print",
                None,
                "temporary probe timeout",
            ));
        }
        if !state.loaded {
            return Ok(None);
        }
        let proc = state
            .pid
            .map(|pid| format!("\tpid = {pid}\n"))
            .unwrap_or_default();
        let exit = state
            .exit_report
            .map(str::to_owned)
            .unwrap_or_else(|| format!("last exit code = {}", state.last_exit));
        Ok(Some(format!(
            "gui/501/{label} = {{\n\tpath = {}\n\tprogram = {}\n{proc}\t{exit}\n\texit timeout = {}\n}}",
            self.plist.display(),
            self.program.display(),
            state.exit_timeout
        )))
    }

    fn print_disabled(&self, _: &str) -> Result<bool, GuiError> {
        let mut state = self.data();
        let failures = if state.disabled {
            &mut state.transient_fence_failures
        } else {
            &mut state.transient_enabled_failures
        };
        if *failures > 0 {
            *failures -= 1;
            return Err(GuiError::launchctl_failed(
                "print-disabled",
                None,
                "temporary probe timeout",
            ));
        }
        Ok(state.disabled)
    }

    fn disable(&self, _: &str) -> Result<(), GuiError> {
        let mut state = self.data();
        assert!(!state.disabled);
        if matches!(state.disable, Disable::PreflightFails) {
            self.record(&mut state, "disable_preflight");
            return Err(GuiError::launchctl_failed("print", None, "no reply"));
        }
        self.record(&mut state, "disable");
        if let Disable::Timeout(delay) = state.disable {
            state.disable_at = Some(self.seconds() + delay);
            return Err(GuiError::launchctl_failed("disable", None, "no reply"));
        }
        state.disabled = true;
        if matches!(state.disable, Disable::JournalWriteFails) {
            self.block_journal_writes();
        }
        if state.pending_on_disable {
            state.pending_on_disable = false;
            state.pid = Some(PID + 5);
            state.started = self.seconds();
        }
        Ok(())
    }

    fn enable(&self, _: &str) -> Result<(), GuiError> {
        let mut state = self.data();
        self.record(&mut state, "enable");
        state.disabled = false;
        if self.update_lock.is_dir() {
            fs::remove_dir(&self.update_lock).unwrap();
        }
        Ok(())
    }

    fn bootout(&self, _: &str) -> Result<(), GuiError> {
        let mut state = self.data();
        assert!(
            state.disabled && state.pid.is_none() && !state.lock_busy,
            "bootout without the stop fence"
        );
        let step = if matches!(state.bootout, Bootout::PreflightFails) {
            "bootout_preflight"
        } else {
            "bootout"
        };
        self.record(&mut state, step);
        match state.bootout {
            Bootout::Unload => {
                state.loaded = false;
                state.seal_at = None;
                state.lock_busy = state.lock_busy_after_unload;
                if state.bundle_probe_error_on_bootout {
                    state.bundle_probe_error = Some("codesign");
                }
                Ok(())
            }
            Bootout::Failed => Err(GuiError::launchctl_failed(
                "bootout",
                Some(5),
                "still loaded",
            )),
            Bootout::Uncertain => Ok(()),
            Bootout::PrintFailed => {
                state.exit_unreadable = true;
                Err(GuiError::launchctl_failed(
                    "bootout",
                    Some(5),
                    "not found or cannot inspect",
                ))
            }
            Bootout::PreflightFails => Err(GuiError::launchctl_failed("print", None, "no reply")),
        }
    }

    fn kickstart(&self, _: &str) -> Result<(), GuiError> {
        let mut state = self.data();
        assert!(state.loaded && state.pid.is_none());
        self.boot(&mut state, "kickstart");
        Ok(())
    }

    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        assert_eq!(plist, self.plist);
        let mut state = self.data();
        assert!(!state.loaded);
        self.boot(&mut state, "bootstrap");
        Ok(())
    }
}

impl InstallDaemon for Fake {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(async {
            let mut state = self.data();
            if state.boot_at.is_some() && matches!(state.boot, Boot::Permission(_)) {
                return Err(GuiError::new(
                    Code::Busy,
                    "store permission",
                    json!({"reason": "store_unavailable", "last_error": {"detail": {"errno": 13}}}),
                ));
            }
            if state.seal_at.is_some_and(|at| self.seconds() > at)
                && matches!(state.stop, Stop::Loop | Stop::RespawnExitsClean)
            {
                self.record(&mut state, "loop_probe");
                return Err(problem(Code::Busy, "daemon_down", "respawn has not served"));
            }
            let pid = state
                .pid
                .ok_or_else(|| problem(Code::Busy, "daemon_down", "daemon unavailable"))?;
            let started =
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + state.started);
            let busy = self.seconds() < state.drain_busy_until;
            Ok(json!({
                "pid": state.answering_pid.unwrap_or(pid),
                "started": strata_types::serde_rfc3339::format(&started).unwrap(),
                "version": state.answering_version.unwrap_or(state.daemon_version),
                "store_schema": 7,
                "features": if state.feature {vec!["daemon.drain"]} else {vec![]},
                "inflight": if busy {vec![json!({"phase": "confirming"})]} else {vec![]},
                "holds": if busy {vec![json!({"origin": "background"})]} else {vec![]},
                "draining": if state.lease {json!({"sealed": state.seal_at.is_some(), "lease_id": "lease"})} else {Value::Null}
            }))
        })
    }

    fn drain<'a>(
        &'a self,
        action: DrainAction<'a>,
    ) -> UpdateFuture<'a, Result<Option<String>, GuiError>> {
        Box::pin(async move {
            let mut state = self.data();
            assert!(state.loaded && state.pid.is_some());
            match action {
                DrainAction::Acquire { ttl_s } => {
                    assert_eq!(ttl_s, 120);
                    self.record(&mut state, "acquire");
                    state.lease = true;
                    Ok(Some("lease".into()))
                }
                DrainAction::Renew { lease_id, ttl_s } => {
                    assert_eq!(lease_id, "lease");
                    assert_eq!(ttl_s, 120);
                    self.record(&mut state, "renew");
                    Ok(Some("lease".into()))
                }
                DrainAction::Release { lease_id } => {
                    assert_eq!(lease_id, "lease");
                    assert!(state.seal_at.is_none());
                    self.record(&mut state, "release");
                    if !state.lease {
                        return Err(problem(Code::Busy, "lease_lost", "lease expired"));
                    }
                    state.lease = false;
                    Ok(None)
                }
                DrainAction::SealStop { lease_id } => {
                    assert_eq!(lease_id, "lease");
                    assert!(state.lease);
                    self.record(&mut state, "seal");
                    if let Some(reason) = state.seal_race.take() {
                        if matches!(reason, "reply_lost" | "clean_reply_lost") {
                            state.lease = false;
                            if reason == "clean_reply_lost" {
                                state.pid = None;
                                state.last_exit = 0;
                            }
                            return Err(problem(
                                Code::Unknown,
                                "client_timeout",
                                "seal reply lost",
                            ));
                        }
                        return Err(problem(Code::Busy, reason, "try again"));
                    }
                    state.seals += 1;
                    state.seal_at = Some(self.seconds());
                    Ok(None)
                }
            }
        })
    }

    fn quiescent(&self) -> Result<bool, GuiError> {
        let state = self.data();
        Ok(state.pid.is_none() && !state.lock_busy)
    }

    fn close_error(&self, _: SystemTime) -> Option<Value> {
        self.data().close_error.clone()
    }

    fn retained_store_schema(&self) -> Option<u32> {
        self.data().retained_schema
    }
}

impl InstallHost for Fake {
    fn validate(&self, _: &Path, plist: &Path, program: &Path) -> Result<(), GuiError> {
        assert_eq!(plist, self.plist);
        assert_eq!(program, self.program);
        Ok(())
    }

    fn prepare<'a>(
        &'a self,
        target: &'a str,
        _: Option<u32>,
        _: Option<u32>,
        rollback_zip: &'a Path,
        _: &'a mut Progress<'_>,
    ) -> UpdateFuture<'a, Result<PreparedUpdate, GuiError>> {
        Box::pin(async move {
            assert_eq!(rollback_zip.file_name().unwrap(), "GIT-STRATA-0.3.0.zip");
            let mut state = self.data();
            self.record(&mut state, "prepare");
            if state.stop_during_prepare {
                state.pid = None;
                state.last_exit = 1;
            }
            Ok(PreparedUpdate {
                update: VerifiedUpdate {
                    bytes: vec![7],
                    installer: Box::new(|_| panic!("test must use its fake bundle")),
                },
                info: Self::info(target),
            })
        })
    }

    fn daemon_version(&self) -> Result<String, GuiError> {
        Ok(self.data().daemon_version.into())
    }

    fn confirm_schema<'a>(
        &'a self,
        live_schema: Option<u32>,
        target: &'a BundleInfo,
    ) -> UpdateFuture<'a, Result<bool, GuiError>> {
        Box::pin(async move {
            assert_eq!(target.store_schema, 8);
            let mut state = self.data();
            self.record(&mut state, "confirm_schema");
            state.confirmed_schemas.push(live_schema);
            if let Some(version) = state.replace_during_confirmation.take() {
                state.bundle = Some(version);
                state.daemon_version = version;
            }
            Ok(state.consent)
        })
    }

    fn restore_kept(&self, kept: &Path, _: &Path) -> Result<(), GuiError> {
        assert!(kept.is_dir());
        let mut state = self.data();
        assert!(state.disabled);
        self.record(&mut state, "restore_kept");
        fs::rename(kept, &self.bundle).unwrap();
        state.bundle = Some(FROM);
        state.daemon_version = FROM;
        Ok(())
    }
}

impl BundleOps for Fake {
    fn extract(&self, _: &[u8], _: &Path) -> Result<PathBuf, GuiError> {
        panic!("preparation belongs to the host")
    }

    fn read_info_plist(&self, bundle: &Path) -> Result<BundleInfo, GuiError> {
        let state = self.data();
        if bundle == self.bundle && state.bundle_probe_error == Some("plist") {
            return Err(problem(
                Code::Unknown,
                "test_bundle_io",
                "cannot read Info.plist",
            ));
        }
        let version = if bundle != self.bundle && bundle.is_dir() {
            Some(FROM)
        } else {
            state.bundle
        };
        version.map(Self::info).ok_or_else(unexpected)
    }

    fn zip_backup(&self, _: &Path, _: &Path) -> Result<(), GuiError> {
        panic!("preparation belongs to the host")
    }

    fn install(&self, update: VerifiedUpdate) -> tauri_plugin_updater::Result<()> {
        let mut state = self.data();
        assert!(state.disabled && !state.loaded && state.pid.is_none());
        assert_eq!(update.bytes, [7]);
        self.record(&mut state, "install");
        match state.swap {
            Swap::Replace => {
                state.bundle = Some(TO);
                state.daemon_version = TO;
                Ok(())
            }
            Swap::Unchanged => Err(tauri_plugin_updater::Error::Io(std::io::Error::other(
                "unchanged",
            ))),
            Swap::Missing | Swap::Kept => {
                state.bundle = None;
                if matches!(state.swap, Swap::Kept) {
                    let kept = self.kept();
                    fs::rename(&self.bundle, &kept).unwrap();
                    Err(tauri_plugin_updater::Error::PreviousAppNotRestored(kept))
                } else {
                    fs::remove_dir(&self.bundle).unwrap();
                    Err(tauri_plugin_updater::Error::Io(std::io::Error::other(
                        "missing",
                    )))
                }
            }
        }
    }

    fn restore_zip(&self, _: &Path, _: &Path) -> Result<(), GuiError> {
        let mut state = self.data();
        assert!(state.disabled && !state.loaded);
        self.record(&mut state, "restore_zip");
        if !state.restore_ok {
            return Err(unexpected());
        }
        fs::create_dir_all(&self.bundle).unwrap();
        state.bundle = Some(FROM);
        state.daemon_version = FROM;
        Ok(())
    }

    fn codesign_verify(&self, bundle: &Path) -> Result<(), GuiError> {
        let state = self.data();
        if bundle == self.bundle && state.bundle_probe_error == Some("codesign") {
            return Err(problem(
                Code::Unknown,
                "test_bundle_io",
                "codesign could not inspect this app",
            ));
        }
        if (bundle == self.bundle && state.bundle.is_some())
            || (bundle != self.bundle && bundle.is_dir())
        {
            Ok(())
        } else {
            Err(problem(
                Code::CheckFailed,
                "bad_codesign",
                "invalid signature",
            ))
        }
    }
}

#[tokio::test]
async fn swaps_under_the_fence_and_only_reconcile_clears_the_txn() {
    let h = Harness::new();
    h.fake.data().close_error = Some(json!({"message": "closing a handle failed"}));
    let mut states = Vec::new();
    assert_eq!(
        h.coordinator
            .install(TO, Some(8), &mut |s| states.push(s))
            .await
            .unwrap(),
        InstallOutcome::Relaunch
    );
    assert_eq!(
        h.fake.trace(),
        [
            "prepare",
            "acquire",
            "seal",
            "disable",
            "bootout",
            "install",
            "enable",
            "bootstrap"
        ]
    );
    assert!(states.iter().any(|state| matches!(
        state,
        UpdateState::StopDetail {
            close_error: Some(_),
            ..
        }
    )));
    assert_eq!(h.txn().unwrap().phase, "relaunching");
    let error = h.coordinator.reconcile(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("update_app_version"));
    assert_eq!(h.txn().unwrap().phase, "relaunching");
    assert_eq!(h.reconcile().await, InstallOutcome::Updated);
    assert!(h.txn().is_none());
    assert_eq!(
        h.fake
            .trace()
            .iter()
            .filter(|op| **op == "bootstrap")
            .count(),
        1
    );
}

#[tokio::test]
async fn preparation_rechecks_the_signed_schema_after_the_daemon_goes_down() {
    for consent in [false, true] {
        let h = Harness::new();
        {
            let mut f = h.fake.data();
            f.stop_during_prepare = true;
            f.retained_schema = None;
            f.consent = consent;
        }
        let outcome = h.coordinator.install(TO, Some(8), &mut |_| {}).await;
        assert_eq!(h.fake.data().confirmed_schemas, [None]);
        assert!(!h.fake.trace().contains(&"acquire"));
        if consent {
            assert_eq!(outcome.unwrap(), InstallOutcome::Relaunch);
            assert!(h.txn().unwrap().from_daemon.is_none());
        } else {
            assert_eq!(
                outcome.unwrap_err().reason(),
                Some("update_schema_not_confirmed")
            );
            assert_eq!(h.fake.trace(), ["prepare", "confirm_schema"]);
            assert!(h.txn().is_none());
        }
    }
}

#[tokio::test]
async fn rejects_disabled_foreign_and_version_drift_before_the_backup_or_drain() {
    for (disabled, pid, bundled, answering, feature, reason) in [
        (true, None, FROM, FROM, true, "agent_disabled"),
        (false, Some(999), FROM, FROM, true, "daemon_identity"),
        (false, None, FROM, FROM, false, "daemon_too_old"),
        (false, None, TO, FROM, true, "daemon_restart_required"),
        (false, None, FROM, TO, true, "daemon_bundle_version"),
    ] {
        let h = Harness::new();
        {
            let mut f = h.fake.data();
            f.disabled = disabled;
            f.answering_pid = pid;
            f.daemon_version = bundled;
            f.answering_version = Some(answering);
            f.feature = feature;
        }
        let e = h
            .coordinator
            .install(TO, None, &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(e.reason(), Some(reason));
        assert!(h.fake.trace().is_empty());
        assert!(h.txn().is_none());
    }
}

#[tokio::test]
async fn an_unresolved_move_refuses_install_before_preparation() {
    let h = Harness::new();
    h.coordinator
        .store
        .edit_document(|document| {
            document.insert("move".into(), json!({"phase": "bootout"}));
            Ok(())
        })
        .unwrap();
    let error = h
        .coordinator
        .install(TO, Some(8), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("update_move_pending"));
    assert!(h.fake.trace().is_empty());
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn renews_drain_and_cancel_releases_the_lease_without_a_seal() {
    let h = Harness::new();
    h.fake.data().drain_busy_until = 90;
    let mut saw_holders = false;
    let mut requested = false;
    let result = h
        .coordinator
        .install(TO, Some(8), &mut |state| {
            if let UpdateState::Draining {
                inflight,
                background,
            } = state
            {
                saw_holders |=
                    inflight == [json!({"phase": "confirming"})] && background.len() == 1;
                if h.fake.seconds() >= 35 && !requested {
                    h.coordinator.cancel().unwrap();
                    requested = true;
                }
            }
        })
        .await
        .unwrap();
    assert!(saw_holders);
    assert_eq!(result, InstallOutcome::Cancelled);
    assert_eq!(h.fake.trace(), ["prepare", "acquire", "renew", "release"]);
    assert_eq!(
        h.fake
            .data()
            .calls
            .iter()
            .find(|(op, _)| *op == "renew")
            .unwrap()
            .1,
        30
    );
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn cancelling_with_an_expired_drain_lease_finishes_without_sealing() {
    let h = Harness::new();
    h.fake.data().drain_busy_until = 90;
    let outcome = h
        .coordinator
        .install(TO, Some(8), &mut |state| {
            if matches!(state, UpdateState::Draining { .. }) {
                h.fake.data().lease = false;
                h.coordinator.cancel().unwrap();
            }
        })
        .await
        .unwrap();
    assert_eq!(outcome, InstallOutcome::Cancelled);
    assert_eq!(h.fake.trace(), ["prepare", "acquire", "release"]);
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn seal_not_idle_or_lost_retries_from_drain() {
    for reason in ["not_idle", "lease_lost", "reply_lost"] {
        let h = Harness::new();
        h.fake.data().seal_race = Some(reason);
        assert_eq!(h.install().await, InstallOutcome::Relaunch);
        let trace = h.fake.trace();
        assert_eq!(trace.iter().filter(|op| **op == "seal").count(), 2);
        assert_eq!(trace.iter().filter(|op| **op == "bootout").count(), 1);
    }
}

#[tokio::test]
async fn clean_exit_with_lost_seal_reply_requires_two_fenced_samples() {
    let h = Harness::new();
    h.fake.data().seal_race = Some("clean_reply_lost");
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    let f = h.fake.data();
    let disable = f.calls.iter().position(|(op, _)| *op == "disable").unwrap();
    let bootout = f.calls.iter().position(|(op, _)| *op == "bootout").unwrap();
    let samples = &f.calls[disable + 1..bootout];
    assert_eq!(samples.len(), 2);
    assert!(samples.iter().all(|(op, _)| *op == "print"));
    assert!(samples[1].1 - samples[0].1 >= NO_PID_GAP.as_secs());
}

#[tokio::test]
async fn clean_exit_of_a_respawn_after_seal_requires_two_fenced_samples() {
    let h = Harness::new();
    h.fake.data().stop = Stop::RespawnExitsClean;
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    let f = h.fake.data();
    let disable = f.calls.iter().position(|(op, _)| *op == "disable").unwrap();
    let bootout = f.calls.iter().position(|(op, _)| *op == "bootout").unwrap();
    assert!(f.calls[..disable].contains(&("loop_probe", 1)));
    let samples = &f.calls[disable + 1..bootout];
    assert_eq!(samples.len(), 2);
    assert!(samples.iter().all(|(op, _)| *op == "print"));
    assert!(samples[1].1 - samples[0].1 >= NO_PID_GAP.as_secs());
}

#[tokio::test]
async fn retries_a_transient_stop_read_without_abandoning_the_sealed_daemon() {
    let h = Harness::new();
    h.fake.data().transient_fence_failures = 1;
    let outcome = h
        .coordinator
        .install(TO, None, &mut |state| {
            if state == UpdateState::Stopping {
                h.fake.data().transient_print_failures = 1;
            }
        })
        .await
        .unwrap();
    assert_eq!(outcome, InstallOutcome::Relaunch);
    assert!(
        h.fake
            .trace()
            .ends_with(&["bootout", "install", "enable", "bootstrap"])
    );
}

#[tokio::test]
async fn retries_the_first_fence_read_after_sealed_stop() {
    let h = Harness::new();
    h.fake.data().close_error = Some(json!("close reported a warning"));
    let outcome = h
        .coordinator
        .install(TO, None, &mut |state| {
            if matches!(
                state,
                UpdateState::StopDetail {
                    close_error: Some(_),
                    ..
                }
            ) {
                h.fake.data().transient_enabled_failures = 1;
            }
        })
        .await
        .unwrap();
    assert_eq!(outcome, InstallOutcome::Relaunch);
    assert!(
        h.fake
            .trace()
            .ends_with(&["disable", "bootout", "install", "enable", "bootstrap"])
    );
}

#[tokio::test]
async fn busy_lock_after_unload_keeps_the_fence_until_a_quiet_reconcile() {
    let h = Harness::new();
    h.fake.data().lock_busy_after_unload = true;
    assert_eq!(h.install().await, InstallOutcome::FenceUncertain);
    let txn = h.txn().unwrap();
    assert_eq!(txn.phase, Phase::Bootout.name());
    assert_eq!(txn.was_enabled, Some(true));
    assert!(h.fake.data().disabled);

    assert_eq!(h.reconcile().await, InstallOutcome::FenceUncertain);
    assert_eq!(h.txn(), Some(txn));
    assert!(h.fake.data().disabled);
    assert!(h.fake.trace().ends_with(&["disable", "bootout"]));
    assert!(!h.fake.trace().contains(&"install"));

    h.fake.data().lock_busy = false;
    assert_eq!(
        h.reconcile().await,
        InstallOutcome::Interrupted { from: FROM.into() }
    );
    assert!(
        h.fake
            .trace()
            .ends_with(&["bootout", "enable", "bootstrap"])
    );
    assert!(h.txn().is_none());
}

async fn assert_unloaded_recovery_keeps_fence_while_lock_held(phase: Phase) {
    let h = Harness::new();
    let txn = h.record(phase, true, Some(true));
    {
        let mut f = h.fake.data();
        f.loaded = false;
        f.disabled = true;
        f.pid = None;
        f.lock_busy = true;
    }

    assert_eq!(h.reconcile().await, InstallOutcome::FenceUncertain);
    assert_eq!(h.txn(), Some(txn));
    assert!(h.fake.data().disabled);
    assert!(h.fake.trace().is_empty());

    h.fake.data().lock_busy = false;
    assert_eq!(
        h.reconcile().await,
        InstallOutcome::Interrupted { from: FROM.into() }
    );
    assert_eq!(h.fake.trace(), ["enable", "bootstrap"]);
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn installing_recovery_keeps_fence_while_lock_held() {
    assert_unloaded_recovery_keeps_fence_while_lock_held(Phase::Installing).await;
}

#[tokio::test]
async fn unloaded_fencing_recovery_keeps_fence_while_lock_held() {
    assert_unloaded_recovery_keeps_fence_while_lock_held(Phase::Fencing).await;
}

#[tokio::test]
async fn inconclusive_inspection_before_swap_reports_recovery_and_keeps_the_fence() {
    let h = Harness::new();
    h.fake.data().bundle_probe_error_on_bootout = true;
    let mut states = Vec::new();
    let error = h
        .coordinator
        .install(TO, None, &mut |state| states.push(state))
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("test_bundle_io"));
    assert_eq!(states.last(), Some(&UpdateState::RestoreFailed));
    assert!(h.fake.trace().ends_with(&["disable", "bootout"]));
    assert!(h.fake.data().disabled);
    assert_eq!(h.txn().unwrap().phase, "bootout");
}

#[tokio::test]
async fn not_serving_accepts_nonzero_exit_and_cancels_by_reenabling() {
    for cancel in [false, true] {
        let h = Harness::new();
        {
            let mut f = h.fake.data();
            f.pid = None;
            f.last_exit = 17;
        }
        let result = h
            .coordinator
            .install(TO, Some(8), &mut |state| {
                if cancel && state == UpdateState::Stopping {
                    h.coordinator.cancel().unwrap();
                }
            })
            .await
            .unwrap();
        let trace = h.fake.trace();
        assert!(!trace.contains(&"acquire"));
        assert!(!trace.contains(&"kickstart"));
        assert!(!h.fake.data().disabled);
        if cancel {
            assert_eq!(result, InstallOutcome::Cancelled);
            assert_eq!(trace, ["prepare"]);
            assert!(h.txn().is_none());
        } else {
            assert_eq!(result, InstallOutcome::Relaunch);
            let f = h.fake.data();
            let at = |name| f.calls.iter().find(|(op, _)| *op == name).unwrap().1;
            assert!(at("prepare") >= 5);
            assert!(at("bootout") - at("disable") >= 5);
            assert!(h.txn().unwrap().from_daemon.is_none());
        }
    }
}

#[tokio::test]
async fn pid_appearing_after_disable_is_drained_before_any_bootout() {
    let h = Harness::new();
    {
        let mut f = h.fake.data();
        f.pid = None;
        f.last_exit = 1;
        f.pending_on_disable = true;
    }
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    assert_eq!(
        h.fake.trace(),
        [
            "prepare",
            "disable",
            "enable",
            "acquire",
            "seal",
            "disable",
            "bootout",
            "install",
            "enable",
            "bootstrap"
        ]
    );
}

#[tokio::test]
async fn slow_sealed_cancel_is_saved_then_waits_for_exit_and_kickstarts() {
    let h = Harness::new();
    h.fake.data().stop = Stop::Clean(130);
    let mut saw_slow = false;
    let result = h
        .coordinator
        .install(TO, Some(8), &mut |state| {
            if !saw_slow && matches!(state, UpdateState::StopDetail { slow: true, .. }) {
                assert_eq!(h.fake.seconds(), 120);
                h.coordinator.cancel().unwrap();
                assert_eq!(h.txn().unwrap().phase, "cancel_after_stop");
                saw_slow = true;
            }
        })
        .await
        .unwrap();
    assert!(saw_slow);
    assert_eq!(result, InstallOutcome::Cancelled);
    assert_eq!(h.fake.trace(), ["prepare", "acquire", "seal", "kickstart"]);
    assert_eq!(h.fake.seconds(), 130);
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn step_seven_never_boots_out_an_unsealed_respawn() {
    for cancel in [false, true] {
        let h = Harness::new();
        h.fake.data().stop = Stop::Respawn;
        let mut requested = false;
        let outcome = h
            .coordinator
            .install(TO, None, &mut |state| {
                if cancel && !requested && state == UpdateState::Stopping {
                    h.coordinator.cancel().unwrap();
                    requested = true;
                }
            })
            .await
            .unwrap();
        if cancel {
            assert_eq!(outcome, InstallOutcome::Cancelled);
            assert_eq!(h.fake.trace(), ["prepare", "acquire", "seal"]);
            assert!(h.txn().is_none());
        } else {
            assert_eq!(outcome, InstallOutcome::Relaunch);
            assert_eq!(
                h.fake.trace(),
                [
                    "prepare",
                    "acquire",
                    "seal",
                    "acquire",
                    "seal",
                    "disable",
                    "bootout",
                    "install",
                    "enable",
                    "bootstrap"
                ]
            );
        }
    }
}

#[tokio::test]
async fn pre_swap_respawn_must_verify_the_bundle_before_reporting_it_unchanged() {
    let h = Harness::new();
    let txn = h.record(Phase::Stopping, true, None);
    {
        let mut f = h.fake.data();
        f.pid = Some(PID + 1);
        f.started = 10;
        f.bundle = Some(TO);
    }
    let error = h.app(FROM).reconcile(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("update_txn_unexpected"));
    assert_eq!(h.txn(), Some(txn));
    assert!(h.fake.trace().is_empty());
}

#[tokio::test]
async fn nonzero_crash_loop_requires_liveness_samples_and_cancel_preserves_a_changed_bundle() {
    let h = Harness::new();
    {
        let mut f = h.fake.data();
        f.stop = Stop::Loop;
        f.exit_report = Some("last exit code = 78: EX_CONFIG");
    }
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    let disabled_at = h
        .fake
        .data()
        .calls
        .iter()
        .find(|(op, _)| *op == "disable")
        .unwrap()
        .1;
    assert_eq!(disabled_at, 120);
    assert!(
        h.fake
            .data()
            .calls
            .iter()
            .any(|(op, _)| *op == "loop_probe")
    );
    assert!(h.txn().unwrap().from_daemon.is_none());

    for different in [false, true] {
        let h = Harness::new();
        {
            let mut f = h.fake.data();
            f.stop = Stop::Loop;
            f.exit_report = Some("last terminating signal = Killed: 9");
        }
        let mut requested = false;
        let outcome = h
            .coordinator
            .install(TO, None, &mut |state| {
                if !requested && state == UpdateState::Stopping {
                    h.coordinator.cancel().unwrap();
                    if different {
                        h.fake.data().bundle = Some(TO);
                    }
                    requested = true;
                }
            })
            .await;
        assert_eq!(h.fake.seconds(), 11);
        assert!(
            h.fake
                .data()
                .calls
                .iter()
                .any(|(op, _)| *op == "loop_probe")
        );
        assert_eq!(h.fake.trace(), ["prepare", "acquire", "seal"]);
        if different {
            assert_eq!(outcome.unwrap_err().reason(), Some("update_txn_unexpected"));
            assert_eq!(h.txn().unwrap().phase, "cancel_after_stop");
        } else {
            assert_eq!(outcome.unwrap(), InstallOutcome::NotStarting);
            assert!(h.txn().is_none());
        }
    }
}

#[tokio::test]
async fn cancel_rejects_older_or_unreadable_and_confirms_a_newer_migration() {
    for (installed, version, consent, expected) in [
        (Some("0.2.9"), "0.2.9", true, InstallOutcome::CancelBlocked),
        (None, FROM, true, InstallOutcome::CancelBlocked),
        (Some(TO), TO, false, InstallOutcome::CancelBlocked),
        (Some(TO), TO, true, InstallOutcome::Cancelled),
    ] {
        let h = Harness::new();
        let mut requested = false;
        let result = h
            .coordinator
            .install(TO, None, &mut |state| {
                if !requested && state == UpdateState::Stopping {
                    let mut fake = h.fake.data();
                    fake.bundle = installed;
                    fake.daemon_version = version;
                    fake.consent = consent;
                    drop(fake);
                    h.coordinator.cancel().unwrap();
                    requested = true;
                }
            })
            .await
            .unwrap();
        assert_eq!(result, expected);
        assert_eq!(h.fake.trace().contains(&"confirm_schema"), version == TO);
        assert_eq!(
            h.fake.trace().contains(&"kickstart"),
            result == InstallOutcome::Cancelled
        );
        assert_eq!(h.txn().is_none(), result == InstallOutcome::Cancelled);
    }
}

#[tokio::test]
async fn cancel_refuses_to_start_a_bundle_replaced_during_schema_confirmation() {
    let h = Harness::new();
    let txn = h.record(Phase::CancelAfterStop, true, None);
    {
        let mut f = h.fake.data();
        f.pid = None;
        f.bundle = Some(TO);
        f.daemon_version = TO;
        f.replace_during_confirmation = Some("0.2.9");
    }
    let result = h.app(FROM).reconcile(&mut |_| {}).await;
    assert_eq!(h.fake.trace(), ["confirm_schema"]);
    assert_eq!(result.unwrap(), Some(InstallOutcome::CancelBlocked));
    assert_eq!(h.txn(), Some(txn));
}

#[tokio::test]
async fn only_definite_bootout_failure_restores_enabled_state_and_restarts() {
    for failure in [Bootout::Failed, Bootout::Uncertain, Bootout::PrintFailed] {
        let h = Harness::new();
        h.fake.data().bootout = failure;
        let outcome = h.install().await;
        let trace = h.fake.trace();
        if matches!(failure, Bootout::Failed) {
            assert_eq!(outcome, InstallOutcome::BootoutFailed);
            assert!(trace.ends_with(&["bootout", "enable", "kickstart"]));
            let fake = h.fake.data();
            let bootout = fake
                .calls
                .iter()
                .find(|(op, _)| *op == "bootout")
                .unwrap()
                .1;
            let enable = fake.calls.iter().find(|(op, _)| *op == "enable").unwrap().1;
            assert!(enable - bootout >= 8);
            assert!(h.txn().is_none());
        } else {
            assert_eq!(outcome, InstallOutcome::FenceUncertain);
            assert!(trace.ends_with(&["disable", "bootout"]));
            assert!(h.fake.data().disabled);
            assert_eq!(h.txn().unwrap().phase, "bootout");
        }
    }
}

#[tokio::test]
async fn failed_bootout_preflight_restores_label_and_relaunch_verifies_daemon() {
    let h = Harness::new();
    {
        let mut f = h.fake.data();
        f.bootout = Bootout::PreflightFails;
        f.boot = Boot::Never;
    }
    let error = h
        .coordinator
        .install(TO, Some(8), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("start_timeout"));
    assert_eq!(h.txn().unwrap().phase, Phase::BootoutFailed.name());
    assert!(!h.fake.data().disabled);
    let trace = h.fake.trace();
    assert!(trace.ends_with(&["bootout_preflight", "enable", "kickstart"]));
    assert!(!trace.contains(&"bootout"));
    {
        let f = h.fake.data();
        let at = |step| f.calls.iter().find(|(op, _)| *op == step).unwrap().1;
        assert_eq!(at("enable"), at("bootout_preflight"));
    }

    h.fake.data().boot = Boot::Ready;
    let elapsed = h.fake.seconds();
    assert_eq!(h.reconcile().await, InstallOutcome::BootoutFailed);
    assert_eq!(h.fake.seconds(), elapsed);
    assert_eq!(h.fake.trace(), trace);
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn outcome_comes_from_verified_files_and_failed_restore_keeps_the_fence() {
    for (swap, restore, outcome, suffix) in [
        (
            Swap::Unchanged,
            true,
            InstallOutcome::Unchanged,
            vec!["install", "enable", "bootstrap"],
        ),
        (
            Swap::Missing,
            true,
            InstallOutcome::Restored,
            vec!["install", "restore_zip", "enable", "bootstrap"],
        ),
        (
            Swap::Kept,
            true,
            InstallOutcome::Restored,
            vec!["install", "restore_kept", "enable", "bootstrap"],
        ),
        (
            Swap::Missing,
            false,
            InstallOutcome::RestoreFailed,
            vec!["install", "restore_zip"],
        ),
    ] {
        let h = Harness::new();
        {
            let mut f = h.fake.data();
            f.swap = swap;
            f.restore_ok = restore;
        }
        assert_eq!(h.install().await, outcome);
        assert!(h.fake.trace().ends_with(&suffix));
        assert_eq!(h.txn().is_some(), !restore);
        assert_eq!(h.fake.data().disabled, !restore);
    }
}

#[tokio::test]
async fn inconclusive_recovery_only_reports_the_error_and_keeps_the_fence() {
    for phase in [Phase::Installing, Phase::Restoring] {
        for probe in ["codesign", "plist"] {
            let h = Harness::new();
            let txn = h.record(phase, true, Some(true));
            let kept = h.fake.kept();
            fs::create_dir(&kept).unwrap();
            h.coordinator
                .store
                .edit_install(&txn, None, |details| {
                    details.kept_bundle = Some(kept.clone());
                })
                .unwrap();
            {
                let mut f = h.fake.data();
                f.bundle_probe_error = Some(probe);
                f.disabled = true;
                f.loaded = false;
                f.pid = None;
            }
            let mut states = Vec::new();
            let error = h
                .app(FROM)
                .reconcile(&mut |state| states.push(state))
                .await
                .unwrap_err();
            assert_eq!(error.reason(), Some("test_bundle_io"));
            assert_eq!(states.last(), Some(&UpdateState::RestoreFailed));
            assert!(h.fake.bundle.is_dir() && kept.is_dir());
            assert!(h.fake.trace().is_empty());
            assert!(h.fake.data().disabled);
            assert_eq!(h.txn(), Some(txn));
        }
    }
}

#[tokio::test]
async fn crash_recovery_uses_the_verified_kept_bundle_without_a_zip() {
    for phase in [Phase::Installing, Phase::Restoring] {
        let h = Harness::new();
        let txn = h.record(phase, true, Some(true));
        let kept = if phase == Phase::Installing {
            h.fake.kept()
        } else {
            let parent = h.fake.bundle.with_file_name("tauri_current_app-test");
            fs::create_dir(&parent).unwrap();
            parent.join("current_app")
        };
        h.coordinator
            .store
            .edit_install(&txn, None, |details| {
                if phase == Phase::Installing {
                    details.kept_candidates = vec![kept.clone()];
                } else {
                    details.kept_bundle = Some(kept.clone());
                }
            })
            .unwrap();
        fs::rename(&h.fake.bundle, &kept).unwrap();
        {
            let mut f = h.fake.data();
            f.bundle = None;
            f.restore_ok = false;
            f.pid = None;
            f.disabled = true;
            f.loaded = false;
        }
        assert_eq!(h.reconcile().await, InstallOutcome::Restored);
        assert_eq!(h.fake.trace(), ["restore_kept", "enable", "bootstrap"]);
        assert!(h.txn().is_none());
    }
}

#[tokio::test]
async fn unchanged_not_serving_finishes_after_verified_bootstrap() {
    let h = Harness::new();
    {
        let mut f = h.fake.data();
        f.pid = None;
        f.last_exit = 12;
        f.swap = Swap::Unchanged;
        f.boot = Boot::Never;
    }
    assert_eq!(h.install().await, InstallOutcome::Unchanged);
    assert!(
        h.fake
            .trace()
            .ends_with(&["install", "enable", "bootstrap"])
    );
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn startup_reconciles_every_written_phase() {
    for (phase, outcome) in [
        (
            Phase::Draining,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (
            Phase::Stopping,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (Phase::CancelAfterStop, InstallOutcome::Cancelled),
        (
            Phase::Fencing,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (
            Phase::Disabling,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (
            Phase::Bootout,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (
            Phase::Installing,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (Phase::Restoring, InstallOutcome::Restored),
        (Phase::StartingDaemon, InstallOutcome::Updated),
        (Phase::Relaunching, InstallOutcome::Updated),
        (Phase::WaitingDaemon, InstallOutcome::Updated),
        (Phase::WaitingPermission, InstallOutcome::Updated),
        (
            Phase::Interrupted,
            InstallOutcome::Interrupted { from: FROM.into() },
        ),
        (Phase::Unchanged, InstallOutcome::Unchanged),
        (Phase::Restored, InstallOutcome::Restored),
        (Phase::BootoutFailed, InstallOutcome::BootoutFailed),
    ] {
        let h = Harness::new();
        let fenced = !matches!(
            phase,
            Phase::Draining | Phase::Stopping | Phase::CancelAfterStop
        );
        h.record(phase, true, fenced.then_some(true));
        {
            let mut f = h.fake.data();
            if phase != Phase::Draining {
                f.pid = None;
            }
            f.disabled = fenced;
            f.loaded = !fenced
                || matches!(
                    phase,
                    Phase::Fencing | Phase::Disabling | Phase::BootoutFailed
                );
            if outcome == InstallOutcome::Updated {
                f.bundle = Some(TO);
                f.daemon_version = TO;
            }
            if phase == Phase::Restoring {
                f.bundle = None;
            }
        }
        assert_eq!(h.reconcile().await, outcome, "phase {}", phase.name());
        assert!(h.txn().is_none(), "phase {}", phase.name());
        assert!(!h.fake.data().disabled);
        let trace = h.fake.trace();
        assert!(!trace.contains(&"prepare"));
        assert!(!trace.contains(&"install"));
        if phase == Phase::Bootout {
            assert_eq!(trace, ["enable", "bootstrap"]);
        }
    }
}

#[tokio::test]
async fn startup_keeps_unknown_phases_and_does_not_enable_an_originally_disabled_label() {
    let h = Harness::new();
    let mut txn = h.record(Phase::Relaunching, false, Some(false));
    {
        let mut f = h.fake.data();
        f.disabled = true;
        f.pid = None;
        f.loaded = false;
        f.bundle = Some(TO);
        f.daemon_version = TO;
    }
    let e = h.app(TO).reconcile(&mut |_| {}).await.unwrap_err();
    assert_eq!(e.reason(), Some("agent_disabled"));
    assert!(h.fake.trace().is_empty());
    assert!(h.fake.data().disabled);

    txn.phase = "future_phase".into();
    h.coordinator.store.save(&txn).unwrap();
    let e = h.coordinator.reconcile(&mut |_| {}).await.unwrap_err();
    assert_eq!(e.reason(), Some("update_phase_unknown"));
    assert_eq!(h.txn(), Some(txn));
    let e = h
        .coordinator
        .install(TO, None, &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(e.reason(), Some("update_txn_pending"));
    assert!(h.fake.trace().is_empty());
}

#[tokio::test]
async fn restart_tolerates_one_codesign_exit_and_pauses_the_budget_for_tcc() {
    for (boot, seconds, permission) in [
        (Boot::Codesign(70), 70, false),
        (Boot::Permission(225), 225, true),
    ] {
        let h = Harness::new();
        h.fake.data().boot = boot;
        assert_eq!(h.install().await, InstallOutcome::Relaunch);
        let installed_at = h.fake.seconds();
        let mut states = Vec::new();
        assert_eq!(
            h.app(TO).reconcile(&mut |s| states.push(s)).await.unwrap(),
            Some(InstallOutcome::Updated)
        );
        assert_eq!(h.fake.seconds() - installed_at, seconds);
        assert_eq!(states.contains(&UpdateState::WaitingPermission), permission);
        assert_eq!(states.last(), Some(&UpdateState::Done));
        assert!(h.txn().is_none());
    }
}

#[tokio::test]
async fn restart_timeout_starts_at_bootstrap_and_checks_version_and_launchd_pid() {
    for (boot, pid, version, predecessor, expected) in [
        (Boot::Never, None, None, true, "start_timeout"),
        (Boot::Ready, Some(999), None, false, "daemon_identity"),
        (Boot::Ready, None, Some(FROM), true, "daemon_version"),
    ] {
        let h = Harness::new();
        {
            let mut f = h.fake.data();
            f.boot = boot;
            if !predecessor {
                f.pid = None;
            }
        }
        assert_eq!(h.install().await, InstallOutcome::Relaunch);
        let at = h.fake.seconds();
        {
            let mut f = h.fake.data();
            f.answering_pid = pid;
            f.answering_version = version;
        }
        h.fake.sleep(Duration::from_secs(20)).await;
        let mut states = Vec::new();
        let e = h
            .app(TO)
            .reconcile(&mut |s| states.push(s))
            .await
            .unwrap_err();
        assert_eq!(e.reason(), Some(expected));
        let after = if matches!(boot, Boot::Never) {
            155
        } else {
            150
        };
        assert_eq!(h.fake.seconds() - at, after);
        assert_eq!(
            states.last(),
            Some(&UpdateState::DaemonFailed {
                reason: expected.into()
            })
        );
        assert!(h.txn().is_some());
    }
}

#[test]
fn shutdown_probe_distinguishes_refused_listening_and_held_lock() {
    use std::os::unix::net::UnixListener;

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("daemon.sock");
    let lock = dir.path().join("daemon.lock");
    let idle = || socket_refusing_and_lock_free(&socket, &lock).unwrap();
    assert!(idle());

    let listener = UnixListener::bind(&socket).unwrap();
    assert!(!idle());
    drop(listener);
    assert!(socket.exists());
    assert!(idle());

    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lock)
        .unwrap();
    holder.lock().unwrap();
    assert!(!idle());
    holder.unlock().unwrap();
    assert!(idle());

    fs::remove_file(&socket).unwrap();
    fs::write(&socket, b"not a socket").unwrap();
    assert!(!socket_refusing_and_lock_free(&socket, &lock).unwrap_or(false));
}

#[tokio::test]
async fn a_timed_out_disable_stays_pending_until_the_late_effect_is_observed() {
    let h = Harness::new();
    h.fake.data().disable = Disable::Timeout(130);
    assert_eq!(h.install().await, InstallOutcome::FenceUncertain);
    assert!(h.fake.trace().ends_with(&["disable"]));
    assert!(!h.fake.data().disabled);
    let txn = h.txn().unwrap();
    assert_eq!(txn.was_enabled, Some(true));
    assert_eq!(txn.phase, "disabling");
    assert_eq!(
        h.reconcile().await,
        InstallOutcome::Interrupted { from: FROM.into() }
    );
    assert!(h.fake.trace().ends_with(&[
        "disable",
        "disable_effect",
        "bootout",
        "enable",
        "bootstrap"
    ]));
    assert!(!h.fake.data().disabled);
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn failed_fencing_write_after_disable_restores_the_label_and_leaves_a_recoverable_txn() {
    for recovering in [false, true] {
        let h = Harness::new();
        let error = if recovering {
            h.record(Phase::Disabling, true, Some(true));
            {
                let mut f = h.fake.data();
                f.pid = None;
                f.disabled = true;
            }
            h.fake.block_journal_writes();
            h.app(FROM).reconcile(&mut |_| {}).await.unwrap_err()
        } else {
            h.fake.data().disable = Disable::JournalWriteFails;
            h.coordinator
                .install(TO, Some(8), &mut |_| {})
                .await
                .unwrap_err()
        };
        assert_eq!(error.reason(), Some("update_txn_io"));
        assert!(!h.fake.data().disabled, "recovering: {recovering}");
        let txn = h.txn().unwrap();
        assert_eq!(txn.phase, Phase::Fencing.name());
        assert_eq!(txn.was_enabled, Some(true));
        let trace = h.fake.trace();
        assert_eq!(trace.last(), Some(&"enable"));
        assert!(!trace.contains(&"bootout"));
        assert!(!trace.contains(&"install"));

        assert_eq!(
            h.reconcile().await,
            InstallOutcome::Interrupted { from: FROM.into() }
        );
        assert!(h.fake.trace().ends_with(&["enable", "kickstart"]));
        assert!(h.txn().is_none());
    }
}

#[tokio::test]
async fn failed_disable_preflight_unwinds_without_waiting_for_a_disable() {
    let h = Harness::new();
    h.fake.data().disable = Disable::PreflightFails;
    assert_eq!(
        h.install().await,
        InstallOutcome::Interrupted { from: FROM.into() }
    );
    assert_eq!(
        h.fake.trace(),
        [
            "prepare",
            "acquire",
            "seal",
            "disable_preflight",
            "kickstart"
        ]
    );
    assert!(!h.fake.data().disabled);
    assert!(h.fake.seconds() < STOP_BUDGET.as_secs());
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn startup_honors_not_serving_cancel_before_or_after_disable_settles() {
    for phase in [Phase::Fencing, Phase::Disabling] {
        let h = Harness::new();
        let txn = h.record(phase, false, Some(true));
        {
            let mut f = h.fake.data();
            f.pid = None;
            f.disabled = phase == Phase::Fencing;
            f.disable_at = (phase == Phase::Disabling).then_some(10);
        }
        let closing_app = h.app(FROM);
        closing_app.set_cancel(CancelMode::NotServing, false);
        closing_app.cancel().unwrap();
        assert!(
            closing_app
                .store
                .install_details(&txn)
                .unwrap()
                .cancel_not_serving
        );
        drop(closing_app);
        assert_eq!(h.reconcile().await, InstallOutcome::Cancelled);
        let expected = if phase == Phase::Disabling {
            vec!["disable_effect", "enable"]
        } else {
            vec!["enable"]
        };
        assert_eq!(h.fake.trace(), expected);
        assert!(!h.fake.data().disabled);
        assert!(h.txn().is_none());
    }
}

#[tokio::test]
async fn permission_pause_survives_relaunch_and_stopped_path_is_proven_at_timeout() {
    let h = Harness::new();
    let txn = h.record(Phase::WaitingPermission, false, Some(true));
    h.coordinator
        .store
        .edit_install(&txn, None, |details| {
            details.start_requested_ms = Some(unix_millis(h.fake.now()));
            details.permission_wait_elapsed_ms = Some(40_000);
        })
        .unwrap();
    {
        let mut f = h.fake.data();
        f.bundle = Some(TO);
        f.daemon_version = TO;
        f.pid = None;
        f.last_exit = 1;
    }
    h.fake.sleep(Duration::from_secs(300)).await;
    let mut states = Vec::new();
    let error = h
        .app(TO)
        .reconcile(&mut |s| states.push(s))
        .await
        .unwrap_err();
    assert_eq!(h.fake.seconds(), 415);
    assert_eq!(error.detail["rollback_path"], "not_serving");
    assert!(states.contains(&UpdateState::WaitingPermission));
    let txn = h.txn().unwrap();
    assert_eq!(txn.phase, "waiting_daemon");
    assert_eq!(
        h.coordinator
            .store
            .install_details(&txn)
            .unwrap()
            .permission_wait_elapsed_ms,
        None
    );
}

#[tokio::test]
async fn expired_budget_on_first_relaunch_still_checks_the_not_serving_path() {
    let h = Harness::new();
    h.fake.data().boot = Boot::Never;
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    h.fake.sleep(START_BUDGET + POLL).await;
    let at = h.fake.seconds();
    let mut failed_at = None;
    let error = h
        .app(TO)
        .reconcile(&mut |state| {
            if matches!(state, UpdateState::DaemonFailed { .. }) {
                failed_at = Some(h.fake.seconds());
            }
        })
        .await
        .unwrap_err();
    assert_eq!(failed_at, Some(at));
    assert_eq!(h.fake.seconds(), at + NO_PID_GAP.as_secs());
    assert_eq!(error.detail["rollback_path"], "not_serving");
    assert!(h.txn().is_some());
}

#[tokio::test]
async fn startup_uses_a_fresh_budget_if_the_app_died_before_bootstrap() {
    let h = Harness::new();
    let txn = h.record(Phase::StartingDaemon, true, Some(true));
    h.coordinator
        .store
        .edit_install(&txn, None, |details| {
            details.start_requested_ms = Some(unix_millis(h.fake.now()));
        })
        .unwrap();
    {
        let mut f = h.fake.data();
        f.bundle = Some(TO);
        f.daemon_version = TO;
        f.loaded = false;
        f.pid = None;
        f.boot = Boot::Codesign(70);
    }
    h.fake.sleep(Duration::from_secs(300)).await;
    assert_eq!(h.reconcile().await, InstallOutcome::Updated);
    assert_eq!(h.fake.seconds(), 370);
    assert_eq!(h.fake.trace(), ["bootstrap"]);
}

#[tokio::test]
async fn a_busy_daemon_holding_its_lock_cannot_use_the_not_serving_path() {
    let h = Harness::new();
    h.fake.data().boot = Boot::Never;
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    h.fake.data().lock_busy = true;
    let error = h.app(TO).reconcile(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("start_timeout"));
    assert!(error.detail["rollback_path"].is_null());
    assert!(h.txn().is_some());
}

#[tokio::test]
async fn startup_waits_for_unload_without_repeating_bootout() {
    let h = Harness::new();
    let txn = h.record(Phase::Bootout, true, Some(true));
    {
        let mut f = h.fake.data();
        f.pid = None;
        f.disabled = true;
    }
    assert_eq!(h.reconcile().await, InstallOutcome::FenceUncertain);
    assert_eq!(h.txn(), Some(txn));
    assert!(h.fake.trace().is_empty());
    assert!(h.fake.data().disabled);
}

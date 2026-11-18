use super::*;
use crate::update::bundle::{VerifiedBundle, probe_bundle};
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
    external_disable_during_refusal: bool,
    disable_at: Option<u64>,
    fail_enable: bool,
    last_exit: i32,
    exit_report: Option<&'static str>,
    exit_timeout: u32,
    exit_unreadable: bool,
    started: u64,
    lease: bool,
    lease_expires: Option<u64>,
    drain_status_override: Option<Option<Value>>,
    drain_status_override_until: Option<u64>,
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
    exchange_refused: bool,
    expected_target_recovery: Option<TargetRecovery>,
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
            external_disable_during_refusal: false,
            disable_at: None,
            fail_enable: false,
            last_exit: 0,
            exit_report: None,
            exit_timeout: 3,
            exit_unreadable: false,
            started: 0,
            lease: false,
            lease_expires: None,
            drain_status_override: None,
            drain_status_override_until: None,
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
            exchange_refused: false,
            expected_target_recovery: None,
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
        if state.lease_expires.is_some_and(|at| now >= at) {
            state.lease = false;
            state.lease_expires = None;
        }
        if state
            .drain_status_override_until
            .is_some_and(|at| now >= at)
        {
            state.drain_status_override = None;
            state.drain_status_override_until = None;
        }
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

    fn assert_target_recovery(&self, state: &FakeState) {
        if let Some(reason) = state.expected_target_recovery {
            let store = TxnStore::new(self.update_lock.with_extension("json"));
            let txn = store.load().unwrap().unwrap();
            let attempt = store.rollback(&txn).unwrap().unwrap();
            assert_eq!(attempt.phase, RollbackPhase::StartingTarget);
            assert_eq!(attempt.target_recovery, Some(reason));
        }
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

    fn swap_previous(&self, bundle: &Path, staged: &Path, displaced: &str) {
        let mut state = self.data();
        assert!(state.disabled && !state.loaded);
        self.record(&mut state, "restore_zip");
        let temporary = staged.with_extension("held");
        fs::rename(bundle, &temporary).unwrap();
        fs::rename(staged, bundle).unwrap();
        fs::rename(temporary, staged).unwrap();
        fs::write(staged.join("test-version"), displaced).unwrap();
        state.bundle = Some(FROM);
        state.daemon_version = FROM;
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
        Self::at(dir, paths)
    }

    fn at(dir: tempfile::TempDir, paths: InstallPaths) -> Self {
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

#[cfg(target_os = "macos")]
pub(in crate::update) async fn upgrade_moved_installation(
    bundle: PathBuf,
    plist: PathBuf,
    app_support: PathBuf,
    peer: &Value,
) {
    assert_eq!(peer["version"], FROM);
    let original_plist = fs::read(&plist).unwrap();
    let mut h = Harness::at(
        tempfile::tempdir().unwrap(),
        InstallPaths {
            bundle,
            plist: plist.clone(),
            app_support,
            app_version: FROM.into(),
        },
    );
    h.fake.data().pid = peer["pid"].as_u64().map(|pid| pid as u32);
    let daemon = Arc::new(WireDaemon::start(h.fake.clone()));
    h.coordinator.adapters.daemon = daemon.clone();
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    let mut app = h.app(TO);
    app.adapters.daemon = daemon;
    assert_eq!(
        app.reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Updated)
    );
    assert_eq!(fs::read(plist).unwrap(), original_plist);
    assert!(h.txn().is_none());
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
            state.disabled = state.external_disable_during_refusal;
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
        self.assert_target_recovery(&state);
        if state.fail_enable {
            return Err(GuiError::launchctl_failed("enable", None, "no reply"));
        }
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
        self.assert_target_recovery(&state);
        assert!(state.loaded && state.pid.is_none());
        self.boot(&mut state, "kickstart");
        Ok(())
    }

    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        assert_eq!(plist, self.plist);
        let mut state = self.data();
        self.assert_target_recovery(&state);
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
            let mut status = json!({
                "pid": state.answering_pid.unwrap_or(pid),
                "started": strata_types::serde_rfc3339::format(&started).unwrap(),
                "version": state.answering_version.unwrap_or(state.daemon_version),
                "store_schema": 7,
                "features": if state.feature {vec!["daemon.drain"]} else {vec![]},
                "inflight": if busy {vec![json!({"phase": "confirming"})]} else {vec![]},
                "holds": if busy {vec![json!({"origin": "background"})]} else {vec![]},
                "draining": if state.lease {
                    let sealed = state.seal_at.is_some();
                    json!({"sealed": sealed, "lease_id": "lease",
                        "then": sealed.then_some("stop"),
                        "expires_at": (!sealed).then_some("2030-01-01T00:00:00Z")})
                } else {Value::Null}
            });
            match &state.drain_status_override {
                Some(Some(value)) => status["draining"] = value.clone(),
                Some(None) => {
                    status.as_object_mut().unwrap().remove("draining");
                }
                None => {}
            }
            Ok(status)
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
                DrainAction::Acquire { ttl_s, .. } => {
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
                        if reason == "reply_lost_with_lease" {
                            return Err(problem(
                                Code::Unknown,
                                "client_timeout",
                                "seal reply lost",
                            ));
                        }
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
                        if reason == "lease_lost" {
                            state.lease = false;
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
            {
                let mut state = self.data();
                self.record(&mut state, "prepare");
                if state.stop_during_prepare {
                    state.pid = None;
                    state.last_exit = 1;
                }
            }
            use super::super::source::{UpdateSource, fake::FakeUpdateSource};
            let mut update = FakeUpdateSource::default()
                .download(target, &mut |_, _| {})
                .await?;
            update.installer = Box::new(|_| panic!("test must use its fake bundle"));
            Ok(PreparedUpdate {
                update,
                info: Self::info(target),
            })
        })
    }

    fn daemon_version(&self) -> Result<String, GuiError> {
        Ok(self.data().daemon_version.into())
    }

    fn rollback_scope(&self) -> Result<(PathBuf, PathBuf), GuiError> {
        let dir = self.update_lock.parent().unwrap();
        Ok((dir.join("test-config"), dir.join("test-data")))
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

    fn restore_kept(&self, kept: &Path, _: &Path, _: &Txn) -> Result<(), GuiError> {
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
            match fs::read_to_string(bundle.join("test-version")).as_deref() {
                Ok(TO) => Some(TO),
                Ok("0.3.2") => Some("0.3.2"),
                _ => Some(FROM),
            }
        } else {
            state.bundle
        };
        version.map(Self::info).ok_or_else(|| {
            let code = if bundle.exists() {
                Code::Corrupt
            } else {
                Code::NoSuchPath
            };
            problem(
                code,
                "fake_bundle_invalid",
                "The fake bundle is missing or invalid",
            )
        })
    }

    fn zip_backup(&self, _: &Path, _: &Path) -> Result<(), GuiError> {
        panic!("preparation belongs to the host")
    }

    fn bundle_identity(
        &self,
        path: &Path,
    ) -> Result<Option<crate::update::bundle::BundleIdentity>, GuiError> {
        match crate::update::bundle::BundleIdentity::at(path) {
            Ok(identity) => Ok(Some(identity)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(GuiError::internal(e.to_string())),
        }
    }

    fn restore_zip_if(
        &self,
        zip: &Path,
        path: &Path,
        guard: &dyn Fn(&BundleInfo) -> Result<crate::update::bundle::RestoreDecision, GuiError>,
    ) -> Result<(), GuiError> {
        match guard(&Self::info(FROM))? {
            crate::update::bundle::RestoreDecision::AlreadyRestored => Ok(()),
            crate::update::bundle::RestoreDecision::Swap { .. } => self.restore_zip(zip, path),
        }
    }

    fn verify_previous_archive(&self, _: &Path, txn: &Txn) -> Result<(), GuiError> {
        assert!(txn.rollback_zip.is_file());
        Ok(())
    }

    fn exchange_previous(
        &self,
        _: &Path,
        bundle: &Path,
        guard: &dyn Fn(&VerifiedBundle) -> Result<VerifiedBundle, GuiError>,
        admit: &dyn Fn(&ExchangeEvidence) -> Result<(), GuiError>,
    ) -> Result<(), ExchangeError> {
        let dir = tempfile::Builder::new()
            .prefix(".git-strata-bundle.")
            .tempdir_in(bundle.parent().unwrap())
            .unwrap();
        let staged = dir.path().join(bundle.file_name().unwrap());
        fs::create_dir(&staged).unwrap();
        let previous = probe_bundle(self, &staged)
            .map_err(|error| ExchangeError::NotActivated(error.into_error()))?;
        let target = guard(&previous).map_err(ExchangeError::NotActivated)?;
        let evidence = ExchangeEvidence {
            staged,
            previous,
            target,
        };
        let _ = dir.keep();
        admit(&evidence).map_err(ExchangeError::Uncertain)?;
        let _ = guard(&evidence.previous).map_err(ExchangeError::NotActivated)?;
        if self.data().exchange_refused {
            return Err(ExchangeError::NotActivated(unexpected()));
        }
        self.swap_previous(bundle, &evidence.staged, TO);
        Ok(())
    }

    fn discard_exchange(&self, _: &Path, evidence: &ExchangeEvidence, _: bool) {
        fs::remove_dir_all(evidence.staged.parent().unwrap()).unwrap();
    }

    fn install(&self, update: VerifiedUpdate) -> tauri_plugin_updater::Result<()> {
        let mut state = self.data();
        assert!(state.disabled && !state.loaded && state.pid.is_none());
        assert_eq!(update.bytes, b"fake app archive");
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
async fn interrupted_recovery_keeps_fence_while_lock_held() {
    assert_unloaded_recovery_keeps_fence_while_lock_held(Phase::Interrupted).await;
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
        let result = h.coordinator.install(TO, Some(8), &mut |_| {}).await;
        if restore {
            assert_eq!(result.unwrap(), outcome);
        } else {
            assert_eq!(result.unwrap_err().reason(), Some("update_txn_unexpected"));
        }
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

#[cfg(target_os = "macos")]
struct WireDaemon {
    fake: Arc<Fake>,
    path: PathBuf,
    _directory: tempfile::TempDir,
    closed: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl WireDaemon {
    fn start(fake: Arc<Fake>) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let path = directory.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let running = closed.clone();
        let handler = fake.clone();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            for stream in listener.incoming() {
                if running.load(AtomicOrdering::SeqCst) {
                    break;
                }
                let stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut writer = &stream;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() != 0 {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    line.clear();
                    let params = &request["params"];
                    let result = match request["method"].as_str().unwrap() {
                        "daemon_status" => {
                            assert_eq!(*params, json!({"verify":false}));
                            runtime.block_on(InstallDaemon::status(handler.as_ref()))
                        }
                        "drain" => {
                            let lease_id = params["lease_id"].as_str();
                            let snapshot = runtime
                                .block_on(InstallDaemon::status(handler.as_ref()))
                                .unwrap();
                            let expected = identity(&snapshot).unwrap();
                            let action = match (
                                params["seal"].as_bool(),
                                params["on"].as_bool(),
                                lease_id,
                            ) {
                                (Some(true), None, Some(lease_id)) => {
                                    assert_eq!(
                                        *params,
                                        strata_client::drain_seal_params(lease_id, "stop")
                                    );
                                    DrainAction::SealStop { lease_id }
                                }
                                (_, Some(true), None) => {
                                    assert_eq!(*params, strata_client::drain_on_params(None));
                                    DrainAction::Acquire {
                                        ttl_s: 120,
                                        expected: &expected,
                                    }
                                }
                                (_, Some(true), Some(lease_id)) => {
                                    assert_eq!(
                                        *params,
                                        strata_client::drain_on_params(Some(lease_id))
                                    );
                                    DrainAction::Renew {
                                        lease_id,
                                        ttl_s: 120,
                                    }
                                }
                                (_, Some(false), Some(lease_id)) => {
                                    assert_eq!(
                                        *params,
                                        strata_client::drain_cancel_params(lease_id)
                                    );
                                    DrainAction::Release { lease_id }
                                }
                                _ => panic!("unexpected drain request shape"),
                            };
                            runtime
                                .block_on(handler.drain(action))
                                .map(|id| id.map_or_else(|| json!({}), |id| json!({"lease_id":id})))
                        }
                        method => panic!("unexpected method {method}"),
                    };
                    let reply = match result {
                        Ok(data) => {
                            json!({"id":request["id"], "ok":true, "data":data, "error":null})
                        }
                        Err(error) => {
                            json!({"id":request["id"], "ok":false, "data":{}, "error":error})
                        }
                    };
                    writeln!(writer, "{reply}").unwrap();
                }
            }
        });
        Self {
            fake,
            path,
            _directory: directory,
            closed,
            thread: Some(thread),
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for WireDaemon {
    fn drop(&mut self) {
        self.closed.store(true, AtomicOrdering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take()
            && !std::thread::panicking()
        {
            thread.join().unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
impl InstallDaemon for WireDaemon {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(super::super::status_at(&self.path))
    }
    fn drain<'a>(
        &'a self,
        action: DrainAction<'a>,
    ) -> UpdateFuture<'a, Result<Option<String>, GuiError>> {
        Box::pin(super::super::drain_at(&self.path, action))
    }
    fn quiescent(&self) -> Result<bool, GuiError> {
        self.fake.quiescent()
    }
    fn close_error(&self, after: SystemTime) -> Option<Value> {
        self.fake.close_error(after)
    }
    fn retained_store_schema(&self) -> Option<u32> {
        self.fake.retained_store_schema()
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn signed_fake_update_drains_over_the_socket_and_finishes_after_relaunch() {
    let mut h = Harness::new();
    let daemon = Arc::new(WireDaemon::start(h.fake.clone()));
    h.coordinator.adapters.daemon = daemon.clone();
    assert_eq!(h.install().await, InstallOutcome::Relaunch);
    assert!(h.txn().is_some());
    let mut app = h.app(TO);
    app.adapters.daemon = daemon;
    assert_eq!(
        app.reconcile(&mut |_| {}).await.unwrap().unwrap(),
        InstallOutcome::Updated
    );
    assert!(h.txn().is_none());
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
}

#[tokio::test]
async fn explicit_rollback_restores_only_with_verified_store_schema() {
    for schema in [7u32, 8] {
        let h = Harness::new();
        let txn = h.record(Phase::WaitingDaemon, false, Some(true));
        {
            let mut state = h.fake.data();
            state.bundle = Some(TO);
            state.daemon_version = TO;
            state.pid = None;
            state.last_exit = 1;
        }
        let (config, root) = h.fake.rollback_scope().unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("store_state.json"),
            json!({"root":root,"user_version":schema,"migrated_from":null,"at":"2026-01-01T00:00:00Z"}).to_string(),
        ).unwrap();
        fs::create_dir_all(txn.rollback_zip.parent().unwrap()).unwrap();
        fs::write(&txn.rollback_zip, b"verified old archive fixture").unwrap();
        let app = h.app(TO);
        let result = app.rollback_to_previous(&config, &root, &mut |_| {}).await;
        if schema == 7 {
            assert!(result.unwrap());
            assert_eq!(
                h.fake.trace(),
                ["disable", "bootout", "restore_zip", "enable", "bootstrap"]
            );
            assert_restored_handoff(&h, &txn).await;
        } else {
            assert_eq!(result.unwrap_err().reason(), Some("rollback_unavailable"));
            assert!(h.fake.trace().is_empty());
            assert_eq!(h.txn(), Some(txn));
        }
    }
}

fn prepare_explicit_rollback(h: &Harness, txn: &Txn) -> (PathBuf, PathBuf) {
    {
        let mut state = h.fake.data();
        state.bundle = Some(TO);
        state.daemon_version = TO;
    }
    let (config, root) = h.fake.rollback_scope().unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("store_state.json"),
        json!({
            "root": root, "user_version": 7, "migrated_from": null, "at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::create_dir_all(txn.rollback_zip.parent().unwrap()).unwrap();
    fs::write(&txn.rollback_zip, b"verified old archive fixture").unwrap();
    (config, root)
}

fn persist_draining_rollback(h: &Harness) -> (Txn, PathBuf, PathBuf) {
    let txn = h.record(Phase::WaitingDaemon, false, Some(true));
    let (config, root) = prepare_explicit_rollback(h, &txn);
    h.begin_rollback(&txn, target_peer(h), &config, &root);
    assert!(h.fake.trace().is_empty());
    (txn, config, root)
}

fn target_peer(h: &Harness) -> RollbackSubject {
    let state = h.fake.data();
    RollbackSubject::Serving(DaemonIdentity {
        pid: state.pid.unwrap(),
        started: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + state.started),
        version: TO.into(),
    })
}

impl Harness {
    fn begin_rollback(&self, txn: &Txn, subject: RollbackSubject, config: &Path, root: &Path) {
        let coordinator = &self.coordinator;
        coordinator
            .store
            .begin_rollback(
                txn,
                &coordinator.store.id(txn).unwrap(),
                subject,
                coordinator.rollback_scope(config, root),
            )
            .unwrap();
    }

    fn rollback(&self, txn: &Txn) -> RollbackAttempt {
        self.coordinator.store.rollback(txn).unwrap().unwrap()
    }

    fn edit_rollback(&self, txn: &Txn, edit: impl FnOnce(&mut RollbackAttempt)) {
        self.coordinator
            .store
            .advance_rollback(txn, &self.rollback(txn), edit)
            .unwrap()
            .unwrap();
    }

    fn seed_stopping(&self, txn: &Txn, uncertain: bool) {
        self.edit_rollback(txn, |attempt| {
            attempt.phase = RollbackPhase::Stopping;
            attempt.seal_accepted = !uncertain;
            attempt.stop_requested_ms = Some(unix_millis(self.fake.now()));
            if uncertain {
                attempt.admit(RollbackAction::Seal);
            }
        });
    }
}

async fn assert_restored_handoff(h: &Harness, txn: &Txn) {
    assert_eq!(h.txn().as_ref(), Some(txn));
    assert_eq!(h.rollback(txn).phase, RollbackPhase::Relaunching);
    let trace = h.fake.trace();
    let source = h.app(TO);
    assert_eq!(
        source.reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Restored)
    );
    assert!(source.rollback_relaunch_pending().unwrap());
    assert_eq!(h.txn().as_ref(), Some(txn));
    assert_eq!(
        h.app(FROM).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Restored)
    );
    assert!(h.txn().is_none());
    assert_eq!(h.fake.trace(), trace);
}

#[tokio::test]
async fn explicit_rollback_cancel_during_drain_does_not_replay_on_startup() {
    let h = Harness::new();
    let txn = h.record(Phase::WaitingDaemon, true, Some(true));
    let (config, root) = prepare_explicit_rollback(&h, &txn);
    {
        let mut fake = h.fake.data();
        fake.drain_busy_until = 90;
        fake.pid = Some(PID + 1);
        fake.started = 1;
    }
    let app = h.app(TO);
    let mut last = None;
    let mut requested = false;
    let cancelled = app
        .rollback_to_previous(&config, &root, &mut |state| {
            if !requested && matches!(state, UpdateState::Draining { .. }) && h.fake.data().lease {
                app.cancel().unwrap();
                requested = true;
            }
            last = Some(state);
        })
        .await;
    assert_eq!(h.fake.trace(), ["acquire", "release"]);
    assert!(!h.fake.data().lease);
    let pending = h.txn().unwrap();
    assert_eq!(pending, txn);
    assert!(app.store.rollback(&pending).unwrap().is_none());

    drop(app);
    h.fake.data().drain_busy_until = 0;
    let relaunched = h.app(TO).reconcile(&mut |_| {}).await;
    assert_eq!(h.fake.trace(), ["acquire", "release"]);
    assert_eq!(h.fake.data().bundle, Some(TO));
    assert_eq!(pending.phase, Phase::WaitingDaemon.name());
    assert!(!cancelled.unwrap());
    assert_eq!(last, Some(UpdateState::Cancelled));
    assert_eq!(relaunched.unwrap(), Some(InstallOutcome::Updated));
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn explicit_rollback_cancel_after_stop_restarts_target_on_startup() {
    let h = Harness::new();
    let (txn, _, _) = persist_draining_rollback(&h);
    h.seed_stopping(&txn, true);
    h.app(TO).cancel().unwrap();
    h.fake.data().pid = None;

    let mut last = None;
    let result = h.app(TO).reconcile(&mut |state| last = Some(state)).await;
    assert_eq!(h.fake.trace(), ["kickstart"]);
    assert_eq!(h.fake.data().bundle, Some(TO));
    assert_eq!(result.unwrap(), Some(InstallOutcome::Cancelled));
    assert_eq!(last, Some(UpdateState::Cancelled));
    assert_eq!(h.txn(), Some(txn.clone()));
    assert!(h.coordinator.store.rollback(&txn).unwrap().is_none());
}

#[tokio::test]
async fn explicit_rollback_cancel_during_slow_stop_restarts_target() {
    let h = Harness::new();
    let txn = h.record(Phase::WaitingDaemon, false, Some(true));
    let (config, root) = prepare_explicit_rollback(&h, &txn);
    h.fake.data().stop = Stop::Clean(130);
    let app = h.app(TO);
    let mut last = None;
    let result = app
        .rollback_to_previous(&config, &root, &mut |state| {
            if matches!(state, UpdateState::StopDetail { slow: true, .. }) {
                app.cancel().unwrap();
                assert_eq!(h.rollback(&txn).phase, RollbackPhase::CancelAfterStop);
            }
            last = Some(state);
        })
        .await;
    assert_eq!(h.fake.trace(), ["acquire", "seal", "kickstart"]);
    assert_eq!(h.fake.data().bundle, Some(TO));
    assert!(!result.unwrap());
    assert_eq!(last, Some(UpdateState::Cancelled));
    assert_eq!(h.txn(), Some(txn.clone()));
    assert!(h.coordinator.store.rollback(&txn).unwrap().is_none());
}

#[tokio::test]
async fn explicit_rollback_resumes_draining_before_first_rpc_on_startup_and_retry() {
    for startup in [true, false] {
        let h = Harness::new();
        let (txn, config, root) = persist_draining_rollback(&h);
        let relaunched = h.app(TO);
        if startup {
            assert_eq!(
                relaunched.reconcile(&mut |_| {}).await.unwrap(),
                Some(InstallOutcome::Restored)
            );
        } else {
            assert!(
                relaunched
                    .rollback_to_previous(&config, &root, &mut |_| {})
                    .await
                    .unwrap()
            );
        }
        assert_eq!(
            h.fake.trace(),
            [
                "acquire",
                "seal",
                "disable",
                "bootout",
                "restore_zip",
                "enable",
                "bootstrap"
            ]
        );
        assert_restored_handoff(&h, &txn).await;
    }
}

#[tokio::test]
async fn explicit_rollback_resume_checks_live_identity_and_lease() {
    let h = Harness::new();
    let (txn, config, root) = persist_draining_rollback(&h);
    h.fake.data().answering_pid = Some(PID + 1);
    let error = h.app(TO).reconcile(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("daemon_identity"));
    assert_eq!(h.txn().as_ref(), Some(&txn));
    assert!(h.fake.trace().is_empty());

    h.fake.data().answering_pid = None;
    h.fake.data().lease = true;
    let error = h
        .app(TO)
        .rollback_to_previous(&config, &root, &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("draining"));
    assert_eq!(h.txn().as_ref(), Some(&txn));
    assert!(h.fake.data().lease);
    assert!(h.fake.trace().is_empty());
}

#[tokio::test]
async fn explicit_rollback_restores_enabled_label_before_redrain_across_relaunch() {
    for crash_before_recheck in [true, false] {
        let h = Harness::new();
        let app = h.app(TO);
        let txn = h.record(Phase::WaitingDaemon, false, Some(true));
        let (config, root) = prepare_explicit_rollback(&h, &txn);
        {
            let mut state = h.fake.data();
            state.pid = None;
            state.pending_on_disable = true;
        }

        if crash_before_recheck {
            h.begin_rollback(&txn, RollbackSubject::NoPid, &config, &root);
            h.edit_rollback(&txn, |attempt| {
                attempt.was_enabled = true;
                attempt.phase = RollbackPhase::Disabling;
                attempt.admit(RollbackAction::Disable);
            });
            h.fake.disable(AGENT_LABEL).unwrap();
        } else {
            h.fake.data().fail_enable = true;
            let error = app
                .rollback_to_previous(&config, &root, &mut |_| {})
                .await
                .unwrap_err();
            assert_eq!(error.reason(), Some("launchctl_failed"));
            assert_eq!(error.detail["step"], "enable");
            assert_eq!(h.rollback(&txn).phase, RollbackPhase::RollbackEnabling);
            h.fake.data().fail_enable = false;
        }
        assert!(h.fake.data().disabled);
        assert_eq!(h.fake.data().pid, Some(PID + 5));
        assert_eq!(h.fake.trace(), ["disable"]);

        assert_eq!(
            h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
            Some(InstallOutcome::Restored)
        );
        assert_eq!(
            h.fake.trace(),
            [
                "disable",
                "enable",
                "acquire",
                "seal",
                "disable",
                "bootout",
                "restore_zip",
                "enable",
                "bootstrap"
            ]
        );
        assert_restored_handoff(&h, &txn).await;
    }
}

#[derive(Clone, Copy)]
enum DrainGate {
    Status,
    StatusAfterAcquire,
    StatusBeforeRenew,
    AcquireReply,
    AcquireBeforeLostReply,
    SealReply,
}

struct ParkedDaemon {
    fake: Arc<Fake>,
    point: DrainGate,
    triggered: std::sync::atomic::AtomicBool,
    parked: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

impl ParkedDaemon {
    fn attach(h: &Harness, app: &mut InstallCoordinator, point: DrainGate) -> Arc<Self> {
        let gate = Arc::new(Self {
            fake: h.fake.clone(),
            point,
            triggered: std::sync::atomic::AtomicBool::new(false),
            parked: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        app.adapters.daemon = gate.clone();
        gate
    }

    async fn park_once(&self) {
        if !self.triggered.swap(true, AtomicOrdering::SeqCst) {
            self.parked.notify_one();
            self.resume.notified().await;
        }
    }

    async fn wait_until_parked(
        &self,
        worker: std::pin::Pin<&mut impl std::future::Future<Output = Result<bool, GuiError>>>,
    ) {
        tokio::select! {
            biased;
            () = self.parked.notified() => {}
            result = worker => panic!("rollback reached {result:?} before the RPC gate"),
        }
    }
}

impl InstallDaemon for ParkedDaemon {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(async {
            let park = match self.point {
                DrainGate::Status => true,
                DrainGate::StatusAfterAcquire => self.fake.data().lease,
                DrainGate::StatusBeforeRenew => self.fake.seconds() >= 30 && self.fake.data().lease,
                _ => false,
            };
            if park {
                self.park_once().await;
            }
            self.fake.status().await
        })
    }

    fn drain<'a>(
        &'a self,
        action: DrainAction<'a>,
    ) -> UpdateFuture<'a, Result<Option<String>, GuiError>> {
        Box::pin(async move {
            let acquire = matches!(action, DrainAction::Acquire { .. });
            let seal = matches!(action, DrainAction::SealStop { .. });
            if acquire && matches!(self.point, DrainGate::AcquireBeforeLostReply) {
                self.park_once().await;
                self.fake.drain(action).await?;
                return Err(problem(
                    Code::Unknown,
                    "socket_closed",
                    "lost acquire reply",
                ));
            }
            let result = self.fake.drain(action).await;
            if acquire && matches!(self.point, DrainGate::AcquireReply)
                || seal && matches!(self.point, DrainGate::SealReply)
            {
                self.park_once().await;
            }
            result
        })
    }

    fn quiescent(&self) -> Result<bool, GuiError> {
        self.fake.quiescent()
    }

    fn close_error(&self, after: SystemTime) -> Option<Value> {
        self.fake.close_error(after)
    }

    fn retained_store_schema(&self) -> Option<u32> {
        self.fake.retained_store_schema()
    }
}

fn journal(h: &Harness) -> PathBuf {
    h.fake.update_lock.with_extension("json")
}

#[tokio::test]
async fn rollback_cancel_ack_survives_exit_and_lease_expiry() {
    let h = Harness::new();
    let txn = h.record(Phase::WaitingPermission, true, Some(true));
    let (config, root) = prepare_explicit_rollback(&h, &txn);
    h.coordinator
        .store
        .edit_install(&txn, None, |details| {
            details.start_requested_ms = Some(unix_millis(h.fake.now()) - 10_000);
            details.permission_wait_elapsed_ms = Some(10_000);
        })
        .unwrap();
    let parent: Value =
        serde_json::from_slice::<Value>(&fs::read(journal(&h)).unwrap()).unwrap()["txn"].clone();
    {
        let mut fake = h.fake.data();
        fake.pid = Some(PID + 20);
        fake.started = 1;
        fake.lease_expires = Some(120);
        fake.drain_busy_until = 90;
    }
    let mut app = h.app(TO);
    let gate = ParkedDaemon::attach(&h, &mut app, DrainGate::StatusAfterAcquire);
    let mut progress = |_| {};
    let mut worker = Box::pin(app.rollback_to_previous(&config, &root, &mut progress));
    gate.wait_until_parked(worker.as_mut()).await;
    assert_eq!(h.fake.trace(), ["acquire"]);
    assert!(h.fake.data().lease);

    app.cancel().unwrap();
    let acknowledged = fs::read(journal(&h)).unwrap();
    drop(worker);
    drop(app);
    assert!(h.rollback(&txn).withdrawn());
    fs::write(journal(&h), &acknowledged).unwrap();

    let mut observed = Vec::new();
    assert_eq!(
        h.app(TO)
            .reconcile(&mut |state| {
                let seconds = h.fake.seconds();
                if seconds < 120 {
                    assert!(matches!(state, UpdateState::Draining { .. }));
                    assert_eq!(fs::read(journal(&h)).unwrap(), acknowledged);
                    assert_eq!(h.fake.trace(), ["acquire"]);
                    observed.push(seconds);
                }
            })
            .await
            .unwrap(),
        Some(InstallOutcome::Cancelled)
    );
    assert_eq!(observed.first(), Some(&0));
    assert_eq!(observed.last(), Some(&119));
    let saved: Value = serde_json::from_slice(&fs::read(journal(&h)).unwrap()).unwrap();
    assert_eq!(saved["txn"], parent);
    assert_eq!(h.fake.data().bundle, Some(TO));
    assert_eq!(h.fake.trace(), ["acquire"]);
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Updated)
    );
    assert!(h.txn().is_none());
}

#[tokio::test]
async fn rollback_saved_stop_redrains_or_retains_target() {
    for lost_seal in [false, true] {
        let h = Harness::new();
        let (txn, config, root) = persist_draining_rollback(&h);
        {
            let mut state = h.fake.data();
            if lost_seal {
                state.seal_race = Some("reply_lost_with_lease");
                state.lease_expires = Some(15);
            } else {
                state.stop = Stop::Respawn;
            }
        }
        assert!(
            h.app(TO)
                .rollback_to_previous(&config, &root, &mut |_| {})
                .await
                .unwrap()
        );
        assert_eq!(
            h.fake.trace(),
            [
                "acquire",
                "seal",
                "acquire",
                "seal",
                "disable",
                "bootout",
                "restore_zip",
                "enable",
                "bootstrap"
            ]
        );
        if lost_seal {
            let spans: Vec<_> = h
                .fake
                .data()
                .calls
                .iter()
                .filter_map(|(op, at)| (*op == "acquire").then_some(*at))
                .collect();
            assert!(spans[1] >= 15);
        }
        assert_restored_handoff(&h, &txn).await;
    }

    for startup in [false, true] {
        for withdrawn in [false, true] {
            for replacement in [false, true] {
                let h = Harness::new();
                let (txn, config, root) = persist_draining_rollback(&h);
                h.seed_stopping(&txn, true);
                if withdrawn {
                    h.app(TO).cancel().unwrap();
                    assert_eq!(h.rollback(&txn).phase, RollbackPhase::CancelAfterStop);
                }
                let app = h.app(TO);
                assert_eq!(
                    app.recovery(&txn, &config, &root).await.unwrap()["rollback_version"],
                    FROM
                );
                {
                    let mut state = h.fake.data();
                    if replacement {
                        state.pid = Some(PID + 2);
                        state.started = 1;
                    } else {
                        state.lease = true;
                        state.lease_expires = Some(7);
                    }
                }
                let mut last = None;
                if startup {
                    let outcome = app
                        .reconcile(&mut |state| last = Some(state))
                        .await
                        .unwrap();
                    assert_eq!(
                        outcome,
                        Some(if withdrawn {
                            InstallOutcome::Cancelled
                        } else {
                            InstallOutcome::Restored
                        })
                    );
                } else {
                    assert_eq!(
                        app.rollback_to_previous(&config, &root, &mut |state| last = Some(state))
                            .await
                            .unwrap(),
                        !withdrawn
                    );
                }
                if withdrawn {
                    assert_eq!(last, Some(UpdateState::Cancelled));
                    assert!(h.fake.trace().is_empty());
                    assert_eq!(h.fake.data().bundle, Some(TO));
                    assert_eq!(h.txn().as_ref(), Some(&txn));
                    assert!(app.store.rollback(&txn).unwrap().is_none());
                } else {
                    assert_eq!(
                        h.fake.trace(),
                        [
                            "acquire",
                            "seal",
                            "disable",
                            "bootout",
                            "restore_zip",
                            "enable",
                            "bootstrap"
                        ]
                    );
                    assert_restored_handoff(&h, &txn).await;
                }
            }
        }
    }

    let h = Harness::new();
    let (txn, _, _) = persist_draining_rollback(&h);
    h.seed_stopping(&txn, true);
    h.app(TO).cancel().unwrap();
    {
        let mut state = h.fake.data();
        state.stop = Stop::Loop;
        state.seal_at = Some(0);
        state.lease = true;
    }
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::NotStarting)
    );
    assert!(h.fake.seconds() >= 10);
    assert!(h.fake.trace().is_empty());
    assert_eq!(h.txn().as_ref(), Some(&txn));

    let h = Harness::new();
    let (txn, config, root) = persist_draining_rollback(&h);
    h.fake.data().seal_race = Some("clean_reply_lost");
    assert!(
        h.app(TO)
            .rollback_to_previous(&config, &root, &mut |_| {})
            .await
            .unwrap()
    );
    assert_eq!(
        h.fake.trace(),
        [
            "acquire",
            "seal",
            "disable",
            "bootout",
            "restore_zip",
            "enable",
            "bootstrap"
        ]
    );
    assert_restored_handoff(&h, &txn).await;
}

#[tokio::test]
async fn rollback_late_rpc_reply_preserves_withdrawal() {
    for (point, refusal, expected) in [
        (DrainGate::Status, None, &[][..]),
        (
            DrainGate::StatusBeforeRenew,
            None,
            &["acquire", "release"][..],
        ),
        (DrainGate::AcquireReply, None, &["acquire", "release"][..]),
        (
            DrainGate::SealReply,
            Some("not_idle"),
            &["acquire", "seal", "release"][..],
        ),
        (
            DrainGate::SealReply,
            Some("lease_lost"),
            &["acquire", "seal"][..],
        ),
    ] {
        let h = Harness::new();
        let (txn, config, root) = persist_draining_rollback(&h);
        {
            let mut state = h.fake.data();
            state.seal_race = refusal;
            if matches!(point, DrainGate::StatusBeforeRenew) {
                state.drain_busy_until = 90;
            }
        }
        let mut app = h.app(TO);
        let gate = ParkedDaemon::attach(&h, &mut app, point);
        let mut observed_cancel_after_stop = false;
        let mut progress = |state| {
            if state == UpdateState::Stopping && matches!(point, DrainGate::SealReply) {
                observed_cancel_after_stop = true;
                let snapshot = h.rollback(&txn);
                assert!(snapshot.withdrawn());
                assert_eq!(snapshot.phase, RollbackPhase::CancelAfterStop);
            }
        };
        let mut worker = Box::pin(app.rollback_to_previous(&config, &root, &mut progress));
        gate.wait_until_parked(worker.as_mut()).await;
        app.cancel().unwrap();
        assert!(h.rollback(&txn).withdrawn());
        gate.resume.notify_one();
        assert!(!worker.await.unwrap());
        assert_eq!(
            observed_cancel_after_stop,
            matches!(point, DrainGate::SealReply)
        );
        assert_eq!(h.fake.trace(), expected);
        assert_eq!(h.txn().as_ref(), Some(&txn));
        assert!(app.store.rollback(&txn).unwrap().is_none());
    }

    let h = Harness::new();
    let (txn, config, root) = persist_draining_rollback(&h);
    let mut app = h.app(TO);
    let gate = ParkedDaemon::attach(&h, &mut app, DrainGate::AcquireBeforeLostReply);
    let mut progress = |_| {};
    let mut worker = Box::pin(app.rollback_to_previous(&config, &root, &mut progress));
    gate.wait_until_parked(worker.as_mut()).await;
    app.cancel().unwrap();
    assert!(!h.fake.data().lease);
    let check_pending = async |expected| {
        let mut last = None;
        let error = h
            .app(TO)
            .reconcile(&mut |state| last = Some(state))
            .await
            .unwrap_err();
        assert_eq!(error.reason(), Some("drain_release_unconfirmed"));
        assert!(matches!(last, Some(UpdateState::Draining { .. })));
        assert_eq!(h.rollback(&txn).action(), Some(RollbackAction::Acquire));
        assert!(h.rollback(&txn).withdrawn());
        assert_eq!(h.fake.trace(), expected);
    };
    check_pending(&[][..]).await;
    h.fake.data().lease_expires = Some(120);
    gate.resume.notify_one();
    assert_eq!(
        worker.await.unwrap_err().reason(),
        Some("drain_release_unconfirmed")
    );
    check_pending(&["acquire"][..]).await;
    h.fake.elapsed.store(240, AtomicOrdering::SeqCst);
    assert!(!h.fake.data().lease);
    check_pending(&["acquire"][..]).await;

    {
        let mut state = h.fake.data();
        state.pid = Some(PID + 22);
        state.started = 241;
    }
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Cancelled)
    );
    assert_eq!(h.fake.trace(), ["acquire"]);

    let h = Harness::new();
    let (txn, _, _) = persist_draining_rollback(&h);
    let app = h.app(TO);
    let before = fs::read(journal(&h)).unwrap();
    h.fake.block_journal_writes();
    assert_eq!(app.cancel().unwrap_err().reason(), Some("update_txn_io"));
    assert_eq!(fs::read(journal(&h)).unwrap(), before);
    fs::remove_dir(&h.fake.update_lock).unwrap();
    app.store.fail_next_cancel_sync();
    assert_eq!(app.cancel().unwrap_err().reason(), Some("update_txn_io"));
    assert!(h.rollback(&txn).withdrawn());
    assert!(h.fake.trace().is_empty());

    let h = Harness::new();
    let (txn, _, _) = persist_draining_rollback(&h);
    h.edit_rollback(&txn, |attempt| {
        attempt.was_enabled = true;
        attempt.phase = RollbackPhase::Bootout;
        attempt.bootout_admitted = true;
        attempt.admit(RollbackAction::Bootout);
    });
    let before = fs::read(journal(&h)).unwrap();
    assert_eq!(
        h.app(TO).cancel().unwrap_err().reason(),
        Some("update_not_cancellable")
    );
    assert_eq!(fs::read(journal(&h)).unwrap(), before);
    assert!(h.fake.trace().is_empty());
}

fn seed_exchange(h: &Harness, txn: &Txn, displaced: &str) -> ExchangeEvidence {
    let installed = &h.fake.bundle;
    let staged = installed
        .parent()
        .unwrap()
        .join(".git-strata-bundle.saved")
        .join(installed.file_name().unwrap());
    fs::create_dir_all(&staged).unwrap();
    let previous = probe_bundle(h.fake.as_ref(), &staged)
        .map_err(super::super::bundle::ProbeError::into_error)
        .unwrap();
    let target = probe_bundle(h.fake.as_ref(), installed)
        .map_err(super::super::bundle::ProbeError::into_error)
        .unwrap();
    let evidence = ExchangeEvidence {
        staged,
        previous,
        target,
    };
    h.edit_rollback(txn, |attempt| {
        attempt.was_enabled = true;
        attempt.owns_fence = true;
        attempt.bootout_admitted = true;
        attempt.unloaded = true;
        attempt.phase = RollbackPhase::Restoring;
        attempt.exchange = Some(evidence.clone());
        attempt.admit(RollbackAction::Exchange);
    });
    h.fake.swap_previous(installed, &evidence.staged, displaced);
    h.fake.data().calls.clear();
    evidence
}

#[tokio::test]
async fn rollback_fence_and_exchange_relaunch_respect_saved_results() {
    for loaded in [true, false] {
        let h = Harness::new();
        let (txn, config, root) = persist_draining_rollback(&h);
        h.edit_rollback(&txn, |attempt| attempt.phase = RollbackPhase::Fencing);
        {
            let mut state = h.fake.data();
            state.pid = None;
            state.disabled = true;
            state.loaded = loaded;
        }
        let before = fs::read(journal(&h)).unwrap();
        let error = h
            .app(TO)
            .rollback_to_previous(&config, &root, &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.reason(), Some("agent_disabled"));
        assert!(h.fake.trace().is_empty());
        assert_eq!(fs::read(journal(&h)).unwrap(), before);
    }

    let h = Harness::new();
    let (txn, config, root) = persist_draining_rollback(&h);
    {
        let mut state = h.fake.data();
        state.pid = None;
        state.disable = Disable::PreflightFails;
        state.external_disable_during_refusal = true;
    }
    assert_eq!(
        h.app(TO)
            .rollback_to_previous(&config, &root, &mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("agent_disabled")
    );
    assert_eq!(
        h.rollback(&txn).target_recovery,
        Some(TargetRecovery::DisableRefused)
    );
    assert!(!h.rollback(&txn).owns_fence);
    assert_eq!(h.fake.trace(), ["disable_preflight"]);

    let h = Harness::new();
    let (txn, _, _) = persist_draining_rollback(&h);
    h.edit_rollback(&txn, |attempt| {
        attempt.phase = RollbackPhase::Disabling;
        attempt.was_enabled = true;
        attempt.admit(RollbackAction::Disable);
    });
    h.fake.data().pid = None;
    let before = fs::read(journal(&h)).unwrap();
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap_err().reason(),
        Some("update_not_quiescent")
    );
    assert_eq!(fs::read(journal(&h)).unwrap(), before);
    assert!(h.fake.trace().is_empty());
    h.fake.data().disabled = true;
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Restored)
    );
    assert_eq!(
        h.fake.trace(),
        ["bootout", "restore_zip", "enable", "bootstrap"]
    );
    assert_restored_handoff(&h, &txn).await;

    for (disable, bootout, exchange_refused, continuation, trace) in [
        (
            Disable::PreflightFails,
            Bootout::Unload,
            false,
            TargetRecovery::DisableRefused,
            &["disable_preflight", "kickstart"][..],
        ),
        (
            Disable::Ready,
            Bootout::Failed,
            false,
            TargetRecovery::BootoutFailed,
            &["disable", "bootout", "enable", "kickstart"][..],
        ),
        (
            Disable::Ready,
            Bootout::PreflightFails,
            false,
            TargetRecovery::BootoutFailed,
            &["disable", "bootout_preflight", "enable", "kickstart"][..],
        ),
        (
            Disable::Ready,
            Bootout::Unload,
            true,
            TargetRecovery::ExchangeRefused,
            &["disable", "bootout", "enable", "bootstrap"][..],
        ),
    ] {
        let h = Harness::new();
        let (txn, config, root) = persist_draining_rollback(&h);
        {
            let mut state = h.fake.data();
            state.pid = None;
            state.disable = disable;
            state.bootout = bootout;
            state.exchange_refused = exchange_refused;
            state.expected_target_recovery = Some(continuation);
        }
        assert!(
            !h.app(TO)
                .rollback_to_previous(&config, &root, &mut |_| {})
                .await
                .unwrap()
        );
        assert_eq!(h.fake.trace(), trace);
        assert_eq!(h.fake.data().bundle, Some(TO));
        assert!(!h.fake.data().disabled);
        assert_eq!(h.txn().as_ref(), Some(&txn));
        assert!(h.coordinator.store.rollback(&txn).unwrap().is_none());
    }

    let h = Harness::new();
    let (txn, _, _) = persist_draining_rollback(&h);
    h.edit_rollback(&txn, |attempt| {
        attempt.phase = RollbackPhase::Bootout;
        attempt.was_enabled = true;
        attempt.owns_fence = true;
        attempt.bootout_admitted = true;
        attempt.admit(RollbackAction::Bootout);
    });
    {
        let mut state = h.fake.data();
        state.pid = None;
        state.disabled = true;
    }
    let before = fs::read(journal(&h)).unwrap();
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::FenceUncertain)
    );
    assert_eq!(fs::read(journal(&h)).unwrap(), before);
    assert!(h.fake.trace().is_empty());
    h.fake.data().loaded = false;
    assert_eq!(
        h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
        Some(InstallOutcome::Restored)
    );
    assert_eq!(h.fake.trace(), ["restore_zip", "enable", "bootstrap"]);
    assert_restored_handoff(&h, &txn).await;

    for displaced in ["0.3.2", TO] {
        let h = Harness::new();
        let (txn, _, _) = persist_draining_rollback(&h);
        {
            let mut state = h.fake.data();
            state.pid = None;
            state.disabled = true;
            state.loaded = false;
        }
        let evidence = seed_exchange(&h, &txn, displaced);
        let before = fs::read(journal(&h)).unwrap();
        if displaced == TO {
            // Startup in the predecessor consumes a verified exchange without a new swap.
            assert_eq!(
                h.app(FROM).reconcile(&mut |_| {}).await.unwrap(),
                Some(InstallOutcome::Restored)
            );
            assert_eq!(h.fake.trace(), ["enable", "bootstrap"]);
            assert!(!evidence.staged.exists());
            assert!(h.txn().is_none());
        } else {
            assert_eq!(
                h.app(FROM)
                    .reconcile(&mut |_| {})
                    .await
                    .unwrap_err()
                    .reason(),
                Some("restore_destination_changed")
            );
            assert!(h.fake.data().disabled);
            assert!(evidence.staged.exists());
            assert!(h.fake.trace().is_empty());
            assert_eq!(fs::read(journal(&h)).unwrap(), before);
        }
    }

    for malformed in [None, Some(json!({"sealed": false}))] {
        for stopping in [true, false] {
            let h = Harness::new();
            let (txn, _, _) = persist_draining_rollback(&h);
            if stopping {
                h.seed_stopping(&txn, true);
            } else {
                h.edit_rollback(&txn, |attempt| {
                    retain_target(attempt, TargetRecovery::Cancelled)
                });
            }
            h.app(TO).cancel().unwrap();
            {
                let mut state = h.fake.data();
                state.drain_status_override = Some(malformed.clone());
                state.drain_status_override_until = Some(6);
            }
            assert_eq!(
                h.app(TO).reconcile(&mut |_| {}).await.unwrap(),
                Some(InstallOutcome::Cancelled)
            );
            assert!(h.fake.seconds() >= 6);
            assert!(h.fake.trace().is_empty());
        }
    }
}

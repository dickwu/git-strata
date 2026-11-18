use std::sync::{Mutex, atomic::AtomicBool};
use std::time::{Instant, SystemTime};

use super::*;
use crate::update::{bundle::BundleInfo, source::VerifiedUpdate};

const FORMULA: &str = "/opt/homebrew/opt/git-strata/bin/git-stratad";
const VERSION: &str = "0.3.0";
const APP_SCHEMA: u32 = 7;
const TIMEOUT: u64 = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Loaded {
    Legacy,
    Bundle,
    None,
}

#[derive(Clone, Copy, Default)]
enum BootoutReply {
    #[default]
    Done,
    Rejected,
    Timeout,
    LoadedAfterSuccess,
    PrintFailed,
}

#[derive(Clone, Copy, Default)]
enum StartReply {
    #[default]
    Working,
    Unloaded,
    NoPid,
    WrongVersion,
    NoAnswer,
    WrongSocketPid,
}

struct Sim {
    loaded: Loaded,
    disabled: bool,
    pid: Option<u32>,
    started: u64,
    lease: Option<String>,
    expires: Option<u64>,
    sealed: bool,
    next_lease: u32,
    busy_until: u64,
    seal_pause: u64,
    missing: bool,
    stopped: bool,
    feature: bool,
    legacy_status: Option<Value>,
    bundle_version: &'static str,
    bundle_version_on_fence: Option<&'static str>,
    legacy_version: &'static str,
    schema: Option<u32>,
    live_schema: Option<u32>,
    primary_bootout: BootoutReply,
    rollback_bootout: BootoutReply,
    new_start: StartReply,
    init_socket_pid: Option<u32>,
    respawn_on_fence: u32,
    disable_times_out: bool,
    bundle_pid_quits_at: Option<u64>,
    tcc_for: u64,
    print_failed: bool,
    bundle_print_failures: u32,
    enable_count: u32,
    events: Vec<String>,
}

impl Default for Sim {
    fn default() -> Self {
        Self {
            loaded: Loaded::Legacy,
            disabled: false,
            pid: Some(10),
            started: 1,
            lease: None,
            expires: None,
            sealed: false,
            next_lease: 1,
            busy_until: 0,
            seal_pause: 0,
            missing: false,
            stopped: true,
            feature: true,
            legacy_status: None,
            bundle_version: VERSION,
            bundle_version_on_fence: None,
            legacy_version: VERSION,
            schema: Some(APP_SCHEMA),
            live_schema: Some(APP_SCHEMA),
            primary_bootout: BootoutReply::Done,
            rollback_bootout: BootoutReply::Done,
            new_start: StartReply::Working,
            init_socket_pid: None,
            respawn_on_fence: 0,
            disable_times_out: false,
            bundle_pid_quits_at: None,
            tcc_for: 0,
            print_failed: false,
            bundle_print_failures: 0,
            enable_count: 0,
            events: Vec::new(),
        }
    }
}

struct Fake {
    sim: Mutex<Sim>,
    time: AtomicU64,
    origin: Instant,
    home: PathBuf,
    program: PathBuf,
}

impl Fake {
    fn plist(&self) -> PathBuf {
        plist_for_home(&self.home)
    }

    fn saved(&self) -> Option<MoveRecord> {
        let bytes = fs::read(self.home.join(APP_SUPPORT).join("update.json")).unwrap();
        let doc: Value = serde_json::from_slice(&bytes).unwrap();
        serde_json::from_value(doc["move"].clone()).ok()
    }

    fn tick(&self, sim: &mut Sim) {
        let now = self.time.load(Ordering::Relaxed);
        if !sim.sealed && sim.expires.is_some_and(|until| now >= until) {
            sim.lease = None;
            sim.expires = None;
        }
        if sim.loaded == Loaded::Bundle && sim.bundle_pid_quits_at.is_some_and(|at| now >= at) {
            sim.loaded = Loaded::None;
            sim.pid = None;
            sim.sealed = false;
            sim.lease = None;
        }
    }

    fn plist_program(&self) -> PathBuf {
        let bytes = fs::read(self.plist()).unwrap();
        let doc: Plist = serde_json::from_slice(&bytes).unwrap();
        let PlistValue::Array(args) = &doc["ProgramArguments"] else {
            panic!("no ProgramArguments")
        };
        let PlistValue::String(program) = &args[0] else {
            panic!("no program")
        };
        PathBuf::from(program)
    }

    fn record_lifecycle(&self, loaded: Loaded, event: &str) -> String {
        let name = match loaded {
            Loaded::Legacy => "old",
            Loaded::Bundle => "bundle",
            Loaded::None => "absent",
        };
        format!("{event}:{name}")
    }
}

impl Clock for Fake {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(self.time.load(Ordering::Relaxed))
    }

    fn monotonic(&self) -> Instant {
        self.origin + Duration::from_secs(self.time.load(Ordering::Relaxed))
    }

    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
        Box::pin(async move {
            self.time.fetch_add(duration.as_secs(), Ordering::Relaxed);
        })
    }
}

impl ServiceManager for Fake {
    fn print(&self, label: &str) -> Result<Option<String>, GuiError> {
        assert_eq!(label, AGENT_LABEL);
        let mut sim = self.sim.lock().unwrap();
        self.tick(&mut sim);
        let event = self.record_lifecycle(sim.loaded, "print");
        sim.events.push(event);
        if sim.print_failed {
            return Err(GuiError::launchctl_failed("print", Some(1), "probe failed"));
        }
        if sim.loaded == Loaded::Bundle && sim.bundle_print_failures > 0 {
            sim.bundle_print_failures -= 1;
            return Err(GuiError::launchctl_failed("print", Some(1), "probe failed"));
        }
        let program = match sim.loaded {
            Loaded::Legacy => Path::new(FORMULA),
            Loaded::Bundle => &self.program,
            Loaded::None => return Ok(None),
        };
        let pid = sim
            .pid
            .map_or(String::new(), |pid| format!("  pid = {pid}\n"));
        Ok(Some(format!(
            "gui/501/{AGENT_LABEL} = {{\n  path = {}\n  program = {}\n  exit timeout = {TIMEOUT} seconds\n{pid}}}",
            self.plist().display(),
            program.display(),
        )))
    }

    fn print_disabled(&self, label: &str) -> Result<bool, GuiError> {
        assert_eq!(label, AGENT_LABEL);
        Ok(self.sim.lock().unwrap().disabled)
    }

    fn kickstart(&self, _: &str) -> Result<(), GuiError> {
        panic!("Move uses bootstrap")
    }

    fn disable(&self, label: &str) -> Result<(), GuiError> {
        assert_eq!(label, AGENT_LABEL);
        let mut sim = self.sim.lock().unwrap();
        let event = self.record_lifecycle(sim.loaded, "disable");
        sim.events.push(event);
        assert_eq!(
            self.saved().unwrap().service_attempt,
            Some(ServiceAttempt::Disable)
        );
        if sim.disable_times_out {
            return Err(GuiError::launchctl_failed(
                "disable",
                None,
                "child still running",
            ));
        }
        sim.disabled = true;
        if let Some(version) = sim.bundle_version_on_fence.take() {
            sim.bundle_version = version;
        }
        if sim.seal_pause != 0 && sim.sealed {
            self.time.fetch_add(sim.seal_pause, Ordering::Relaxed);
            self.tick(&mut sim);
            assert!(sim.sealed && sim.lease.is_some(), "hold expired");
        }
        if sim.respawn_on_fence != 0 && sim.loaded != Loaded::None {
            sim.respawn_on_fence -= 1;
            sim.pid = Some(sim.pid.unwrap_or(10) + 1);
            sim.started += 1;
            sim.lease = None;
            sim.expires = None;
            sim.sealed = false;
        }
        Ok(())
    }

    fn bootout(&self, label: &str) -> Result<(), GuiError> {
        assert_eq!(label, AGENT_LABEL);
        let mut sim = self.sim.lock().unwrap();
        self.tick(&mut sim);
        assert!(sim.disabled);
        let record = self.saved().unwrap();
        let expected = if sim.loaded == Loaded::Legacy {
            Phase::Bootout
        } else {
            Phase::RollbackHold
        };
        assert_eq!(record.phase, expected);
        assert_eq!(record.bootout.as_ref().unwrap().pid, sim.pid);
        assert!(
            sim.pid.is_none() || sim.sealed,
            "bootout of an unsealed process"
        );
        let event = self.record_lifecycle(sim.loaded, "bootout");
        sim.events.push(event);
        let reply = if sim.loaded == Loaded::Legacy {
            sim.primary_bootout
        } else {
            sim.rollback_bootout
        };
        match reply {
            BootoutReply::Done => {
                sim.loaded = Loaded::None;
                sim.pid = None;
                sim.lease = None;
                sim.sealed = false;
                Ok(())
            }
            BootoutReply::Rejected => Err(GuiError::launchctl_failed(
                "bootout",
                Some(5),
                "child exited",
            )),
            BootoutReply::Timeout => Err(GuiError::launchctl_failed(
                "bootout",
                None,
                "child still running",
            )),
            BootoutReply::LoadedAfterSuccess => Ok(()),
            BootoutReply::PrintFailed => {
                sim.print_failed = true;
                Err(GuiError::launchctl_failed(
                    "bootout",
                    Some(5),
                    "child exited",
                ))
            }
        }
    }

    fn enable(&self, label: &str) -> Result<(), GuiError> {
        assert_eq!(label, AGENT_LABEL);
        let program = self.plist_program();
        let mut sim = self.sim.lock().unwrap();
        sim.enable_count += 1;
        sim.events.push(
            if program == self.program {
                "enable:bundle"
            } else {
                "enable:old"
            }
            .into(),
        );
        sim.disabled = false;
        Ok(())
    }

    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        assert_eq!(plist, self.plist());
        let record = self.saved().unwrap();
        assert!(matches!(record.phase, Phase::Start | Phase::StartLegacy));
        assert_eq!(record.service_attempt, Some(ServiceAttempt::Bootstrap));
        let program = self.plist_program();
        let mut sim = self.sim.lock().unwrap();
        assert!(!sim.disabled);
        assert!(sim.loaded == Loaded::None);
        sim.started = self.time.load(Ordering::Relaxed) + 10;
        if program == self.program {
            sim.events.push("bootstrap:bundle".into());
            sim.loaded = Loaded::Bundle;
            sim.pid = Some(20);
            match sim.new_start {
                StartReply::Unloaded => {
                    sim.loaded = Loaded::None;
                    sim.pid = None;
                }
                StartReply::NoPid => sim.pid = None,
                _ => {}
            }
            if matches!(
                sim.new_start,
                StartReply::Unloaded | StartReply::NoPid | StartReply::NoAnswer
            ) {
                return Err(GuiError::launchctl_failed(
                    "bootstrap",
                    Some(5),
                    "startup failed",
                ));
            }
        } else {
            sim.events.push("bootstrap:old".into());
            assert_eq!(program, Path::new(FORMULA));
            assert!(!sim.missing, "bootstrapping a missing program");
            sim.loaded = Loaded::Legacy;
            sim.pid = Some(30);
        }
        Ok(())
    }
}

impl MoveDrain for Fake {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(async move {
            let mut sim = self.sim.lock().unwrap();
            self.tick(&mut sim);
            let event = self.record_lifecycle(sim.loaded, "status");
            sim.events.push(event);
            if sim.loaded == Loaded::Legacy
                && let Some(status) = &sim.legacy_status
            {
                return Ok(status.clone());
            }
            let now = self.time.load(Ordering::Relaxed);
            if sim.loaded == Loaded::Bundle && now < sim.tcc_for {
                return Err(GuiError::new(
                    Code::Busy,
                    "waiting for access",
                    json!({
                        "reason": "store_unavailable", "last_error": { "detail": { "errno": 13 } },
                    }),
                ));
            }
            let pid = match (sim.loaded, sim.new_start) {
                (Loaded::Legacy, _) => sim.init_socket_pid.or(sim.pid),
                (Loaded::Bundle, StartReply::NoAnswer) => None,
                (Loaded::Bundle, StartReply::WrongSocketPid) => Some(99),
                _ => sim.pid,
            }
            .ok_or_else(|| move_error(Code::Busy, "daemon_down", "connection refused"))?;
            let version = match (sim.loaded, sim.new_start) {
                (Loaded::Legacy, _) => sim.legacy_version,
                (Loaded::Bundle, StartReply::WrongVersion) => "9.0.0",
                _ => VERSION,
            };
            let held = sim.lease.as_ref().map(|id| {
                json!({
                    "lease_id": id, "sealed": sim.sealed, "then": sim.sealed.then_some("hold"),
                    "expires_at": sim.expires,
                })
            });
            let busy = now < sim.busy_until;
            Ok(json!({
                "pid": pid, "version": version,
                "started": strata_types::serde_rfc3339::format(&(SystemTime::UNIX_EPOCH + Duration::from_secs(sim.started))).unwrap(),
                "features": if sim.feature { vec!["daemon.drain"] } else { Vec::new() },
                "store_schema": sim.live_schema,
                "draining": held,
                "inflight": if busy { vec![json!({"phase":"confirming"})] } else { vec![] },
                "holds": if busy { vec![json!({"origin":"background"})] } else { vec![] },
            }))
        })
    }

    fn drain(&self, params: Value) -> UpdateFuture<'_, Result<Value, GuiError>> {
        Box::pin(async move {
            let mut sim = self.sim.lock().unwrap();
            self.tick(&mut sim);
            let from_bundle = sim.loaded == Loaded::Bundle;
            let now = self.time.load(Ordering::Relaxed);
            assert!(sim.pid.is_some());
            if from_bundle && params["on"] != false {
                assert_eq!(self.saved().unwrap().phase, Phase::RollbackHold);
            }
            if params["seal"] == true {
                let record = self.saved().unwrap();
                assert_eq!(
                    record.phase,
                    if from_bundle {
                        Phase::RollbackHold
                    } else {
                        Phase::SealedHold
                    }
                );
                let hold = record.hold.unwrap();
                assert_eq!(Some(hold.daemon.pid), sim.pid);
                assert_eq!(params["lease_id"], hold.lease_id);
                assert_eq!(params["then"], "hold");
                assert_eq!(params["lease_id"].as_str(), sim.lease.as_deref());
                assert!(now >= sim.busy_until);
                sim.sealed = true;
                sim.expires = None;
                let event = self.record_lifecycle(sim.loaded, "seal");
                sim.events.push(event);
            } else if params["on"] == true {
                assert_eq!(params["ttl_s"], TTL_S);
                if let Some(id) = params["lease_id"].as_str() {
                    assert_eq!(Some(id), sim.lease.as_deref());
                    sim.events.push("renew".into());
                } else {
                    assert!(sim.lease.is_none());
                    sim.lease = Some(format!("lease-{}", sim.next_lease));
                    sim.next_lease += 1;
                    let event = self.record_lifecycle(sim.loaded, "drain");
                    sim.events.push(event);
                }
                sim.expires = Some(now + u64::from(TTL_S));
            } else {
                assert_eq!(params["on"], false);
                assert_eq!(params["lease_id"].as_str(), sim.lease.as_deref());
                sim.events.push("unseal".into());
                sim.lease = None;
                sim.sealed = false;
            }
            Ok(json!({ "lease_id": sim.lease }))
        })
    }
}

impl MovePlatform for Fake {
    fn bundle_version(&self) -> Result<String, GuiError> {
        Ok(self.sim.lock().unwrap().bundle_version.into())
    }

    fn render_plist(&self, program: &Path) -> Result<Vec<u8>, GuiError> {
        self.sim
            .lock()
            .unwrap()
            .events
            .push(format!("render:{}", program.display()));
        self.encode_plist(&template(program))
    }

    fn lint_plist(&self, _: &Path) -> Result<(), GuiError> {
        Ok(())
    }

    fn decode_plist(&self, bytes: &[u8]) -> Result<Plist, GuiError> {
        serde_json::from_slice(bytes).map_err(|e| GuiError::internal(e.to_string()))
    }

    fn encode_plist(&self, plist: &Plist) -> Result<Vec<u8>, GuiError> {
        Ok(serde_json::to_vec(plist).unwrap())
    }

    fn store_schema(&self) -> Option<u32> {
        self.sim.lock().unwrap().schema
    }

    fn socket_refusing_and_lock_free(&self) -> Result<bool, GuiError> {
        let mut sim = self.sim.lock().unwrap();
        self.tick(&mut sim);
        let event = self.record_lifecycle(sim.loaded, "stopped");
        sim.events.push(event);
        Ok(sim.stopped && sim.pid.is_none())
    }

    fn program_missing(&self, program: &Path) -> Result<bool, GuiError> {
        Ok(program == Path::new(FORMULA) && self.sim.lock().unwrap().missing)
    }
}

impl BundleOps for Fake {
    fn read_info_plist(&self, _: &Path) -> Result<BundleInfo, GuiError> {
        Ok(BundleInfo {
            version: VERSION.into(),
            store_schema: APP_SCHEMA,
            downgrade_floor: VERSION.into(),
        })
    }

    fn codesign_verify(&self, _: &Path) -> Result<(), GuiError> {
        Ok(())
    }

    fn extract(&self, _: &[u8], _: &Path) -> Result<PathBuf, GuiError> {
        panic!("Move does not extract a bundle")
    }

    fn zip_backup(&self, _: &Path, _: &Path) -> Result<(), GuiError> {
        panic!("Move backs up the plist only")
    }

    fn install(&self, _: VerifiedUpdate) -> tauri_plugin_updater::Result<()> {
        panic!("Move uses the installed bundle")
    }

    fn restore_zip(&self, _: &Path, _: &Path) -> Result<(), GuiError> {
        panic!("Move restores the plist only")
    }
}

fn template(program: &Path) -> Plist {
    let text = |s: &str| PlistValue::String(s.into());
    Plist::from([
        ("Label".into(), text(AGENT_LABEL)),
        (
            "ProgramArguments".into(),
            PlistValue::Array(vec![text(program.to_str().unwrap())]),
        ),
        (
            "KeepAlive".into(),
            PlistValue::Dictionary(Plist::from([(
                "SuccessfulExit".into(),
                PlistValue::Bool(false),
            )])),
        ),
        ("RunAtLoad".into(), PlistValue::Bool(true)),
        ("ExitTimeOut".into(), PlistValue::Other(vec![0x90, 0x00])),
        ("StandardErrorPath".into(), text("/tmp/daemon.err.log")),
        (
            "EnvironmentVariables".into(),
            PlistValue::Dictionary(Plist::from([("PATH".into(), text("/usr/bin:/bin"))])),
        ),
    ])
}

struct Test {
    _temp: tempfile::TempDir,
    fake: Arc<Fake>,
    mover: MoveDaemon,
    legacy: Vec<u8>,
}

impl Test {
    fn new(sim: Sim) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let bundle = home.join("GIT STRATA.app");
        let fake = Arc::new(Fake {
            sim: Mutex::new(sim),
            time: AtomicU64::new(2),
            origin: Instant::now(),
            home: home.clone(),
            program: bundle.join("Contents/MacOS/git-stratad"),
        });
        let mover = MoveDaemon::scoped(
            home,
            bundle,
            VERSION.into(),
            MoveAdapters {
                service: fake.clone(),
                daemon: fake.clone(),
                platform: fake.clone(),
                bundle: fake.clone(),
                clock: fake.clone(),
            },
        );
        let mut old = template(Path::new(FORMULA));
        old.insert("KeepAlive".into(), PlistValue::Bool(true));
        let legacy = fake.encode_plist(&old).unwrap();
        atomic_write(&mover.plist(), &legacy).unwrap();
        atomic_write(
            &mover.journal(),
            br#"{"future_check":{ "enabled": true },"txn":null}"#,
        )
        .unwrap();
        Self {
            _temp: temp,
            fake,
            mover,
            legacy,
        }
    }

    async fn run(&self) -> Result<(), GuiError> {
        self.mover.run(None, None, &mut |_| {}).await
    }

    fn recorded(&self, phase: Phase) -> MoveRecord {
        let record = self.fake.saved().unwrap();
        assert_eq!(record.phase, phase);
        record
    }

    fn events(&self) -> Vec<String> {
        self.fake.sim.lock().unwrap().events.clone()
    }

    fn actions(&self) -> Vec<String> {
        let actions = [
            "disable:",
            "bootout:",
            "enable:",
            "bootstrap:",
            "seal:",
            "drain:",
            "unseal",
        ];
        self.events()
            .into_iter()
            .filter(|event| actions.iter().any(|action| event.starts_with(action)))
            .collect()
    }

    async fn interrupted(&self, phase: Phase, attempt: Option<BootoutAttempt>) {
        let Prepared {
            mut record,
            rendered,
            ..
        } = self.mover.prepare(None).await.unwrap();
        atomic_write(&self.mover.backup(), &self.legacy).unwrap();
        atomic_write(&self.mover.staged(), &rendered).unwrap();
        atomic_write(&self.mover.replacement(), &rendered).unwrap();
        record.phase = phase;
        record.bootout = attempt;
        self.mover.save(Some(&record)).unwrap();
        self.fake.sim.lock().unwrap().events.clear();
    }
}

#[tokio::test]
async fn moves_formula_in_order_with_renewal_and_a_hold_longer_than_any_ttl() {
    let test = Test::new(Sim {
        busy_until: 35,
        seal_pause: 1_800,
        ..Sim::default()
    });
    let mut progress = Vec::new();
    test.mover
        .run(None, None, &mut |p| progress.push(p))
        .await
        .unwrap();
    assert_eq!(
        test.actions(),
        [
            "drain:old",
            "seal:old",
            "disable:old",
            "bootout:old",
            "enable:bundle",
            "bootstrap:bundle",
        ]
    );
    assert!(test.events().contains(&"renew".into()));
    assert!(progress.iter().any(|p| matches!(p, UpdateState::Draining { inflight, background } if inflight.len() == 1 && background.len() == 1)));
    assert_eq!(progress.last(), Some(&UpdateState::Done));
    assert_eq!(test.fake.plist_program(), test.mover.program());
    assert_eq!(fs::read(test.mover.backup()).unwrap(), test.legacy);
    assert_eq!(
        test.mover.update_doc().unwrap()["future_check"],
        json!({ "enabled": true })
    );
    assert!(test.fake.saved().is_none());
}

#[tokio::test]
async fn preconditions_refuse_disabled_foreign_pid_old_feature_and_wrong_versions() {
    for (sim, reason) in [
        (
            Sim {
                disabled: true,
                ..Sim::default()
            },
            "agent_disabled",
        ),
        (
            Sim {
                init_socket_pid: Some(99),
                ..Sim::default()
            },
            "move_identity_changed",
        ),
        (
            Sim {
                feature: false,
                ..Sim::default()
            },
            "daemon_too_old",
        ),
        (
            Sim {
                bundle_version: "0.2.9",
                ..Sim::default()
            },
            "move_bundle_version",
        ),
        (
            Sim {
                legacy_version: "0.3.1",
                ..Sim::default()
            },
            "move_version_mismatch",
        ),
    ] {
        let test = Test::new(sim);
        assert_eq!(test.run().await.unwrap_err().reason(), Some(reason));
        assert!(test.actions().is_empty());
        assert!(test.fake.saved().is_none());
    }
}

#[tokio::test]
async fn legacy_status_without_identity_reports_missing_drain_feature_first() {
    let reply: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/legacy/daemon_status-c98794f.json"
    ))
    .unwrap();
    let test = Test::new(Sim {
        legacy_status: Some(reply["data"].clone()),
        ..Sim::default()
    });
    let err = test.run().await.unwrap_err();
    assert_eq!(err.code, Code::Unsupported.as_str());
    assert_eq!(err.reason(), Some("daemon_too_old"));
    assert!(test.actions().is_empty());
}

#[tokio::test]
async fn an_existing_drain_is_refused_before_saving_a_move() {
    let test = Test::new(Sim {
        lease: Some("other-operation".into()),
        ..Sim::default()
    });
    assert_eq!(test.run().await.unwrap_err().reason(), Some("draining"));
    assert!(test.actions().is_empty());
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn legacy_env_edits_and_unknown_keys_require_an_exact_review() {
    let test = Test::new(Sim::default());
    let mut old = test.fake.decode_plist(&test.legacy).unwrap();
    let PlistValue::Dictionary(env) = old.get_mut("EnvironmentVariables").unwrap() else {
        unreachable!()
    };
    env.insert("PATH".into(), PlistValue::String("/somewhere:/bin".into()));
    let unknown = PlistValue::Dictionary(Plist::from([(
        "data".into(),
        PlistValue::Other(vec![1, 2, 3]),
    )]));
    old.insert("Custom".into(), unknown.clone());
    old.insert(
        "StandardErrorPath".into(),
        PlistValue::String("/somewhere/log".into()),
    );
    old.insert(
        "Program".into(),
        PlistValue::String("/different/program".into()),
    );
    atomic_write(&test.mover.plist(), &test.fake.encode_plist(&old).unwrap()).unwrap();

    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_plist_decision"));
    assert_eq!(
        error.detail["carry_over"],
        json!(["Custom", "EnvironmentVariables.PATH"])
    );
    assert_eq!(
        error.detail["requires_review"],
        json!(["Program", "StandardErrorPath"])
    );
    assert!(test.actions().is_empty());
    let review = test.mover.review(None).await.unwrap().unwrap();

    old.insert("Custom".into(), PlistValue::Bool(true));
    atomic_write(&test.mover.plist(), &test.fake.encode_plist(&old).unwrap()).unwrap();
    let error = test
        .mover
        .run(None, Some(&review), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("move_plist_decision"));
    assert!(test.actions().is_empty());

    old.insert("Custom".into(), unknown.clone());
    atomic_write(&test.mover.plist(), &test.fake.encode_plist(&old).unwrap()).unwrap();
    test.mover
        .run(None, Some(&review), &mut |_| {})
        .await
        .unwrap();
    let installed = test
        .fake
        .decode_plist(&fs::read(test.mover.plist()).unwrap())
        .unwrap();
    let mut wanted = template(&test.mover.program());
    wanted.insert("Custom".into(), unknown);
    let PlistValue::Dictionary(env) = wanted.get_mut("EnvironmentVariables").unwrap() else {
        unreachable!()
    };
    env.insert("PATH".into(), PlistValue::String("/somewhere:/bin".into()));
    assert_eq!(installed, wanted);
}

#[tokio::test]
async fn program_missing_skips_old_render_and_drain_and_requires_schema_consent() {
    for schema in [None, Some(APP_SCHEMA - 1), Some(APP_SCHEMA)] {
        let test = Test::new(Sim {
            pid: None,
            missing: true,
            schema,
            ..Sim::default()
        });
        if schema != Some(APP_SCHEMA) {
            let error = test.run().await.unwrap_err();
            assert_eq!(error.reason(), Some("move_schema_ack_required"));
            assert_eq!(error.detail["from_schema"], json!(schema));
            assert_eq!(error.detail["to_schema"], APP_SCHEMA);
            assert!(test.fake.saved().is_none());
            assert!(test.actions().is_empty());
        }
        test.mover
            .run(Some(APP_SCHEMA), None, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            test.actions(),
            [
                "disable:old",
                "bootout:old",
                "enable:bundle",
                "bootstrap:bundle"
            ]
        );
        let events = test.events();
        assert!(!events.contains(&format!("render:{FORMULA}")));
        assert!(!events.contains(&"status:old".into()));
        assert!(test.fake.time.load(Ordering::Relaxed) >= 2 + 2 * STEADY.as_secs());
    }
}

#[tokio::test]
async fn program_missing_refuses_older_schema_or_disabled_job_before_changes() {
    for (disabled, expected) in [(true, "agent_disabled"), (false, "move_schema_too_old")] {
        let test = Test::new(Sim {
            pid: None,
            missing: true,
            disabled,
            schema: Some(APP_SCHEMA + 1),
            ..Sim::default()
        });
        let error = test
            .mover
            .run(Some(APP_SCHEMA), None, &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.reason(), Some(expected));
        assert!(test.actions().is_empty());
    }
}

#[tokio::test]
async fn program_missing_checks_and_definite_failures_restore_enabled_without_daemon_status() {
    for (respawn_on_fence, primary_bootout, expected) in [
        (1, BootoutReply::Done, vec!["disable:old", "enable:old"]),
        (
            0,
            BootoutReply::Rejected,
            vec!["disable:old", "bootout:old", "enable:old"],
        ),
    ] {
        let test = Test::new(Sim {
            pid: None,
            missing: true,
            respawn_on_fence,
            primary_bootout,
            ..Sim::default()
        });
        let error = test.run().await.unwrap_err();
        assert_eq!(error.reason(), Some("move_incomplete"));
        assert_eq!(error.detail["retry_move"], true);
        assert_eq!(test.actions(), expected);
        assert!(!test.events().contains(&"status:old".into()));
        assert!(!test.mover.has_pending_move().unwrap());
        assert!(!test.fake.sim.lock().unwrap().disabled);
    }
}

#[tokio::test]
async fn program_missing_failed_bootstrap_keeps_bundle_plist_and_ends_incomplete() {
    let test = Test::new(Sim {
        pid: None,
        missing: true,
        new_start: StartReply::NoPid,
        ..Sim::default()
    });
    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(test.fake.plist_program(), test.mover.program());
    assert_eq!(
        test.actions(),
        [
            "disable:old",
            "bootout:old",
            "enable:bundle",
            "bootstrap:bundle",
            "enable:bundle"
        ]
    );
    assert!(!test.events().iter().any(|e| e.starts_with("status:")));
    test.recorded(Phase::Incomplete);
}

#[tokio::test]
async fn identity_change_after_hold_restores_enabled_and_resumes_from_drain() {
    let test = Test::new(Sim {
        respawn_on_fence: 1,
        ..Sim::default()
    });
    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_identity_changed"));
    assert_eq!(
        test.actions(),
        ["drain:old", "seal:old", "disable:old", "enable:old"]
    );
    test.recorded(Phase::Drain);
    test.run().await.unwrap();
    assert_eq!(
        test.actions()
            .iter()
            .filter(|s| *s == "bootout:old")
            .count(),
        1
    );
}

#[tokio::test]
async fn explicit_bootout_failure_waits_and_unseals_the_original_daemon() {
    let test = Test::new(Sim {
        primary_bootout: BootoutReply::Rejected,
        ..Sim::default()
    });
    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_restored"));
    assert_eq!(
        test.actions(),
        [
            "drain:old",
            "seal:old",
            "disable:old",
            "bootout:old",
            "unseal",
            "enable:old",
        ]
    );
    assert_eq!(
        test.fake.time.load(Ordering::Relaxed),
        2 + TIMEOUT + STEADY.as_secs()
    );
    assert_eq!(test.fake.plist_program(), Path::new(FORMULA));
    assert!(test.fake.saved().is_none());
}

#[tokio::test]
async fn unknown_print_and_unfinished_child_both_preserve_the_fence_and_bootout_record() {
    for case in [
        BootoutReply::Timeout,
        BootoutReply::PrintFailed,
        BootoutReply::LoadedAfterSuccess,
    ] {
        let test = Test::new(Sim {
            primary_bootout: case,
            ..Sim::default()
        });
        let error = test.run().await.unwrap_err();
        assert_eq!(error.reason(), Some("move_incomplete"));
        assert!(error.detail["manual_steps"].as_array().unwrap().len() >= 3);
        assert!(test.fake.sim.lock().unwrap().disabled);
        assert_eq!(
            test.actions(),
            ["drain:old", "seal:old", "disable:old", "bootout:old"]
        );
        test.recorded(Phase::Bootout);
    }
}

fn attempted(pid: u32) -> Option<BootoutAttempt> {
    Some(BootoutAttempt {
        pid: Some(pid),
        exit_timeout_s: TIMEOUT,
        timeout_reported: true,
        child_failed: false,
    })
}

#[tokio::test]
async fn resumed_bootout_probes_first_never_reissues_and_swaps_when_unloaded() {
    for check_repair in [false, true] {
        let test = Test::new(Sim::default());
        test.interrupted(Phase::Bootout, attempted(10)).await;
        {
            let mut sim = test.fake.sim.lock().unwrap();
            sim.disabled = true;
            sim.loaded = Loaded::None;
            sim.pid = None;
        }
        if check_repair {
            test.mover.check_repair(&mut |_| {}).await.unwrap();
        } else {
            test.run().await.unwrap();
        }
        assert_eq!(test.events().first().unwrap(), "print:absent");
        assert_eq!(test.actions(), ["enable:bundle", "bootstrap:bundle"]);
    }
}

#[tokio::test]
async fn confirmed_repair_completes_a_bootout_journaled_before_submission() {
    let test = Test::new(Sim::default());
    interrupted_hold(&test).await;
    let mut record = test.recorded(Phase::SealedHold);
    record.bootout = attempted(10);
    test.mover.phase(&mut record, Phase::Bootout).unwrap();
    test.fake.time.fetch_add(1_800, Ordering::Relaxed);

    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(error.detail["confirm_service_exit"], true);
    assert!(test.actions().is_empty());

    test.mover.check_repair(&mut |_| {}).await.unwrap();
    assert_eq!(
        test.actions(),
        ["bootout:old", "enable:bundle", "bootstrap:bundle"]
    );
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn bootout_repair_requires_the_disabled_label_and_the_exact_sealed_job() {
    let changes: [fn(&mut Sim); 5] = [
        |sim| sim.disabled = false,
        |sim| sim.pid = Some(11),
        |sim| sim.started += 1,
        |sim| sim.lease = Some("different-lease".into()),
        |sim| sim.sealed = false,
    ];
    for change in changes {
        let test = Test::new(Sim::default());
        interrupted_hold(&test).await;
        let mut record = test.recorded(Phase::SealedHold);
        record.bootout = attempted(10);
        test.mover.phase(&mut record, Phase::Bootout).unwrap();
        change(&mut test.fake.sim.lock().unwrap());

        let error = test.mover.check_repair(&mut |_| {}).await.unwrap_err();
        assert_eq!(error.reason(), Some("move_incomplete"));
        assert!(test.actions().is_empty());
        assert!(test.recorded(Phase::Bootout).bootout.is_some());
    }
}

#[tokio::test]
async fn confirmed_repair_completes_an_interrupted_rollback_bootout() {
    let test = Test::new(Sim {
        new_start: StartReply::WrongVersion,
        rollback_bootout: BootoutReply::Timeout,
        ..Sim::default()
    });
    assert_eq!(
        test.run().await.unwrap_err().reason(),
        Some("move_incomplete")
    );
    assert!(test.recorded(Phase::RollbackHold).bootout.is_some());
    test.fake.sim.lock().unwrap().rollback_bootout = BootoutReply::Done;
    let already_ran = test.actions().len();

    let result = test.mover.check_repair(&mut |_| {}).await;
    assert_eq!(result.unwrap_err().reason(), Some("move_restored"));
    assert_eq!(
        &test.actions()[already_ran..],
        ["bootout:bundle", "enable:old", "bootstrap:old"]
    );
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn missing_program_bootout_repair_rechecks_no_process_and_free_lock() {
    for (missing, stopped) in [(true, true), (false, true), (true, false)] {
        let test = Test::new(Sim {
            missing: true,
            pid: None,
            ..Sim::default()
        });
        let attempt = BootoutAttempt {
            pid: None,
            ..attempted(10).unwrap()
        };
        test.interrupted(Phase::Bootout, Some(attempt)).await;
        {
            let mut sim = test.fake.sim.lock().unwrap();
            sim.disabled = true;
            sim.missing = missing;
            sim.stopped = stopped;
        }
        let result = test.mover.check_repair(&mut |_| {}).await;
        if missing && stopped {
            result.unwrap();
            assert_eq!(
                test.actions(),
                ["bootout:old", "enable:bundle", "bootstrap:bundle"]
            );
        } else {
            assert_eq!(result.unwrap_err().reason(), Some("move_incomplete"));
            assert!(test.actions().is_empty());
            assert!(test.recorded(Phase::Bootout).bootout.is_some());
        }
    }
}

#[tokio::test]
async fn rollback_handles_new_daemon_with_pid_no_pid_and_unloaded() {
    for (reply, expected) in [
        (
            StartReply::WrongVersion,
            vec![
                "drain:bundle",
                "seal:bundle",
                "disable:bundle",
                "bootout:bundle",
                "enable:old",
                "bootstrap:old",
            ],
        ),
        (
            StartReply::NoPid,
            vec![
                "disable:bundle",
                "bootout:bundle",
                "enable:old",
                "bootstrap:old",
            ],
        ),
        (
            StartReply::Unloaded,
            vec!["disable:absent", "enable:old", "bootstrap:old"],
        ),
    ] {
        let test = Test::new(Sim {
            new_start: reply,
            ..Sim::default()
        });
        let error = test.run().await.unwrap_err();
        assert_eq!(error.reason(), Some("move_restored"));
        assert_eq!(&test.actions()[6..], expected);
        assert_eq!(test.fake.plist_program(), Path::new(FORMULA));
        assert!(test.fake.saved().is_none());
    }
}

#[tokio::test]
async fn rollback_pid_without_a_reply_is_never_booted_out() {
    for quits in [None, Some(5)] {
        let test = Test::new(Sim {
            new_start: StartReply::NoAnswer,
            bundle_pid_quits_at: quits,
            ..Sim::default()
        });
        let error = test.run().await.unwrap_err();
        let actions = test.actions();
        assert!(!actions.contains(&"bootout:bundle".into()));
        assert!(!actions.contains(&"drain:bundle".into()));
        if quits.is_some() {
            assert_eq!(error.reason(), Some("move_restored"));
            assert_eq!(
                &actions[6..],
                ["disable:absent", "enable:old", "bootstrap:old"]
            );
            assert!(test.fake.time.load(Ordering::Relaxed) >= 5);
        } else {
            assert_eq!(error.reason(), Some("move_incomplete"));
            assert_eq!(test.fake.time.load(Ordering::Relaxed), 2 + TIMEOUT);
            test.recorded(Phase::RollbackHold);
        }
    }
}

#[tokio::test]
async fn resumed_rollback_re_fences_an_unloaded_job_if_its_label_is_enabled() {
    for (attempt, disabled, expected) in [
        (attempted(20), true, vec!["enable:old", "bootstrap:old"]),
        (
            attempted(20),
            false,
            vec!["disable:absent", "enable:old", "bootstrap:old"],
        ),
        (
            None,
            false,
            vec!["disable:absent", "enable:old", "bootstrap:old"],
        ),
    ] {
        let test = Test::new(Sim::default());
        test.interrupted(Phase::RollbackHold, attempt).await;
        fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
        {
            let mut sim = test.fake.sim.lock().unwrap();
            sim.loaded = Loaded::None;
            sim.pid = None;
            sim.disabled = disabled;
        }
        assert_eq!(
            test.run().await.unwrap_err().reason(),
            Some("move_restored")
        );
        assert_eq!(test.events().first().unwrap(), "print:absent");
        if !disabled {
            let events = test.events();
            let fence = events.iter().position(|e| e == "disable:absent").unwrap();
            assert_eq!(events[fence + 1], "print:absent");
        }
        assert_eq!(test.actions(), expected);
    }
}

#[tokio::test]
async fn rollback_retries_identity_change_once() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.pid = Some(20);
        sim.respawn_on_fence = 2;
    }
    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(
        test.actions(),
        [
            "drain:bundle",
            "seal:bundle",
            "disable:bundle",
            "enable:bundle",
            "drain:bundle",
            "seal:bundle",
            "disable:bundle",
            "enable:bundle",
        ]
    );
    assert!(test.recorded(Phase::Incomplete).rollback_retried);
}

#[tokio::test]
async fn failed_restore_keeps_the_fence_and_does_not_start_any_daemon() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    fs::remove_file(test.mover.backup()).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::None;
        sim.pid = None;
    }
    assert_eq!(
        test.run().await.unwrap_err().reason(),
        Some("move_incomplete")
    );
    assert_eq!(test.actions(), ["disable:absent"]);
    assert!(test.fake.sim.lock().unwrap().disabled);
    test.recorded(Phase::RestoreLegacy);
}

#[tokio::test]
async fn verify_rejects_wrong_answering_pid_and_pauses_the_budget_for_tcc() {
    let wrong = Test::new(Sim {
        new_start: StartReply::WrongSocketPid,
        ..Sim::default()
    });
    let error = wrong.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert!(!wrong.actions().contains(&"bootout:bundle".into()));
    assert!(
        wrong
            .recorded(Phase::RollbackHold)
            .failure
            .unwrap()
            .contains("answering daemon")
    );
    let waiting = Test::new(Sim {
        tcc_for: 200,
        ..Sim::default()
    });
    let mut progress = Vec::new();
    waiting
        .mover
        .run(None, None, &mut |p| progress.push(p))
        .await
        .unwrap();
    assert!(progress.contains(&UpdateState::WaitingPermission));
    assert!(waiting.fake.time.load(Ordering::Relaxed) > VERIFY_TIMEOUT.as_secs());
}

#[tokio::test]
async fn verify_retries_a_transient_print_failure() {
    let test = Test::new(Sim {
        bundle_print_failures: 1,
        ..Sim::default()
    });
    test.run().await.unwrap();
    assert!(!test.mover.has_pending_move().unwrap());
    assert_eq!(
        test.actions(),
        [
            "drain:old",
            "seal:old",
            "disable:old",
            "bootout:old",
            "enable:bundle",
            "bootstrap:bundle"
        ]
    );
}

#[tokio::test]
async fn unknown_move_phase_or_pending_update_is_preserved() {
    for (doc, reason) in [
        (
            br#"{"move":{"phase":"future"}}"#.as_slice(),
            "move_record_unknown",
        ),
        (
            br#"{"txn":{"phase":"installing"}}"#.as_slice(),
            "update_in_progress",
        ),
    ] {
        let test = Test::new(Sim::default());
        atomic_write(&test.mover.journal(), doc).unwrap();
        assert_eq!(test.run().await.unwrap_err().reason(), Some(reason));
        assert_eq!(fs::read(test.mover.journal()).unwrap(), doc);
        assert!(test.events().is_empty());
    }
}

async fn interrupted_hold(test: &Test) {
    test.interrupted(Phase::SealedHold, None).await;
    let mut record = test.recorded(Phase::SealedHold);
    record.hold = Some(Hold {
        daemon: record.predecessor.clone().unwrap(),
        lease_id: "saved-hold".into(),
    });
    test.mover.save(Some(&record)).unwrap();
    let doc = test.mover.update_doc().unwrap();
    assert_eq!(doc["move"]["pid"], 10);
    assert_eq!(doc["move"]["lease_id"], "saved-hold");
    let mut sim = test.fake.sim.lock().unwrap();
    sim.disabled = true;
    sim.lease = Some("saved-hold".into());
    sim.sealed = true;
}

#[tokio::test]
async fn resume_reuses_the_saved_hold_but_refuses_a_recycled_pid() {
    let matching = Test::new(Sim::default());
    interrupted_hold(&matching).await;
    matching.fake.time.fetch_add(1_800, Ordering::Relaxed);
    matching.run().await.unwrap();
    assert_eq!(
        matching.actions(),
        [
            "disable:old",
            "bootout:old",
            "enable:bundle",
            "bootstrap:bundle",
        ]
    );

    let restarted = Test::new(Sim::default());
    interrupted_hold(&restarted).await;
    {
        let mut sim = restarted.fake.sim.lock().unwrap();
        sim.started += 1;
        sim.sealed = false;
        sim.lease = None;
    }
    assert_eq!(
        restarted.run().await.unwrap_err().reason(),
        Some("move_identity_changed")
    );
    restarted.recorded(Phase::Drain);
    assert_eq!(restarted.actions(), ["enable:old"]);
}

#[tokio::test]
async fn cancel_unseals_the_saved_hold_but_refuses_an_outstanding_bootout() {
    let safe = Test::new(Sim::default());
    interrupted_hold(&safe).await;
    let mut progress = Vec::new();
    assert!(safe.mover.has_pending_move().unwrap());
    safe.mover
        .cancel_pending(&mut |state| progress.push(state))
        .await
        .unwrap();
    assert_eq!(progress.last(), Some(&UpdateState::Cancelled));
    assert_eq!(safe.actions(), ["unseal", "enable:old"]);
    assert!(!safe.mover.has_pending_move().unwrap());

    let unsafe_to_cancel = Test::new(Sim::default());
    unsafe_to_cancel
        .interrupted(Phase::Bootout, attempted(10))
        .await;
    let error = unsafe_to_cancel
        .mover
        .cancel_pending(&mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("move_cancel_blocked"));
    assert!(unsafe_to_cancel.actions().is_empty());
}

#[tokio::test]
async fn a_replaced_bundle_is_not_started_using_saved_move_consent() {
    let test = Test::new(Sim {
        bundle_version_on_fence: Some("0.2.9"),
        ..Sim::default()
    });
    assert_eq!(
        test.run().await.unwrap_err().reason(),
        Some("move_restored")
    );
    assert_eq!(
        test.actions(),
        [
            "drain:old",
            "seal:old",
            "disable:old",
            "bootout:old",
            "disable:absent",
            "enable:old",
            "bootstrap:old",
        ]
    );
}

#[tokio::test]
async fn rollback_preserves_a_plist_edit_made_after_backup() {
    let test = Test::new(Sim::default());
    let mut plist = test.fake.decode_plist(&test.legacy).unwrap();
    plist.insert(
        "StandardErrorPath".into(),
        PlistValue::String("/tmp/edited.log".into()),
    );
    let edited = test.fake.encode_plist(&plist).unwrap();
    let error = test
        .mover
        .run(None, None, &mut |state| {
            if state == UpdateState::Stopping {
                atomic_write(&test.mover.plist(), &edited).unwrap();
            }
        })
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(fs::read(test.mover.plist()).unwrap(), edited);
    assert_eq!(fs::read(test.mover.backup()).unwrap(), test.legacy);
    let steps = error.detail["manual_steps"].to_string();
    assert!(steps.contains("Preserve your edited LaunchAgent"));
    assert!(steps.contains("Compare it with the saved original"));
    assert!(!steps.contains("plutil -lint"));
    assert!(!steps.contains("launchctl enable"));
    assert!(!steps.contains("bootstrap"));
    assert_eq!(
        test.actions(),
        [
            "drain:old",
            "seal:old",
            "disable:old",
            "bootout:old",
            "disable:absent"
        ]
    );
    test.recorded(Phase::RestoreLegacy);

    // The operator started the expected formula with their corrected plist.
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Legacy;
        sim.disabled = false;
        sim.pid = Some(30);
        sim.lease = Some("another-operation".into());
        sim.sealed = true;
    }
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    assert!(test.mover.has_pending_move().unwrap());
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.lease = None;
        sim.sealed = false;
    }
    let mut progress = Vec::new();
    test.mover
        .check_repair(&mut |state| progress.push(state))
        .await
        .unwrap();
    assert_eq!(
        progress,
        [UpdateState::Restored {
            from: VERSION.into()
        }]
    );
    assert!(!test.mover.has_pending_move().unwrap());
    assert_eq!(fs::read(test.mover.plist()).unwrap(), edited);
}

#[tokio::test]
async fn resumed_start_refuses_both_an_edited_replacement_and_a_legacy_program_override() {
    for phase in [Phase::Start, Phase::StartLegacy] {
        let test = Test::new(Sim::default());
        test.interrupted(phase, None).await;
        let bundle = phase == Phase::Start;
        let mut plist = if bundle {
            template(&test.mover.program())
        } else {
            test.fake.decode_plist(&test.legacy).unwrap()
        };
        plist.insert(
            "Program".into(),
            PlistValue::String("/unexpected/executable".into()),
        );
        let bytes = test.fake.encode_plist(&plist).unwrap();
        if !bundle {
            atomic_write(&test.mover.backup(), &bytes).unwrap();
        }
        atomic_write(&test.mover.plist(), &bytes).unwrap();
        {
            let mut sim = test.fake.sim.lock().unwrap();
            sim.loaded = Loaded::None;
            sim.pid = None;
            sim.disabled = true;
        }
        assert_eq!(
            test.run().await.unwrap_err().reason(),
            Some("move_incomplete")
        );
        assert!(
            test.actions()
                .iter()
                .all(|action| !action.starts_with("enable:") && !action.starts_with("bootstrap:"))
        );
        assert_eq!(fs::read(test.mover.plist()).unwrap(), bytes);
    }
}

#[tokio::test]
async fn timed_out_disable_keeps_the_hold_and_cannot_race_an_enable() {
    let test = Test::new(Sim {
        disable_times_out: true,
        ..Sim::default()
    });
    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(error.detail["confirm_service_exit"], true);
    assert_eq!(
        test.recorded(Phase::SealedHold).service_attempt,
        Some(ServiceAttempt::Disable)
    );
    assert_eq!(test.actions(), ["drain:old", "seal:old", "disable:old"]);
    assert_eq!(
        test.run().await.unwrap_err().reason(),
        Some("move_incomplete")
    );
    assert_eq!(test.actions(), ["drain:old", "seal:old", "disable:old"]);
    {
        let mut sim = test.fake.sim.lock().unwrap();
        assert!(sim.sealed);
        assert!(!sim.disabled);
        sim.disabled = true; // The earlier child completed after its wrapper timed out.
    }
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_restored")
    );
    assert_eq!(&test.actions()[3..], ["unseal", "enable:old"]);
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn rollback_resume_releases_its_hold_when_the_formula_disappears() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    let mut record = test.recorded(Phase::RollbackHold);
    record.hold = Some(Hold {
        daemon: DaemonIdentity {
            pid: 20,
            started: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            version: VERSION.into(),
        },
        lease_id: "rollback-hold".into(),
    });
    test.mover.save(Some(&record)).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.pid = Some(20);
        sim.started = 20;
        sim.lease = Some("rollback-hold".into());
        sim.sealed = true;
        sim.disabled = true;
        sim.missing = true;
    }
    assert_eq!(
        test.run().await.unwrap_err().reason(),
        Some("move_incomplete")
    );
    assert_eq!(test.actions(), ["unseal", "enable:bundle"]);
    assert!(test.recorded(Phase::Incomplete).hold.is_none());
    assert_eq!(test.fake.plist_program(), test.mover.program());
}

#[tokio::test]
async fn rollback_fence_repair_releases_a_hold_only_after_the_disable_is_resolved() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    let mut record = test.recorded(Phase::RollbackHold);
    record.service_attempt = Some(ServiceAttempt::Disable);
    record.hold = Some(Hold {
        daemon: DaemonIdentity {
            pid: 20,
            started: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            version: VERSION.into(),
        },
        lease_id: "rollback-hold".into(),
    });
    test.mover.save(Some(&record)).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.pid = Some(20);
        sim.started = 20;
        sim.lease = Some("rollback-hold".into());
        sim.sealed = true;
    }
    let error = test.run().await.unwrap_err();
    assert_eq!(error.detail["confirm_service_exit"], true);
    assert!(test.actions().is_empty());
    test.fake.sim.lock().unwrap().disabled = true;
    let error = test.mover.check_repair(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(error.detail["confirm_service_exit"], false);
    assert_eq!(test.actions(), ["unseal", "enable:bundle"]);
    test.mover.check_repair(&mut |_| {}).await.unwrap();
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn resumed_mismatched_rollback_hold_cannot_spend_a_second_retry() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    let mut record = test.recorded(Phase::RollbackHold);
    record.rollback_retried = true;
    record.hold = Some(Hold {
        daemon: record.predecessor.clone().unwrap(),
        lease_id: "old-hold".into(),
    });
    test.mover.save(Some(&record)).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.pid = Some(20);
    }
    assert_eq!(
        test.run().await.unwrap_err().reason(),
        Some("move_incomplete")
    );
    assert_eq!(test.actions(), ["enable:bundle"]);
    assert!(test.recorded(Phase::Incomplete).rollback_retried);
}

#[tokio::test]
async fn repair_retries_a_rollback_enable_interrupted_before_submission() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    let mut record = test.recorded(Phase::RollbackHold);
    record.service_attempt = Some(ServiceAttempt::Enable);
    record.hold = Some(Hold {
        daemon: DaemonIdentity {
            pid: 20,
            started: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            version: VERSION.into(),
        },
        lease_id: "lost-rollback-hold".into(),
    });
    test.mover.save(Some(&record)).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.disabled = true;
        sim.pid = Some(21);
        sim.started = 21;
    }

    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(error.detail["confirm_service_exit"], true);
    assert!(
        error.detail["manual_steps"]
            .to_string()
            .contains("choose Check repair to reconcile")
    );
    assert!(test.actions().is_empty());

    let error = test
        .mover
        .check_repair(&mut |state| {
            if state == UpdateState::Stopping {
                assert!(test.recorded(Phase::RollbackHold).rollback_retried);
            }
        })
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("move_restored"));
    assert_eq!(
        test.actions(),
        [
            "enable:bundle",
            "drain:bundle",
            "seal:bundle",
            "disable:bundle",
            "bootout:bundle",
            "enable:old",
            "bootstrap:old",
        ]
    );
    assert!(!test.fake.sim.lock().unwrap().disabled);
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn no_pid_rollback_repair_enables_before_draining_and_consumes_the_retry() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::RollbackHold, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    let mut record = test.recorded(Phase::RollbackHold);
    record.service_attempt = Some(ServiceAttempt::Enable);
    assert!(record.hold.is_none());
    test.mover.save(Some(&record)).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.disabled = true;
        sim.pid = Some(21);
        sim.started = 21;
        sim.respawn_on_fence = 1;
    }

    let error = test
        .mover
        .check_repair(&mut |state| {
            if matches!(state, UpdateState::Draining { .. }) {
                assert!(test.recorded(Phase::RollbackHold).rollback_retried);
                assert!(!test.fake.sim.lock().unwrap().disabled);
            }
        })
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(
        test.actions(),
        [
            "enable:bundle",
            "drain:bundle",
            "seal:bundle",
            "disable:bundle",
            "enable:bundle"
        ]
    );
    assert!(test.recorded(Phase::Incomplete).rollback_retried);
}

#[tokio::test]
async fn resumes_unissued_starts_only_after_resolving_any_outstanding_bootstrap() {
    for (phase, expected) in [
        (Phase::Start, ["enable:bundle", "bootstrap:bundle"]),
        (Phase::StartLegacy, ["enable:old", "bootstrap:old"]),
    ] {
        for uncertain in [false, true] {
            let test = Test::new(Sim::default());
            test.interrupted(phase, None).await;
            if phase == Phase::Start {
                fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
            }
            {
                let mut sim = test.fake.sim.lock().unwrap();
                sim.loaded = Loaded::None;
                sim.pid = None;
                sim.disabled = true;
            }
            let result = if uncertain {
                let mut record = test.recorded(phase);
                record.service_attempt = Some(ServiceAttempt::Bootstrap);
                test.mover.save(Some(&record)).unwrap();
                assert_eq!(
                    test.run().await.unwrap_err().reason(),
                    Some("move_incomplete")
                );
                assert!(test.actions().is_empty());
                test.mover.check_repair(&mut |_| {}).await
            } else {
                test.run().await
            };
            if phase == Phase::Start {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().reason(), Some("move_restored"));
            }
            assert_eq!(test.actions(), expected);
            assert!(!test.mover.has_pending_move().unwrap());
        }
    }
}

#[tokio::test]
async fn interrupted_enable_recovers_the_loaded_formula_only_with_the_saved_plist() {
    for plist_edited in [false, true] {
        let test = Test::new(Sim::default());
        test.interrupted(Phase::StartLegacy, None).await;
        let mut record = test.recorded(Phase::StartLegacy);
        record.service_attempt = Some(ServiceAttempt::Enable);
        test.mover.save(Some(&record)).unwrap();
        test.fake.sim.lock().unwrap().disabled = true;
        if plist_edited {
            let mut plist = template(Path::new(FORMULA));
            plist.insert(
                "ProcessType".into(),
                PlistValue::String("Interactive".into()),
            );
            atomic_write(
                &test.mover.plist(),
                &test.fake.encode_plist(&plist).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            test.run().await.unwrap_err().reason(),
            Some("move_incomplete")
        );
        assert!(test.actions().is_empty());

        let error = test.mover.check_repair(&mut |_| {}).await.unwrap_err();
        if plist_edited {
            assert_eq!(error.reason(), Some("move_incomplete"));
            assert!(test.actions().is_empty());
            assert!(test.mover.has_pending_move().unwrap());
        } else {
            assert_eq!(error.reason(), Some("move_restored"));
            assert_eq!(test.actions(), ["enable:old"]);
            assert!(!test.fake.sim.lock().unwrap().disabled);
            assert!(!test.mover.has_pending_move().unwrap());
        }
    }
}

#[tokio::test]
async fn interrupted_bootstrap_with_a_loaded_but_failed_bundle_rolls_back() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::Start, None).await;
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    let mut record = test.recorded(Phase::Start);
    record.service_attempt = Some(ServiceAttempt::Bootstrap);
    test.mover.save(Some(&record)).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.pid = Some(20);
        sim.new_start = StartReply::WrongVersion;
    }

    let error = test.mover.check_repair(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("move_restored"));
    assert_eq!(
        test.actions(),
        [
            "drain:bundle",
            "seal:bundle",
            "disable:bundle",
            "bootout:bundle",
            "enable:old",
            "bootstrap:old"
        ]
    );
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn interrupted_enable_of_the_missing_formula_restores_enabled_and_allows_retry() {
    let test = Test::new(Sim {
        missing: true,
        pid: None,
        ..Sim::default()
    });
    test.interrupted(Phase::RestoreMissing, None).await;
    let mut record = test.recorded(Phase::RestoreMissing);
    record.service_attempt = Some(ServiceAttempt::Enable);
    test.mover.save(Some(&record)).unwrap();
    test.fake.sim.lock().unwrap().disabled = true;

    let error = test.mover.check_repair(&mut |_| {}).await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(error.detail["retry_move"], true);
    assert_eq!(test.actions(), ["enable:old"]);
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn ordinary_legacy_repair_still_requires_the_saved_version() {
    let test = Test::new(Sim::default());
    test.interrupted(Phase::VerifyLegacy, None).await;
    test.fake.sim.lock().unwrap().legacy_version = "0.4.0";
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    test.recorded(Phase::VerifyLegacy);
    assert!(test.actions().is_empty());
}

#[tokio::test]
async fn a_failed_intent_save_submits_no_service_call_and_restores_in_memory_state() {
    let test = Test::new(Sim::default());
    interrupted_hold(&test).await;
    let mut record = test.recorded(Phase::SealedHold);
    atomic_write(&test.mover.journal(), b"{").unwrap();

    assert!(test.mover.disable(&mut record).await.is_err());
    assert!(record.service_attempt.is_none());
    let mut job = test.mover.job().await.unwrap().unwrap();
    job.exit_timeout += STEADY;
    assert!(
        test.mover
            .begin_bootout(&mut record, &job, false)
            .await
            .is_err()
    );
    assert_eq!(record.phase, Phase::SealedHold);
    assert_eq!(record.exit_timeout_s, TIMEOUT);
    assert!(record.bootout.is_none());
    assert!(test.actions().is_empty());
}

#[tokio::test]
async fn active_drain_consumes_cancel_without_waiting_for_the_daemon_to_be_idle() {
    let test = Test::new(Sim {
        busy_until: 120,
        ..Sim::default()
    });
    let cancel = AtomicBool::new(false);
    let mut progress = Vec::new();
    test.mover
        .run_cancellable(
            None,
            None,
            &|| cancel.load(Ordering::Relaxed),
            &mut |state| {
                if matches!(state, UpdateState::Draining { .. }) {
                    cancel.store(true, Ordering::Relaxed);
                }
                progress.push(state);
            },
        )
        .await
        .unwrap();
    assert_eq!(progress.last(), Some(&UpdateState::Cancelled));
    assert_eq!(test.actions(), ["drain:old", "unseal"]);
    assert!(!test.fake.sim.lock().unwrap().disabled);
    assert!(test.fake.time.load(Ordering::Relaxed) < 120);
    assert!(!test.mover.has_pending_move().unwrap());
}

#[tokio::test]
async fn missing_program_reappearing_before_disable_clears_the_no_change_move() {
    let test = Test::new(Sim {
        pid: None,
        missing: true,
        ..Sim::default()
    });
    test.interrupted(Phase::Drain, None).await;
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.missing = false;
        sim.pid = Some(10);
    }
    let error = test.run().await.unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(error.detail["retry_move"], true);
    assert!(!test.mover.has_pending_move().unwrap());
    assert!(test.actions().is_empty());
    test.run().await.unwrap();
}

#[tokio::test]
async fn schema_refusal_after_unload_is_saved_and_never_advises_bootstrapping() {
    let test = Test::new(Sim {
        pid: None,
        missing: true,
        ..Sim::default()
    });
    let error = test
        .mover
        .run(None, None, &mut |state| {
            if state == UpdateState::Stopping {
                test.fake.sim.lock().unwrap().schema = Some(APP_SCHEMA + 1);
            }
        })
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some("move_incomplete"));
    assert_eq!(
        test.recorded(Phase::Incomplete)
            .schema_block
            .unwrap()
            .reason(),
        Some("move_schema_too_old")
    );
    let steps = error.detail["manual_steps"].to_string();
    assert!(steps.contains("Keep the original LaunchAgent"));
    assert!(!steps.contains("bootstrap"));
    assert_eq!(test.actions(), ["disable:old", "bootout:old", "enable:old"]);
    assert_eq!(fs::read(test.mover.plist()).unwrap(), test.legacy);

    // Even an answering job cannot clear the saved refusal for this bundle.
    fs::rename(test.mover.staged(), test.mover.plist()).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Bundle;
        sim.pid = Some(20);
    }
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    assert!(test.mover.has_pending_move().unwrap());

    // An operator can restore a compatible formula at the saved executable.
    atomic_write(&test.mover.plist(), &test.legacy).unwrap();
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.loaded = Loaded::Legacy;
        sim.missing = false;
        sim.pid = Some(30);
        sim.legacy_version = "0.4.0";
        sim.init_socket_pid = Some(99);
    }
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.init_socket_pid = None;
        sim.lease = Some("other-operation".into());
    }
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    {
        let mut sim = test.fake.sim.lock().unwrap();
        sim.lease = None;
        sim.live_schema = None;
    }
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    test.fake.sim.lock().unwrap().live_schema = Some(APP_SCHEMA);
    assert_eq!(
        test.mover
            .check_repair(&mut |_| {})
            .await
            .unwrap_err()
            .reason(),
        Some("move_incomplete")
    );
    test.fake.sim.lock().unwrap().live_schema = Some(APP_SCHEMA + 1);
    let mut progress = Vec::new();
    test.mover
        .check_repair(&mut |state| progress.push(state))
        .await
        .unwrap();
    assert_eq!(
        progress.last(),
        Some(&UpdateState::Restored {
            from: "0.4.0".into()
        })
    );
    assert_eq!(test.actions(), ["disable:old", "bootout:old", "enable:old"]);
    assert!(!test.mover.has_pending_move().unwrap());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn move_then_update_uses_the_same_plist_and_journal() {
    let test = Test::new(Sim::default());
    let journal = test.mover.journal();
    // Keep the production cache writer active while the Move and install
    // coordinators use the same document. Writes must never replace the roots
    // owned by the other coordinator.
    let cache_path = journal.clone();
    let writer = std::thread::spawn(move || {
        for _ in 0..64 {
            super::super::cache::write_automatic(&cache_path, false).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    test.run().await.unwrap();
    assert!(!test.mover.has_pending_move().unwrap());
    assert_eq!(test.fake.plist_program(), test.mover.program());
    let peer = MoveDrain::status(test.fake.as_ref()).await.unwrap();
    super::super::install::tests::upgrade_moved_installation(
        test.mover.bundle.clone(),
        test.mover.plist(),
        journal.parent().unwrap().to_owned(),
        &peer,
    )
    .await;
    writer.join().unwrap();
    let document = super::super::cache::read_document(&journal).unwrap();
    assert_eq!(document.get("future_check"), Some(&json!({"enabled":true})));
    assert_eq!(document.get("check").unwrap()["automatic"], false);
    assert!(document.get("txn").is_none());
    assert!(document.get("move").is_none());
}

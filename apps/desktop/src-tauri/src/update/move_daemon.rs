//! One-time move of the login user's formula LaunchAgent into the app (§6.2).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_types::Code;

use super::{
    Clock, UpdateFuture,
    bundle::{BundleInfo, BundleOps},
    state::UpdateState,
    txn::DaemonIdentity,
};
use crate::error::GuiError;
use crate::service::{
    AGENT_LABEL, ServiceManager, launchctl::BootoutTarget, plist_for_home, scope,
};

const INSTALLED_APP: &str = "/Applications/GIT STRATA.app";
const APP_SUPPORT: &str = "Library/Application Support/io.github.dickwu.git-strata";
const PLIST_NAME: &str = "com.gwd.git-stratad.plist";
const POLL: Duration = Duration::from_secs(1);
const STEADY: Duration = Duration::from_secs(5);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(150);
const RENEW: Duration = Duration::from_secs(30);
const TTL_S: u32 = 120;
const DEFAULT_EXIT_TIMEOUT: Duration = Duration::from_secs(900);

/// Adapter to the daemon.drain client. The socket and every RPC deadline are
/// chosen by the adapter, never by the webview.
pub trait MoveDrain: Send + Sync {
    fn status(&self) -> UpdateFuture<'_, Result<Value, GuiError>>;
    fn acquire(&self, _expected: &DaemonIdentity) -> UpdateFuture<'_, Result<Value, GuiError>> {
        self.drain(strata_client::drain_on_params(None))
    }
    fn drain(&self, params: Value) -> UpdateFuture<'_, Result<Value, GuiError>>;
}

pub type Plist = BTreeMap<String, PlistValue>;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Serialize, Deserialize))]
pub enum PlistValue {
    String(String),
    Bool(bool),
    Array(Vec<PlistValue>),
    Dictionary(Plist),
    /// A canonical standalone plist value for the remaining native types.
    /// Round-tripping it preserves the original type (e.g. dates and data).
    Other(Vec<u8>),
}

/// Platform operations wired in T13. A failed socket or lock probe is an
/// error, not evidence that the daemon has stopped.
pub trait MovePlatform: Send + Sync {
    fn bundle_version(&self) -> Result<String, GuiError>;
    fn render_plist(&self, program: &Path) -> Result<Vec<u8>, GuiError>;
    fn lint_plist(&self, staged: &Path) -> Result<(), GuiError>;
    fn decode_plist(&self, bytes: &[u8]) -> Result<Plist, GuiError>;
    fn encode_plist(&self, plist: &Plist) -> Result<Vec<u8>, GuiError>;

    /// Read store_state.json in the login home's config directory. An
    /// absent, unreadable or different configured root gives no baseline.
    fn store_schema(&self) -> Option<u32>;
    /// True only for an absent/refusing socket (including a stale file) and
    /// a takeable daemon.lock. Never probes or modifies the data root.
    fn socket_refusing_and_lock_free(&self) -> Result<bool, GuiError>;

    fn program_missing(&self, program: &Path) -> Result<bool, GuiError> {
        match fs::metadata(program) {
            Ok(_) => Ok(false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(file_error(program, e)),
        }
    }
}

pub struct MoveAdapters {
    pub service: Arc<dyn ServiceManager>,
    pub daemon: Arc<dyn MoveDrain>,
    pub platform: Arc<dyn MovePlatform>,
    pub bundle: Arc<dyn BundleOps>,
    pub clock: Arc<dyn Clock>,
}

/// An exact review of the old plist and the two templates. The IPC stops at
/// a review when it has no explicit decision. A decision from the host must
/// refer to this review, including its original values, before it can apply.
#[derive(Clone, PartialEq, Eq)]
pub struct PlistReview {
    original: Plist,
    old_template: Plist,
    new_template: Plist,
    carry_over: Vec<String>,
    requires_review: Vec<String>,
}

impl PlistReview {
    pub fn carry_over(&self) -> &[String] {
        &self.carry_over
    }

    pub fn requires_review(&self) -> &[String] {
        &self.requires_review
    }
}

pub struct MoveDaemon {
    adapters: MoveAdapters,
    home: PathBuf,
    bundle: PathBuf,
    app_version: String,
    busy: tokio::sync::Mutex<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Render,
    Backup,
    Drain,
    SealedHold,
    Bootout,
    Swap,
    Start,
    Verify,
    RestoreLegacy,
    StartLegacy,
    VerifyLegacy,
    RollbackHold,
    RestoreMissing,
    RestoreEnabledAfterRollback,
    Incomplete,
}

/// A launchctl request whose outcome was not yet recorded. A wrapper timeout
/// leaves the child running, so an opposite operation must not race it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ServiceAttempt {
    Disable,
    Enable,
    Bootstrap,
}

impl ServiceAttempt {
    fn verb(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Enable => "enable",
            Self::Bootstrap => "bootstrap",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Bundle,
    Legacy,
}

#[derive(Debug, Serialize, Deserialize)]
struct MoveRecord {
    phase: Phase,
    old_program: PathBuf,
    was_enabled: bool,
    program_missing: bool,
    bundle_version: String,
    target_schema: u32,
    schema_acknowledgement: Option<u32>,
    exit_timeout_s: u64,
    predecessor: Option<DaemonIdentity>,
    #[serde(flatten)]
    hold: Option<Hold>,
    bootout: Option<BootoutAttempt>,
    #[serde(default)]
    cancelled: bool,
    #[serde(default)]
    rollback_retried: bool,
    #[serde(default)]
    retry_enable_pending: bool,
    #[serde(default)]
    failure: Option<String>,
    #[serde(default)]
    service_attempt: Option<ServiceAttempt>,
    #[serde(default)]
    schema_block: Option<GuiError>,
    #[serde(default)]
    retry_after_restore: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Hold {
    #[serde(flatten)]
    daemon: DaemonIdentity,
    lease_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BootoutAttempt {
    pid: Option<u32>,
    exit_timeout_s: u64,
    timeout_reported: bool,
    child_failed: bool,
}

#[derive(Debug)]
struct Job {
    program: PathBuf,
    pid: Option<u32>,
    exit_timeout: Duration,
    timeout_reported: bool,
}

struct Prepared {
    record: MoveRecord,
    original: Vec<u8>,
    review: Option<PlistReview>,
    rendered: Vec<u8>,
}

enum Bootout {
    Done,
    Failed,
    Uncertain,
}

type Progress<'a> = dyn FnMut(UpdateState) + Send + 'a;

impl MoveDaemon {
    /// Only the installed release app in the login user's scope can own a
    /// Move. T13 supplies the actual bundle path and fixed-argv adapters.
    pub fn new(
        home: &Path,
        login: Option<&Path>,
        bundle: &Path,
        app_version: String,
        adapters: MoveAdapters,
    ) -> Result<Self, GuiError> {
        let home = scope::lifecycle_scope(home, login)?;
        if cfg!(debug_assertions)
            || bundle != Path::new(INSTALLED_APP)
            || fs::canonicalize(bundle).ok().as_deref() != Some(bundle)
        {
            return Err(refuse(
                "move_bundle_location",
                "Open the installed GIT STRATA app in /Applications to move the daemon.",
            ));
        }
        Ok(Self::scoped(home, bundle.to_owned(), app_version, adapters))
    }

    fn scoped(home: PathBuf, bundle: PathBuf, app_version: String, adapters: MoveAdapters) -> Self {
        Self {
            adapters,
            home,
            bundle,
            app_version,
            busy: tokio::sync::Mutex::new(()),
        }
    }

    fn program(&self) -> PathBuf {
        self.bundle.join("Contents/MacOS/git-stratad")
    }

    fn plist(&self) -> PathBuf {
        plist_for_home(&self.home)
    }

    fn staged(&self) -> PathBuf {
        self.plist()
            .with_file_name(".com.gwd.git-stratad.move-stage")
    }

    fn backup(&self) -> PathBuf {
        self.home.join(APP_SUPPORT).join("legacy").join(PLIST_NAME)
    }

    /// Keep the exact replacement through a crash after rename. The staged
    /// plist is consumed by the rename and cannot serve as the reference.
    fn replacement(&self) -> PathBuf {
        self.home.join(APP_SUPPORT).join("move-replacement.plist")
    }

    fn journal(&self) -> PathBuf {
        self.home.join(APP_SUPPORT).join("update.json")
    }

    fn load(&self) -> Result<Option<MoveRecord>, GuiError> {
        let doc = self.update_doc()?;
        match doc.get("move") {
            None | Some(Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| {
                    refuse(
                        "move_record_unknown",
                        "GIT STRATA could not read the saved Move phase.",
                    )
                })
                .and_then(|record: MoveRecord| {
                    if record.was_enabled && record.old_program.is_absolute() {
                        Ok(Some(record))
                    } else {
                        Err(refuse(
                            "move_record_unknown",
                            "The saved Move has an invalid LaunchAgent.",
                        ))
                    }
                }),
        }
    }

    fn update_doc(&self) -> Result<serde_json::Map<String, Value>, GuiError> {
        let path = self.journal();
        let doc = super::cache::read_document(&path).map_err(|e| file_error(&path, e))?;
        Self::require_no_update(&doc)?;
        Ok(doc)
    }

    fn require_no_update(doc: &serde_json::Map<String, Value>) -> Result<(), GuiError> {
        if doc.get("txn").is_some_and(|v| !v.is_null()) {
            return Err(move_error(
                Code::Busy,
                "update_in_progress",
                "Finish the pending app update before moving the daemon.",
            ));
        }
        Ok(())
    }

    fn save(&self, record: Option<&MoveRecord>) -> Result<(), GuiError> {
        let path = self.journal();
        super::txn::TxnStore::new(path.clone()).edit_document(|doc| {
            Self::require_no_update(doc)?;
            match record {
                Some(record) => {
                    doc.insert(
                        "move".into(),
                        serde_json::to_value(record).map_err(|e| file_error(&path, e))?,
                    );
                }
                None => {
                    doc.remove("move");
                }
            }
            Ok(())
        })
    }

    fn clear(&self) -> Result<(), GuiError> {
        self.save(None)?;
        for path in [self.staged(), self.replacement()] {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    fn phase(&self, record: &mut MoveRecord, phase: Phase) -> Result<(), GuiError> {
        let old = record.phase;
        record.phase = phase;
        self.save(Some(record)).inspect_err(|_| record.phase = old)
    }

    async fn service<T: Send + 'static>(
        &self,
        op: impl FnOnce(&dyn ServiceManager) -> Result<T, GuiError> + Send + 'static,
    ) -> Result<T, GuiError> {
        let service = self.adapters.service.clone();
        tokio::task::spawn_blocking(move || op(service.as_ref()))
            .await
            .map_err(|e| GuiError::internal(format!("LaunchAgent operation failed: {e}")))?
    }

    async fn job(&self) -> Result<Option<Job>, GuiError> {
        let plist = self.plist();
        self.service(move |s| {
            s.print(AGENT_LABEL)?
                .map(|dump| parse_job(&dump, &plist))
                .transpose()
        })
        .await
    }

    async fn disabled(&self) -> Result<bool, GuiError> {
        self.service(|s| s.print_disabled(AGENT_LABEL)).await
    }

    async fn change_service(
        &self,
        record: &mut MoveRecord,
        attempt: ServiceAttempt,
        expected: Option<Vec<u8>>,
    ) -> Result<(), GuiError> {
        if record.service_attempt.is_some() {
            return Err(self.incomplete(record, None));
        }
        record.service_attempt = Some(attempt);
        self.save(Some(record))
            .inspect_err(|_| record.service_attempt = None)?;
        let plist = self.plist();
        let result = self
            .service(move |s| {
                if let Some(expected) = expected
                    && read_optional(&plist)?.as_deref() != Some(expected.as_slice())
                {
                    return Err(plist_changed());
                }
                match attempt {
                    ServiceAttempt::Disable => s.disable(AGENT_LABEL),
                    ServiceAttempt::Enable => s.enable(AGENT_LABEL),
                    ServiceAttempt::Bootstrap => s.bootstrap(&plist),
                }
            })
            .await;
        if result.is_ok()
            || result.as_ref().is_err_and(|e| {
                matches!(e.reason(), Some("move_plist_changed" | "move_file"))
                    || exited(e, attempt.verb())
            })
        {
            record.service_attempt = None;
            let phase = match (attempt, record.phase) {
                (ServiceAttempt::Bootstrap, Phase::Start) => Phase::Verify,
                (ServiceAttempt::Bootstrap, Phase::StartLegacy) => Phase::VerifyLegacy,
                _ => record.phase,
            };
            if let Err(e) = self.phase(record, phase) {
                record.service_attempt = Some(attempt);
                return Err(e);
            }
        }
        result
    }

    async fn disable(&self, record: &mut MoveRecord) -> Result<(), GuiError> {
        self.change_service(record, ServiceAttempt::Disable, None)
            .await
    }

    async fn restore_enabled(
        &self,
        record: &mut MoveRecord,
        target: Target,
    ) -> Result<(), GuiError> {
        let expected = self.installed_plist(record, target)?;
        self.change_service(record, ServiceAttempt::Enable, Some(expected))
            .await
    }

    fn installed_plist(&self, record: &MoveRecord, target: Target) -> Result<Vec<u8>, GuiError> {
        let reference = match target {
            Target::Bundle => self.replacement(),
            Target::Legacy => self.backup(),
        };
        let installed = read_optional(&self.plist())?.ok_or_else(plist_changed)?;
        if read_optional(&reference)?.as_deref() != Some(installed.as_slice()) {
            return Err(plist_changed());
        }
        self.check_plist_program(record, target, &installed)?;
        Ok(installed)
    }

    fn check_plist_program(
        &self,
        record: &MoveRecord,
        target: Target,
        bytes: &[u8],
    ) -> Result<(), GuiError> {
        let plist = self.adapters.platform.decode_plist(bytes)?;
        match target {
            Target::Bundle => validate_template(&plist, &self.program()),
            Target::Legacy
                if plist.get("Label") == Some(&PlistValue::String(AGENT_LABEL.into()))
                    && effective_program(&plist)
                        .is_some_and(|program| same_program(program, &record.old_program)) =>
            {
                Ok(())
            }
            Target::Legacy => Err(refuse(
                "move_legacy_program",
                "The saved LaunchAgent would start a different program.",
            )),
        }
    }

    fn stopped(&self) -> Result<bool, GuiError> {
        self.adapters.platform.socket_refusing_and_lock_free()
    }

    pub fn has_pending_move(&self) -> Result<bool, GuiError> {
        Ok(self.load()?.is_some())
    }

    fn lock_move(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, GuiError> {
        self.busy.try_lock().map_err(|_| {
            move_error(
                Code::Busy,
                "move_in_progress",
                "The daemon move is already in progress.",
            )
        })
    }

    /// Cancel a saved Move before any bootout intent has been recorded.
    pub async fn cancel_pending(&self, progress: &mut Progress<'_>) -> Result<(), GuiError> {
        let _guard = self.lock_move()?;
        match self.load()? {
            Some(record) if record.service_attempt.is_some() => Err(self.incomplete(&record, None)),
            Some(mut record) if matches!(record.phase, Phase::Drain | Phase::SealedHold) => {
                self.cancel_pre_bootout(&mut record, progress).await
            }
            Some(record) if !matches!(record.phase, Phase::Render | Phase::Backup) => Err(refuse(
                "move_cancel_blocked",
                "Finish checking the saved Move before cancelling.",
            )),
            _ => {
                self.clear()?;
                progress(UpdateState::Cancelled);
                Ok(())
            }
        }
    }

    async fn cancel_pre_bootout(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        record.cancelled = true;
        self.save(Some(record))?;
        let cancelled = move_error(Code::Unknown, "move_cancelled", "The move was cancelled.");
        self.before_swap_failure(record, cancelled, progress).await
    }

    /// Read-only preflight for an explicit carry-over decision from the host.
    /// Values stay in Rust. The UI can display the affected names.
    pub async fn review(
        &self,
        acknowledgement: Option<u32>,
    ) -> Result<Option<PlistReview>, GuiError> {
        Ok(self.prepare(acknowledgement).await?.review)
    }

    /// Start or resume the saved phases. A reviewed plist is an explicit
    /// decision to carry its allowed edits and use the new managed keys.
    pub async fn run(
        &self,
        acknowledgement: Option<u32>,
        approved: Option<&PlistReview>,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        self.run_cancellable(acknowledgement, approved, &|| false, progress)
            .await
    }

    pub async fn run_cancellable(
        &self,
        acknowledgement: Option<u32>,
        approved: Option<&PlistReview>,
        cancel_requested: &(dyn Fn() -> bool + Sync),
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let _guard = self.lock_move()?;
        let record = match self.load()? {
            Some(record) if !matches!(record.phase, Phase::Render | Phase::Backup) => {
                match record.phase {
                    Phase::Drain | Phase::SealedHold if record.service_attempt.is_none() => {
                        progress(UpdateState::Reconciling);
                    }
                    Phase::Bootout | Phase::RollbackHold if record.bootout.is_some() => {
                        progress(UpdateState::Stopping);
                    }
                    _ => {}
                }
                record
            }
            _ if cancel_requested() => {
                self.clear()?;
                progress(UpdateState::Cancelled);
                return Ok(());
            }
            _ => self.stage_move(acknowledgement, approved).await?,
        };
        self.drive(record, cancel_requested, progress).await
    }

    async fn drive(
        &self,
        mut record: MoveRecord,
        cancel_requested: &(dyn Fn() -> bool + Sync),
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        if record.service_attempt.is_some() {
            return Err(self.incomplete(&record, None));
        }
        match record.phase {
            Phase::Incomplete => return self.verify_repair(&record, progress).await,
            Phase::RollbackHold => return self.rollback(&mut record, progress).await,
            Phase::RestoreMissing => {
                return self.finish_missing_before_swap(&mut record, progress).await;
            }
            Phase::RestoreEnabledAfterRollback => {
                return self.restore_enabled_after_rollback(&mut record).await;
            }
            Phase::RestoreLegacy | Phase::StartLegacy | Phase::VerifyLegacy => {
                return self.finish_legacy(&mut record, progress).await;
            }
            _ => {}
        }

        if matches!(record.phase, Phase::Drain | Phase::SealedHold) {
            if record.cancelled || cancel_requested() {
                return self.cancel_pre_bootout(&mut record, progress).await;
            }
            let result = if record.program_missing {
                self.fence_missing(&mut record, cancel_requested, progress)
                    .await
            } else {
                self.fence_serving(&mut record, false, cancel_requested, progress)
                    .await
            };
            if let Err(e) = result {
                if record.service_attempt.is_some() || record.bootout.is_some() {
                    progress(UpdateState::FenceUncertain);
                    return Err(self.incomplete(&record, Some(&e.message)));
                }
                if e.reason() == Some("move_cancelled") {
                    return self.cancel_pre_bootout(&mut record, progress).await;
                }
                return self.before_swap_failure(&mut record, e, progress).await;
            }
        }

        if record.phase == Phase::Bootout {
            let outcome = self.wait_bootout(&record, &record.old_program).await;
            match outcome {
                Bootout::Done => {
                    record.bootout = None;
                    record.hold = None;
                    self.phase(&mut record, Phase::Swap)
                        .map_err(|e| self.incomplete(&record, Some(&e.message)))?;
                }
                Bootout::Failed => {
                    record.bootout = None;
                    let e = move_error(
                        Code::Unknown,
                        "bootout_failed",
                        "Launchd could not unload the old agent.",
                    );
                    return self.before_swap_failure(&mut record, e, progress).await;
                }
                Bootout::Uncertain => {
                    progress(UpdateState::FenceUncertain);
                    return Err(self.incomplete(
                        &record,
                        Some("Launchd did not confirm that the old daemon stopped."),
                    ));
                }
            }
        }

        match self.install_and_verify(&mut record, progress).await {
            Ok(()) => {
                self.clear()?;
                progress(UpdateState::Done);
                Ok(())
            }
            Err(e) if record.service_attempt.is_some() => {
                progress(UpdateState::FenceUncertain);
                Err(self.incomplete(&record, Some(&e.message)))
            }
            Err(e) => {
                if matches!(
                    e.reason(),
                    Some("move_schema_too_old" | "move_schema_ack_required")
                ) {
                    record.schema_block = Some(e.clone());
                }
                record.failure = Some(e.message);
                self.rollback(&mut record, progress).await
            }
        }
    }

    /// The main window offers this only after showing the recovery steps. An
    /// ambiguous child exit needs the user's explicit confirmation first.
    pub async fn check_repair(&self, progress: &mut Progress<'_>) -> Result<(), GuiError> {
        let _guard = self.lock_move()?;
        let mut record = self
            .load()?
            .ok_or_else(|| refuse("move_no_repair", "There is no saved daemon move to check."))?;
        let observed = self
            .job()
            .await
            .map_err(|e| self.incomplete(&record, Some(&e.message)))?;
        match (record.phase, record.service_attempt) {
            (Phase::Drain | Phase::SealedHold, Some(ServiceAttempt::Disable)) => {
                record.service_attempt = None;
                self.save(Some(&record))?;
                let failure = refuse(
                    "move_fence_interrupted",
                    "The launchd fence was interrupted.",
                );
                self.before_swap_failure(&mut record, failure, progress)
                    .await
            }
            (Phase::RollbackHold, Some(ServiceAttempt::Disable)) if record.bootout.is_none() => {
                record.service_attempt = None;
                self.leave_incomplete(&mut record, Some("The rollback fence was interrupted."))
                    .await
            }
            (
                Phase::Start | Phase::StartLegacy,
                Some(ServiceAttempt::Bootstrap | ServiceAttempt::Enable),
            ) => {
                let target = if record.phase == Phase::Start {
                    Target::Bundle
                } else {
                    Target::Legacy
                };
                self.check_start_job(&record, target, observed.as_ref())
                    .and_then(|_| self.installed_plist(&record, target))
                    .map_err(|e| self.incomplete(&record, Some(&e.message)))?;
                // Re-enter the normal start boundary. A loaded job only
                // needs its enabled state reconciled, then verification.
                record.service_attempt = None;
                self.save(Some(&record))?;
                self.drive(record, &|| false, progress).await
            }
            (Phase::RollbackHold, Some(ServiceAttempt::Enable)) => {
                record.service_attempt = None;
                self.begin_rollback_retry(&mut record)?;
                self.drive(record, &|| false, progress).await
            }
            (
                Phase::RestoreMissing | Phase::RestoreEnabledAfterRollback,
                Some(ServiceAttempt::Enable),
            ) => {
                record.service_attempt = None;
                self.save(Some(&record))?;
                self.drive(record, &|| false, progress).await
            }
            (Phase::Bootout | Phase::RollbackHold, None) if record.bootout.is_some() => {
                progress(UpdateState::Stopping);
                if let Some(job) = observed
                    && record
                        .bootout
                        .as_ref()
                        .is_some_and(|attempt| !(attempt.child_failed && attempt.timeout_reported))
                {
                    self.retry_bootout(&mut record, &job)
                        .await
                        .map_err(|e| self.incomplete(&record, Some(&e.message)))?;
                }
                self.drive(record, &|| false, progress).await
            }
            _ => self.verify_repair(&record, progress).await,
        }
    }

    async fn stage_move(
        &self,
        acknowledgement: Option<u32>,
        approved: Option<&PlistReview>,
    ) -> Result<MoveRecord, GuiError> {
        let Prepared {
            mut record,
            original,
            review,
            rendered,
        } = self.prepare(acknowledgement).await?;
        let rendered = match review {
            Some(review) if review.has_changes() => {
                if approved != Some(&review) {
                    return Err(review.error(&self.plist(), &self.program(), &record.old_program));
                }
                self.adapters.platform.encode_plist(&review.replacement())?
            }
            _ => rendered,
        };
        self.save(Some(&record))?;
        atomic_write(&self.staged(), &rendered)?;
        let platform = self.adapters.platform.clone();
        let staged = self.staged();
        tokio::task::spawn_blocking(move || platform.lint_plist(&staged))
            .await
            .map_err(|e| GuiError::internal(format!("Plist lint failed: {e}")))??;
        self.phase(&mut record, Phase::Backup)?;
        if read_optional(&self.plist())?.as_deref() != Some(&original) {
            return Err(plist_changed());
        }
        atomic_write(&self.backup(), &original)?;
        atomic_write(&self.replacement(), &rendered)?;
        self.phase(&mut record, Phase::Drain)?;
        Ok(record)
    }

    async fn prepare(&self, acknowledgement: Option<u32>) -> Result<Prepared, GuiError> {
        let job = self.job().await?.ok_or_else(|| {
            refuse(
                "move_no_agent",
                "Load the existing LaunchAgent before moving the daemon.",
            )
        })?;
        if self.disabled().await? {
            return Err(refuse(
                "agent_disabled",
                "The daemon's launch agent is disabled.",
            ));
        }

        let platform = &self.adapters.platform;
        let program_missing = job.pid.is_none() && platform.program_missing(&job.program)?;
        let predecessor = if program_missing {
            None
        } else {
            if !formula_program(&job.program) {
                return Err(refuse(
                    "move_not_formula",
                    "The loaded daemon is outside the Homebrew formula.",
                ));
            }
            let (daemon, status) = self.serving(&job).await?;
            if !status["draining"].is_null() {
                return Err(drain_busy());
            }
            Some(daemon)
        };
        let info = self.bundle_info().await?;
        if let Some(old) = &predecessor {
            self.check_version(old)?;
        } else {
            self.check_schema(info.store_schema, acknowledgement)?;
        }

        let original =
            read_optional(&self.plist())?.ok_or_else(|| GuiError::plist_missing(&self.plist()))?;
        let platform = self.adapters.platform.clone();
        let program = self.program();
        let old_program = job.program.clone();
        let original_for_review = original.clone();
        let (review, rendered) = tokio::task::spawn_blocking(move || {
            let rendered = platform.render_plist(&program)?;
            let template = platform.decode_plist(&rendered)?;
            validate_template(&template, &program)?;
            let review = if program_missing {
                None
            } else {
                let baseline = platform.decode_plist(&platform.render_plist(&old_program)?)?;
                validate_template(&baseline, &old_program)?;
                Some(PlistReview::new(
                    platform.decode_plist(&original_for_review)?,
                    baseline,
                    template,
                ))
            };
            Ok::<_, GuiError>((review, rendered))
        })
        .await
        .map_err(|e| GuiError::internal(format!("LaunchAgent rendering failed: {e}")))??;
        Ok(Prepared {
            record: MoveRecord {
                phase: Phase::Render,
                old_program: job.program,
                was_enabled: true,
                program_missing,
                bundle_version: info.version,
                target_schema: info.store_schema,
                schema_acknowledgement: acknowledgement,
                exit_timeout_s: job.exit_timeout.as_secs(),
                predecessor,
                hold: None,
                bootout: None,
                cancelled: false,
                rollback_retried: false,
                retry_enable_pending: false,
                failure: None,
                service_attempt: None,
                schema_block: None,
                retry_after_restore: false,
            },
            original,
            review,
            rendered,
        })
    }

    async fn bundle_info(&self) -> Result<BundleInfo, GuiError> {
        let bundle = self.bundle.clone();
        let bundle_ops = self.adapters.bundle.clone();
        let platform = self.adapters.platform.clone();
        let (info, version) = tokio::task::spawn_blocking(move || {
            bundle_ops.codesign_verify(&bundle)?;
            Ok::<_, GuiError>((
                bundle_ops.read_info_plist(&bundle)?,
                platform.bundle_version()?,
            ))
        })
        .await
        .map_err(|e| GuiError::internal(format!("Bundle inspection failed: {e}")))??;
        if info.version != self.app_version || version != self.app_version {
            return Err(refuse(
                "move_bundle_version",
                "The installed bundle and its daemon must match the running app.",
            ));
        }
        Ok(info)
    }

    fn check_version(&self, daemon: &DaemonIdentity) -> Result<(), GuiError> {
        if daemon.version == self.app_version {
            return Ok(());
        }
        Err(move_error(
            Code::Unsupported,
            "move_version_mismatch",
            format!(
                "This app is {} and the running daemon is {}. Install a matching GIT STRATA app before moving the daemon.",
                self.app_version, daemon.version
            ),
        ))
    }

    fn check_schema(&self, target: u32, acknowledgement: Option<u32>) -> Result<(), GuiError> {
        let current = self.adapters.platform.store_schema();
        if current.is_some_and(|v| v > target) {
            return Err(refuse(
                "move_schema_too_old",
                "The bundled daemon cannot open this newer store. Install a newer GIT STRATA app.",
            ));
        }
        if current != Some(target) && acknowledgement != Some(target) {
            return Err(GuiError::new(
                Code::NotConfirmed,
                "Confirm the store upgrade before moving the daemon.",
                json!({"reason": "move_schema_ack_required", "from_schema": current, "to_schema": target}),
            ));
        }
        Ok(())
    }

    async fn serving(&self, job: &Job) -> Result<(DaemonIdentity, Value), GuiError> {
        let status = self.adapters.daemon.status().await?;
        require_drain(&status)?;
        let daemon = status_identity(&status)?;
        if job.pid != Some(daemon.pid) {
            return Err(identity_changed());
        }
        Ok((daemon, status))
    }

    async fn job_for(&self, program: &Path) -> Result<Job, GuiError> {
        match self.job().await? {
            Some(job) if same_program(&job.program, program) => Ok(job),
            _ => Err(identity_changed()),
        }
    }

    async fn sealed_job(&self, program: &Path, hold: &Hold) -> Result<Job, GuiError> {
        let job = self.job_for(program).await?;
        let (daemon, status) = self.serving(&job).await.map_err(|_| identity_changed())?;
        if owns_lease(hold, &daemon, &status)
            && status["draining"]["sealed"] == true
            && status["draining"]["then"] == "hold"
        {
            Ok(job)
        } else {
            Err(identity_changed())
        }
    }

    async fn drain_to_hold(
        &self,
        record: &mut MoveRecord,
        program: &Path,
        rollback: bool,
        cancel_requested: &(dyn Fn() -> bool + Sync),
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        check_cancel(cancel_requested)?;
        let job = self.job_for(program).await?;
        let (daemon, status) = self.serving(&job).await?;
        if !rollback {
            self.check_version(&daemon)?;
            record.predecessor = Some(daemon.clone());
        }
        if let Some(hold) = &record.hold
            && owns_lease(hold, &daemon, &status)
        {
            if status["draining"]["sealed"] == true && status["draining"]["then"] == "hold" {
                if !rollback {
                    self.phase(record, Phase::SealedHold)?;
                }
                return Ok(());
            }
        } else {
            if rollback && record.hold.is_some() {
                return Err(identity_changed());
            }
            if !status["draining"].is_null() {
                return Err(drain_busy());
            }
            record.hold = None;
            let reply = self.adapters.daemon.acquire(&daemon).await?;
            let lease_id = lease_id(&reply)?.to_string();
            record.hold = Some(Hold { daemon, lease_id });
            // In rollback_hold, this replaces intent with the new daemon's
            // identity before seal. Resume may only reuse that exact lease.
            self.save(Some(record))?;
        }

        let daemon = &self.adapters.daemon;
        let clock = &self.adapters.clock;
        let mut renewed = clock.monotonic();
        let mut last_progress = None;
        loop {
            check_cancel(cancel_requested)?;
            let job = self.job_for(program).await?;
            let (identity, status) = self.serving(&job).await.map_err(|_| identity_changed())?;
            let hold = record.hold.as_ref().expect("drain lease recorded");
            if !owns_lease(hold, &identity, &status) || status["draining"]["sealed"] != false {
                return Err(identity_changed());
            }
            let inflight = status["inflight"]
                .as_array()
                .ok_or_else(|| {
                    refuse(
                        "move_drain_status",
                        "The daemon omitted its in-flight writes.",
                    )
                })?
                .clone();
            let background: Vec<_> = status["holds"]
                .as_array()
                .ok_or_else(|| {
                    refuse(
                        "move_drain_status",
                        "The daemon omitted its background holders.",
                    )
                })?
                .iter()
                .filter(|h| h["origin"] == "background")
                .cloned()
                .collect();
            if inflight.is_empty() && background.is_empty() {
                check_cancel(cancel_requested)?;
                let seal = json!({ "seal": true, "lease_id": hold.lease_id, "then": "hold" });
                // The journal must survive even if the seal RPC succeeds and
                // the app dies before seeing the reply.
                if !rollback {
                    self.phase(record, Phase::SealedHold)?;
                }
                match daemon.drain(seal).await {
                    Ok(_) => return Ok(()),
                    Err(e) if e.reason() == Some("not_idle") => {
                        if !rollback {
                            self.phase(record, Phase::Drain)?;
                        }
                    }
                    Err(e) if e.reason() == Some("lease_lost") => return Err(identity_changed()),
                    Err(e) => return Err(e),
                }
            } else {
                let update = UpdateState::Draining {
                    inflight,
                    background,
                };
                if last_progress.as_ref() != Some(&update) {
                    progress(update.clone());
                    last_progress = Some(update);
                }
            }
            if clock.monotonic().saturating_duration_since(renewed) >= RENEW {
                check_cancel(cancel_requested)?;
                let hold = record.hold.as_ref().expect("drain lease recorded");
                let reply = match daemon
                    .drain(json!({ "on": true, "ttl_s": TTL_S, "lease_id": hold.lease_id }))
                    .await
                {
                    Err(e) if e.reason() == Some("lease_lost") => return Err(identity_changed()),
                    result => result?,
                };
                if lease_id(&reply)? != hold.lease_id {
                    return Err(identity_changed());
                }
                renewed = clock.monotonic();
            }
            clock.sleep(POLL).await;
        }
    }

    async fn fence_serving(
        &self,
        record: &mut MoveRecord,
        rollback: bool,
        cancel_requested: &(dyn Fn() -> bool + Sync),
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let program = if rollback {
            self.program()
        } else {
            record.old_program.clone()
        };
        if record.phase != Phase::SealedHold {
            self.drain_to_hold(record, &program, rollback, cancel_requested, progress)
                .await?;
        }
        let hold = record.hold.as_ref().ok_or_else(identity_changed)?;
        // The seal is an in-memory promise by this exact daemon instance.
        self.sealed_job(&program, hold).await?;
        check_cancel(cancel_requested)?;
        self.disable(record).await?;
        self.confirm_disabled().await?;
        let hold = record.hold.as_ref().ok_or_else(identity_changed)?;
        let job = self.sealed_job(&program, hold).await?;
        check_cancel(cancel_requested)?;
        progress(UpdateState::Stopping);
        self.begin_bootout(record, &job, rollback).await
    }

    async fn still_no_pid(&self, program: &Path) -> Result<Job, GuiError> {
        let first = self.job_for(program).await?;
        if first.pid.is_some() {
            return Err(identity_changed());
        }
        self.adapters.clock.sleep(STEADY).await;
        let second = self.job_for(program).await?;
        if second.pid.is_some() {
            return Err(identity_changed());
        }
        if !self.stopped()? {
            return Err(refuse(
                "move_daemon_not_stopped",
                "The daemon socket or instance lock is still in use.",
            ));
        }
        Ok(second)
    }

    async fn confirm_disabled(&self) -> Result<(), GuiError> {
        if self.disabled().await? {
            Ok(())
        } else {
            Err(refuse(
                "move_fence_lost",
                "The LaunchAgent label is enabled again.",
            ))
        }
    }

    async fn missing_program_stopped(&self, program: &Path) -> Result<Job, GuiError> {
        let job = self.still_no_pid(program).await?;
        if self.adapters.platform.program_missing(program)? {
            Ok(job)
        } else {
            Err(identity_changed())
        }
    }

    async fn fence_missing(
        &self,
        record: &mut MoveRecord,
        cancel_requested: &(dyn Fn() -> bool + Sync),
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        self.missing_program_stopped(&record.old_program).await?;
        check_cancel(cancel_requested)?;
        self.disable(record).await?;
        self.confirm_disabled().await?;
        let job = self.missing_program_stopped(&record.old_program).await?;
        self.confirm_disabled().await?;
        check_cancel(cancel_requested)?;
        progress(UpdateState::Stopping);
        self.begin_bootout(record, &job, false).await
    }

    // Check repair is explicit confirmation that no earlier bootout child is
    // still running. Recheck the same fence before submitting a fresh one.
    async fn retry_bootout(&self, record: &mut MoveRecord, observed: &Job) -> Result<(), GuiError> {
        let rollback = record.phase == Phase::RollbackHold;
        let program = if rollback {
            self.program()
        } else {
            record.old_program.clone()
        };
        let attempt = record.bootout.as_ref().ok_or_else(identity_changed)?;
        if attempt.pid != observed.pid || !same_program(&observed.program, &program) {
            return Err(identity_changed());
        }
        self.confirm_disabled().await?;
        let job = match attempt.pid {
            Some(pid) => {
                let hold = record
                    .hold
                    .as_ref()
                    .filter(|hold| hold.daemon.pid == pid)
                    .ok_or_else(identity_changed)?;
                self.sealed_job(&program, hold).await?
            }
            None => {
                let job = if rollback {
                    self.still_no_pid(&program).await?
                } else if record.program_missing {
                    self.missing_program_stopped(&program).await?
                } else {
                    return Err(identity_changed());
                };
                self.confirm_disabled().await?;
                job
            }
        };
        self.begin_bootout(record, &job, rollback).await
    }

    async fn begin_bootout(
        &self,
        record: &mut MoveRecord,
        job: &Job,
        rollback: bool,
    ) -> Result<(), GuiError> {
        let previous_phase = record.phase;
        let exit_timeout_s = job.exit_timeout.as_secs();
        let previous_timeout = std::mem::replace(&mut record.exit_timeout_s, exit_timeout_s);
        let previous_attempt = record.bootout.replace(BootoutAttempt {
            pid: job.pid,
            exit_timeout_s,
            timeout_reported: job.timeout_reported,
            child_failed: false,
        });
        let phase = if rollback {
            Phase::RollbackHold
        } else {
            Phase::Bootout
        };
        if let Err(error) = self.phase(record, phase) {
            record.bootout = previous_attempt;
            record.exit_timeout_s = previous_timeout;
            return Err(error);
        }

        let target = job
            .pid
            .map_or(BootoutTarget::Stopped, BootoutTarget::Sealed);
        let result = self
            .service(move |s| s.bootout_targeted(AGENT_LABEL, target))
            .await;
        if let Err(error) = &result
            && crate::service::pre_dispatch_error(error)
        {
            record.bootout = previous_attempt;
            record.exit_timeout_s = previous_timeout;
            self.phase(record, previous_phase)?;
            return Err(if error.reason() == Some("bootout_target_changed") {
                identity_changed()
            } else {
                error.clone()
            });
        }
        if result.as_ref().is_err_and(|e| exited(e, "bootout")) {
            record
                .bootout
                .as_mut()
                .expect("bootout recorded")
                .child_failed = true;
            if self.save(Some(record)).is_err() {
                record
                    .bootout
                    .as_mut()
                    .expect("bootout recorded")
                    .child_failed = false;
            }
        }
        Ok(())
    }

    async fn wait_bootout(&self, record: &MoveRecord, program: &Path) -> Bootout {
        let Some(attempt) = &record.bootout else {
            return Bootout::Uncertain;
        };
        let clock = &self.adapters.clock;
        let started = clock.monotonic();
        let timeout = Duration::from_secs(attempt.exit_timeout_s);
        loop {
            let expired = clock.monotonic().saturating_duration_since(started) >= timeout;
            let observed = self.job().await;
            if matches!(observed, Ok(None)) && matches!(self.stopped(), Ok(true)) {
                return Bootout::Done;
            }
            if expired {
                if attempt.child_failed
                    && attempt.timeout_reported
                    && same_loaded_job(&observed, program, attempt.pid)
                {
                    clock.sleep(STEADY).await;
                    let observed = self.job().await;
                    if matches!(observed, Ok(None)) && matches!(self.stopped(), Ok(true)) {
                        return Bootout::Done;
                    }
                    if same_loaded_job(&observed, program, attempt.pid) {
                        return Bootout::Failed;
                    }
                }
                return Bootout::Uncertain;
            }
            let remaining =
                timeout.saturating_sub(clock.monotonic().saturating_duration_since(started));
            clock.sleep(remaining.min(POLL)).await;
        }
    }

    async fn release_owned_hold(&self, record: &MoveRecord, job: &Job) -> Result<(), GuiError> {
        let Some(hold) = &record.hold else {
            return Ok(());
        };
        if job.pid != Some(hold.daemon.pid) {
            return Ok(());
        }
        let (identity, status) = self.serving(job).await?;
        if owns_lease(hold, &identity, &status) {
            if status["draining"]["sealed"] == true && status["draining"]["then"] != "hold" {
                return Err(identity_changed());
            }
            self.adapters
                .daemon
                .drain(json!({ "on": false, "lease_id": hold.lease_id }))
                .await?;
        }
        Ok(())
    }

    async fn before_swap_failure(
        &self,
        record: &mut MoveRecord,
        cause: GuiError,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let observed = self.job().await;
        record.failure = Some(cause.message.clone());
        if record.program_missing {
            observed.map_err(|e| self.incomplete(record, Some(&e.message)))?;
            self.phase(record, Phase::RestoreMissing)?;
            return self.finish_missing_before_swap(record, progress).await;
        }

        let loaded = match observed {
            Ok(Some(job)) if same_program(&job.program, &record.old_program) => Some(job),
            Ok(None) => None,
            _ => return Err(self.incomplete(record, None)),
        };
        let changed = cause.reason() == Some("move_identity_changed");
        if let Some(job) = &loaded {
            self.release_owned_hold(record, job)
                .await
                .map_err(|e| self.incomplete(record, Some(&e.message)))?;
        } else if !matches!(self.stopped(), Ok(true))
            || self
                .adapters
                .platform
                .program_missing(&record.old_program)?
        {
            return Err(self.incomplete(record, None));
        }

        record.bootout = None;
        record.retry_after_restore = changed && !record.cancelled;
        record.hold = None;
        self.phase(record, Phase::StartLegacy)?;
        self.finish_legacy(record, progress).await
    }

    async fn finish_missing_before_swap(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let job = self
            .job()
            .await
            .map_err(|e| self.incomplete(record, Some(&e.message)))?;
        if job.is_some_and(|job| !same_program(&job.program, &record.old_program)) {
            return Err(self.incomplete(record, Some(&identity_changed().message)));
        }
        self.installed_plist(record, Target::Legacy)
            .map_err(|e| self.incomplete(record, Some(&e.message)))?;
        if self.disabled().await? {
            self.restore_enabled(record, Target::Legacy)
                .await
                .map_err(|e| self.incomplete(record, Some(&e.message)))?;
        }
        let mut failure = self.incomplete(record, None);
        self.clear()?;
        if record.cancelled {
            progress(UpdateState::Cancelled);
            return Ok(());
        }
        failure.detail["retry_move"] = json!(true);
        Err(failure)
    }

    async fn install_and_verify(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let program = self.program();
        if matches!(record.phase, Phase::Swap | Phase::Start) {
            let info = self.bundle_info().await?;
            if info.version != record.bundle_version || info.store_schema != record.target_schema {
                return Err(refuse(
                    "move_bundle_changed",
                    "The app bundle changed after the Move was prepared.",
                ));
            }
            if record.program_missing {
                self.check_schema(info.store_schema, record.schema_acknowledgement)?;
            }
        }
        if record.phase == Phase::Swap {
            self.confirm_disabled().await?;
            if self.job().await?.is_some() || !self.stopped()? {
                return Err(identity_changed());
            }
            progress(UpdateState::Installing);
            let plist = self.plist();
            let staged = self.staged();
            if let Some(bytes) = read_optional(&staged)? {
                if read_optional(&self.replacement())?.as_deref() != Some(bytes.as_slice()) {
                    return Err(plist_changed());
                }
                validate_template(&self.adapters.platform.decode_plist(&bytes)?, &program)?;
                let backup = read_optional(&self.backup())?;
                if backup.is_none() || read_optional(&plist)? != backup {
                    return Err(plist_changed());
                }
                fs::rename(&staged, &plist).map_err(|e| file_error(&plist, e))?;
                sync_parent(&plist)?;
            }
            // rename may already have happened when resuming this phase.
            self.installed_plist(record, Target::Bundle)?;
            self.phase(record, Phase::Start)?;
        }
        if record.phase == Phase::Start {
            self.start_agent(record, Target::Bundle, progress).await?;
        }
        self.verify(
            &program,
            &record.bundle_version,
            record.predecessor.as_ref(),
            progress,
        )
        .await
    }

    async fn start_agent(
        &self,
        record: &mut MoveRecord,
        target: Target,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let observed = self.job().await?;
        let program = self.check_start_job(record, target, observed.as_ref())?;
        if self.disabled().await? {
            self.restore_enabled(record, target).await?;
        }
        let expected = self.installed_plist(record, target)?;
        progress(UpdateState::StartingDaemon);
        match self.job().await? {
            None if self.stopped()? => {
                self.change_service(record, ServiceAttempt::Bootstrap, Some(expected))
                    .await
            }
            Some(job) if same_program(&job.program, &program) => {
                let phase = match target {
                    Target::Bundle => Phase::Verify,
                    Target::Legacy => Phase::VerifyLegacy,
                };
                self.phase(record, phase)
            }
            _ => Err(identity_changed()),
        }
    }

    fn check_start_job(
        &self,
        record: &MoveRecord,
        target: Target,
        job: Option<&Job>,
    ) -> Result<PathBuf, GuiError> {
        let program = match target {
            Target::Bundle => self.program(),
            Target::Legacy => record.old_program.clone(),
        };
        if job.is_some_and(|job| !same_program(&job.program, &program))
            || (job.is_none() && !self.stopped()?)
            || self.adapters.platform.program_missing(&program)?
        {
            return Err(identity_changed());
        }
        Ok(program)
    }

    async fn rollback(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        if record.phase != Phase::RollbackHold {
            record.hold = None;
            record.bootout = None;
            self.phase(record, Phase::RollbackHold)?;
        }
        if record.retry_enable_pending {
            self.finish_rollback_retry_enable(record).await?;
        }
        let first = self.job().await;

        let program = self.program();
        if record.bootout.is_some() {
            return self
                .finish_rollback_bootout(record, &program, progress)
                .await;
        }
        if record.program_missing
            || self
                .adapters
                .platform
                .program_missing(&record.old_program)?
        {
            first.map_err(|e| self.incomplete(record, Some(&e.message)))?;
            return self.leave_incomplete(record, None).await;
        }
        let mut observed = first;

        // A changed identity (including a pid appearing under the no-pid
        // fence) permits one fresh inspection/drain. Persist the retry bit
        // so reopening the app cannot turn it into repeated bootouts.
        loop {
            let result = self.fence_rollback_job(record, observed, progress).await;
            match result {
                Ok(false) => return self.restore_legacy(record, progress).await,
                Ok(true) => {
                    return self
                        .finish_rollback_bootout(record, &program, progress)
                        .await;
                }
                Err(e) if e.reason() == Some("move_identity_changed") => {
                    if record.rollback_retried {
                        return self.leave_incomplete(record, Some(&e.message)).await;
                    }
                    self.begin_rollback_retry(record)?;
                    self.finish_rollback_retry_enable(record).await?;
                    observed = self.job().await;
                }
                Err(e)
                    if record.hold.is_some()
                        && record.service_attempt.is_none()
                        && record.bootout.is_none() =>
                {
                    return self.leave_incomplete(record, Some(&e.message)).await;
                }
                Err(e) => return Err(self.incomplete(record, Some(&e.message))),
            }
        }
    }

    fn begin_rollback_retry(&self, record: &mut MoveRecord) -> Result<(), GuiError> {
        record.rollback_retried = true;
        record.retry_enable_pending = true;
        self.save(Some(record))
    }

    async fn finish_rollback_retry_enable(&self, record: &mut MoveRecord) -> Result<(), GuiError> {
        let enabled = async {
            if self
                .job()
                .await?
                .is_some_and(|job| !same_program(&job.program, &self.program()))
            {
                return Err(identity_changed());
            }
            self.installed_plist(record, Target::Bundle)?;
            self.release_rollback_hold(record).await?;
            record.hold = None;
            if self.disabled().await? {
                self.restore_enabled(record, Target::Bundle).await?;
            }
            if self.disabled().await? {
                return Err(refuse(
                    "agent_disabled",
                    "The LaunchAgent is still disabled.",
                ));
            }
            Ok::<_, GuiError>(())
        }
        .await;
        enabled.map_err(|e| self.incomplete(record, Some(&e.message)))?;
        record.retry_enable_pending = false;
        self.save(Some(record))
    }

    async fn fence_rollback_job(
        &self,
        record: &mut MoveRecord,
        observed: Result<Option<Job>, GuiError>,
        progress: &mut Progress<'_>,
    ) -> Result<bool, GuiError> {
        let program = self.program();
        let observed = self.wait_for_loaded_pid(observed?, &program).await?;
        let Some(job) = observed else {
            self.disable(record).await?;
            self.confirm_unloaded_fence().await?;
            return Ok(false);
        };
        if !same_program(&job.program, &program) {
            return Err(refuse(
                "move_rollback_program",
                "Launchd reports a different program during rollback.",
            ));
        }
        if job.pid.is_some() {
            if record.hold.is_none() && self.disabled().await? {
                return Err(identity_changed());
            }
            self.fence_serving(record, true, &|| false, progress)
                .await?;
        } else {
            self.disable(record).await?;
            self.confirm_disabled().await?;
            let job = self.still_no_pid(&program).await?;
            self.confirm_disabled().await?;
            self.begin_bootout(record, &job, true).await?;
        }
        Ok(true)
    }

    async fn confirm_unloaded_fence(&self) -> Result<(), GuiError> {
        self.confirm_disabled().await?;
        if self.job().await?.is_some() || !self.stopped()? {
            return Err(refuse(
                "move_rollback_fence",
                "The unloaded daemon did not stay stopped under the rollback fence.",
            ));
        }
        Ok(())
    }

    async fn wait_for_loaded_pid(
        &self,
        mut observed: Option<Job>,
        program: &Path,
    ) -> Result<Option<Job>, GuiError> {
        let clock = &self.adapters.clock;
        let started = clock.monotonic();
        let timeout = observed
            .as_ref()
            .map_or(DEFAULT_EXIT_TIMEOUT, |job| job.exit_timeout);
        while let Some(job) = &observed
            && same_program(&job.program, program)
            && job.pid.is_some()
        {
            match self.adapters.daemon.status().await {
                Ok(status) if status_identity(&status).is_ok_and(|id| Some(id.pid) == job.pid) => {
                    break;
                }
                _ => {}
            }
            let remaining =
                timeout.saturating_sub(clock.monotonic().saturating_duration_since(started));
            if remaining.is_zero() {
                return Err(refuse(
                    "move_rollback_not_serving",
                    "The loaded daemon has a pid and did not answer before its exit timeout.",
                ));
            }
            clock.sleep(remaining.min(POLL)).await;
            observed = self.job().await?;
        }
        Ok(observed)
    }

    async fn finish_rollback_bootout(
        &self,
        record: &mut MoveRecord,
        program: &Path,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        match self.wait_bootout(record, program).await {
            Bootout::Done => {
                record.bootout = None;
                record.hold = None;
                self.save(Some(record))?;
                let fence = async {
                    if !self.disabled().await? {
                        self.disable(record).await?;
                    }
                    self.confirm_unloaded_fence().await
                };
                fence
                    .await
                    .map_err(|e| self.incomplete(record, Some(&e.message)))?;
                self.restore_legacy(record, progress).await
            }
            Bootout::Failed => {
                record.bootout = None;
                self.leave_incomplete(
                    record,
                    Some("Launchd did not unload the bundled daemon during rollback."),
                )
                .await
            }
            Bootout::Uncertain => {
                progress(UpdateState::FenceUncertain);
                Err(self.incomplete(
                    record,
                    Some("Launchd did not confirm that the bundled daemon stopped."),
                ))
            }
        }
    }

    async fn restore_legacy(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        if self
            .adapters
            .platform
            .program_missing(&record.old_program)?
        {
            return self.leave_incomplete(record, None).await;
        }
        self.phase(record, Phase::RestoreLegacy)?;
        self.finish_legacy(record, progress).await
    }

    async fn finish_legacy(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let result = self.restore_and_verify(record, progress).await;
        if let Err(e) = result {
            return Err(self.incomplete(record, Some(&e.message)));
        }
        if record.retry_after_restore {
            record.retry_after_restore = false;
            self.phase(record, Phase::Drain)?;
            return Err(identity_changed());
        }
        self.clear()?;
        if record.cancelled {
            progress(UpdateState::Cancelled);
            return Ok(());
        }
        progress(UpdateState::Restored {
            from: self.app_version.clone(),
        });
        Err(move_error(
            Code::Unknown,
            "move_restored",
            format!(
                "{} The formula daemon is running again.",
                record
                    .failure
                    .as_deref()
                    .unwrap_or("The move was interrupted.")
            ),
        ))
    }

    async fn restore_and_verify(
        &self,
        record: &mut MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        if record.phase == Phase::RestoreLegacy {
            self.confirm_disabled().await?;
            if self.job().await?.is_some() || !self.stopped()? {
                return Err(identity_changed());
            }
            let backup = read_optional(&self.backup())?.ok_or_else(|| {
                refuse(
                    "move_backup_missing",
                    "The legacy LaunchAgent backup is missing.",
                )
            })?;
            self.check_plist_program(record, Target::Legacy, &backup)?;
            if read_optional(&self.plist())?.as_deref() != Some(backup.as_slice()) {
                // A failed swap may be unwinding with the backup already
                // installed. Otherwise only overwrite Move's replacement.
                self.installed_plist(record, Target::Bundle)?;
                atomic_write(&self.plist(), &backup)?;
            }
            self.phase(record, Phase::StartLegacy)?;
        }
        if record.phase == Phase::StartLegacy {
            self.start_agent(record, Target::Legacy, progress).await?;
        }
        self.verify(&record.old_program, &record.bundle_version, None, progress)
            .await
    }

    async fn release_rollback_hold(&self, record: &MoveRecord) -> Result<(), GuiError> {
        let release = async {
            if let Some(job) = self.job().await?
                && same_program(&job.program, &self.program())
            {
                self.release_owned_hold(record, &job).await?;
            }
            Ok::<_, GuiError>(())
        };
        release
            .await
            .map_err(|e| self.incomplete(record, Some(&e.message)))
    }

    async fn leave_incomplete(
        &self,
        record: &mut MoveRecord,
        cause: Option<&str>,
    ) -> Result<(), GuiError> {
        if let Some(cause) = cause {
            record.failure = Some(cause.to_string());
        }
        self.phase(record, Phase::RestoreEnabledAfterRollback)?;
        self.restore_enabled_after_rollback(record).await
    }

    async fn restore_enabled_after_rollback(
        &self,
        record: &mut MoveRecord,
    ) -> Result<(), GuiError> {
        self.release_rollback_hold(record).await?;
        record.hold = None;
        let target = self
            .recorded_plist_target()
            .ok_or_else(|| self.incomplete(record, Some(&plist_changed().message)))?;
        let program = match target {
            Target::Bundle => self.program(),
            Target::Legacy => record.old_program.clone(),
        };
        if self
            .job()
            .await
            .map_err(|e| self.incomplete(record, Some(&e.message)))?
            .is_some_and(|job| !same_program(&job.program, &program))
        {
            return Err(self.incomplete(record, Some(&identity_changed().message)));
        }
        self.restore_enabled(record, target)
            .await
            .map_err(|e| self.incomplete(record, Some(&e.message)))?;
        self.phase(record, Phase::Incomplete)?;
        Err(self.incomplete(record, None))
    }

    fn recorded_plist_target(&self) -> Option<Target> {
        let current = read_optional(&self.plist()).ok()??;
        if read_optional(&self.backup()).ok().flatten().as_ref() == Some(&current) {
            Some(Target::Legacy)
        } else if read_optional(&self.replacement()).ok().flatten().as_ref() == Some(&current) {
            Some(Target::Bundle)
        } else {
            None
        }
    }

    async fn verify_repair(
        &self,
        record: &MoveRecord,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let repair = async {
            let job = self.job().await?.ok_or_else(identity_changed)?;
            if self.disabled().await? {
                return Err(refuse(
                    "agent_disabled",
                    "The LaunchAgent is still disabled.",
                ));
            }
            let target = if same_program(&job.program, &self.program()) {
                Target::Bundle
            } else if same_program(&job.program, &record.old_program) {
                Target::Legacy
            } else {
                return Err(identity_changed());
            };
            let installed = read_optional(&self.plist())?.ok_or_else(plist_changed)?;
            self.check_plist_program(record, target, &installed)?;
            let (program, version, predecessor) = match target {
                Target::Bundle => {
                    let info = self.bundle_info().await?;
                    if record.program_missing || record.schema_block.is_some() {
                        self.check_schema(info.store_schema, record.schema_acknowledgement)?;
                    }
                    (
                        self.program(),
                        Some(info.version),
                        record.predecessor.as_ref(),
                    )
                }
                Target::Legacy => {
                    let schema_repair = record.program_missing
                        && record.schema_block.is_some()
                        && formula_program(&record.old_program);
                    (
                        record.old_program.clone(),
                        (!schema_repair).then(|| record.bundle_version.clone()),
                        None,
                    )
                }
            };
            let status = self.adapters.daemon.status().await?;
            let daemon = verify_daemon(
                Some(&job),
                &status,
                &program,
                version.as_deref(),
                predecessor,
            )?;
            if version.is_none() {
                self.verify_formula_schema(&program, &status)?;
            }
            Ok::<_, GuiError>((target, daemon.version))
        };
        let (target, version) = repair
            .await
            .map_err(|e| self.incomplete(record, Some(&e.message)))?;
        self.clear()?;
        progress(match target {
            Target::Bundle => UpdateState::Done,
            Target::Legacy => UpdateState::Restored { from: version },
        });
        Ok(())
    }

    fn verify_formula_schema(&self, program: &Path, status: &Value) -> Result<(), GuiError> {
        let platform = &self.adapters.platform;
        // Live status comes from the verified PID after it opens the store.
        // Check it against the marker when the configured root is known.
        let live_schema = status["store_schema"]
            .as_u64()
            .and_then(|version| u32::try_from(version).ok())
            .filter(|version| *version > 0);
        if live_schema.is_none()
            || status.get("draining") != Some(&Value::Null)
            || platform
                .store_schema()
                .is_some_and(|schema| Some(schema) != live_schema)
            || platform.program_missing(program)?
        {
            return Err(refuse(
                "move_repair_schema",
                "The formula daemon must be available and report a compatible open store and write readiness.",
            ));
        }
        Ok(())
    }

    async fn verify(
        &self,
        program: &Path,
        version: &str,
        predecessor: Option<&DaemonIdentity>,
        progress: &mut Progress<'_>,
    ) -> Result<(), GuiError> {
        let clock = &self.adapters.clock;
        let since = clock.now();
        let mut budget = VERIFY_TIMEOUT;
        let mut last_progress = None;
        loop {
            let start = clock.monotonic();
            let state = match self.job().await {
                Ok(job) => match self.adapters.daemon.status().await {
                    Ok(status) => {
                        if self.disabled().await? {
                            return Err(refuse(
                                "agent_disabled",
                                "The LaunchAgent is still disabled.",
                            ));
                        }
                        verify_daemon(job.as_ref(), &status, program, Some(version), predecessor)?;
                        return Ok(());
                    }
                    Err(e) if tcc_pending(&e) => UpdateState::WaitingPermission,
                    Err(_) => UpdateState::WaitingDaemon { since },
                },
                Err(e) if e.reason() == Some("launchctl_failed") => {
                    UpdateState::WaitingDaemon { since }
                }
                Err(e) => return Err(e),
            };
            let paused = state == UpdateState::WaitingPermission;
            if last_progress.as_ref() != Some(&state) {
                progress(state.clone());
                last_progress = Some(state);
            }
            if budget.is_zero() {
                return Err(refuse(
                    "move_verify_timeout",
                    "The daemon did not answer within 150 seconds.",
                ));
            }
            clock.sleep(budget.min(POLL)).await;
            if !paused {
                budget = budget.saturating_sub(clock.monotonic().saturating_duration_since(start));
            }
        }
    }

    fn incomplete(&self, record: &MoveRecord, cause: Option<&str>) -> GuiError {
        let launch_agent = format!(r#""gui/$(id -u)/{AGENT_LABEL}""#);
        let print = format!("/bin/launchctl print {launch_agent}");
        let plist = shell_word(&self.plist());
        let support = self.home.join("Library/Application Support/git-strata");
        let log = shell_word(&support.join("daemon.err.log"));
        let socket = shell_word(&support.join("daemon.sock"));
        let backup = shell_word(&self.backup());
        let pending = record.service_attempt.is_some() || record.bootout.is_some();
        let mut steps = vec![format!(
            r#"In Terminal run {print} and /bin/launchctl print-disabled "gui/$(id -u)". See {log} if the expected daemon is failing to start."#
        )];
        if pending {
            let verb = record
                .service_attempt
                .map_or("bootout", ServiceAttempt::verb);
            steps.push(format!(
                r#"Keep the plist and enabled state as they are while any earlier launchctl {verb} is running. Use /usr/bin/pgrep -x -u "$(id -u)" launchctl to find remaining launchctl processes, and /bin/ps -p <PID> -o command= to identify their operations."#
            ));
            if record.bootout.is_some() {
                steps.push("After confirming that the earlier bootout exited or was never submitted, choose Check repair. If the job is still loaded, the app rechecks the disabled label and the saved sealed hold or no-process checks before sending bootout.".into());
                steps.push(format!("If the fence or daemon identity has changed, inspect {print} and git-strata daemon status. Keep the label disabled until the earlier unload is resolved."));
                return self.incomplete_error(record, cause, steps);
            }
            if matches!(
                (record.phase, record.service_attempt),
                (
                    Phase::Drain | Phase::SealedHold | Phase::RollbackHold,
                    Some(ServiceAttempt::Disable)
                ) | (
                    Phase::Start | Phase::StartLegacy,
                    Some(ServiceAttempt::Bootstrap | ServiceAttempt::Enable)
                ) | (
                    Phase::RollbackHold
                        | Phase::RestoreMissing
                        | Phase::RestoreEnabledAfterRollback,
                    Some(ServiceAttempt::Enable)
                )
            ) {
                steps.push("After confirming that the earlier command exited or was never submitted, choose Check repair to reconcile the saved service operation.".into());
                return self.incomplete_error(record, cause, steps);
            }
        }

        let Some(installed) = self
            .recorded_plist_target()
            .filter(|&target| self.installed_plist(record, target).is_ok())
        else {
            let replacement = shell_word(&self.replacement());
            steps.push(format!(
                "Preserve your edited LaunchAgent at {plist}. Compare it with the saved original at {backup} and the saved bundle plist at {replacement}. Recheck the label ({AGENT_LABEL}), the effective executable, and whether that daemon supports the current store."
            ));
            return self.incomplete_error(record, cause, steps);
        };
        match installed {
            Target::Bundle => steps.push(format!("Keep the bundled LaunchAgent at {plist}.")),
            Target::Legacy => steps.push(format!("Keep the original LaunchAgent at {plist}.")),
        }
        let old_program = shell_word(&record.old_program);
        if let Some(ref guard) = record.schema_block {
            steps.push(format!(
                "{} Leave the app daemon unloaded until a signed app supports the store and any required store upgrade has been acknowledged. To recover with the formula, use a daemon compatible with the current store at {old_program} and a plist that selects it.",
                guard.message
            ));
            steps.push("After restoring a compatible running daemon, choose Check repair.".into());
        } else {
            if record.program_missing || installed == Target::Legacy {
                steps.push(format!(
                    "If the plist still selects the formula and {old_program} is missing, restore the matching formula executable before starting that plist."
                ));
            }
            steps.push(format!(
                "If the job is loaded with no pid, confirm no pid in two {print} reads five seconds apart, that {socket} refuses connections and that daemon.lock is free. Run /bin/launchctl disable {launch_agent}, repeat the checks, then run /bin/launchctl bootout {launch_agent} and wait for {print} to report the service missing. If there is a pid, leave the job loaded and inspect its status."
            ));
            steps.push(format!(
                r#"With the job confirmed unloaded, verify that the plist uses label {AGENT_LABEL}, selects the intended available executable, and that the daemon supports the current store. After those checks pass, run /usr/bin/plutil -lint {plist}, /bin/launchctl enable {launch_agent}, then /bin/launchctl bootstrap "gui/$(id -u)" {plist}."#
            ));
            steps.push(if record.phase == Phase::RestoreMissing {
                "After restoring the executable or fixing the loaded job, try the Move again.".into()
            } else {
                "Run git-strata daemon status, compare its pid with launchd, then choose Check repair.".into()
            });
        }
        self.incomplete_error(record, cause, steps)
    }

    fn incomplete_error(
        &self,
        record: &MoveRecord,
        cause: Option<&str>,
        steps: Vec<String>,
    ) -> GuiError {
        GuiError::new(
            Code::Unknown,
            "The daemon move is incomplete.",
            json!({
                "reason": "move_incomplete",
                "phase": record.phase,
                "cause": cause.or(record.failure.as_deref()),
                "manual_steps": steps,
                "confirm_service_exit": record.service_attempt.is_some() || record.bootout.is_some(),
                "can_resume": record.service_attempt.is_none()
                    && matches!(record.phase, Phase::Bootout | Phase::RollbackHold),
            }),
        )
    }
}

impl PlistReview {
    fn new(original: Plist, old_template: Plist, new_template: Plist) -> Self {
        let mut carry_over = Vec::new();
        let mut requires_review = Vec::new();
        let keys: BTreeSet<_> = original.keys().chain(old_template.keys()).collect();
        for key in keys {
            let old = original.get(key);
            let baseline = old_template.get(key);
            if old == baseline {
                continue;
            }
            if key == "EnvironmentVariables" {
                match environment_diff(old, baseline) {
                    Some(edits) => carry_over.extend(
                        edits
                            .into_iter()
                            .map(|key| format!("EnvironmentVariables.{key}")),
                    ),
                    None => requires_review.push(key.clone()),
                }
            } else if key == "KeepAlive" && old == Some(&PlistValue::Bool(true)) {
                // The formula renderer originally wrote this release default.
                continue;
            } else if new_template.contains_key(key)
                || baseline.is_some()
                || matches!(key.as_str(), "Program" | "BundleProgram")
            {
                requires_review.push(key.clone());
            } else {
                carry_over.push(key.clone());
            }
        }
        Self {
            original,
            old_template,
            new_template,
            carry_over,
            requires_review,
        }
    }

    fn has_changes(&self) -> bool {
        !self.carry_over.is_empty() || !self.requires_review.is_empty()
    }

    fn error(&self, plist: &Path, renderer: &Path, old_program: &Path) -> GuiError {
        let print_defaults = format!(
            "{} --print-plist {}",
            shell_word(renderer),
            shell_word(old_program)
        );
        GuiError::new(
            Code::NotConfirmed,
            "Review the custom LaunchAgent settings before moving the daemon.",
            json!({
                "reason": "move_plist_decision",
                "plist": plist,
                "carry_over": self.carry_over(),
                "requires_review": self.requires_review(),
                "print_defaults": print_defaults,
            }),
        )
    }

    fn replacement(&self) -> Plist {
        let mut next = self.new_template.clone();
        for (key, value) in &self.original {
            if !next.contains_key(key)
                && !self.old_template.contains_key(key)
                && !matches!(key.as_str(), "Program" | "BundleProgram")
            {
                next.insert(key.clone(), value.clone());
            }
        }
        let old = self.original.get("EnvironmentVariables");
        let baseline = self.old_template.get("EnvironmentVariables");
        if let Some(edits) = environment_diff(old, baseline)
            && let Some(PlistValue::Dictionary(target)) = next.get_mut("EnvironmentVariables")
        {
            for key in edits {
                match old.and_then(as_dictionary).and_then(|env| env.get(&key)) {
                    Some(value) => {
                        target.insert(key, value.clone());
                    }
                    None => {
                        target.remove(&key);
                    }
                }
            }
        }
        next
    }
}

fn as_dictionary(value: &PlistValue) -> Option<&Plist> {
    match value {
        PlistValue::Dictionary(values) => Some(values),
        _ => None,
    }
}

fn environment_diff(
    old: Option<&PlistValue>,
    baseline: Option<&PlistValue>,
) -> Option<Vec<String>> {
    let empty = Plist::new();
    let old = match old {
        Some(value) => as_dictionary(value)?,
        None => &empty,
    };
    let baseline = match baseline {
        Some(value) => as_dictionary(value)?,
        None => &empty,
    };
    let keys: BTreeSet<_> = old.keys().chain(baseline.keys()).collect();
    Some(
        keys.into_iter()
            .filter(|key| old.get(*key) != baseline.get(*key))
            .cloned()
            .collect(),
    )
}

pub(super) fn validate_template(plist: &Plist, program: &Path) -> Result<(), GuiError> {
    let expected = PlistValue::String(program.to_string_lossy().into_owned());
    if plist.get("Label") != Some(&PlistValue::String(AGENT_LABEL.into()))
        || plist.get("ProgramArguments") != Some(&PlistValue::Array(vec![expected]))
        || plist.get("KeepAlive").and_then(as_dictionary)
            != Some(&Plist::from([(
                "SuccessfulExit".into(),
                PlistValue::Bool(false),
            )]))
        || plist.get("RunAtLoad") != Some(&PlistValue::Bool(true))
        || plist
            .get("EnvironmentVariables")
            .and_then(as_dictionary)
            .is_none()
        || plist.contains_key("Program")
        || plist.contains_key("BundleProgram")
    {
        return Err(refuse(
            "move_plist_template",
            "The rendered LaunchAgent does not match the bundled daemon.",
        ));
    }
    Ok(())
}

fn effective_program(plist: &Plist) -> Option<&Path> {
    if plist.contains_key("BundleProgram") {
        return None;
    }
    let selected = match plist.get("Program") {
        Some(program) => program,
        None => match plist.get("ProgramArguments")? {
            PlistValue::Array(args) => args.first()?,
            _ => return None,
        },
    };
    match selected {
        PlistValue::String(program) if Path::new(program).is_absolute() => Some(Path::new(program)),
        _ => None,
    }
}

pub(super) fn formula_program(path: &Path) -> bool {
    if [
        Path::new("/opt/homebrew/opt/git-strata/bin/git-stratad"),
        Path::new("/usr/local/opt/git-strata/bin/git-stratad"),
    ]
    .contains(&path)
    {
        return true;
    }
    [
        "/opt/homebrew/Cellar/git-strata",
        "/usr/local/Cellar/git-strata",
    ]
    .iter()
    .filter_map(|prefix| path.strip_prefix(prefix).ok())
    .any(|tail| {
        let mut parts = tail.components();
        matches!(parts.next(), Some(Component::Normal(_)))
            && parts.as_path() == Path::new("bin/git-stratad")
    })
}

fn parse_job(dump: &str, plist: &Path) -> Result<Job, GuiError> {
    let invalid = || {
        refuse(
            "move_launchd_print",
            "Launchd did not identify the LaunchAgent's plist, program or pid.",
        )
    };
    if scope::loaded_path(dump).is_none() {
        return Err(invalid());
    }
    scope::owned(dump, plist)?;
    let mut program = None;
    let mut pid = None;
    let mut exit_timeout = None;
    let mut depth = 0;
    for line in dump.lines() {
        let line = line.trim();
        if line.starts_with('}') {
            depth -= 1;
        } else {
            if depth == 1
                && let Some((key, value)) = line.split_once(" = ")
            {
                match key {
                    "program" => {
                        let path = PathBuf::from(value);
                        if !path.is_absolute() || program.replace(path).is_some() {
                            return Err(invalid());
                        }
                    }
                    "pid" => {
                        let n = value
                            .parse::<u32>()
                            .ok()
                            .filter(|n| *n != 0)
                            .ok_or_else(invalid)?;
                        if pid.replace(n).is_some() {
                            return Err(invalid());
                        }
                    }
                    "exit timeout" => {
                        let seconds = value
                            .split_ascii_whitespace()
                            .next()
                            .and_then(|s| s.parse::<u64>().ok())
                            .filter(|s| *s > 0)
                            .ok_or_else(invalid)?;
                        if exit_timeout.replace(Duration::from_secs(seconds)).is_some() {
                            return Err(invalid());
                        }
                    }
                    _ => {}
                }
            }
            if line.ends_with('{') {
                depth += 1;
            }
        }
    }
    if depth != 0 {
        return Err(invalid());
    }
    Ok(Job {
        program: program.ok_or_else(invalid)?,
        pid,
        exit_timeout: exit_timeout.unwrap_or(DEFAULT_EXIT_TIMEOUT),
        timeout_reported: exit_timeout.is_some(),
    })
}

fn same_program(observed: &Path, expected: &Path) -> bool {
    observed == expected || scope::anchored(observed) == scope::anchored(expected)
}

fn same_loaded_job(
    observed: &Result<Option<Job>, GuiError>,
    program: &Path,
    pid: Option<u32>,
) -> bool {
    observed.as_ref().is_ok_and(|job| {
        job.as_ref()
            .is_some_and(|job| same_program(&job.program, program) && job.pid == pid)
    })
}

fn status_identity(status: &Value) -> Result<DaemonIdentity, GuiError> {
    serde_json::from_value(status.clone())
        .map_err(|_| identity_changed())
        .and_then(|identity: DaemonIdentity| {
            if identity.pid == 0 || identity.version.is_empty() {
                Err(identity_changed())
            } else {
                Ok(identity)
            }
        })
}

fn verify_daemon(
    job: Option<&Job>,
    status: &Value,
    program: &Path,
    version: Option<&str>,
    predecessor: Option<&DaemonIdentity>,
) -> Result<DaemonIdentity, GuiError> {
    let daemon = status_identity(status)?;
    if let Some(job) = job
        && same_program(&job.program, program)
        && job.pid == Some(daemon.pid)
        && version.is_none_or(|version| daemon.version == version)
        && status["draining"].is_null()
        && predecessor.is_none_or(|old| daemon.pid != old.pid && daemon.started > old.started)
    {
        return Ok(daemon);
    }
    Err(refuse(
        "move_verify_identity",
        "The answering daemon must match the LaunchAgent and expected version and be ready for writes.",
    ))
}

fn require_drain(status: &Value) -> Result<(), GuiError> {
    if status["features"]
        .as_array()
        .is_some_and(|features| features.iter().any(|f| f == "daemon.drain"))
    {
        Ok(())
    } else {
        Err(GuiError::new(
            Code::Unsupported,
            "This daemon does not support a safe move from the app.",
            json!({ "reason": "daemon_too_old", "feature": "daemon.drain" }),
        ))
    }
}

fn drain_busy() -> GuiError {
    move_error(
        Code::Busy,
        "draining",
        "Another operation holds the daemon drain lease.",
    )
}

fn exited(error: &GuiError, verb: &str) -> bool {
    error.reason() == Some("launchctl_failed")
        && error.detail["step"] == verb
        && error.detail["status"]
            .as_i64()
            .is_some_and(|code| code != 0)
}

fn check_cancel(requested: &(dyn Fn() -> bool + Sync)) -> Result<(), GuiError> {
    if requested() {
        Err(refuse("move_cancelled", "The move was cancelled."))
    } else {
        Ok(())
    }
}

fn plist_changed() -> GuiError {
    refuse(
        "move_plist_changed",
        "The installed LaunchAgent no longer matches the plist saved by the move.",
    )
}

fn owns_lease(hold: &Hold, daemon: &DaemonIdentity, status: &Value) -> bool {
    hold.daemon == *daemon && status["draining"]["lease_id"].as_str() == Some(&hold.lease_id)
}

fn lease_id(reply: &Value) -> Result<&str, GuiError> {
    reply["lease_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            refuse(
                "move_lease_missing",
                "The daemon did not return a drain lease.",
            )
        })
}

fn tcc_pending(error: &GuiError) -> bool {
    error.reason() == Some("store_unavailable")
        && matches!(
            error.detail["last_error"]["detail"]["errno"].as_i64(),
            Some(1 | 4 | 13)
        )
}

fn file_error(path: &Path, error: impl std::fmt::Display) -> GuiError {
    move_error(
        Code::Unknown,
        "move_file",
        format!("{}: {error}", path.display()),
    )
}

fn identity_changed() -> GuiError {
    move_error(
        Code::Busy,
        "move_identity_changed",
        "The LaunchAgent and the daemon no longer match. No unverified process was stopped.",
    )
}

fn refuse(reason: &str, message: impl Into<String>) -> GuiError {
    move_error(Code::InvalidInput, reason, message)
}

fn move_error(code: Code, reason: &str, message: impl Into<String>) -> GuiError {
    GuiError::new(code, message, json!({ "reason": reason }))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, GuiError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let mut file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(file_error(path, e)),
    };
    if !file.metadata().map_err(|e| file_error(path, e))?.is_file() {
        return Err(file_error(path, "expected a regular file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| file_error(path, e))?;
    Ok(Some(bytes))
}

fn ensure_dir(path: &Path) -> Result<(), GuiError> {
    match fs::canonicalize(path) {
        Ok(canonical) if canonical == path && path.is_dir() => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| file_error(path, e))?;
            ensure_dir(parent)?;
            fs::create_dir(path).map_err(|e| file_error(path, e))?;
            sync_parent(path)
        }
        _ => Err(file_error(
            path,
            "expected a canonical directory in the login home",
        )),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), GuiError> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().expect("Move paths have a parent");
    ensure_dir(parent)?;
    let name = format!(
        ".git-strata-move-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
    );
    let temp = parent.join(name);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|e| file_error(&temp, e))?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temp, path));
    if let Err(e) = result {
        let _ = fs::remove_file(temp);
        return Err(file_error(path, e));
    }
    sync_parent(path)
}

fn sync_parent(path: &Path) -> Result<(), GuiError> {
    let parent = path.parent().expect("Move paths have a parent");
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| file_error(parent, e))
}

fn shell_word(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;

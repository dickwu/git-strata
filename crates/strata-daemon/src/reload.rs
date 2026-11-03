//! One reload worker and catalog committer; readers never wait for a reload.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::{DaemonWarning, PinnedFlags, Result, catalog::Catalog, confirm_flow, startup::Core};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_types::{Action, Code, Config, RepoName, StrataError};

pub static HUP: AtomicBool = AtomicBool::new(false);
pub const RELOAD_WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReloadReport {
    pub outcome: String,
    pub generation: u64,
    pub repos: Vec<RepoName>,
    pub added: Vec<RepoName>,
    pub changed: Vec<String>,
    pub restart_required: Vec<String>,
    pub pending_relax: Vec<RepoName>,
    pub reasons: Vec<Value>,
}

#[derive(Default)]
struct State {
    requested: u64,
    finished: u64,
    last: Option<ReloadReport>,
    at: Option<SystemTime>,
}

#[derive(Default)]
pub struct ReloadState {
    state: Mutex<State>,
    cv: Condvar,
    #[cfg(feature = "test-seams")]
    pub fail_spawn: AtomicBool,
    #[cfg(feature = "test-seams")]
    pub fail_pins: AtomicBool,
}
impl ReloadState {
    pub fn wake(&self) {
        self.cv.notify_all();
    }
    pub fn status(&self) -> Value {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(report) = &s.last else {
            return Value::Null;
        };
        let mut value = json!(report);
        value["at"] = json!(s.at);
        if let Some(reason) = report
            .reasons
            .first()
            .and_then(|e| e["detail"]["reason"].as_str())
        {
            value["reason"] = json!(reason);
        }
        value
    }
    fn rejected(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last
            .as_ref()
            .is_some_and(|r| r.outcome == "rejected")
    }
}

fn report(c: &Catalog, outcome: &str) -> ReloadReport {
    ReloadReport {
        outcome: outcome.into(),
        generation: c.generation,
        repos: c.config.repos.keys().cloned().collect(),
        added: Vec::new(),
        changed: Vec::new(),
        restart_required: Vec::new(),
        pending_relax: c.pending_relax.clone(),
        reasons: Vec::new(),
    }
}
fn error(reason: &str, message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

/// Each loop is spawned before durable publication and waits without doing I/O.
#[derive(Clone, Default)]
struct Latch(Arc<(Mutex<Option<bool>>, Condvar)>);
impl Latch {
    fn set(&self, start: bool) {
        let (m, cv) = &*self.0;
        *m.lock().unwrap_or_else(|p| p.into_inner()) = Some(start);
        cv.notify_all();
    }
    fn wait(&self) -> bool {
        let (m, cv) = &*self.0;
        let mut s = m.lock().unwrap_or_else(|p| p.into_inner());
        while s.is_none() {
            s = cv.wait(s).unwrap_or_else(|p| p.into_inner());
        }
        s.unwrap_or(false)
    }
}
struct Parked(Vec<Latch>);
impl Drop for Parked {
    fn drop(&mut self) {
        for latch in &self.0 {
            latch.set(false);
        }
    }
}

fn restart_fields(old: &Config, new: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if old.paths != new.paths {
        out.push("paths.root".into());
    }
    if old.status != new.status {
        out.push("status.parallel".into());
    }
    for (n, a) in &old.repos {
        let Some(b) = new.repos.get(n) else {
            out.push(format!("repos.{n} (removed)"));
            continue;
        };
        for (field, changed) in [
            ("path", a.path != b.path),
            ("default_branch", a.default_branch != b.default_branch),
            ("publish.branch", a.publish.branch != b.publish.branch),
            ("deps_dirs", a.deps_dirs != b.deps_dirs),
            ("seed_caches", a.seed_caches != b.seed_caches),
        ] {
            if changed {
                out.push(format!("repos.{n}.{field}"));
            }
        }
    }
    out
}
fn changed_fields(old: &Value, new: &Value, prefix: &str, out: &mut Vec<String>) {
    if old == new {
        return;
    }
    if let (Some(a), Some(b)) = (old.as_object(), new.as_object()) {
        let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
        for key in keys {
            let name = if key == "repo" { "repos" } else { key };
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}.{name}")
            };
            changed_fields(&old[key], &new[key], &path, out);
        }
    } else {
        out.push(prefix.to_owned());
    }
}

impl Core {
    /// The report always belongs to an attempt whose read started after this
    /// request. A simultaneous poll that started earlier cannot satisfy it.
    pub fn request_reload(&self, wait: Duration) -> Result<ReloadReport> {
        let deadline = Instant::now() + wait;
        let mut s = self.reload.state.lock().unwrap_or_else(|p| p.into_inner());
        s.requested += 1;
        let ticket = s.requested;
        self.reload.cv.notify_all();
        loop {
            if self.locks.is_shut_down() {
                return Err(
                    StrataError::new(Code::ReadOnly, "the daemon is shutting down")
                        .with_detail(json!({ "reason": "shutting_down" })),
                );
            }
            if s.finished >= ticket
                && let Some(r) = &s.last
            {
                return Ok(r.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(report(self.catalog.current(), "pending"));
            }
            // The periodic wake also covers an external stop before the accept
            // thread closes Locks. Locks::shutdown explicitly wakes this wait.
            s = self
                .reload
                .cv
                .wait_timeout(s, left.min(Duration::from_millis(50)))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    fn apply_reload(
        &self,
        prepare: &mut dyn FnMut(&RepoName) -> Result<Vec<Latch>>,
    ) -> (ReloadReport, String) {
        let mut attempted_hash = String::new();
        let mut restart = Vec::new();
        let attempted = (|| -> Result<ReloadReport> {
            let bytes = fs::read(&self.opts.config_file)
                .map_err(|e| error("read_failed", e.to_string()))?;
            attempted_hash = source_hash(&bytes)?;
            let text = std::str::from_utf8(&bytes).map_err(|e| error("parse", e.to_string()))?;
            let source = Config::parse(text)?;
            source.validate()?;
            source.dev_ports.blocks()?;
            for (name, cfg) in &source.repos {
                crate::startup::check_realpath(name, &cfg.path)?;
            }
            crate::startup::check_realpath_root(&source.paths.root)?;
            loop {
                let before = self.catalog.current();
                restart = restart_fields(&before.source, &source);
                if !restart.is_empty() {
                    return Err(error(
                        "restart_required",
                        "config contains changes that require an owner restart",
                    ));
                }
                let added: Vec<_> = source
                    .repos
                    .keys()
                    .filter(|n| !before.gits.contains_key(*n))
                    .cloned()
                    .collect();
                let mut gits = before.gits.clone();
                for name in &added {
                    let cfg = &source.repos[name];
                    self.store.check_checkout(&cfg.path)?;
                    let git = (self.open_git)(&cfg.path)?;
                    if gits.values().any(|g| g.repo_id() == git.repo_id()) {
                        return Err(StrataError::new(
                            Code::NameTaken,
                            "two configured names identify the same repository",
                        )
                        .with_detail(json!({ "reason": "repo_id", "repo": name })));
                    }
                    if self.store.repo_has_state(name)?
                        || crate::resolve::push_scenes(&source.paths.root)
                            .iter()
                            .any(|(r, _, _)| r == name)
                        || source
                            .paths
                            .root
                            .join("quarantine")
                            .join(name.as_str())
                            .exists()
                    {
                        restart.push(format!("repos.{name} (stale_rows)"));
                        return Err(error(
                            "stale_rows",
                            "hot-add requires a repository name with no retained state",
                        ));
                    }
                    gits.insert(name.clone(), git);
                }
                let _commit = self.commit.lock().unwrap_or_else(|p| p.into_inner());
                let current = self.catalog.current();
                if current.generation != before.generation {
                    continue;
                }
                if current.hash == attempted_hash {
                    return Ok(report(current, "unchanged"));
                }
                let flags = source
                    .repos
                    .iter()
                    .map(|(n, r)| (n.clone(), confirm_flow::flags_of(r)))
                    .collect();
                let (pins, _) = confirm_flow::reconcile(&current.pins, &flags);
                let next = Catalog::build(
                    source.clone(),
                    gits,
                    pins,
                    current.generation + 1,
                    attempted_hash.clone(),
                )?;
                let mut parked = Parked(Vec::new());
                for name in &added {
                    let git = &next.gits[name];
                    for path in git.clear_stale_strata_locks(self.started) {
                        self.warnings.add(DaemonWarning::StaleRefLock {
                            repo: name.clone(),
                            path,
                        });
                    }
                    strata_publish::pending::reconstruct(strata_publish::pending::Scope {
                        store: &self.store,
                        git,
                        repo: name,
                        branch: &next.config.repos[name].publish.branch,
                    })
                    .map_err(|e| error("recovery_failed", e.message))?;
                    #[cfg(feature = "test-seams")]
                    if self.reload.fail_spawn.load(Ordering::SeqCst) {
                        return Err(error("spawn_failed", "injected loop spawn failure"));
                    }
                    parked.0.extend(prepare(name)?);
                }
                #[cfg(feature = "test-seams")]
                if self.reload.fail_pins.load(Ordering::SeqCst) {
                    return Err(error("pins_write_failed", "injected pins write failure"));
                }
                self.pins.write(&next.pins)?;
                let tightened: Vec<_> = current
                    .pins
                    .iter()
                    .filter(|(n, old)| {
                        next.pins
                            .get(*n)
                            .is_some_and(|new| confirm_flow::needs_fence(old, new))
                    })
                    .map(|(n, _)| n.clone())
                    .collect();
                let mut r = report(&next, "applied");
                r.added = added.clone();
                changed_fields(
                    &serde_json::to_value(&current.source).unwrap(),
                    &serde_json::to_value(&source).unwrap(),
                    "",
                    &mut r.changed,
                );
                for name in added {
                    self.locks.insert(name);
                }
                self.locks.tighten(&tightened, next.generation);
                self.pins
                    .replace(next.pins.clone(), next.file_flags.clone());
                self.catalog.publish(next);
                self.refresh_config_warnings();
                for latch in parked.0.drain(..) {
                    latch.set(true);
                }
                return Ok(r);
            }
        })();
        let report = match attempted {
            Ok(r) => {
                self.warnings.remove(|w| {
                    matches!(
                        w,
                        DaemonWarning::ConfigReloadFailed { .. }
                            | DaemonWarning::ConfigRestartRequired { .. }
                    )
                });
                r
            }
            Err(e) => {
                let reason = e
                    .detail
                    .as_ref()
                    .and_then(|d| d["reason"].as_str())
                    .unwrap_or("invalid_config")
                    .to_owned();
                self.warnings.remove(|w| {
                    matches!(
                        w,
                        DaemonWarning::ConfigReloadFailed { .. }
                            | DaemonWarning::ConfigRestartRequired { .. }
                    )
                });
                self.warnings.add(DaemonWarning::ConfigReloadFailed {
                    reason: reason.clone(),
                    detail: json!(&e),
                    hash: attempted_hash.clone(),
                });
                if !restart.is_empty() {
                    self.warnings.add(DaemonWarning::ConfigRestartRequired {
                        fields: restart.clone(),
                    });
                }
                self.warnings.alert_first(
                    &format!("reload:{attempted_hash}"),
                    &format!("config.toml not applied: {}", e.message),
                );
                let mut r = report(self.catalog.current(), "rejected");
                r.restart_required = restart;
                r.reasons.push(json!(e));
                r
            }
        };
        (report, attempted_hash)
    }

    fn refresh_config_warnings(&self) {
        self.warnings
            .remove(|w| matches!(w, DaemonWarning::ConfigRelaxPending { .. }));
        for repo in &self.catalog.current().pending_relax {
            self.warnings
                .add(DaemonWarning::ConfigRelaxPending { repo: repo.clone() });
        }
        if self.catalog.len() > 1000 {
            self.warnings.alert_first(
                "catalog_growth",
                "more than 1000 catalogs retained; an owner restart frees old snapshots",
            );
            self.warnings.add(DaemonWarning::CatalogGrowth {
                generations: self.catalog.len(),
            });
        }
    }

    pub fn relax_shown(
        &self,
        repo: &RepoName,
        shown: &PinnedFlags,
        between: &dyn Fn(),
    ) -> Result<()> {
        let _admission = self.admit();
        let old = self.snapshot()?;
        let file = old
            .file_flags
            .get(repo)
            .ok_or_else(|| StrataError::new(Code::NoSuchRepo, "unknown repo"))?;
        let disk = || {
            fs::read(&self.opts.config_file)
                .map_err(|e| error("read_failed", e.to_string()))
                .and_then(|b| source_hash(&b))
        };
        if disk()? != old.hash || self.reload.rejected() {
            return Err(error(
                "config_not_applied",
                "apply the current config with config reload before relaxing it",
            ));
        }
        if shown != file {
            return Err(error(
                "action_changed",
                "shown flags differ from applied config",
            ));
        }
        if !confirm_flow::relaxes(&old.pins[repo], file) {
            return Err(error(
                "not_relaxed",
                "configuration is not looser than its pins",
            ));
        }
        let id = old.gits[repo].repo_id();
        confirm_flow::require_validated_with(
            &self.confirmer,
            Action::ConfigRelax { repo: id.clone() },
            between,
        )?;
        let _commit = self.commit.lock().unwrap_or_else(|p| p.into_inner());
        let current = self.catalog.current();
        if current.file_flags.get(repo) != Some(shown)
            || current.gits.get(repo).map(|g| g.repo_id()) != Some(id)
            || disk()? != current.hash
            || self.reload.rejected()
        {
            return Err(
                StrataError::new(Code::NotConfirmed, "config changed after confirmation")
                    .with_detail(json!({ "reason": "action_changed" })),
            );
        }
        let mut pins = current.pins.clone();
        pins.insert(repo.clone(), shown.clone());
        let next = Catalog::build(
            current.source.clone(),
            current.gits.clone(),
            pins,
            current.generation + 1,
            current.hash.clone(),
        )?;
        self.pins.write(&next.pins)?;
        self.pins
            .replace(next.pins.clone(), next.file_flags.clone());
        self.catalog.publish(next);
        self.refresh_config_warnings();
        Ok(())
    }

    #[cfg(feature = "test-seams")]
    pub fn reload_for_test(&self) -> ReloadReport {
        let (r, _) = self.apply_reload(&mut |_| Ok(Vec::new()));
        let mut s = self.reload.state.lock().unwrap_or_else(|p| p.into_inner());
        s.last = Some(r.clone());
        s.at = Some(SystemTime::now());
        r
    }
}

type Stamp = (u64, u64, u64, i64, i64);
fn stat(core: &Core) -> Option<Stamp> {
    fs::metadata(&core.opts.config_file)
        .ok()
        .map(|m| (m.dev(), m.ino(), m.size(), m.mtime(), m.mtime_nsec()))
}

/// Also supervises the parked per-repo loops. Scope owns every join handle;
/// cancellation only releases parked loops, never waits under the committer.
pub fn run(core: &Core, stop: &(dyn Fn() -> bool + Sync)) {
    std::thread::scope(|scope| {
        let mut prepare = |repo: &RepoName| -> Result<Vec<Latch>> {
            let mut parked = Parked(Vec::new());
            for install in [true, false] {
                let latch = Latch::default();
                let child = latch.clone();
                let repo = repo.clone();
                std::thread::Builder::new()
                    .name(format!("config-{repo}-{install}"))
                    .spawn_scoped(scope, move || {
                        if child.wait() {
                            crate::main_sync::run_loop(core, &repo, install, stop);
                        }
                    })
                    .map_err(|e| error("spawn_failed", e.to_string()))?;
                parked.0.push(latch);
            }
            Ok(std::mem::take(&mut parked.0))
        };
        for repo in core.catalog.current().config.repos.keys() {
            match prepare(repo) {
                Ok(latches) => {
                    for latch in latches {
                        latch.set(true);
                    }
                }
                Err(e) => core.warnings.alert_once(&e.message),
            }
        }
        let mut observed = stat(core);
        let mut handled = observed;
        let mut initialized = false;
        let mut next_stat = Instant::now() + core.opts.config_poll_every;
        let mut hash = core.catalog.current().hash.clone();
        let mut retry_at = None;
        let mut backoff = Duration::from_secs(60);
        while !stop() && !core.locks.is_shut_down() {
            let requested = core
                .reload
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .requested;
            let finished = core
                .reload
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .finished;
            let mut due = requested > finished || HUP.swap(false, Ordering::SeqCst);
            if Instant::now() >= next_stat {
                let stamp = stat(core);
                if stamp == observed && (!initialized || stamp != handled) {
                    initialized = true;
                    handled = stamp;
                    let new_hash = fs::read(&core.opts.config_file)
                        .ok()
                        .and_then(|b| source_hash(&b).ok())
                        .unwrap_or_default();
                    due |= new_hash != hash;
                }
                observed = stamp;
                next_stat = Instant::now() + core.opts.config_poll_every;
            }
            due |= retry_at.is_some_and(|at| Instant::now() >= at);
            if due {
                // Capture before the read. Requests arriving during this attempt
                // require another one, even if it publishes their desired file.
                let stamp = core
                    .reload
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .requested;
                let (result, attempted_hash) = core.apply_reload(&mut prepare);
                // Cache the bytes the attempt actually parsed, not a second
                // read that could race an editor's rename-save.
                hash = attempted_hash;
                let missing = result
                    .reasons
                    .iter()
                    .any(|e| e["detail"]["reason"] == "missing");
                retry_at = missing.then(|| Instant::now() + backoff);
                backoff = if missing {
                    (backoff * 2).min(Duration::from_secs(600))
                } else {
                    Duration::from_secs(60)
                };
                let mut s = core.reload.state.lock().unwrap_or_else(|p| p.into_inner());
                s.finished = stamp;
                s.last = Some(result);
                s.at = Some(SystemTime::now());
                core.reload.cv.notify_all();
            }
            let s = core.reload.state.lock().unwrap_or_else(|p| p.into_inner());
            let _ = core.reload.cv.wait_timeout(s, Duration::from_millis(20));
        }
        core.reload.wake();
    });
}

/// SHA-256 from macOS's system crypto implementation. The workspace is macOS
/// only; CommonDigest.h defines CC_LONG as uint32_t and the output as 32 bytes.
/// No new crate dependency or external command is needed.
pub(crate) fn source_hash(bytes: &[u8]) -> Result<String> {
    unsafe extern "C" {
        fn CC_SHA256(data: *const libc::c_void, len: u32, output: *mut u8) -> *mut u8;
    }
    let len = u32::try_from(bytes.len()).map_err(|_| {
        error(
            "config_too_large",
            "config exceeds the supported digest input size",
        )
    })?;
    let mut out = [0u8; 32];
    // SAFETY: input points to len initialized bytes and remains live during the
    // call; output is a separate writable buffer of CC_SHA256_DIGEST_LENGTH.
    unsafe {
        CC_SHA256(bytes.as_ptr().cast(), len, out.as_mut_ptr());
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn source_hash_is_sha256() {
        assert_eq!(
            super::source_hash(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            super::source_hash(b"").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}

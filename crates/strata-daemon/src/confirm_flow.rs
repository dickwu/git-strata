//! daemon.md §5.3 step 4 and step 7 (the confirm order), §4 `pins.json`,
//! §5.5 `config_relax`.
//!
//! The daemon never checks a TTY (the CLI does that) and never
//! trusts any "confirmed" marker in params: it derives the `Action`,
//! `require`s it, `validate`s (redeems) it at once, with no queue or repo
//! lock held, and later re-derives the action inside the lock.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::{IoSide, classify_io_error};
use strata_confirm::Confirmer;
use strata_types::{Action, Code, Config, ConfirmToken, RepoConfig, RepoName, StrataError};

use crate::{PinnedFlags, Result};

/// The flags a repo's configuration file asks for.
pub fn flags_of(rc: &RepoConfig) -> PinnedFlags {
    PinnedFlags { deploy_on_push: rc.deploy_on_push, confirm: rc.publish.confirm, push: rc.publish.push }
}

/// The stricter of `pin` and `file`, field by field (a tightening applies at
/// once, a relaxation does not).
pub fn tighten(pin: PinnedFlags, file: PinnedFlags) -> PinnedFlags {
    PinnedFlags {
        deploy_on_push: pin.deploy_on_push || file.deploy_on_push,
        confirm: pin.confirm || file.confirm,
        push: pin.push && file.push,
    }
}

/// Whether `file` is looser than `pin` in any field.
pub fn relaxes(pin: PinnedFlags, file: PinnedFlags) -> bool {
    (pin.deploy_on_push && !file.deploy_on_push) || (pin.confirm && !file.confirm) || (!pin.push && file.push)
}

/// `needs_human_confirm` over pinned flags.
pub fn needs_confirm(f: PinnedFlags) -> bool {
    f.confirm || f.deploy_on_push
}

/// Apply pinned (effective) flags onto a repo config.
pub fn apply(rc: &mut RepoConfig, f: PinnedFlags) {
    rc.deploy_on_push = f.deploy_on_push;
    rc.publish.confirm = f.confirm;
    rc.publish.push = f.push;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinsFile {
    repos: BTreeMap<RepoName, PinnedFlags>,
}

/// `pins.json` plus what the configuration file asked for.
pub struct Pins {
    path: PathBuf,
    inner: Mutex<PinsState>,
}

struct PinsState {
    pinned: BTreeMap<RepoName, PinnedFlags>,
    file: BTreeMap<RepoName, PinnedFlags>,
}

impl Pins {
    /// Startup step 5. No file: the loaded configuration becomes the pins
    /// (not a relaxation). Otherwise tighten at once and write back; record
    /// relaxations as pending. A repo new to the file is pinned as
    /// configured. Pins of repos no longer configured are kept. Never
    /// prompts.
    pub fn load(path: &Path, config: &Config) -> Result<Pins> {
        let file: BTreeMap<RepoName, PinnedFlags> =
            config.repos.iter().map(|(n, rc)| (n.clone(), flags_of(rc))).collect();
        let existing = match fs::read(path) {
            Ok(bytes) => Some(serde_json::from_slice::<PinsFile>(&bytes).map_err(|e| {
                StrataError::new(Code::InvalidInput, format!("{} is not a pins file: {e}", path.display()))
                    .with_detail(json!({ "reason": "pins", "path": path }))
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(classify_io_error(&e, IoSide::Read, path)),
        };
        let mut pinned = existing.clone().map(|p| p.repos).unwrap_or_default();
        for (n, f) in &file {
            let next = match pinned.get(n) {
                Some(pin) => tighten(*pin, *f),
                None => *f,
            };
            pinned.insert(n.clone(), next);
        }
        let pins = Pins { path: path.to_path_buf(), inner: Mutex::new(PinsState { pinned, file }) };
        let changed = existing.map(|e| e.repos) != Some(pins.state().pinned.clone());
        if changed {
            pins.write(&pins.state().pinned)?;
        }
        Ok(pins)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PinsState> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The effective flags of `repo` (the pinned ones).
    pub fn pinned(&self, repo: &RepoName) -> Option<PinnedFlags> {
        self.state().pinned.get(repo).copied()
    }

    /// What the configuration file asks for.
    pub fn file(&self, repo: &RepoName) -> Option<PinnedFlags> {
        self.state().file.get(repo).copied()
    }

    /// Repos whose file is looser than the pin (`ConfigRelaxPending`).
    pub fn pending(&self) -> Vec<RepoName> {
        let s = self.state();
        s.file
            .iter()
            .filter(|(n, f)| s.pinned.get(*n).is_some_and(|p| relaxes(*p, **f)))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Accept the file's flags for `repo` (after `validate`), write.
    fn accept(&self, repo: &RepoName) -> Result<()> {
        let mut s = self.state();
        let Some(f) = s.file.get(repo).copied() else {
            return Err(StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")));
        };
        let mut next = s.pinned.clone();
        next.insert(repo.clone(), f);
        self.write(&next)?;
        s.pinned = next;
        Ok(())
    }

    /// Atomic replace: temp file (0600) → fsync → rename → fsync dir.
    fn write(&self, pinned: &BTreeMap<RepoName, PinnedFlags>) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("/"));
        let tmp = dir.join(format!(".pins.json.{}.tmp", std::process::id()));
        let mut body = serde_json::to_vec_pretty(&PinsFile { repos: pinned.clone() })
            .map_err(|e| StrataError::new(Code::InvalidInput, e.to_string()))?;
        body.push(b'\n');
        let io = |e: std::io::Error, p: &Path| classify_io_error(&e, IoSide::Write, p);
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC)
                .open(&tmp)
                .map_err(|e| io(e, &tmp))?;
            f.write_all(&body).map_err(|e| io(e, &tmp))?;
            f.sync_all().map_err(|e| io(e, &tmp))?;
        }
        if let Err(e) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(io(e, &self.path));
        }
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

/// §5.3 step 4: `require` then `validate` at once, on the calling (blocking)
/// thread, with no queue or repo lock held. `between` runs after `require`
/// returned and before `validate` (a test seam; production passes `&|| {}`).
pub fn require_validated_with(confirmer: &Confirmer, action: Action, between: &dyn Fn()) -> Result<ConfirmToken> {
    let token = confirmer.require(action.clone())?;
    between();
    confirmer.validate(&token, &action)?;
    Ok(token)
}

pub fn require_validated(confirmer: &Confirmer, action: Action) -> Result<ConfirmToken> {
    require_validated_with(confirmer, action, &|| {})
}

/// §5.3 step 7: the action re-derived inside the lock must be the one the
/// human confirmed; otherwise `NotConfirmed` and nothing is done.
pub fn recheck(token: &ConfirmToken, in_lock: &Action) -> Result<()> {
    if &token.action != in_lock {
        return Err(StrataError::new(
            Code::NotConfirmed,
            "the request changed after it was confirmed; confirm again",
        )
        .with_detail(json!({ "reason": "action_changed", "confirmed": token.action, "now": in_lock })));
    }
    Ok(())
}

/// §5.5 `config_relax`: no queue, no repo lock, no TTY check.
/// An unknown repo is `NoSuchRepo` and nothing to relax is `InvalidInput`
/// `not_relaxed` (D20), both without a prompt; otherwise
/// `require(ConfigRelax {RepoId})`, `validate`, then write `pins.json`.
/// Any failure leaves the pin bytes as they were.
pub fn config_relax_with(
    confirmer: &Confirmer,
    pins: &Pins,
    repo: &RepoName,
    repo_id: strata_types::RepoId,
    between: &dyn Fn(),
) -> Result<()> {
    let (Some(pin), Some(file)) = (pins.pinned(repo), pins.file(repo)) else {
        return Err(StrataError::new(Code::NoSuchRepo, format!("no repo named {repo}")));
    };
    if !relaxes(pin, file) {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!("{repo}: the configuration is not looser than the pinned flags"),
        )
        .with_detail(json!({ "reason": "not_relaxed", "repo": repo.as_str(), "pinned": pin, "file": file })));
    }
    require_validated_with(confirmer, Action::ConfigRelax { repo: repo_id }, between)?;
    pins.accept(repo)
}

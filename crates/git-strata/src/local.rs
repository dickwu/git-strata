//! The local read-only path (cli.md §5.1): only when the daemon is not
//! listening, only for status / grep / files / diff / deps list / daemon
//! status. The store is opened as a reader; the lock handed to `Sessions`
//! answers `Busy` at once and the `BeforeCheckpoint` refuses
//! (`readonly_client`): nothing here may reach a write.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use strata_deps::{InstallEnv, RepoWriteLock};
use strata_gitio::Git;
use strata_overlay::{Base, Checkpointed, Overlay, WorkspaceRef};
use strata_search::OverlaySource;
use strata_session::{BeforeCheckpoint, Sessions};
use strata_store::Store;
use strata_types::{BaseGen, CheckpointReason, Code, Config, RepoName, SessionId, StrataError};

use crate::Result;

pub fn readonly_client(what: &str) -> StrataError {
    StrataError::new(Code::InvalidInput, format!("{what} is a write; the read-only client cannot do it"))
        .with_detail(json!({ "reason": "readonly_client" }))
}

/// Never waits, never runs `f`.
pub struct NoLock;

impl RepoWriteLock for NoLock {
    fn with_write(&self, repo: &RepoName, _f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>> {
        Err(StrataError::new(Code::Busy, format!("the read-only client takes no write lock on {repo}"))
            .with_detail(json!({ "reason": "readonly_client", "repo": repo.as_str() })))
    }
}

impl strata_publish::PublishLocks for NoLock {
    fn with_sessions(&self, _s: &[SessionId], _f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>> {
        Err(StrataError::new(Code::Busy, "the read-only client takes no session queue")
            .with_detail(json!({ "reason": "readonly_client" })))
    }
    fn with_repo(&self, repo: &RepoName, f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>> {
        RepoWriteLock::with_write(self, repo, f)
    }
}

/// Refuses every checkpoint.
pub struct NoBefore;

impl BeforeCheckpoint for NoBefore {
    fn checkpoint(&self, _ws: &WorkspaceRef<'_>, _base: &Base, _reason: CheckpointReason) -> Result<Checkpointed> {
        Err(readonly_client("a checkpoint"))
    }
}

/// The base generation with its manifest and exported directory.
pub fn base_of(store: &Store, g: &BaseGen) -> Result<Base> {
    let m = store.manifest(&g.repo, &g.tree)?.ok_or_else(|| {
        StrataError::new(Code::Unknown, format!("the manifest of base {} of {} is missing", g.tree.as_str(), g.repo))
            .with_detail(json!({ "reason": "base_manifest_missing", "repo": g.repo.as_str() }))
    })?;
    Ok(Base { dir: store.base_dir(g), r#gen: g.clone(), manifest: Arc::new(m) })
}

/// The current overlay of one workspace, recomputed from the files (a cold
/// scan; reads only, no store write, no `Sessions`).
pub fn overlay_of(config: &Config, store: &Store, git: &Git, s: &SessionId, repo: &RepoName) -> Result<Overlay> {
    let rec = crate::ro::record(store, s)?;
    let ws = rec.manifest.repos.get(repo).cloned().ok_or_else(|| {
        StrataError::new(Code::NoSuchRepo, format!("session {s} has no repo {repo}"))
            .with_detail(json!({ "session": s.as_str(), "repo": repo.as_str() }))
    })?;
    let cfg = config.repo(repo)?;
    let base = base_of(store, &ws.base)?;
    let w = WorkspaceRef { session: s, repo, root: &ws.path, config: cfg, git, store };
    strata_overlay::reconcile_full(&w, &base)
}

/// Everything the read-only path needs, opened from the config.
pub struct Local {
    pub config: Config,
    pub gits: BTreeMap<RepoName, Git>,
    pub store: Store,
    pub env: InstallEnv,
}

impl Local {
    pub fn open(config: Config) -> Result<Local> {
        let store = Store::open_read_only(&config.paths.root)?;
        let mut gits = BTreeMap::new();
        for (name, rc) in &config.repos {
            gits.insert(name.clone(), Git::open(&rc.path)?);
        }
        // The daemon's PATH (D1), so toolchain lines match what it computes.
        let path = match std::env::var_os("HOME") {
            Some(h) => strata_daemon::install_path_for_home(std::path::Path::new(&h)),
            None => strata_daemon::DEFAULT_PATH.to_string(),
        };
        let env = InstallEnv { path, timeout: Duration::from_secs(60) };
        Ok(Local { config, gits, store, env })
    }

    pub fn git(&self, repo: &RepoName) -> Result<&Git> {
        self.gits.get(repo).ok_or_else(|| {
            StrataError::new(Code::NoSuchRepo, format!("no repo {repo} in the configuration")).with_detail(json!({ "repo": repo.as_str() }))
        })
    }

    /// `Sessions` over the reader store; only while no daemon runs.
    pub fn sessions(&self) -> Result<Sessions<'_>> {
        Sessions::open(&self.config, &self.gits, &self.store, &NoLock, &self.env, &NoBefore)
    }
}

/// Search's overlay source: a fresh cold scan per view.
pub struct ScanSource<'a> {
    pub local: &'a Local,
}

impl OverlaySource for ScanSource<'_> {
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay> {
        let git = self.local.gits.get(repo)?;
        overlay_of(&self.local.config, &self.local.store, git, session, repo).ok()
    }
    fn needs_full(&self, _session: &SessionId, _repo: &RepoName) -> bool {
        false
    }
}

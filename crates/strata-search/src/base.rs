//! The shared base index (search.md §4.1, §5.1 steps 8–12).
//!
//! One `GenerationManager` per repo, over the repo's main checkout (never a
//! session's shadow repository), stored at `<root>/search/<repo>/`; tgrep
//! adds the `<identity>/` level and `publication.lock` below it. Opened
//! generations are kept as strong `Arc`s per (repo, tree), so every view of
//! a tree shares one `Arc<Generation>` and therefore one `Arc<IndexReader>`.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;
use strata_types::{Code, CommitOid, Config, RepoName, StrataError, TreeOid};
use tgrep_core::generations::{Generation, GenerationError, GenerationKey, GenerationManager, IndexingProfile, Repository};

use crate::{Result, err, lock};

/// The v1 profile (D3): tgrep's default with overlay's size cap.
pub(crate) fn profile() -> IndexingProfile {
    IndexingProfile { max_blob_bytes: Some(strata_overlay::MERGE_MAX_BYTES), ..IndexingProfile::default() }
}

/// tgrep's legacy-key schema number. `GenerationKey::legacy` is crate
/// private; the key is serde, so search names the same key to tell an
/// existing published directory from a new build when `ensure` fails.
const TGREP_SCHEMA_VERSION: u32 = 1;

pub(crate) struct RepoBase {
    pub(crate) manager: GenerationManager,
    /// tree hex → the opened generation (strong: tgrep's own cache is weak).
    pub(crate) generations: Mutex<BTreeMap<String, Arc<Generation>>>,
}

#[derive(Default)]
pub(crate) struct Bases {
    repos: Mutex<BTreeMap<RepoName, Arc<RepoBase>>>,
    #[cfg(feature = "test-seams")]
    pub(crate) fail_after_stage: std::sync::atomic::AtomicBool,
}

fn unknown(msg: String, reason: &str) -> StrataError {
    err(Code::Unknown, msg, reason)
}

fn git_missing(e: &std::io::Error) -> StrataError {
    err(Code::Unsupported, format!("git is not on PATH: {e}"), "git_missing")
}

impl Bases {
    /// The repo's manager, created on first use (§5.1 step 8).
    pub(crate) fn repo(&self, config: &Config, repo: &RepoName) -> Result<Arc<RepoBase>> {
        let mut repos = lock(&self.repos);
        if let Some(r) = repos.get(repo) {
            return Ok(r.clone());
        }
        let Some(rc) = config.repos.get(repo) else {
            return Err(StrataError::new(Code::NoSuchRepo, format!("no repo {repo} in the config"))
                .with_detail(json!({ "repo": repo.as_str() })));
        };
        // With the main checkout present, a NotFound out of discover is the
        // git executable itself.
        if !rc.path.is_dir() {
            return Err(StrataError::new(Code::NoSuchPath, format!("main checkout {} is missing", rc.path.display()))
                .with_detail(json!({ "path": rc.path.to_string_lossy() })));
        }
        let repository = Repository::discover(&rc.path).map_err(|e| match e {
            GenerationError::Io(io) if io.kind() == ErrorKind::NotFound => git_missing(&io),
            other => unknown(format!("discover {}: {other}", rc.path.display()), "index_storage"),
        })?;
        let storage = storage_parent(config, repo);
        fs::create_dir_all(&storage)
            .map_err(|e| unknown(format!("create {}: {e}", storage.display()), "index_storage"))?;
        let manager = GenerationManager::with_storage(repository, &storage).map_err(|e| match e {
            GenerationError::Io(io) if io.kind() == ErrorKind::NotFound => git_missing(&io),
            GenerationError::Unsupported(m) => {
                err(Code::Unsupported, format!("index storage {} refused: {m}", storage.display()), "index_storage")
            }
            other => unknown(format!("index storage {}: {other}", storage.display()), "index_storage"),
        })?;
        let r = Arc::new(RepoBase { manager, generations: Mutex::new(BTreeMap::new()) });
        repos.insert(repo.clone(), r.clone());
        Ok(r)
    }

    pub(crate) fn existing(&self, repo: &RepoName) -> Option<Arc<RepoBase>> {
        lock(&self.repos).get(repo).cloned()
    }

    /// §5.1 steps 9–12: the generation of `head`'s tree, which must be
    /// `tree`. No search lock is held here; tgrep's `publication.lock` is
    /// the only wait (D5).
    pub(crate) fn ensure(&self, config: &Config, repo: &RepoName, head: &CommitOid, tree: &TreeOid) -> Result<Arc<Generation>> {
        let rb = self.repo(config, repo)?;
        let repository = rb.manager.repository();
        // Resolve first, so a missing commit or git is named before any
        // build starts.
        let (commit, resolved) = repository.resolve_commit_tree(head.as_str()).map_err(|e| match e {
            GenerationError::Io(io) if io.kind() == ErrorKind::NotFound => git_missing(&io),
            GenerationError::Git { stderr, .. } => {
                StrataError::new(Code::NoSuchCommit, format!("commit {head} is not in the main checkout of {repo}: {}", stderr.trim()))
                    .with_detail(json!({ "commit": head.as_str() }))
            }
            other => unknown(format!("resolve {head}: {other}"), "index_build"),
        })?;
        if commit.len() == 64 || resolved.len() == 64 {
            return Err(err(Code::Unsupported, format!("repo {repo} uses SHA-256 objects"), "sha256"));
        }
        let key = legacy_key(repository, &resolved);
        let published_before = key
            .as_ref()
            .map(|k| rb.manager.directory().join(k.storage_name()).exists())
            .unwrap_or(false);

        #[cfg(feature = "test-seams")]
        if self.fail_after_stage.swap(false, std::sync::atomic::Ordering::SeqCst) && !published_before {
            // tgrep has no hook between staging and the publishing rename;
            // stand in for it: a `.stage-*` exists and the build fails.
            let stage = rb.manager.directory().join(format!(".stage-{}", crate::rand_hex(6)));
            fs::create_dir(&stage).map_err(|e| unknown(format!("stage {}: {e}", stage.display()), "index_build"))?;
            return Err(StrataError::new(Code::Unknown, format!("index build of {resolved} failed after staging (seam)"))
                .with_detail(json!({ "reason": "index_build", "stage": stage.to_string_lossy() })));
        }

        let predecessor = lock(&rb.generations).iter().find(|(t, _)| **t != resolved).map(|(_, g)| g.clone());
        let res = rb.manager.ensure(head.as_str(), profile(), predecessor.as_ref()).map_err(|e| match e {
            GenerationError::Io(io) if io.kind() == ErrorKind::NotFound && !published_before => git_missing(&io),
            GenerationError::Git { stderr, .. } if !published_before => {
                StrataError::new(Code::NoSuchCommit, format!("commit {head}: {}", stderr.trim()))
                    .with_detail(json!({ "commit": head.as_str() }))
            }
            other if published_before => StrataError::new(Code::Unknown, format!("published index of {resolved} is unreadable: {other}"))
                .with_detail(json!({ "reason": "index_unreadable", "dir": rb.manager.directory().to_string_lossy() })),
            other => StrataError::new(Code::Unknown, format!("index build of {resolved} failed: {other}"))
                .with_detail(json!({ "reason": "index_build", "dir": rb.manager.directory().to_string_lossy() })),
        })?;
        let g = {
            let mut gens = lock(&rb.generations);
            gens.entry(res.generation.key().tree_oid().to_string()).or_insert(res.generation).clone()
        };
        // Step 11: a known mismatch, not an uncertain one.
        if g.key().tree_oid() != tree.as_str() {
            return Err(StrataError::new(
                Code::InvalidInput,
                format!("commit {head} has tree {}, the workspace base is {tree}", g.key().tree_oid()),
            )
            .with_detail(json!({ "reason": "head_tree_mismatch", "tree": g.key().tree_oid(), "base": tree.as_str() })));
        }
        Ok(g)
    }

    /// An already opened generation of (repo, tree).
    pub(crate) fn opened(&self, repo: &RepoName, tree: &str) -> Option<Arc<Generation>> {
        let rb = self.existing(repo)?;
        lock(&rb.generations).get(tree).cloned()
    }
}

/// `<root>/search/<repo>/`, the storage handed to `with_storage`.
pub(crate) fn storage_parent(config: &Config, repo: &RepoName) -> PathBuf {
    config.paths.root.join("search").join(repo.as_str())
}

fn legacy_key(repository: &Repository, tree: &str) -> Option<GenerationKey> {
    serde_json::from_value(json!({
        "repository": repository.identity(),
        "tree": tree,
        "profile": profile(),
        "index_format": tgrep_core::meta::INDEX_FORMAT_VERSION,
        "schema": TGREP_SCHEMA_VERSION,
    }))
    .ok()
}


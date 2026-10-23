//! git-strata publish (docs/modules/publish.md): one commit per repo for a
//! release group, merged by path under the repo write lock, checked
//! read-only, written onto the publish chain and pushed.

use strata_deps as deps;
use strata_gitio::Git;
use strata_session::Sessions;
use strata_store::Store;
use strata_types::{RepoConfig, RepoName, SessionId, StrataError};

pub mod chain;
pub mod check;
pub mod stage;

pub use chain::{advance_anchor, anchor_ref, chain_ref, merge_base};
pub use check::{CHECK_OUTPUT_TAIL, ChecksRan, check};
pub use stage::{HUNK_TEXT_CAP, Staged, stage};

pub type Result<T> = std::result::Result<T, StrataError>;

/// One publish call's context (§3). Every field is owned by the daemon.
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub git: &'a Git,
    pub cfg: &'a RepoConfig,
    pub repo: &'a RepoName,
    /// `repo`'s deps context: `key_of`, `ensure`, `propose`, `observe_main`.
    pub deps: &'a deps::Ctx<'a>,
    /// The session queue and the repo write lock (§5.1).
    pub locks: &'a dyn PublishLocks,
    /// The session handle: `ensure_base_for` and `move_workspace` for the
    /// post-publish rebase (§5.8). Not in the §3 signature list; recorded
    /// in docs/impl-progress/publish.md.
    pub sessions: &'a Sessions<'a>,
}

/// The daemon's two resources (§3). Order is §5.1's; the trait does not
/// enforce it. The outer `Err` is only a `Busy` before `f` ran.
pub trait PublishLocks: Sync {
    /// Hold these sessions' queue places (ascending) while `f` runs.
    fn with_sessions(&self, sessions: &[SessionId], f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>>;
    /// Hold `repo`'s write lock while `f` runs.
    fn with_repo(&self, repo: &RepoName, f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>>;
}

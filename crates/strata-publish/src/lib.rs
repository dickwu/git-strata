//! git-strata publish (docs/modules/publish.md): one commit per repo for a
//! release group, merged by path under the repo write lock, checked
//! read-only, written onto the publish chain and pushed.

use strata_deps as deps;
use strata_gitio::Git;
use strata_session::Sessions;
use strata_store::Store;
use strata_types::{RepoConfig, RepoName, SessionId, StrataError};

pub mod chain;
pub(crate) mod deps_select;
pub mod check;
pub mod pending;
pub mod push;
pub mod rebase;
pub mod seams;
pub mod siblings;
/// Review-2 N1: stage's selection rules as a read-only query, for the
/// daemon's re-run filter (the daemon never stages: it calls `publish`).
pub mod selection {
    pub use crate::stage::{at_or_under, selects};
}
pub mod stage;

pub use chain::{advance_anchor, anchor_ref, chain_ref, merge_base};
pub use check::{CHECK_OUTPUT_TAIL, ChecksRan, check};
pub use rebase::{
    BetweenDiff, HunkPreview, Merge3Preview, MovePolicy, RebaseReport, assign, diff_between, rebase_session,
    rebase_session_with,
};
pub use push::{PublishWarning, Published, WarningKind, commit, publish, publish_bound, push, push_to, remote_changed};
pub use seams::PUSH_TIMEOUT;
#[cfg(feature = "test-seams")]
pub use seams::PublishFault;
pub use stage::{HUNK_TEXT_CAP, Staged, remove_layout, stage};

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
    /// The rebase's take of its sessions' queue places (§5.8): what
    /// `with_sessions` holds plus anything else a workspace move needs
    /// quiet (the daemon's du hold, status-perf D8). A `Busy` is waited
    /// out like the queue's.
    fn with_rebase_sessions(&self, sessions: &[SessionId], f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>> {
        self.with_sessions(sessions, f)
    }
    /// Whether a `Busy` take is waited out (§5.1 after the first visible
    /// write). `false` for the catch-up pass (rm-followups §1.6 T2), which
    /// never queues behind a request: its `Busy` defers the row.
    fn waits(&self) -> bool {
        true
    }
}

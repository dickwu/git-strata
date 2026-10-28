//! Test seams (publish.md §3 `PublishFault`). Compiled only under the
//! `test-seams` feature; the release build gets the no-op half below.
//! Seams are keyed by the repo's git common dir, so parallel tests with
//! their own sandboxes never see each other's faults.

use std::time::Duration;

#[cfg(not(feature = "test-seams"))]
use strata_gitio::Git;

#[cfg(not(feature = "test-seams"))]
use crate::Result;

/// The default push timeout (§5.10: "the same as gitio's"; gitio has no
/// public push default, so this is gitio's fetch timeout).
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// Pause points that live in publish itself.
#[cfg_attr(not(feature = "test-seams"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    BeforeCommit,
    AfterCommit,
    /// Before each session's post-publish rebase: the queue and the repo
    /// lock are released, the row is still `Running` (the `publishing` gap).
    BeforeRebase,
    /// After the check siblings were re-checked (`siblings::place`), before
    /// the layout directory exists and they are cloned (publish.md D21).
    AfterSiblingPlace,
    /// deps-drift §2.4: R1 is closed and no publish lock is held, before
    /// the cold install of the staged install inputs starts.
    BeforeColdInstall,
}

/// Steps after `commit_tree` where a test can inject one failure
/// (`injected_failure_after_commit_is_unknown`): the first (before the chain
/// and anchor transaction), a middle one (before the push) and the last
/// (before the row is closed `Done`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailPoint {
    BeforeRefs,
    BeforePush,
    BeforeDone,
    /// Before each session's post-publish rebase.
    BeforeRebase,
    /// Inside a check sibling's fetch section, before its anchor step
    /// (keyed by the sibling's git; publish.md D21).
    SiblingAnchor,
    /// deps-drift §2.4: in place of the cold install (R1 closed, no publish
    /// lock held), as if the daemon stopped there.
    ColdInstall,
}

#[cfg(feature = "test-seams")]
mod imp {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::Duration;

    use serde_json::json;
    use strata_gitio::Git;
    use strata_types::{Code, StrataError};

    use super::{FailPoint, PUSH_TIMEOUT, Point};
    use crate::Result;

    /// The faults a test can arm (§3).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum PublishFault {
        /// Stop before `commit_tree`.
        PauseBeforeCommit,
        /// Stop after `commit_tree`, before the chain/anchor transaction.
        PauseAfterCommit,
        /// Stop inside the recording push call after it started, before
        /// the push is sent (gitio `pause_push_before_send`).
        PauseDuringPush,
        /// Stop after the remote accepted, before the tracking ref and the
        /// anchor are recorded (gitio `pause_push_before_record`).
        PauseBeforePushRecorded,
        /// Stop before a session's post-publish rebase, with the session
        /// queue and the repo lock released and the row still `Running`.
        PauseBeforeRebase,
        /// Stop after the check siblings were re-checked, before the layout
        /// exists and they are cloned (publish.md D21).
        PauseAfterSiblingPlace,
        /// Stop after R1 closed, before the cold install (deps-drift §2.4),
        /// with no publish lock held.
        PauseBeforeColdInstall,
    }

    #[derive(Default)]
    struct PauseState {
        m: Mutex<(bool, bool)>,
        cv: Condvar,
    }

    enum Inner {
        Own(Arc<PauseState>),
        Git(strata_gitio::Pause),
    }

    /// A one-shot pause. The next publish step that reaches it blocks until
    /// `release` (or drop).
    pub struct Pause(Option<Inner>);

    impl Pause {
        pub fn wait_until_paused(&self) {
            match &self.0 {
                Some(Inner::Own(s)) => {
                    let mut g = s.m.lock().unwrap();
                    while !g.0 {
                        g = s.cv.wait(g).unwrap();
                    }
                }
                Some(Inner::Git(p)) => p.wait_until_paused(),
                None => {}
            }
        }

        pub fn release(self) {
            drop(self);
        }
    }

    impl Drop for Pause {
        fn drop(&mut self) {
            if let Some(Inner::Own(s)) = &self.0 {
                let mut g = s.m.lock().unwrap();
                g.1 = true;
                s.cv.notify_all();
            }
        }
    }

    #[derive(Default)]
    struct Armed {
        pauses: Vec<(Point, Arc<PauseState>)>,
        fails: Vec<FailPoint>,
        push_timeout: Option<Duration>,
    }

    fn table() -> &'static Mutex<HashMap<PathBuf, Armed>> {
        static T: OnceLock<Mutex<HashMap<PathBuf, Armed>>> = OnceLock::new();
        T.get_or_init(Default::default)
    }

    fn key(git: &Git) -> PathBuf {
        git.common_dir().to_path_buf()
    }

    /// Arm `fault` for the next publish on `git`'s repository.
    pub fn pause(git: &Git, fault: PublishFault) -> Pause {
        let point = match fault {
            PublishFault::PauseBeforeCommit => Point::BeforeCommit,
            PublishFault::PauseAfterCommit => Point::AfterCommit,
            PublishFault::PauseBeforeRebase => Point::BeforeRebase,
            PublishFault::PauseAfterSiblingPlace => Point::AfterSiblingPlace,
            PublishFault::PauseBeforeColdInstall => Point::BeforeColdInstall,
            PublishFault::PauseDuringPush => return Pause(Some(Inner::Git(git.pause_push_before_send()))),
            PublishFault::PauseBeforePushRecorded => return Pause(Some(Inner::Git(git.pause_push_before_record()))),
        };
        let s = Arc::new(PauseState::default());
        table().lock().unwrap().entry(key(git)).or_default().pauses.push((point, s.clone()));
        Pause(Some(Inner::Own(s)))
    }

    /// Make the next publish on `git`'s repository fail once at `at`.
    pub fn fail_once(git: &Git, at: FailPoint) {
        table().lock().unwrap().entry(key(git)).or_default().fails.push(at);
    }

    /// Override the push timeout for `git`'s repository (`None` restores
    /// the default).
    pub fn set_push_timeout(git: &Git, d: Option<Duration>) {
        table().lock().unwrap().entry(key(git)).or_default().push_timeout = d;
    }

    pub(crate) fn hit(git: &Git, p: Point) {
        let s = {
            let mut t = table().lock().unwrap();
            let Some(a) = t.get_mut(&key(git)) else { return };
            let Some(i) = a.pauses.iter().position(|(q, _)| *q == p) else { return };
            a.pauses.remove(i).1
        };
        let mut g = s.m.lock().unwrap();
        g.0 = true;
        s.cv.notify_all();
        while !g.1 {
            g = s.cv.wait(g).unwrap();
        }
    }

    pub(crate) fn fail(git: &Git, at: FailPoint) -> Result<()> {
        let mut t = table().lock().unwrap();
        let Some(a) = t.get_mut(&key(git)) else { return Ok(()) };
        let Some(i) = a.fails.iter().position(|f| *f == at) else { return Ok(()) };
        a.fails.remove(i);
        Err(StrataError::new(Code::Unknown, format!("injected failure at {at:?}"))
            .with_detail(json!({ "reason": "injected", "at": format!("{at:?}") })))
    }

    pub(crate) fn push_timeout(git: &Git) -> Duration {
        table().lock().unwrap().get(&key(git)).and_then(|a| a.push_timeout).unwrap_or(PUSH_TIMEOUT)
    }
}

#[cfg(feature = "test-seams")]
pub use imp::{Pause, PublishFault, fail_once, pause, set_push_timeout};
#[cfg(feature = "test-seams")]
pub(crate) use imp::{fail, hit, push_timeout};

#[cfg(not(feature = "test-seams"))]
pub(crate) fn hit(_: &Git, _: Point) {}

#[cfg(not(feature = "test-seams"))]
pub(crate) fn fail(_: &Git, _: FailPoint) -> Result<()> {
    Ok(())
}

#[cfg(not(feature = "test-seams"))]
pub(crate) fn push_timeout(_: &Git) -> Duration {
    PUSH_TIMEOUT
}

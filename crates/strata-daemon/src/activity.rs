//! `daemon_status.activity[]` (status-bar design §3.1, §11.2, §12): the long,
//! normal work in flight, for display and for the `LongHold` rule: moves,
//! deps rollouts, base builds, session creation, dependency installs,
//! publishes and pushes. The registry's mutex is a leaf: it is held only to
//! insert, update, remove or copy entries, never across work and never while
//! another lock is taken.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_session::observe::{self, RequestTag, Work};
use strata_types::{ProposalId, RepoName, SessionId, TreeOid};

use crate::locks::{self, Origin};
use crate::startup::Core;

/// Why a move runs: the request that runs it. Background work (the
/// catch-up pass) and any other caller are `CatchUp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    CatchUp,
    Publish,
    Sync,
    Deps,
    Replay,
}

impl Cause {
    fn of(method: Option<&str>) -> Cause {
        match method {
            Some("publish") => Cause::Publish,
            Some("sync") => Cause::Sync,
            Some("deps_approve" | "deps_propose") => Cause::Deps,
            Some("push" | "chain_resolve") => Cause::Replay,
            _ => Cause::CatchUp,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Cause::CatchUp => "catch_up",
            Cause::Publish => "publish",
            Cause::Sync => "sync",
            Cause::Deps => "deps",
            Cause::Replay => "replay",
        }
    }
}

/// What an activity does, with the fields its kind reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A workspace move; `phase` is `None` until its checkpoint.
    Move {
        cause: Cause,
        phase: Option<String>,
    },
    /// `deps approve`'s rollout over `sessions` sessions, `done` of them handled.
    DepsRollout {
        sessions: u64,
        done: u64,
    },
    BaseBuild {
        tree: Option<TreeOid>,
    },
    SessionCreate {
        repos: Vec<RepoName>,
        step: Option<String>,
    },
    DepsInstall {
        proposal: Option<ProposalId>,
    },
    Publish,
    Push,
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Move { .. } => "move",
            Kind::DepsRollout { .. } => "deps_rollout",
            Kind::BaseBuild { .. } => "base_build",
            Kind::SessionCreate { .. } => "session_create",
            Kind::DepsInstall { .. } => "deps_install",
            Kind::Publish => "publish",
            Kind::Push => "push",
        }
    }
}

/// One entry of the registry.
#[derive(Clone, Debug)]
pub struct Activity {
    pub kind: Kind,
    pub origin: Origin,
    pub repo: Option<RepoName>,
    pub session: Option<SessionId>,
    /// The request tag's id of the thread that began it (`None` on
    /// background work).
    request: Option<u64>,
    started: SystemTime,
    started_at: Instant,
    /// When its current step began: a deps rollout's start, then each of
    /// its session moves (status-bar §12.1); a session creation's current
    /// step; every other kind's start.
    step_at: Instant,
}

impl Activity {
    /// `kind` begun now on this thread, with the thread's origin and request.
    pub fn new(kind: Kind, repo: Option<RepoName>, session: Option<SessionId>) -> Activity {
        let now = Instant::now();
        Activity {
            kind,
            origin: locks::origin(),
            repo,
            session,
            request: observe::request_tag().map(|t| t.id),
            started: SystemTime::now(),
            started_at: now,
            step_at: now,
        }
    }

    /// Whether this is long work done by the holder of `repo` whose thread
    /// works for request `request`: such a hold raises `LongHold` only after
    /// the work's step runs longer than the background max (status-bar
    /// §11.2, §12.1). A move, rollout or base build begins and ends inside
    /// its repo's lock, so it is that holder's. A session creation takes its
    /// locks briefly and on several threads, so it is only its own request's
    /// (another holder of the repo meanwhile is not exempted by it). Publish
    /// and push are not exempt (Q16).
    fn exempts(&self, repo: &RepoName, request: Option<u64>) -> bool {
        match &self.kind {
            Kind::Move { .. } | Kind::DepsRollout { .. } | Kind::BaseBuild { .. } => {
                self.repo.as_ref() == Some(repo)
            }
            Kind::SessionCreate { repos, .. } => {
                request.is_some() && self.request == request && repos.contains(repo)
            }
            Kind::DepsInstall { .. } | Kind::Publish | Kind::Push => false,
        }
    }

    /// One `daemon_status.activity[]` item at `now`.
    fn json(&self, id: u64, now: Instant) -> Value {
        let mut v = json!({
            "id": id,
            "kind": self.kind.name(),
            "origin": self.origin,
            "repo": self.repo,
            "session": self.session,
            "started": strata_types::serde_rfc3339::format(&self.started).unwrap_or_default(),
            "elapsed_ms": ms(now.saturating_duration_since(self.started_at)),
            "step_elapsed_ms": ms(now.saturating_duration_since(self.step_at)),
        });
        match &self.kind {
            Kind::Move { cause, phase } => {
                v["cause"] = json!(cause.as_str());
                v["phase"] = json!(phase);
            }
            Kind::DepsRollout { sessions, done } => {
                v["sessions"] = json!(sessions);
                v["done"] = json!(done);
            }
            Kind::BaseBuild { tree } => v["tree"] = json!(tree),
            Kind::SessionCreate { repos, step } => {
                v["repos"] = json!(repos);
                v["step"] = json!(step);
            }
            // The wire contract has the proposal id as a string.
            Kind::DepsInstall { proposal } => {
                v["proposal"] = json!(proposal.map(|p| p.get().to_string()))
            }
            Kind::Publish | Kind::Push => {}
        }
        v
    }
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// The registry in `Core`: entries by id, oldest first.
#[derive(Default)]
pub struct Activities {
    next: AtomicU64,
    map: Mutex<BTreeMap<u64, Activity>>,
}

impl Activities {
    fn map(&self) -> MutexGuard<'_, BTreeMap<u64, Activity>> {
        // A panic elsewhere never leaves the registry unusable.
        self.map.lock().unwrap_or_else(|p| {
            self.map.clear_poison();
            p.into_inner()
        })
    }

    fn insert(&self, a: Activity) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.map().insert(id, a);
        id
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Activity)) {
        if let Some(a) = self.map().get_mut(&id) {
            f(a);
        }
    }

    fn remove(&self, id: u64) {
        self.map().remove(&id);
    }

    /// A copy of every entry, oldest first, as of now; the mutex is
    /// released before this returns.
    pub fn snapshot(&self) -> Snapshot {
        let entries = self.map().iter().map(|(id, a)| (*id, a.clone())).collect();
        Snapshot {
            at: Instant::now(),
            entries,
        }
    }
}

/// The registry copied at one instant (status-bar §12.3: taken, and its
/// mutex released, before any FIFO or store read).
pub struct Snapshot {
    at: Instant,
    entries: Vec<(u64, Activity)>,
}

impl Snapshot {
    /// `daemon_status.activity`.
    pub fn json(&self) -> Value {
        Value::Array(
            self.entries
                .iter()
                .map(|(id, a)| a.json(*id, self.at))
                .collect(),
        )
    }

    /// For [`crate::locks::Locks::long_holds_with`]: the age of the current
    /// step of the exempt work the holder of `repo` (its thread working for
    /// request `request`) does, the youngest when several match (the
    /// innermost: a base build inside a session creation, a move inside a
    /// rollout); `None` without any.
    pub fn exempt_step(&self, repo: &RepoName, request: Option<u64>) -> Option<Duration> {
        self.entries
            .iter()
            .filter(|(_, a)| a.exempts(repo, request))
            .map(|(_, a)| self.at.saturating_duration_since(a.step_at))
            .min()
    }
}

/// One entry in flight: removed when dropped, also during unwinding.
#[must_use = "the activity ends when this is dropped"]
pub struct ActivityGuard {
    registry: Arc<Activities>,
    id: u64,
}

impl ActivityGuard {
    pub fn begin(registry: &Arc<Activities>, a: Activity) -> ActivityGuard {
        ActivityGuard {
            registry: registry.clone(),
            id: registry.insert(a),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// A deps rollout's move of `session` begins: its step timer restarts
    /// (status-bar §12.1).
    pub fn rollout_move(&self, session: &SessionId) {
        self.registry.update(self.id, |a| {
            a.session = Some(session.clone());
            a.step_at = Instant::now();
        });
    }

    /// A deps rollout step (a move or a skip) is done: no session is being
    /// moved until the next move begins.
    pub fn rollout_done(&self) {
        self.registry.update(self.id, |a| {
            if let Kind::DepsRollout { done, .. } = &mut a.kind {
                *done += 1;
            }
            a.session = None;
        });
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

impl Core {
    /// Begin `kind` in the registry (status-bar §3.1), with this thread's
    /// origin; it ends when the guard is dropped.
    pub fn begin_activity(
        &self,
        kind: Kind,
        repo: Option<RepoName>,
        session: Option<SessionId>,
    ) -> ActivityGuard {
        ActivityGuard::begin(&self.activity, Activity::new(kind, repo, session))
    }
}

/// The session layer's hook (status-bar §11.2): its moves, base builds and
/// session creations. A move's cause is the request it runs for.
impl observe::Observer for Activities {
    fn begin(&self, work: Work) -> u64 {
        let a = match work {
            Work::Move { repo, session } => {
                let cause = Cause::of(locks::request_method().as_deref());
                Activity::new(Kind::Move { cause, phase: None }, Some(repo), Some(session))
            }
            Work::BaseBuild { repo, tree } => {
                Activity::new(Kind::BaseBuild { tree: Some(tree) }, Some(repo), None)
            }
            Work::SessionCreate { repos, session } => {
                Activity::new(Kind::SessionCreate { repos, step: None }, None, session)
            }
        };
        self.insert(a)
    }

    fn step(&self, id: u64, step: &str) {
        self.update(id, |a| match &mut a.kind {
            Kind::Move { phase, .. } => *phase = Some(step.to_string()),
            Kind::SessionCreate { step: current, .. } => {
                *current = Some(step.to_string());
                a.step_at = Instant::now();
            }
            _ => {}
        });
    }

    fn end(&self, id: u64) {
        self.remove(id);
    }
}

/// Numbers the requests of this daemon for their tags.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// Install the request tag (with the request's own number) and the
/// observer on a request's thread (status-bar §11.1); scoped helpers
/// inherit both.
pub(crate) fn enter_request(
    core: &Core,
    method: &str,
    session: Option<SessionId>,
) -> observe::Scope {
    observe::enter(
        Some(RequestTag {
            method: method.to_string(),
            session,
            id: REQUESTS.fetch_add(1, Ordering::Relaxed) + 1,
        }),
        Some(core.activity.clone() as Arc<dyn observe::Observer>),
    )
}

/// The observer alone, on a background loop that runs session work.
pub(crate) fn enter_background(core: &Core) -> observe::Scope {
    observe::enter(
        None,
        Some(core.activity.clone() as Arc<dyn observe::Observer>),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rn(s: &str) -> RepoName {
        RepoName::try_from(s.to_string()).unwrap()
    }

    #[test]
    fn exempt_kinds_and_the_youngest_step() {
        let reg = Arc::new(Activities::default());
        let tool = rn("tool");
        let publish =
            ActivityGuard::begin(&reg, Activity::new(Kind::Publish, Some(tool.clone()), None));
        let snap = reg.snapshot();
        assert_eq!(
            snap.exempt_step(&tool, None),
            None,
            "publish is not exempt (Q16)"
        );
        drop(publish);
        // A session creation begun on request 7's thread.
        let create = {
            let _tag = observe::enter(
                Some(RequestTag {
                    method: "new".into(),
                    session: None,
                    id: 7,
                }),
                None,
            );
            ActivityGuard::begin(
                &reg,
                Activity::new(
                    Kind::SessionCreate {
                        repos: vec![tool.clone()],
                        step: None,
                    },
                    None,
                    None,
                ),
            )
        };
        let snap = reg.snapshot();
        assert!(snap.exempt_step(&tool, Some(7)).is_some());
        assert_eq!(
            snap.exempt_step(&tool, Some(8)),
            None,
            "another request's hold of the repo"
        );
        assert_eq!(
            snap.exempt_step(&tool, None),
            None,
            "a background hold of the repo"
        );
        assert_eq!(snap.exempt_step(&rn("web"), Some(7)), None);
        std::thread::sleep(Duration::from_millis(20));
        let build = ActivityGuard::begin(
            &reg,
            Activity::new(Kind::BaseBuild { tree: None }, Some(tool.clone()), None),
        );
        let snap = reg.snapshot();
        let youngest = snap.exempt_step(&tool, Some(7)).unwrap();
        assert!(youngest < Duration::from_millis(20), "{youngest:?}");
        assert!(
            snap.exempt_step(&tool, None).is_some(),
            "a base build is its holder's, whoever that is"
        );
        assert_eq!(snap.json().as_array().unwrap().len(), 2);
        drop(build);
        drop(create);
        assert_eq!(reg.snapshot().json(), json!([]));
    }

    #[test]
    fn a_rollout_step_timer_restarts_per_session_move() {
        let reg = Arc::new(Activities::default());
        let tool = rn("tool");
        let rollout = ActivityGuard::begin(
            &reg,
            Activity::new(
                Kind::DepsRollout {
                    sessions: 2,
                    done: 0,
                },
                Some(tool.clone()),
                None,
            ),
        );
        std::thread::sleep(Duration::from_millis(30));
        let s = SessionId::try_from("s-aaaaaaaa".to_string()).unwrap();
        rollout.rollout_move(&s);
        assert_eq!(reg.snapshot().json()[0]["session"], "s-aaaaaaaa");
        rollout.rollout_done();
        let v = reg.snapshot().json();
        let a = &v[0];
        assert_eq!(
            (
                a["kind"].as_str(),
                a["sessions"].as_u64(),
                a["done"].as_u64()
            ),
            (Some("deps_rollout"), Some(2), Some(1))
        );
        assert!(
            a["session"].is_null(),
            "no session is moved between steps: {a}"
        );
        assert!(a["elapsed_ms"].as_u64().unwrap() >= 30, "{a}");
        assert!(
            a["step_elapsed_ms"].as_u64().unwrap() < a["elapsed_ms"].as_u64().unwrap(),
            "{a}"
        );
        assert_eq!(a["origin"], "background");
    }
}

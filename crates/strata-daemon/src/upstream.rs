//! main-sync §5: local history cached off the request path; store-only
//! metadata and a conservative live Tracker verdict are joined at read time.
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use strata_deps::{ProposalOrigin, ProposalState};
use strata_gitio::CommitSummary;
use strata_store::{Health, SessionRecord};
use strata_types::{
    BaseGen, Code, CommitOid, RepoName, RepoUpstream, SessionId, StrataError, SyncDeps, Upstream,
    UpstreamAuto, UpstreamCommit, UpstreamDeps, UpstreamMain, WorkspaceState,
};

use crate::Result;
use crate::startup::Core;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    Full,
    Cached,
}

/// Last completed Tracker scan's install-input identity. Missing state
/// cannot prove a clean workspace; status itself never fills it by walking.
pub struct Live {
    pub unpublished: bool,
    pub inputs: Option<(String, String)>, // (workspace inputs, base-tree inputs)
}
impl Default for Live {
    fn default() -> Self {
        Self {
            unpublished: true,
            inputs: None,
        }
    }
}

type Key = (RepoName, CommitOid, Option<CommitOid>, Option<CommitOid>);
type DepsKey = (SessionId, BaseGen, u64, strata_types::ProposalId);
#[derive(Clone)]
struct Graph {
    main: Option<UpstreamMain>,
    behind: u32,
    ahead: u32,
    capped: bool,
    diverged: bool,
    base_only: Vec<CommitOid>,
    commits: Vec<CommitSummary>,
}
#[derive(Default)]
struct Entries {
    escalations: BTreeMap<(SessionId, RepoName), (std::time::SystemTime, String)>,
    tips: BTreeMap<RepoName, (Option<CommitOid>, Option<CommitOid>)>,
    graphs: VecDeque<(Key, Graph)>,
    deps: VecDeque<(DepsKey, String)>,
}

#[derive(Default)]
pub struct Cache(
    Mutex<Entries>,
    #[cfg(feature = "test-seams")] std::sync::atomic::AtomicBool,
);
const CACHE_LIMIT: usize = 512;

impl Cache {
    #[cfg(feature = "test-seams")]
    pub fn fail_for_test(&self, fail: bool) {
        self.1.store(fail, std::sync::atomic::Ordering::Relaxed);
    }
    fn graph(
        &self,
        core: &Core,
        repo: &RepoName,
        ws: &WorkspaceState,
        mode: ReadMode,
    ) -> Result<Option<Graph>> {
        let (tip, b) = if mode == ReadMode::Full {
            let tip = strata_deps::main_tip(&core.config.repos[repo], &core.gits[repo])?;
            let b = strata_publish::merge_base(&core.gits[repo], &core.config.repos[repo]).ok();
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .tips
                .insert(repo.clone(), (tip.clone(), b.clone()));
            (tip, b)
        } else {
            let c = self.0.lock().unwrap_or_else(|p| p.into_inner());
            let Some(tip) = c.tips.get(repo) else {
                return Ok(None);
            };
            tip.clone()
        };
        let key = (repo.clone(), ws.head.clone(), tip.clone(), b.clone());
        if let Some((_, g)) = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .graphs
            .iter()
            .find(|(k, _)| *k == key)
        {
            return Ok(Some(g.clone()));
        }
        if mode == ReadMode::Cached {
            return Ok(None);
        }
        let git = &core.gits[repo];
        let mut graph = Graph {
            main: None,
            behind: 0,
            ahead: 0,
            capped: false,
            diverged: false,
            base_only: Vec::new(),
            commits: Vec::new(),
        };
        if let Some(tip) = tip {
            // Without B we can prove acceptance on main, but cannot decide
            // an ahead/diverged head. Do not report a guessed ahead count.
            if b.is_none() && !git.is_ancestor(&ws.head, &tip)? { return Ok(None); }
            graph.main = Some(UpstreamMain {
                tree: git.tree_of(&tip)?,
                head: tip.clone(),
            });
            let (base_only, more) = git.base_only(&ws.head, &tip, b.as_ref().unwrap_or(&tip), 1000)?;
            graph.ahead = base_only.len() as u32;
            graph.capped |= more;
            graph.base_only = base_only.into_iter().take(20).map(|(c, _)| c).collect();
            if !git.is_ancestor(&tip, &ws.head)? {
                let count = git.count_between(&ws.head, &tip, 1001)?;
                graph.behind = count.min(1000);
                graph.capped |= count > 1000;
                graph.diverged = !git.is_ancestor(&ws.head, &tip)?;
                graph.commits = git.history_between(&ws.head, &tip, 20)?;

            }
        }
        let mut c = self.0.lock().unwrap_or_else(|p| p.into_inner());
        c.graphs.retain(|(k, _)| *k != key);
        c.graphs.push_back((key, graph.clone()));
        while c.graphs.len() > CACHE_LIMIT {
            c.graphs.pop_front();
        }
        Ok(Some(graph))
    }

    /// Only a Stop-hook check consumes this latch. Status reads do not.
    pub fn hand_back(&self, core: &Core, id: &SessionId, upstream: &mut Upstream) -> Result<()> {
        self.hand_back_inner(core, id, upstream, || {})
    }

    #[cfg(feature = "test-seams")]
    pub fn hand_back_rechecking_for_test(
        &self,
        core: &Core,
        id: &SessionId,
        upstream: &mut Upstream,
        before_recheck: impl FnOnce(),
    ) -> Result<()> {
        self.hand_back_inner(core, id, upstream, before_recheck)
    }

    fn hand_back_inner(
        &self,
        core: &Core,
        id: &SessionId,
        upstream: &mut Upstream,
        before_recheck: impl FnOnce(),
    ) -> Result<()> {
        let now = std::time::SystemTime::now();
        let rows = {
            let mut cache = self.0.lock().unwrap_or_else(|p| p.into_inner());
            let rows = core.store.rebase_pending(None)?;
            cache.escalations.retain(|(s, repo), (first, _)| {
                rows.iter()
                    .any(|p| &p.session == s && &p.repo == repo && p.first_at == *first)
            });
            rows
        };
        before_recheck();
        for (repo, returned) in &mut upstream.repos {
            let snapshot = rows.iter().find(|p| &p.session == id && &p.repo == repo);
            let (current, escalation) = {
                let mut cache = self.0.lock().unwrap_or_else(|p| p.into_inner());
                // Serialize latch checks across hooks and revalidate the revision
                // AND eligibility fields (deferral itself need not bump rev).
                let current = core.store.rebase_pending_of(id, repo)?;
                let escalation = current
                    .as_ref()
                    .filter(|p| snapshot == Some(*p) && strata_session::rebase_stuck(p, now))
                    .map(|p| {
                        let key = (id.clone(), repo.clone());
                        let fresh = cache
                            .escalations
                            .get(&key)
                            .is_none_or(|(first, _)| *first != p.first_at);
                        if fresh {
                            cache
                                .escalations
                                .insert(key.clone(), (p.first_at, timestamp(&now)));
                        }
                        (cache.escalations[&key].1.clone(), fresh)
                    });
                (current, escalation)
            };
            returned.auto = current
                .as_ref()
                .map(|p| self.auto_snapshot(p, now))
                .transpose()?;
            if let Some((at, fresh)) = escalation {
                let auto = returned
                    .auto
                    .as_mut()
                    .expect("eligible current row was rendered");
                auto.escalated_at = Some(at);
                auto.escalation_new = fresh;
            }
        }
        upstream.clean = is_clean(&upstream.repos);
        Ok(())
    }

    fn auto_snapshot(
        &self,
        p: &strata_store::RebasePending,
        now: std::time::SystemTime,
    ) -> Result<UpstreamAuto> {
        let escalated_at = if strata_session::rebase_stuck(p, now) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .escalations
                .get(&(p.session.clone(), p.repo.clone()))
                .filter(|(first, _)| *first == p.first_at)
                .map(|(_, at)| at.clone())
        } else {
            None
        };
        Ok(UpstreamAuto {
            escalated_at,
            escalation_new: false,
            source: match p.source {
                strata_store::PendingSource::Main => "main",
                strata_store::PendingSource::Replay => "replay",
                strata_store::PendingSource::Publish => "publish",
            }
            .into(),
            state: if p.head.is_none()
                || p.unpushed
                || matches!(
                    p.reason.as_str(),
                    "installing" | "waiting_install" | "needs_push" | "publish_unfinished"
                ) {
                "waiting"
            } else if p.attempts > 0 {
                "deferred"
            } else {
                "pending"
            }
            .into(),
            reason: Some(p.reason.clone()),
            since: timestamp(&p.first_at),
            attempts: p.attempts,
            target: match (&p.head, &p.tree) {
                (Some(head), Some(tree)) => Some(UpstreamMain {
                    head: CommitOid::try_from(head.clone())?,
                    tree: strata_types::TreeOid::try_from(tree.clone())?,
                }),
                _ => None,
            },
            detail: p.detail.clone(),
        })
    }

    /// No Git, filesystem walk, or store write. A changed base without a
    /// warmed entry is explicitly unchecked rather than reported as zero.
    pub fn behind(
        &self,
        core: &Core,
        row: &SessionRecord,
    ) -> BTreeMap<RepoName, strata_session::BehindStatus> {
        row.manifest
            .repos
            .iter()
            .filter(|(repo, w)| {
                w.tracked
                    && row
                        .workspaces
                        .get(*repo)
                        .is_some_and(|r| r.health == Health::Ok)
            })
            .map(|(repo, ws)| {
                let g = self.graph(core, repo, ws, ReadMode::Cached).ok().flatten();
                (
                    repo.clone(),
                    strata_session::BehindStatus {
                        behind: g.as_ref().map(|g| g.behind),
                        unchecked: g.is_none(),
                    },
                )
            })
            .collect()
    }

    pub fn session(
        &self,
        core: &Core,
        row: &SessionRecord,
        mode: ReadMode,
        mut live: impl FnMut(&RepoName, &WorkspaceState, u64) -> Live,
    ) -> Result<Upstream> {
        #[cfg(feature = "test-seams")]
        if self.1.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(StrataError::new(
                Code::Unknown,
                "injected upstream enrichment failure",
            ));
        }
        let mut repos = BTreeMap::new();
        let mut fetched = Vec::new();
        for (repo, ws) in &row.manifest.repos {
            let Some(rec) = row.workspaces.get(repo).filter(|r| r.health == Health::Ok) else {
                continue;
            };
            if core.store.workspace_scene_live(&row.manifest.id, repo, rec.scene.is_some())? {
                continue;
            }
            if !ws.tracked {
                continue;
            }
            let Some(cfg) = core.config.repos.get(repo) else {
                return Err(StrataError::new(
                    Code::NoSuchRepo,
                    format!("no repo named {repo}"),
                ));
            };
            let graph = self.graph(core, repo, ws, mode)?;
            let live = live(repo, ws, rec.generation);
            let deps = self.deps(
                core,
                row,
                repo,
                ws,
                rec.generation,
                mode,
                live.inputs.as_ref(),
            )?;
            let auto = core
                .store
                .rebase_pending_of(&row.manifest.id, repo)?
                .map(|p| self.auto_snapshot(&p, std::time::SystemTime::now()))
                .transpose()?;
            fetched.push(core.store.repo_fetch(repo)?.and_then(|f| f.ok_at));
            repos.insert(
                repo.clone(),
                RepoUpstream {
                    ahead: graph.as_ref().map_or(0, |g| g.ahead),
                    following: cfg.follow_main(),
                    behind: graph.as_ref().map(|g| g.behind),
                    capped: graph.as_ref().is_some_and(|g| g.capped),
                    unchecked: graph.is_none(),
                    diverged: graph.as_ref().is_some_and(|g| g.diverged),
                    base_only: graph
                        .as_ref()
                        .map(|g| g.base_only.clone())
                        .unwrap_or_default(),
                    main: graph.as_ref().and_then(|g| g.main.clone()),
                    base_head: ws.head.clone(),
                    commits: if mode == ReadMode::Full {
                        graph
                            .as_ref()
                            .map(|g| {
                                g.commits
                                    .iter()
                                    .map(|c| UpstreamCommit {
                                        oid: c.oid.clone(),
                                        subject: c.subject.clone(),
                                        author: c.author.clone(),
                                        own: c
                                            .sessions
                                            .iter()
                                            .any(|s| s == row.manifest.id.as_str()),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    deps,
                    auto,
                    conflicts: rec.conflicts.clone(),
                    unpublished: Some(live.unpublished),
                    fix: cfg.follow_main().then(|| {
                        format!(
                            "cd ~ && git-strata sync --session {} --repo {repo}",
                            row.manifest.id
                        )
                    }),
                },
            );
        }
        let as_of = if fetched.iter().all(Option::is_some) {
            fetched.into_iter().flatten().min().map(|t| timestamp(&t))
        } else {
            None
        };
        Ok(Upstream {
            as_of_rm: None,
            as_of,
            clean: is_clean(&repos),
            repos,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn deps(
        &self,
        core: &Core,
        row: &SessionRecord,
        repo: &RepoName,
        ws: &WorkspaceState,
        generation: u64,
        mode: ReadMode,
        inputs: Option<&(String, String)>,
    ) -> Result<Option<UpstreamDeps>> {
        let open = strata_deps::open_proposals(&core.store, Some(repo))?;
        let Some(p) = open
            .iter()
            .find(|p| p.state == ProposalState::Pending && p.to != ws.base.deps)
        else {
            return Ok(None);
        };
        let key = (row.manifest.id.clone(), ws.base.clone(), generation, p.id);
        let summary = if mode == ReadMode::Full {
            let cfg = &core.config.repos[repo];
            let git = &core.gits[repo];
            let summary = p.head.as_ref().and_then(|head| {
                match strata_deps::changes(git, cfg, &ws.head, head) {
                    Ok(c) => Some(c.summary),
                    Err(e) => {
                        tracing::debug!(%repo, error = %e, "upstream deps summary omitted");
                        None
                    }
                }
            });
            let mut c = self.0.lock().unwrap_or_else(|p| p.into_inner());
            // A transient busy/error/timeout keeps the last good value for this
            // exact key. Other generations/proposals cannot reuse that value;
            // their old entries age out under the existing cache bound.
            if let Some(summary) = summary {
                c.deps.retain(|(k, _)| *k != key);
                c.deps.push_back((key.clone(), summary));
            }
            while c.deps.len() > CACHE_LIMIT {
                c.deps.pop_front();
            }
            c.deps.iter().find(|(k, _)| *k == key).map(|(_, s)| s.clone())
        } else {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .deps
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, x)| x.clone())
        };
        // Rollout's input comparisons, using a completed live scan rather
        // than hashing the workspace on a status/check/hook request.
        let mut local_state = None;
        if let Some((found, base_inputs)) = inputs
            && found != &p.to_inputs
        {
            if found != base_inputs
                && open.iter().any(|q| {
                    q.state == ProposalState::AwaitingPublish
                        && q.origin
                            == (ProposalOrigin::Session {
                                session: row.manifest.id.clone(),
                            })
                        && q.to_inputs != p.to_inputs
                })
            {
                local_state = Some("own_change".into());
            } else if strata_deps::find_set(&core.store, repo, &ws.base.deps)?
                .is_some_and(|set| *found != set.inputs_hash)
            {
                local_state = Some("drift".into());
            }
        }
        let policy = core.effective_sync_deps(repo);
        let state = match local_state {
            Some(s) => s,
            None if policy == SyncDeps::Confirm => "waiting_human".into(),
            None => match core.store.deps_auto(p.id)?.map(|a| a.state) {
                Some(strata_store::DepsAutoState::InstallFailed) => "install_failed",
                Some(strata_store::DepsAutoState::Installing) => "installing",
                Some(strata_store::DepsAutoState::Ready | strata_store::DepsAutoState::Applied) => {
                    "ready"
                }
                None if strata_deps::find_set(&core.store, repo, &p.to)?.is_some() => "ready",
                None => "installing",
            }
            .into(),
        };
        let error = if state == "install_failed" {
            core.store.deps_auto(p.id)?.and_then(|a| a.error)
        } else {
            None
        };
        Ok(Some(UpstreamDeps {
            proposal: p.id,
            mode: policy,
            state,
            from: p.from.clone(),
            to: p.to.clone(),
            summary,
            error,
        }))
    }

    /// T2 fills immutable history and package summaries without a request
    /// waiting on it. Errors leave that entry cold; no store writes here.
    pub fn warm(&self, core: &Core) {
        if let Ok(rows) = core.store.sessions() {
            for row in rows
                .into_iter()
                .filter(|r| r.lifecycle == strata_store::Lifecycle::Active)
            {
                if let Err(e) = self.session(core, &row, ReadMode::Full, |_, _, _| Live::default())
                {
                    tracing::debug!(session = %row.manifest.id, error = %e, "upstream cache warm failed");
                }
            }
        }
    }
}

pub fn is_clean(repos: &BTreeMap<RepoName, RepoUpstream>) -> bool {
    repos.values().all(|r| {
        r.behind == Some(0) && r.deps.is_none() && r.auto.is_none() && r.conflicts.is_empty()
    })
}

fn timestamp(t: &std::time::SystemTime) -> String {
    strata_types::serde_rfc3339::format(t).unwrap_or_default()
}

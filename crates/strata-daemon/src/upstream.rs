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

type Key = (RepoName, CommitOid, Option<CommitOid>, String);
type DepsKey = (SessionId, BaseGen, u64, strata_types::ProposalId, String);
#[derive(Clone)]
struct Graph {
    main: Option<UpstreamMain>,
    behind: u32,
    capped: bool,
    diverged: bool,
    base_only: Vec<CommitOid>,
    commits: Vec<CommitSummary>,
}
#[derive(Default)]
struct Entries {
    tips: BTreeMap<(RepoName, String), Option<CommitOid>>,
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
        let tip = if mode == ReadMode::Full {
            let tip = strata_deps::main_tip(
                &core.snapshot()?.config.repos[repo],
                &core.snapshot()?.gits[repo],
            )?;
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .tips
                .insert((repo.clone(), core.cfg_hash(repo)?), tip.clone());
            tip
        } else {
            let c = self.0.lock().unwrap_or_else(|p| p.into_inner());
            let Some(tip) = c.tips.get(&(repo.clone(), core.cfg_hash(repo)?)) else {
                return Ok(None);
            };
            tip.clone()
        };
        let key = (
            repo.clone(),
            ws.head.clone(),
            tip.clone(),
            core.cfg_hash(repo)?,
        );
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
        let git = &core.snapshot()?.gits[repo];
        let mut graph = Graph {
            main: None,
            behind: 0,
            capped: false,
            diverged: false,
            base_only: Vec::new(),
            commits: Vec::new(),
        };
        if let Some(tip) = tip {
            graph.main = Some(UpstreamMain {
                tree: git.tree_of(&tip)?,
                head: tip.clone(),
            });
            if !git.is_ancestor(&tip, &ws.head)? {
                let count = git.count_between(&ws.head, &tip, 1001)?;
                graph.behind = count.min(1000);
                graph.capped = count > 1000;
                graph.diverged = !git.is_ancestor(&ws.head, &tip)?;
                graph.commits = git.history_between(&ws.head, &tip, 20)?;
                if graph.diverged {
                    graph.base_only = git
                        .history_between(&tip, &ws.head, 20)?
                        .into_iter()
                        .map(|c| c.oid)
                        .collect();
                }
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
        let _catalog = core.admit();
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
            if core
                .store
                .workspace_scene_live(&row.manifest.id, repo, rec.scene.is_some())?
            {
                continue;
            }
            if !ws.tracked {
                continue;
            }
            let Some(cfg) = core.snapshot()?.config.repos.get(repo) else {
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
                .map(|p| -> Result<_> {
                    Ok(UpstreamAuto {
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
                                "installing"
                                    | "waiting_install"
                                    | "needs_push"
                                    | "publish_unfinished"
                            ) {
                            "waiting"
                        } else if p.attempts > 0 {
                            "deferred"
                        } else {
                            "pending"
                        }
                        .into(),
                        reason: Some(p.reason),
                        since: timestamp(&p.first_at),
                        attempts: p.attempts,
                        target: match (p.head, p.tree) {
                            (Some(head), Some(tree)) => Some(UpstreamMain {
                                head: CommitOid::try_from(head)?,
                                tree: strata_types::TreeOid::try_from(tree)?,
                            }),
                            _ => None,
                        },
                        detail: p.detail,
                    })
                })
                .transpose()?;
            fetched.push(core.store.repo_fetch(repo)?.and_then(|f| f.ok_at));
            repos.insert(
                repo.clone(),
                RepoUpstream {
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
                            "git-strata sync --session {} --repo {repo}",
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
        let key = (
            row.manifest.id.clone(),
            ws.base.clone(),
            generation,
            p.id,
            core.cfg_hash(repo)?,
        );
        let summary = if mode == ReadMode::Full {
            let cfg = &core.snapshot()?.config.repos[repo];
            let git = &core.snapshot()?.gits[repo];
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
            c.deps
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, s)| s.clone())
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
        let _catalog = core.admit();
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

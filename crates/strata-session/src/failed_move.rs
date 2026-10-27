//! `resolve_failed_move` (§5.9): after a human has looked at the scene of a
//! move that failed once its swap began, abandon it (never swapped) or
//! accept it (swapped) [D45].

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use strata_store::{Health, Lifecycle, MoveIntent, MoveState};
use strata_types::{BaseGen, Code, CommitOid, RepoName, SessionId, StrataError};

use crate::new::pin_ref;
use crate::scene::{cause_of, scene_of, unknown_error_named};
use crate::seams::is_stop;
use crate::status::no_such_session;
use crate::sys::lstat;
use crate::{Result, Sessions};

/// A human's choice for a failed move (module-rulings "Resolving a failed
/// move"; §5.9) [D45]. Serialised lowercase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveResolution {
    /// The swap did not happen: retire the staging, close the intent, keep
    /// the old base; health back to Ok.
    Abandon,
    /// The swap happened: keep the new tree, keep the old one as a retained
    /// old tree, finish the intent; health back to Ok.
    Accept,
}

impl MoveResolution {
    fn as_str(self) -> &'static str {
        match self {
            MoveResolution::Abandon => "abandon",
            MoveResolution::Accept => "accept",
        }
    }
}

/// What `resolve_failed_move` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionReport {
    pub session: SessionId,
    pub repo: RepoName,
    pub choice: MoveResolution,
    /// The generation of the intent that was closed.
    pub n: u64,
    /// The workspace's base and head afterwards: the old ones for Abandon,
    /// the intent's target for Accept.
    pub base: BaseGen,
    pub head: CommitOid,
    /// Accept: where the old tree is; never collected automatically,
    /// retired with remove.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_tree: Option<PathBuf>,
    /// Abandon: internal paths that could not be retired (status reports
    /// `MoveLeftover`; the next move or remove collects them).
    #[serde(default)]
    pub leftover: Vec<PathBuf>,
}

fn invalid(reason: &str, message: String, extra: Value) -> StrataError {
    let mut d = match extra {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    d.insert("reason".into(), json!(reason));
    StrataError::new(Code::InvalidInput, message).with_detail(Value::Object(d))
}

/// What a tree's `.git` says (§5.9 step 3).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Gitdir {
    Points(PathBuf),
    /// No usable gitfile; `Some(code)` when gitio refused it with a code.
    Unusable(Option<Code>),
}

impl Gitdir {
    fn read(git: &strata_gitio::Git, tree: &Path) -> Result<Gitdir> {
        match git.worktree_gitdir(tree) {
            Ok(Some(p)) => Ok(Gitdir::Points(p)),
            Ok(None) => Ok(Gitdir::Unusable(None)),
            Err(e)
                if matches!(e.code, Code::NotFile | Code::NotDirectory | Code::InvalidInput | Code::IsDirectory) =>
            {
                Ok(Gitdir::Unusable(Some(e.code)))
            }
            Err(e) => Err(e),
        }
    }

    /// Path equality compares components, so a trailing `/` does not matter.
    fn is(&self, p: &Path) -> bool {
        matches!(self, Gitdir::Points(q) if q.as_path() == p)
    }

    fn found(&self) -> Value {
        match self {
            Gitdir::Points(p) => json!(p),
            Gitdir::Unusable(_) => Value::Null,
        }
    }

    fn refusal(&self) -> Option<String> {
        match self {
            Gitdir::Unusable(Some(c)) => Some(format!("{c:?}")),
            _ => None,
        }
    }
}

fn present(p: &Path) -> bool {
    matches!(lstat(p), Ok(Some(_)))
}

impl Sessions<'_> {
    /// §5.9: resolve a move that failed after its swap began.
    pub fn resolve_failed_move(
        &self,
        session: &SessionId,
        repo: &RepoName,
        choice: MoveResolution,
    ) -> Result<ResolutionReport> {
        // ---- Phase A: read only.
        // 1.
        let rec = match self.store.session(session)? {
            Some(r) if r.lifecycle != Lifecycle::Removed => r,
            _ => return Err(no_such_session(session.as_str())),
        };
        if rec.lifecycle != Lifecycle::Active {
            let lc = format!("{:?}", rec.lifecycle).to_lowercase();
            return Err(invalid(
                "lifecycle",
                format!("session {session} is {lc}; resolving a move needs an active session"),
                json!({ "session": session.as_str(), "lifecycle": lc }),
            ));
        }
        let (Some(ws), Some(row)) = (rec.manifest.repos.get(repo), rec.workspaces.get(repo)) else {
            return Err(StrataError::not_in_session(session.as_str(), repo.as_str()));
        };
        let git = self.git(repo)?;
        let intents: Vec<MoveIntent> =
            self.store.move_intents(session)?.into_iter().filter(|i| &i.repo == repo).collect();
        let latest = intents.iter().max_by_key(|i| i.n);
        let shadow_dir = self.shadow_dir(session, repo);
        if row.health != Health::Unknown {
            // A retry of a resolution already made (review M-4).
            if let Some(m) = latest {
                let same = matches!(
                    (choice, m.state),
                    (MoveResolution::Abandon, MoveState::Abandoned) | (MoveResolution::Accept, MoveState::Done)
                );
                let scene_is_m = match row.scene.as_ref().map(|v| scene_of(v, "workspace")) {
                    Some(Ok(sc)) => sc.op == crate::SceneOp::Move && sc.kept.iter().any(|k| k == &m.staging),
                    _ => false,
                };
                if row.health == Health::Ok && same && scene_is_m {
                    let mut leftover = Vec::new();
                    if choice == MoveResolution::Abandon {
                        let admin = shadow_dir.join("worktrees").join(m.n.to_string());
                        for p in [m.staging.clone(), admin] {
                            if present(&p) {
                                leftover.push(p);
                            }
                        }
                    }
                    return Ok(ResolutionReport {
                        session: session.clone(),
                        repo: repo.clone(),
                        choice,
                        n: m.n,
                        base: ws.base.clone(),
                        head: ws.head.clone(),
                        old_tree: if choice == MoveResolution::Accept { m.old_tree.clone() } else { None },
                        leftover,
                    });
                }
            }
            return Err(invalid(
                "not_failed",
                format!("workspace {session}/{repo} has not failed a move"),
                json!({ "session": session.as_str(), "repo": repo.as_str(), "choice": choice.as_str() }),
            ));
        }
        let shadow = git.shadow_at(&shadow_dir)?;
        let wt = shadow.dir().join("worktrees");
        // 2.
        let m = match latest {
            Some(m) if m.state == MoveState::Unknown => m.clone(),
            _ => {
                return Err(invalid(
                    "no_failed_move",
                    format!("the latest move of {session}/{repo} did not fail"),
                    json!({ "session": session.as_str(), "repo": repo.as_str(), "n": latest.map(|m| m.n) }),
                ));
            }
        };
        let (n, s, h1) = (m.n, m.staging.clone(), m.head.clone());
        let phase = m.phase.clone().unwrap_or_default();
        let (g, h0) = (row.generation, ws.head.clone());
        // 3.
        // A directory `.git` in the workspace is no gitfile either; one in S
        // is attach stopped at its step 3 (the tree never attached).
        let k_ws = Gitdir::read(git, &ws.path)?;
        let k_s = Gitdir::read(git, &s)?;
        let pin = pin_ref(session);
        let p = git.read_ref(&pin)?;
        let at_g = wt.join(g.to_string());
        let at_n = wt.join(n.to_string());
        let not_swapped = k_ws.is(&at_g)
            && (k_s.is(&at_n) || k_s == Gitdir::Unusable(Some(Code::IsDirectory)))
            && p.as_ref() == Some(&h0);
        let swapped = k_ws.is(&at_n) && k_s.is(&at_g) && (p.as_ref() == Some(&h0) || p.as_ref() == Some(&h1));
        // 4.
        let abandon_fits = not_swapped && matches!(phase.as_str(), "recheck" | "swap");
        let accept_fits = swapped && matches!(phase.as_str(), "swap" | "pin" | "record");
        let fits = match choice {
            MoveResolution::Abandon => abandon_fits,
            MoveResolution::Accept => accept_fits,
        };
        if !fits {
            let matches = if abandon_fits {
                json!("abandon")
            } else if accept_fits {
                json!("accept")
            } else {
                Value::Null
            };
            let mut found = Map::new();
            found.insert("workspace_gitdir".into(), k_ws.found());
            found.insert("staging_gitdir".into(), k_s.found());
            found.insert("pin".into(), p.as_ref().map(|c| json!(c)).unwrap_or(Value::Null));
            if let Some(c) = k_ws.refusal() {
                found.insert("workspace_refusal".into(), json!(c));
            }
            if let Some(c) = k_s.refusal() {
                found.insert("staging_refusal".into(), json!(c));
            }
            return Err(invalid(
                "scene_mismatch",
                format!(
                    "the scene of move {n} of {session}/{repo} does not fit {}; nothing was changed",
                    choice.as_str()
                ),
                json!({
                    "session": session.as_str(),
                    "repo": repo.as_str(),
                    "choice": choice.as_str(),
                    "n": n,
                    "phase": phase,
                    "found": Value::Object(found),
                    "matches": matches,
                }),
            ));
        }

        match choice {
            MoveResolution::Abandon => {
                // 5.
                self.store.resolve_move(session, repo, n, false, SystemTime::now())?;
                // 6. failures only leave leftovers.
                let mut leftover = Vec::new();
                match self.seams.hit("resolve.retire", Some(repo)) {
                    Err(e) if is_stop(&e) => return Err(e),
                    Err(_) => {
                        for p in [s.clone(), at_n.clone()] {
                            if present(&p) {
                                leftover.push(p);
                            }
                        }
                    }
                    Ok(()) => {
                        if self.retire_present(&s).is_err() && present(&s) {
                            leftover.push(s.clone());
                        }
                        if self.collect_admin(shadow.dir(), n).is_err() {
                            let admin = shadow.dir().join("worktrees").join(n.to_string());
                            if present(&admin) {
                                leftover.push(admin);
                            } else if present(&wt) {
                                leftover.push(wt.clone());
                            }
                        }
                    }
                }
                Ok(ResolutionReport {
                    session: session.clone(),
                    repo: repo.clone(),
                    choice,
                    n,
                    base: ws.base.clone(),
                    head: h0,
                    old_tree: None,
                    leftover,
                })
            }
            MoveResolution::Accept => {
                // 7.
                self.seams.hit("resolve.pin", Some(repo))?;
                strata_apfs::full_fsync(&strata_apfs::open_root(&rec.manifest.root)?)?;
                let moved = p.as_ref() != Some(&h1);
                if moved {
                    self.with_lock(repo, || git.update_ref(&pin, &h1, Some(&h0)))?;
                }
                // 8.
                let recorded = self
                    .seams
                    .hit("resolve.record", Some(repo))
                    .and_then(|()| self.store.resolve_move(session, repo, n, true, SystemTime::now()));
                if let Err(e) = recorded {
                    if is_stop(&e) || !moved {
                        return Err(e);
                    }
                    let mut extra = Map::new();
                    extra.insert("repo".into(), json!(repo.as_str()));
                    extra.insert("n".into(), json!(n));
                    return Err(unknown_error_named(
                        "resolve",
                        session,
                        Some(repo),
                        "record",
                        cause_of(&e),
                        &[ws.path.clone(), s.clone()],
                        extra,
                    ));
                }
                // 9.
                Ok(ResolutionReport {
                    session: session.clone(),
                    repo: repo.clone(),
                    choice,
                    n,
                    base: m.to.clone(),
                    head: h1,
                    old_tree: Some(s),
                    leftover: Vec::new(),
                })
            }
        }
    }
}

//! Sessions, workspaces, seeds and move intents (store.md §5.7, §5.10).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use strata_types::{BaseGen, Code, CommitOid, RepoName, RepoPath, SessionId, SessionManifest, StrataError, WorkspaceState};

use crate::codec::*;
use crate::errors::{NOT_WRITTEN, bad_row, err, invalid, invalid_field, no_such_repo, no_such_session, sql, sql_read, too_large};
use crate::txn::Tx;
use crate::{Health, Lifecycle, MoveIntent, MoveState, Result, SeedRow, SessionRecord, Store, WorkspaceRecord};

// ---------------------------------------------------------------- helpers

fn w(op: &'static str) -> impl Fn(rusqlite::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

fn lifecycle_err(id: &str, lc: &str, want: &str) -> StrataError {
    err(
        Code::InvalidInput,
        format!("session {id} is {lc}; this needs {want}"),
        json!({ "reason": "lifecycle", "session": id, "lifecycle": lc }),
    )
}

fn no_such_intent(id: &str, repo: &str, n: u64) -> StrataError {
    err(
        Code::InvalidInput,
        format!("no move intent {n} for {id}/{repo}"),
        json!({ "reason": "no_such_intent", "session": id, "repo": repo, "n": n }),
    )
}

fn intent_state_err(id: &str, repo: &str, n: u64, state: &str, want: &str) -> StrataError {
    err(
        Code::InvalidInput,
        format!("move intent {n} of {id}/{repo} is {state}; this needs {want}"),
        json!({ "reason": "intent_state", "session": id, "repo": repo, "n": n, "state": state }),
    )
}

fn conflicts_json(c: &[RepoPath]) -> String {
    serde_json::to_string(&c.iter().map(RepoPath::as_str).collect::<Vec<_>>()).expect("string array")
}

fn conflicts_of(s: &str, what: &str) -> Result<Vec<RepoPath>> {
    let v: Vec<String> = from_json(s, what)?;
    v.into_iter().map(|p| parse(p, "conflict path")).collect()
}

fn scene_of(s: Option<String>, what: &str) -> Result<Option<Value>> {
    s.map(|s| from_json(&s, what)).transpose()
}

pub(crate) fn session_lifecycle(c: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT lifecycle FROM sessions WHERE id = ?1", [id], |r| r.get(0)).optional()
}

pub(crate) struct Ws {
    pub(crate) tracked: bool,
    generation: i64,
    health: String,
}

pub(crate) fn workspace(c: &Connection, id: &str, repo: &str) -> rusqlite::Result<Option<Ws>> {
    c.query_row(
        "SELECT tracked, generation, health FROM workspaces WHERE session = ?1 AND repo = ?2",
        [id, repo],
        |r| Ok(Ws { tracked: r.get::<_, i64>(0)? == 1, generation: r.get(1)?, health: r.get(2)? }),
    )
    .optional()
}

struct IntentRow {
    state: String,
    conflicts: Option<String>,
    to_tree: String,
    to_deps: String,
    head: String,
    old_tree: Option<String>,
}

fn intent(c: &Connection, id: &str, repo: &str, n: i64) -> rusqlite::Result<Option<IntentRow>> {
    c.query_row(
        "SELECT state, conflicts, to_tree, to_deps, head, old_tree FROM move_intents \
         WHERE session = ?1 AND repo = ?2 AND n = ?3",
        params![id, repo, n],
        |r| {
            Ok(IntentRow {
                state: r.get(0)?,
                conflicts: r.get(1)?,
                to_tree: r.get(2)?,
                to_deps: r.get(3)?,
                head: r.get(4)?,
                old_tree: r.get(5)?,
            })
        },
    )
    .optional()
}

pub(crate) fn base_gen_id(c: &Connection, repo: &str, tree: &str, deps: &str) -> rusqlite::Result<Option<i64>> {
    c.query_row("SELECT id FROM base_gens WHERE repo = ?1 AND tree = ?2 AND deps = ?3", [repo, tree, deps], |r| r.get(0))
        .optional()
}

fn base_gen_missing(repo: &str, tree: &str, deps: &str) -> StrataError {
    err(
        Code::Corrupt,
        format!("the target base generation {repo}/{tree}/{deps} of an open move intent has no row"),
        json!({ "reason": "base_gen_missing", "repo": repo, "tree": tree, "deps": deps }),
    )
}

/// A move intent's `n` going into a query (`n` ≥ 2 in every row).
fn n_int(n: u64) -> Result<i64> {
    int(n, "n")
}

impl Store {
    /// `<root>/sessions/<id>`, as configured (the text callers use).
    pub(crate) fn session_root_text(&self, id: &str) -> Result<String> {
        Ok(format!("{}/sessions/{id}", path_text(&self.root, "root")?.trim_end_matches('/')))
    }

    /// `p` = `<root>/sessions/<id>/<name>` with `<name>` one component;
    /// returns `(text of p, name)`.
    pub(crate) fn session_child<'a>(&self, id: &str, p: &'a Path) -> Option<(&'a str, &'a str)> {
        let t = p.to_str()?;
        let prefix = self.session_root_text(id).ok()?;
        let name = t.strip_prefix(&prefix)?.strip_prefix('/')?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
            return None;
        }
        Some((t, name))
    }
}

/// `p` is a one-component child of the session root that is not a workspace
/// directory; returns its text.
fn staging_shape<'a>(s: &Store, c: &Connection, id: &str, p: &'a Path, field: &str, op: &'static str) -> Result<&'a str> {
    let bad = || {
        err(
            Code::InvalidInput,
            format!("{field} must be <root>/sessions/{id}/<name>, not a workspace directory: {}", p.display()),
            json!({ "field": field, "reason": "staging_shape", "path": p.to_string_lossy() }),
        )
    };
    let (text, name) = s.session_child(id, p).ok_or_else(bad)?;
    let is_ws: bool = c
        .query_row("SELECT EXISTS (SELECT 1 FROM workspaces WHERE session = ?1 AND repo = ?2)", [id, name], |r| r.get(0))
        .map_err(w(op))?;
    if is_ws {
        return Err(bad());
    }
    Ok(text)
}

fn allowed_edge(from: Lifecycle, to: Lifecycle) -> bool {
    use Lifecycle::*;
    matches!(
        (from, to),
        (Creating, Active) | (Creating, Unknown) | (Active, Removing) | (Removing, Removed) | (Removing, Unknown) | (Unknown, Removing)
    )
}

// ---------------------------------------------------------------- row mappers

fn seed_row(r: &Row<'_>) -> rusqlite::Result<Result<(String, SeedRow)>> {
    let repo: String = r.get(0)?;
    let path: String = r.get(1)?;
    let source: String = r.get(2)?;
    let link: Option<String> = r.get(3)?;
    let size: i64 = r.get(4)?;
    let mtime: i64 = r.get(5)?;
    let copied: i64 = r.get(6)?;
    Ok((|| {
        Ok((
            repo,
            SeedRow {
                path: parse(path, "seed path")?,
                source: pathbuf(source),
                link: link.map(pathbuf),
                size: nonneg(size, "seed size")?,
                source_mtime: time(mtime),
                copied: time(copied),
            },
        ))
    })())
}

const INTENT_COLS: &str =
    "session, repo, n, to_tree, to_deps, head, staging, phase, state, conflicts, old_tree, started_ns, ended_ns";

fn intent_row(r: &Row<'_>) -> rusqlite::Result<Result<MoveIntent>> {
    let session: String = r.get(0)?;
    let repo: String = r.get(1)?;
    let n: i64 = r.get(2)?;
    let to_tree: String = r.get(3)?;
    let to_deps: String = r.get(4)?;
    let head: String = r.get(5)?;
    let staging: String = r.get(6)?;
    let phase: Option<String> = r.get(7)?;
    let state: String = r.get(8)?;
    let conflicts: Option<String> = r.get(9)?;
    let old_tree: Option<String> = r.get(10)?;
    let started: i64 = r.get(11)?;
    let ended: Option<i64> = r.get(12)?;
    Ok((|| {
        let state = move_state_of(&state)?;
        if n < 2 || matches!(state, MoveState::Preparing | MoveState::Swapping) != ended.is_none() {
            return Err(bad_row(format!("move intent {session}/{repo}/{n}: state and columns disagree")));
        }
        let repo: RepoName = parse(repo, "repo")?;
        Ok(MoveIntent {
            session: parse(session, "session id")?,
            n: n as u64,
            to: BaseGen { repo: repo.clone(), tree: parse(to_tree, "tree")?, deps: parse(to_deps, "deps set id")? },
            repo,
            head: parse(head, "commit")?,
            staging: pathbuf(staging),
            phase,
            state,
            conflicts: conflicts.map(|c| conflicts_of(&c, "move_intents.conflicts")).transpose()?,
            old_tree: old_tree.map(pathbuf),
            started: time(started),
            ended: opt_time(ended),
        })
    })())
}

fn collect<T>(c: &Connection, q: &str, p: impl rusqlite::Params, f: fn(&Row<'_>) -> rusqlite::Result<Result<T>>) -> Result<Vec<T>> {
    let mut st = c.prepare(q).map_err(sql_read)?;
    let rows = st.query_map(p, f).map_err(sql_read)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(sql_read)??);
    }
    Ok(out)
}

struct SessionRow {
    id: String,
    ticket: Option<String>,
    root: String,
    lifecycle: String,
    phase: Option<String>,
    scene: Option<String>,
    created: i64,
    removed: Option<i64>,
}

fn session_rows(c: &Connection, id: Option<&str>) -> Result<Vec<SessionRow>> {
    let mut st = c
        .prepare(
            "SELECT id, ticket, root, lifecycle, phase, scene, created_ns, removed_ns FROM sessions \
             WHERE ?1 IS NULL OR id = ?1 ORDER BY created_ns, id",
        )
        .map_err(sql_read)?;
    st.query_map([id], |r| {
        Ok(SessionRow {
            id: r.get(0)?,
            ticket: r.get(1)?,
            root: r.get(2)?,
            lifecycle: r.get(3)?,
            phase: r.get(4)?,
            scene: r.get(5)?,
            created: r.get(6)?,
            removed: r.get(7)?,
        })
    })
    .map_err(sql_read)?
    .collect::<rusqlite::Result<Vec<_>>>()
    .map_err(sql_read)
}

fn load_session(c: &Connection, s: SessionRow) -> Result<SessionRecord> {
    let lifecycle = lifecycle_of(&s.lifecycle)?;
    if (lifecycle == Lifecycle::Removed) != s.removed.is_some() {
        return Err(bad_row(format!("session {}: lifecycle and removed_ns disagree", s.id)));
    }
    let mut seeds: BTreeMap<String, Vec<SeedRow>> = BTreeMap::new();
    for (repo, seed) in collect(
        c,
        "SELECT repo, path, source, link, size, source_mtime_ns, copied_ns FROM seeds WHERE session = ?1 ORDER BY repo, path",
        [&s.id],
        seed_row,
    )? {
        seeds.entry(repo).or_default().push(seed);
    }
    let mut st = c
        .prepare(
            "SELECT w.repo, g.tree, g.deps, w.head, w.path, w.tracked, w.generation, w.health, w.scene, w.conflicts \
             FROM workspaces w LEFT JOIN base_gens g ON g.repo = w.repo AND g.id = w.base_gen \
             WHERE w.session = ?1 ORDER BY w.repo",
        )
        .map_err(sql_read)?;
    type WsTuple = (String, Option<String>, Option<String>, String, String, i64, i64, String, Option<String>, String);
    let rows: Vec<WsTuple> = st
        .query_map([&s.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))
        })
        .map_err(sql_read)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_read)?;
    let mut repos = BTreeMap::new();
    let mut workspaces = BTreeMap::new();
    for (repo, tree, deps, head, path, tracked, generation, health, scene, conflicts) in rows {
        let (Some(tree), Some(deps)) = (tree, deps) else {
            return Err(bad_row(format!("workspace {}/{repo}: its base generation has no row", s.id)));
        };
        let name: RepoName = parse(repo.clone(), "repo")?;
        let health = health_of(&health)?;
        let scene = scene_of(scene, "workspaces.scene")?;
        if health == Health::Unknown && scene.is_none() {
            return Err(bad_row(format!("workspace {}/{repo}: unknown health without a scene", s.id)));
        }
        repos.insert(
            name.clone(),
            WorkspaceState {
                base: BaseGen { repo: name.clone(), tree: parse(tree, "tree")?, deps: parse(deps, "deps set id")? },
                head: parse(head, "commit")?,
                path: pathbuf(path),
                tracked: tracked == 1,
            },
        );
        workspaces.insert(
            name,
            WorkspaceRecord {
                generation: seq(generation, "generation")?,
                health,
                scene,
                conflicts: conflicts_of(&conflicts, "workspaces.conflicts")?,
                seeds: seeds.remove(&repo).unwrap_or_default(),
            },
        );
    }
    Ok(SessionRecord {
        manifest: SessionManifest { id: parse(s.id, "session id")?, ticket: s.ticket, root: pathbuf(s.root), repos },
        lifecycle,
        created: time(s.created),
        removed: opt_time(s.removed),
        phase: s.phase,
        scene: scene_of(s.scene, "sessions.scene")?,
        workspaces,
    })
}

// ---------------------------------------------------------------- writes

impl Store {
    pub fn create_session(&self, manifest: &SessionManifest, at: SystemTime) -> Result<()> {
        const OP: &str = "create_session";
        self.writable()?;
        let id = manifest.id.as_str();
        if manifest.repos.is_empty() {
            return Err(invalid_field("repos", format!("session {id} has no repos")));
        }
        let root = self.session_root_text(id)?;
        if manifest.root.to_str() != Some(root.as_str()) {
            return Err(err(
                Code::InvalidInput,
                format!("session root must be {root}, not {}", manifest.root.display()),
                json!({ "field": "root", "expected": root }),
            ));
        }
        let mut ws = Vec::new();
        for (repo, st) in &manifest.repos {
            let want = format!("{root}/{}", repo.as_str());
            if st.path.to_str() != Some(want.as_str()) {
                return Err(err(
                    Code::InvalidInput,
                    format!("workspace path of {repo} must be {want}, not {}", st.path.display()),
                    json!({ "field": "path", "repo": repo.as_str(), "expected": want }),
                ));
            }
            if st.base.repo != *repo {
                return Err(err(
                    Code::InvalidInput,
                    format!("workspace {repo} names a base of {}", st.base.repo),
                    json!({ "reason": "repo_mismatch", "repo": repo.as_str(), "base_repo": st.base.repo.as_str() }),
                ));
            }
            ws.push((repo.as_str(), st, want));
        }
        let at = ns(at, "at")?;
        self.write(OP, |c| {
            if session_lifecycle(c, id).map_err(w(OP))?.is_some() {
                return Err(err(Code::Exists, format!("session {id} already has a row"), json!({ "reason": "store_row", "session": id })));
            }
            let taken: Option<String> =
                c.query_row("SELECT id FROM sessions WHERE root = ?1", [&root], |r| r.get(0)).optional().map_err(w(OP))?;
            if let Some(other) = taken {
                return Err(err(Code::Exists, format!("{root} belongs to session {other}"), json!({ "reason": "root_taken", "path": root, "session": other })));
            }
            let mut gens = Vec::new();
            for (repo, st, path) in &ws {
                let g = base_gen_id(c, repo, st.base.tree.as_str(), st.base.deps.as_str()).map_err(w(OP))?;
                let Some(g) = g else {
                    return Err(err(
                        Code::InvalidInput,
                        format!("base {}/{}/{} is not registered", repo, st.base.tree, st.base.deps),
                        json!({ "reason": "base_unregistered", "repo": repo, "tree": st.base.tree.as_str(), "deps": st.base.deps.as_str() }),
                    ));
                };
                let other: Option<String> =
                    c.query_row("SELECT session FROM workspaces WHERE path = ?1", [path], |r| r.get(0)).optional().map_err(w(OP))?;
                if let Some(other) = other {
                    return Err(err(Code::Exists, format!("{path} belongs to session {other}"), json!({ "reason": "path_taken", "path": path, "session": other })));
                }
                gens.push(g);
            }
            c.execute(
                "INSERT INTO sessions (id, ticket, root, lifecycle, created_ns) VALUES (?1, ?2, ?3, 'creating', ?4)",
                params![id, manifest.ticket, root, at],
            )
            .map_err(w(OP))?;
            for ((repo, st, path), g) in ws.iter().zip(gens) {
                c.execute(
                    "INSERT INTO workspaces (session, repo, base_gen, head, path, tracked, generation, health, conflicts) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'ok', '[]')",
                    params![id, repo, g, st.head.as_str(), path, st.tracked as i64],
                )
                .map_err(w(OP))?;
            }
            Ok(Tx::Commit(()))
        })
    }

    pub fn set_session_phase(&self, id: &SessionId, phase: &str) -> Result<()> {
        const OP: &str = "set_session_phase";
        self.writable()?;
        let id = id.as_str();
        self.write(OP, |c| {
            let lc = session_lifecycle(c, id).map_err(w(OP))?.ok_or_else(|| no_such_session(id))?;
            if lc != "creating" && lc != "removing" {
                return Err(lifecycle_err(id, &lc, "creating or removing"));
            }
            c.execute("UPDATE sessions SET phase = ?2 WHERE id = ?1", params![id, phase]).map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    pub fn set_lifecycle(&self, id: &SessionId, from: Lifecycle, to: Lifecycle, scene: Option<&Value>, at: SystemTime) -> Result<bool> {
        const OP: &str = "set_lifecycle";
        self.writable()?;
        if !allowed_edge(from, to) {
            return Err(err(
                Code::InvalidInput,
                format!("lifecycle {from:?} → {to:?} is not allowed"),
                json!({ "reason": "transition", "from": lifecycle_text(from), "to": lifecycle_text(to) }),
            ));
        }
        if (to == Lifecycle::Unknown) != scene.is_some() {
            return Err(err(
                Code::InvalidInput,
                "a scene goes with entering unknown, and only with that",
                json!({ "reason": "scene", "field": "scene", "to": lifecycle_text(to) }),
            ));
        }
        let scene = scene.map(|s| to_json(s, "scene")).transpose()?;
        let removed = if to == Lifecycle::Removed { Some(ns(at, "at")?) } else { None };
        let id = id.as_str();
        self.write(OP, |c| {
            let n = c
                .execute(
                    "UPDATE sessions SET lifecycle = ?3, \
                       scene = CASE WHEN ?4 IS NULL THEN scene ELSE ?4 END, \
                       removed_ns = CASE WHEN ?5 IS NULL THEN removed_ns ELSE ?5 END \
                     WHERE id = ?1 AND lifecycle = ?2",
                    params![id, lifecycle_text(from), lifecycle_text(to), scene, removed],
                )
                .map_err(w(OP))?;
            if n == 1 {
                return Ok(Tx::Commit(true));
            }
            session_lifecycle(c, id).map_err(w(OP))?.ok_or_else(|| no_such_session(id))?;
            Ok(Tx::Rollback(false))
        })
    }

    pub fn record_seeds(&self, id: &SessionId, repo: &RepoName, seeds: &[SeedRow]) -> Result<()> {
        const OP: &str = "record_seeds";
        self.writable()?;
        let mut rows = Vec::with_capacity(seeds.len());
        for s in seeds {
            rows.push((
                s.path.as_str(),
                path_text(&s.source, "source")?,
                s.link.as_deref().map(|l| path_text(l, "link")).transpose()?,
                int(s.size, "size")?,
                ns(s.source_mtime, "source_mtime")?,
                ns(s.copied, "copied")?,
            ));
        }
        let (id, repo) = (id.as_str(), repo.as_str());
        self.write(OP, |c| {
            let lc = session_lifecycle(c, id).map_err(w(OP))?.ok_or_else(|| no_such_session(id))?;
            if lc != "creating" {
                return Err(lifecycle_err(id, &lc, "creating"));
            }
            workspace(c, id, repo).map_err(w(OP))?.ok_or_else(|| no_such_repo(id, repo))?;
            let mut seen = BTreeSet::new();
            for r in &rows {
                let dup = !seen.insert(r.0)
                    || c.query_row(
                        "SELECT EXISTS (SELECT 1 FROM seeds WHERE session = ?1 AND repo = ?2 AND path = ?3)",
                        [id, repo, r.0],
                        |x| x.get::<_, bool>(0),
                    )
                    .map_err(w(OP))?;
                if dup {
                    return Err(err(
                        Code::InvalidInput,
                        format!("seed {} of {id}/{repo} is already recorded", r.0),
                        json!({ "reason": "seed_exists", "path": r.0 }),
                    ));
                }
            }
            if rows.is_empty() {
                return Ok(Tx::Rollback(()));
            }
            let mut st = c
                .prepare(
                    "INSERT INTO seeds (session, repo, path, source, link, size, source_mtime_ns, copied_ns) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(w(OP))?;
            for r in &rows {
                st.execute(params![id, repo, r.0, r.1, r.2, r.3, r.4, r.5]).map_err(w(OP))?;
            }
            Ok(Tx::Commit(()))
        })
    }

    pub fn begin_move(&self, id: &SessionId, repo: &RepoName, to: &BaseGen, head: &CommitOid, staging: &Path, at: SystemTime) -> Result<u64> {
        const OP: &str = "begin_move";
        self.writable()?;
        let at = ns(at, "at")?;
        let (id, repo) = (id.as_str(), repo.as_str());
        self.write(OP, |c| {
            let lc = session_lifecycle(c, id).map_err(w(OP))?;
            let lc = match lc.as_deref() {
                None | Some("removed") => return Err(no_such_session(id)),
                Some(l) => l.to_string(),
            };
            if lc != "active" {
                return Err(lifecycle_err(id, &lc, "active"));
            }
            let ws = workspace(c, id, repo).map_err(w(OP))?.ok_or_else(|| no_such_repo(id, repo))?;
            if !ws.tracked {
                return Err(err(Code::InvalidInput, format!("workspace {id}/{repo} is untracked and never moves"), json!({ "reason": "untracked", "repo": repo })));
            }
            if ws.health != "ok" {
                return Err(err(Code::InvalidInput, format!("workspace {id}/{repo} has unknown health"), json!({ "reason": "health_unknown", "repo": repo })));
            }
            if to.repo.as_str() != repo {
                return Err(err(
                    Code::InvalidInput,
                    format!("target base belongs to {}, not {repo}", to.repo),
                    json!({ "reason": "repo_mismatch", "repo": repo, "base_repo": to.repo.as_str() }),
                ));
            }
            if base_gen_id(c, repo, to.tree.as_str(), to.deps.as_str()).map_err(w(OP))?.is_none() {
                return Err(err(
                    Code::InvalidInput,
                    format!("target base {repo}/{}/{} is not registered", to.tree, to.deps),
                    json!({ "reason": "base_unregistered", "repo": repo, "tree": to.tree.as_str(), "deps": to.deps.as_str() }),
                ));
            }
            let staging = staging_shape(self, c, id, staging, "staging", OP)?;
            let open: Option<i64> = c
                .query_row(
                    "SELECT n FROM move_intents WHERE session = ?1 AND repo = ?2 AND state IN ('preparing', 'swapping')",
                    [id, repo],
                    |r| r.get(0),
                )
                .optional()
                .map_err(w(OP))?;
            if let Some(n) = open {
                return Err(err(Code::Exists, format!("workspace {id}/{repo} already has open move intent {n}"), json!({ "n": n, "repo": repo })));
            }
            let max: i64 = c
                .query_row("SELECT COALESCE(MAX(n), 0) FROM move_intents WHERE session = ?1 AND repo = ?2", [id, repo], |r| r.get(0))
                .map_err(w(OP))?;
            let n = ws.generation.max(max).checked_add(1).ok_or_else(|| too_large("move_generation"))?;
            c.execute(
                "INSERT INTO move_intents (session, repo, n, to_tree, to_deps, head, staging, state, started_ns) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'preparing', ?8)",
                params![id, repo, n, to.tree.as_str(), to.deps.as_str(), head.as_str(), staging, at],
            )
            .map_err(w(OP))?;
            Ok(Tx::Commit(n as u64))
        })
    }

    pub fn set_move_phase(&self, id: &SessionId, repo: &RepoName, n: u64, phase: &str) -> Result<()> {
        const OP: &str = "set_move_phase";
        self.writable()?;
        let (id, repo, ni) = (id.as_str(), repo.as_str(), n_int(n)?);
        self.write(OP, |c| {
            let it = intent(c, id, repo, ni).map_err(w(OP))?.ok_or_else(|| no_such_intent(id, repo, n))?;
            if it.state != "preparing" && it.state != "swapping" {
                return Err(intent_state_err(id, repo, n, &it.state, "preparing or swapping"));
            }
            c.execute("UPDATE move_intents SET phase = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3", params![id, repo, ni, phase])
                .map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_move_state(
        &self,
        id: &SessionId,
        repo: &RepoName,
        n: u64,
        from: MoveState,
        to: MoveState,
        conflicts: Option<&[RepoPath]>,
        at: SystemTime,
    ) -> Result<bool> {
        const OP: &str = "set_move_state";
        self.writable()?;
        if from != MoveState::Preparing || !matches!(to, MoveState::Swapping | MoveState::Refused | MoveState::Abandoned) {
            return Err(err(
                Code::InvalidInput,
                format!("move intent {from:?} → {to:?} is not allowed here"),
                json!({ "reason": "transition", "from": move_state_text(from), "to": move_state_text(to) }),
            ));
        }
        if (to == MoveState::Swapping) != conflicts.is_some() {
            return Err(err(
                Code::InvalidInput,
                "conflicts go with entering swapping, and only with that",
                json!({ "reason": "conflicts", "field": "conflicts", "to": move_state_text(to) }),
            ));
        }
        let conflicts = conflicts.map(conflicts_json);
        let ended = if to == MoveState::Swapping { None } else { Some(ns(at, "at")?) };
        let (id, repo, ni) = (id.as_str(), repo.as_str(), n_int(n)?);
        self.write(OP, |c| {
            intent(c, id, repo, ni).map_err(w(OP))?.ok_or_else(|| no_such_intent(id, repo, n))?;
            if to == MoveState::Swapping {
                let lc = session_lifecycle(c, id).map_err(w(OP))?.unwrap_or_default();
                if lc != "active" {
                    return Err(lifecycle_err(id, &lc, "active"));
                }
            }
            let k = c
                .execute(
                    "UPDATE move_intents SET state = ?5, conflicts = COALESCE(?6, conflicts), ended_ns = ?7 \
                     WHERE session = ?1 AND repo = ?2 AND n = ?3 AND state = ?4",
                    params![id, repo, ni, move_state_text(from), move_state_text(to), conflicts, ended],
                )
                .map_err(w(OP))?;
            Ok(if k == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    pub fn finish_move(&self, id: &SessionId, repo: &RepoName, n: u64, conflicts: &[RepoPath], at: SystemTime) -> Result<()> {
        const OP: &str = "finish_move";
        self.writable()?;
        let at = ns(at, "at")?;
        let (id, repo, ni) = (id.as_str(), repo.as_str(), n_int(n)?);
        self.write(OP, |c| {
            let it = intent(c, id, repo, ni).map_err(w(OP))?.ok_or_else(|| no_such_intent(id, repo, n))?;
            if it.state != "swapping" {
                return Err(intent_state_err(id, repo, n, &it.state, "swapping"));
            }
            let recorded = it.conflicts.as_deref().map(|s| conflicts_of(s, "move_intents.conflicts")).transpose()?.unwrap_or_default();
            if recorded != conflicts {
                return Err(err(
                    Code::InvalidInput,
                    format!("conflicts differ from those recorded when intent {n} of {id}/{repo} entered swapping"),
                    json!({ "reason": "conflicts_mismatch", "n": n }),
                ));
            }
            let g = base_gen_id(c, repo, &it.to_tree, &it.to_deps)
                .map_err(w(OP))?
                .ok_or_else(|| base_gen_missing(repo, &it.to_tree, &it.to_deps))?;
            c.execute(
                "UPDATE workspaces SET base_gen = ?3, head = ?4, generation = ?5, conflicts = ?6 WHERE session = ?1 AND repo = ?2",
                params![id, repo, g, it.head, ni, conflicts_json(&recorded)],
            )
            .map_err(w(OP))?;
            c.execute(
                "UPDATE move_intents SET state = 'done', ended_ns = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3",
                params![id, repo, ni, at],
            )
            .map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    pub fn fail_move(&self, id: &SessionId, repo: &RepoName, n: u64, scene: &Value, at: SystemTime) -> Result<()> {
        const OP: &str = "fail_move";
        self.writable()?;
        let at = ns(at, "at")?;
        let scene = to_json(scene, "scene")?;
        let (id, repo, ni) = (id.as_str(), repo.as_str(), n_int(n)?);
        self.write(OP, |c| {
            let it = intent(c, id, repo, ni).map_err(w(OP))?.ok_or_else(|| no_such_intent(id, repo, n))?;
            if it.state != "swapping" {
                return Err(intent_state_err(id, repo, n, &it.state, "swapping"));
            }
            c.execute(
                "UPDATE move_intents SET state = 'unknown', ended_ns = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3",
                params![id, repo, ni, at],
            )
            .map_err(w(OP))?;
            c.execute("UPDATE workspaces SET health = 'unknown', scene = ?3 WHERE session = ?1 AND repo = ?2", params![id, repo, scene])
                .map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    pub fn resolve_move(&self, id: &SessionId, repo: &RepoName, n: u64, accept: bool, at: SystemTime) -> Result<()> {
        const OP: &str = "resolve_move";
        self.writable()?;
        let at = ns(at, "at")?;
        let (id, repo) = (id.as_str(), repo.as_str());
        self.write(OP, |c| {
            let lc = session_lifecycle(c, id).map_err(w(OP))?;
            let lc = match lc.as_deref() {
                None | Some("removed") => return Err(no_such_session(id)),
                Some(l) => l.to_string(),
            };
            if lc != "active" {
                return Err(lifecycle_err(id, &lc, "active"));
            }
            let ws = workspace(c, id, repo).map_err(w(OP))?.ok_or_else(|| no_such_repo(id, repo))?;
            if ws.health != "unknown" {
                return Err(err(Code::InvalidInput, format!("workspace {id}/{repo} has not failed a move"), json!({ "reason": "not_failed", "repo": repo })));
            }
            let no_failed = || {
                err(
                    Code::InvalidInput,
                    format!("move intent {n} is not the failed (unknown) latest intent of {id}/{repo}"),
                    json!({ "reason": "no_failed_move", "repo": repo, "n": n }),
                )
            };
            let ni = i64::try_from(n).map_err(|_| no_failed())?;
            let max: i64 = c
                .query_row("SELECT COALESCE(MAX(n), 0) FROM move_intents WHERE session = ?1 AND repo = ?2", [id, repo], |r| r.get(0))
                .map_err(w(OP))?;
            let it = intent(c, id, repo, ni).map_err(w(OP))?.ok_or_else(no_failed)?;
            if ni != max || it.state != "unknown" {
                return Err(no_failed());
            }
            if accept {
                let g = base_gen_id(c, repo, &it.to_tree, &it.to_deps)
                    .map_err(w(OP))?
                    .ok_or_else(|| base_gen_missing(repo, &it.to_tree, &it.to_deps))?;
                let conflicts = it.conflicts.clone().ok_or_else(|| bad_row(format!("unknown intent {id}/{repo}/{n} has no conflicts")))?;
                c.execute(
                    "UPDATE workspaces SET health = 'ok', base_gen = ?3, head = ?4, generation = ?5, conflicts = ?6 \
                     WHERE session = ?1 AND repo = ?2",
                    params![id, repo, g, it.head, ni, conflicts],
                )
                .map_err(w(OP))?;
                c.execute(
                    "UPDATE move_intents SET state = 'done', old_tree = staging, ended_ns = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3",
                    params![id, repo, ni, at],
                )
                .map_err(w(OP))?;
            } else {
                c.execute("UPDATE workspaces SET health = 'ok' WHERE session = ?1 AND repo = ?2", [id, repo]).map_err(w(OP))?;
                c.execute(
                    "UPDATE move_intents SET state = 'abandoned', ended_ns = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3",
                    params![id, repo, ni, at],
                )
                .map_err(w(OP))?;
            }
            Ok(Tx::Commit(()))
        })
    }

    pub fn set_old_tree(&self, id: &SessionId, repo: &RepoName, n: u64, path: &Path) -> Result<()> {
        const OP: &str = "set_old_tree";
        self.writable()?;
        let (id, repo, ni) = (id.as_str(), repo.as_str(), n_int(n)?);
        self.write(OP, |c| {
            let it = intent(c, id, repo, ni).map_err(w(OP))?.ok_or_else(|| no_such_intent(id, repo, n))?;
            if it.state != "done" {
                return Err(intent_state_err(id, repo, n, &it.state, "done"));
            }
            if it.old_tree.is_some() {
                return Err(invalid("old_tree_set", format!("move intent {n} of {id}/{repo} already has an old tree")));
            }
            let path = staging_shape(self, c, id, path, "path", OP)?;
            c.execute(
                "UPDATE move_intents SET old_tree = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3",
                params![id, repo, ni, path],
            )
            .map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    // ------------------------------------------------------------ reads

    pub fn session(&self, id: &SessionId) -> Result<Option<SessionRecord>> {
        self.read(|c| session_rows(c, Some(id.as_str()))?.into_iter().next().map(|s| load_session(c, s)).transpose())
    }

    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        self.read(|c| session_rows(c, None)?.into_iter().map(|s| load_session(c, s)).collect())
    }

    pub fn move_intents(&self, id: &SessionId) -> Result<Vec<MoveIntent>> {
        let q = format!("SELECT {INTENT_COLS} FROM move_intents WHERE session = ?1 ORDER BY repo, n");
        self.read(|c| collect(c, &q, [id.as_str()], intent_row))
    }

    pub fn open_move_intents(&self) -> Result<Vec<MoveIntent>> {
        let q = format!("SELECT {INTENT_COLS} FROM move_intents WHERE state IN ('preparing', 'swapping') ORDER BY session, repo, n");
        self.read(|c| collect(c, &q, [], intent_row))
    }
}


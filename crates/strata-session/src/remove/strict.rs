//! Auto-close §3.1: what a strict remove reads and says, apart from the
//! agent's. One tracked repo's overlay is let go only by the projection of the
//! whole of it onto the current landed target: the newest own publish on
//! `origin/<default_branch>` that strictly descends from the workspace base.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use strata_gitio::Git;
use strata_overlay::{Base, Overlay};
use strata_store::Store;
use strata_types::{
    Code, OverlayEntry, RepoConfig, RepoName, RepoPath, SessionId, TreeOid, WorkspaceState,
};

use super::{Candidate, Split, candidates, late_line, repo_line, scenes_line, split_on};
use crate::{Result, Sessions};

/// One tracked repo's judgement. The errors are not refusals: every entry stays
/// and the hook reads the repo as unjudged (§4).
pub(super) struct StrictRepo {
    pub(super) remaining: Vec<OverlayEntry>,
    pub(super) landing: Value,
    pub(super) landed: Option<Value>,
    pub(super) projected: Option<(TreeOid, Vec<OverlayEntry>)>,
    pub(super) landed_error: Option<String>,
    pub(super) projection_error: Option<String>,
}

/// What the current landed target did for this overlay.
enum Projection {
    Discharged(TreeOid, Vec<OverlayEntry>),
    /// `landing.why_none`.
    Nothing(&'static str),
}

/// `landing.published`: a publish row with a commit exists for this repo, which
/// tells a never-published edit from one that landed only as a replayed copy.
fn has_published(store: &Store, session: &SessionId, repo: &RepoName) -> Result<bool> {
    Ok(store.publishes()?.iter().any(|r| {
        r.group
            .batches
            .iter()
            .any(|b| &b.session == session && &b.repo == repo)
            && r.outcome.as_ref().is_some_and(|o| {
                o.get("repo").and_then(Value::as_str) == Some(repo.as_str())
                    && o.get("commit").and_then(Value::as_str).is_some()
            })
    }))
}

impl Sessions<'_> {
    /// Candidates first, so a close that discharges everything reads no blobs;
    /// the content match runs over the remainder only, and only to fill
    /// `landing.historical`: nothing it finds is let go.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn judge_strict(
        &self,
        id: &SessionId,
        repo: &RepoName,
        ws: &WorkspaceState,
        git: &Git,
        base: &Base,
        full: &Overlay,
        unstable: &BTreeSet<RepoPath>,
        cfg: &RepoConfig,
    ) -> Result<StrictRepo> {
        let published = has_published(self.store, id, repo)?;
        let mut out = StrictRepo {
            remaining: full.entries.clone(),
            landing: json!({
                "target": null,
                "anchor": format!("refs/strata/remotes/origin/{}", cfg.default_branch),
                "published": published,
            }),
            landed: None,
            projected: None,
            landed_error: None,
            projection_error: None,
        };
        let cands = match candidates(self.store, git, id, repo, &cfg.default_branch) {
            Ok(c) => c,
            Err(e) => {
                out.landed_error = Some(e.message);
                return Ok(out);
            }
        };
        let Some(newest) = cands.first().cloned() else {
            out.landing["why_none"] = json!("no_landed_publish");
            out.landing["historical"] = json!([]);
            return Ok(out);
        };
        match self.onto_target(repo, ws, git, base, full, unstable, &newest) {
            Err(e) => out.projection_error = Some(e.message),
            Ok(Projection::Nothing(why)) => out.landing["why_none"] = json!(why),
            Ok(Projection::Discharged(tree, discharged)) => {
                out.landing["target"] = json!({
                    "publish": newest.publish,
                    "commit": newest.commit.as_str(),
                });
                record_discharged(&mut out, &newest, tree, discharged);
            }
        }
        fill_historical(&mut out, git, cands, unstable);
        Ok(out)
    }

    /// The `newer` guard, then `project` of the whole entry set. A failed read
    /// goes up: `newer`'s `unwrap_or(false)` must not hide it here.
    #[allow(clippy::too_many_arguments)]
    fn onto_target(
        &self,
        repo: &RepoName,
        ws: &WorkspaceState,
        git: &Git,
        base: &Base,
        full: &Overlay,
        unstable: &BTreeSet<RepoPath>,
        newest: &Candidate,
    ) -> Result<Projection> {
        let newer = newest.commit != ws.head
            && match git.is_ancestor(&ws.head, &newest.commit) {
                Ok(b) => b,
                Err(e) if e.code == Code::NoSuchCommit => false,
                Err(e) => return Err(e),
            };
        if !newer {
            return Ok(Projection::Nothing("no_newer_publish"));
        }
        let tree = git.tree_of(&newest.commit)?;
        if tree == base.r#gen.tree {
            return Ok(Projection::Nothing("base_tree"));
        }
        let discharged = self.project(repo, ws, git, base, full, &full.entries, &tree, unstable)?;
        Ok(match discharged {
            Some(d) => Projection::Discharged(tree, d),
            None => Projection::Nothing("no_built_base"),
        })
    }
}

fn record_discharged(
    out: &mut StrictRepo,
    newest: &Candidate,
    tree: TreeOid,
    d: Vec<OverlayEntry>,
) {
    if d.is_empty() {
        return;
    }
    out.remaining.retain(|e| !d.contains(e));
    let shown = Split {
        used: vec![newest.clone()],
        ..Split::default()
    };
    let mut summary = shown.summary();
    summary["count"] = json!(d.len());
    summary["rebased"] = json!(d.len());
    summary["rebased_onto"] = json!(newest.publish);
    out.landed = Some(summary);
    out.projected = Some((tree, d));
}

/// D57's content match over what the projection left: which of those an older
/// publish holds. A refusal is already standing, so a failure only says so.
fn fill_historical(
    out: &mut StrictRepo,
    git: &Git,
    cands: Vec<Candidate>,
    unstable: &BTreeSet<RepoPath>,
) {
    if out.remaining.is_empty() {
        return;
    }
    match split_on(git, cands, out.remaining.clone(), unstable) {
        Ok(sp) => {
            let mut paths: Vec<String> = sp
                .landed
                .iter()
                .map(|e| e.path.as_str().to_owned())
                .collect();
            paths.sort();
            out.landing["historical"] = json!(paths);
        }
        Err(e) => out.landing["historical_error"] = json!(e.message),
    }
}

/// The sections of a strict Dirty refusal.
pub(super) struct StrictSections<'a> {
    pub(super) repos: &'a Map<String, Value>,
    pub(super) landed: &'a Map<String, Value>,
    pub(super) untracked: &'a [Value],
    pub(super) unresolved: &'a [Value],
    pub(super) old_trees: &'a [Value],
    pub(super) embedded: &'a Map<String, Value>,
    pub(super) regenerable: &'a Map<String, Value>,
    pub(super) landing: &'a Map<String, Value>,
}

fn strict_repo_line(repo: &str, entry: &Value, s: &StrictSections<'_>) -> String {
    let untracked = s.untracked.iter().any(|v| v.as_str() == Some(repo));
    let mut line = repo_line(repo, entry, s.landed.get(repo), untracked);
    let regen = s
        .regenerable
        .get(repo)
        .and_then(|r| r.get("count"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if regen > 0 {
        line.push_str(&format!(
            "; {regen} of them match rm.regenerable; a `git-strata rm` of your own lets those go"
        ));
    }
    let landing = s.landing.get(repo);
    let why = landing
        .and_then(|l| l.get("why_none"))
        .and_then(Value::as_str);
    // When the projection stopped for no_built_base, historical is only a
    // diagnostic: do not claim those paths are merely older-publish content.
    let older = landing
        .and_then(|l| l.get("historical"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if older > 0 && why != Some("no_built_base") {
        line.push_str(&format!(
            "; {older} of them have the content of an older publish, not main's current content"
        ));
    }
    line
}

fn kept_tree_line(v: &Value) -> String {
    let changed = v.get("changed").and_then(Value::as_bool).unwrap_or(true);
    format!(
        "{} #{}: pre-move tree kept at {} ({})",
        v.get("repo").and_then(Value::as_str).unwrap_or("?"),
        v.get("n").and_then(Value::as_u64).unwrap_or(0),
        v.get("path").and_then(Value::as_str).unwrap_or("?"),
        if changed { "changed" } else { "unchanged" },
    )
}

fn embedded_lines(embedded: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    for (repo, v) in embedded {
        let paths = v.get("paths").and_then(Value::as_array);
        for p in paths.into_iter().flatten().filter_map(Value::as_str) {
            out.push(format!(
                "{repo}: nested git repository {p}; no publish covers it"
            ));
        }
    }
    out
}

/// The refusal in the daemon's name: it advises no discard, and says of an
/// unchanged tree that it is unchanged.
pub(super) fn strict_dirty_message(prefix: &str, s: &StrictSections<'_>) -> String {
    let mut parts: Vec<String> = s
        .repos
        .iter()
        .map(|(repo, e)| strict_repo_line(repo, e, s))
        .collect();
    parts.extend(embedded_lines(s.embedded));
    parts.extend(scenes_line(s.unresolved));
    for v in s.old_trees {
        parts.push(if v.get("late_writes").is_some() {
            late_line(v)
        } else {
            kept_tree_line(v)
        });
    }
    if parts.is_empty() {
        return prefix.to_string();
    }
    format!("{prefix} {}.", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_each_section_and_never_advises_a_discard() {
        let repos = Map::from_iter([(
            "tool".to_string(),
            json!({ "paths": ["gen/a.json", "src/p.ts"], "count": 2, "truncated": false }),
        )]);
        let regenerable = Map::from_iter([(
            "tool".to_string(),
            json!({ "paths": ["gen/a.json"], "count": 1, "truncated": false }),
        )]);
        let landing = Map::from_iter([("tool".to_string(), json!({ "historical": ["src/p.ts"] }))]);
        let embedded = Map::from_iter([(
            "tool".to_string(),
            json!({ "paths": ["vendor/x"], "count": 1, "truncated": false }),
        )]);
        let old_trees = [
            json!({ "repo": "tool", "n": 3, "path": "/s/old", "changed": false }),
            json!({ "repo": "tool", "n": 4, "path": "/s/old2", "changed": true }),
        ];
        let empty = Map::new();
        let m = strict_dirty_message(
            "PFX",
            &StrictSections {
                repos: &repos,
                landed: &empty,
                untracked: &[],
                unresolved: &[],
                old_trees: &old_trees,
                embedded: &embedded,
                regenerable: &regenerable,
                landing: &landing,
            },
        );
        assert!(m.starts_with("PFX tool: 2 paths not published"), "{m}");
        assert!(
            m.contains(
                "1 of them match rm.regenerable; a `git-strata rm` of your own lets those go"
            ),
            "{m}"
        );
        assert!(
            m.contains(
                "1 of them have the content of an older publish, not main's current content"
            ),
            "{m}"
        );
        assert!(
            m.contains("tool: nested git repository vendor/x; no publish covers it"),
            "{m}"
        );
        assert!(
            m.contains("tool #3: pre-move tree kept at /s/old (unchanged)"),
            "{m}"
        );
        assert!(
            m.contains("tool #4: pre-move tree kept at /s/old2 (changed)"),
            "{m}"
        );
        assert!(!m.contains("--discard"), "{m}");
    }

    #[test]
    fn strict_dirty_message_never_names_discard() {
        // Design §10 row 21: repos, old_trees changed true/false, late_writes,
        // embedded_repos and unresolved → no `--discard`; changed:false reads
        // "unchanged" and never "changed old trees"; the nested path is named.
        // The Agent dirty_message assertions in remove.rs stay untouched.
        let repos = Map::from_iter([(
            "tool".to_string(),
            json!({ "paths": ["src/a.ts"], "count": 1, "truncated": false }),
        )]);
        let embedded = Map::from_iter([(
            "tool".to_string(),
            json!({ "paths": ["vendor/x"], "count": 1, "truncated": false }),
        )]);
        let old_trees = [
            json!({ "repo": "tool", "n": 3, "path": "/s/old", "changed": false }),
            json!({ "repo": "tool", "n": 4, "path": "/s/old2", "changed": true }),
            json!({
                "repo": "tool", "n": 5, "path": "/s/late",
                "late_writes": { "count": 1, "paths": ["src/late.ts"] }
            }),
        ];
        let unresolved = [json!({ "kind": "restore_scene", "repo": "tool", "path": "/s/r" })];
        let empty = Map::new();
        let m = strict_dirty_message(
            "PFX",
            &StrictSections {
                repos: &repos,
                landed: &empty,
                untracked: &[],
                unresolved: &unresolved,
                old_trees: &old_trees,
                embedded: &embedded,
                regenerable: &empty,
                landing: &empty,
            },
        );
        assert!(!m.contains("--discard"), "{m}");
        assert!(m.contains("unchanged"), "{m}");
        assert!(!m.contains("changed old trees"), "{m}");
        assert!(m.contains("vendor/x"), "{m}");
        assert!(m.contains("pre-move tree kept at /s/old2 (changed)"), "{m}");
        assert!(
            m.contains("src/late.ts") || m.contains("written to the pre-move tree"),
            "{m}"
        );
        assert!(m.contains("unresolved"), "{m}");
    }

    #[test]
    fn unresolved_scenes_are_named_without_the_discard_sentence() {
        let empty = Map::new();
        let unresolved = [json!({ "kind": "restore_scene", "repo": "tool", "path": "/s/r" })];
        let m = strict_dirty_message(
            "PFX",
            &StrictSections {
                repos: &empty,
                landed: &empty,
                untracked: &[],
                unresolved: &unresolved,
                old_trees: &[],
                embedded: &empty,
                regenerable: &empty,
                landing: &empty,
            },
        );
        assert_eq!(m, "PFX unresolved: restore scene in tool.");
    }
}

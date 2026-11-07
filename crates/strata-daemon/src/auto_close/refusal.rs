//! What the clauses and the judge's answer say, as codes, a `detail` and the
//! lines of the note (p1-autoclose §2, §4). Pure: no store, no git, no lock.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use strata_types::{Code, ReleaseGroup, RepoName, SessionId, StrataError, Upstream};

/// `validate_mark_note`'s cap, in UTF-8 bytes.
const NOTE_MAX: usize = 2000;
/// How many §2.1 items `detail.outstanding` lists.
const OUTSTANDING_SHOWN: usize = 20;

/// Reasons a session stayed open. A code `add` records is a refusal and makes a
/// mark; one `unread` records is a failed read, which makes none.
#[derive(Debug, Default)]
pub(super) struct Refusals {
    pub(super) reasons: Vec<String>,
    pub(super) detail: Map<String, Value>,
    marked: Vec<String>,
    lines: Vec<String>,
}

fn push_once(list: &mut Vec<String>, code: &str) {
    if !list.iter().any(|c| c == code) {
        list.push(code.to_owned());
    }
}

fn text<'v>(v: &'v Value, key: &str) -> &'v str {
    v.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn strs(v: &Value) -> Vec<&str> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn short(oid: &str) -> String {
    let head: String = oid.chars().take(10).collect();
    format!("{head}…")
}

/// `refs/strata/remotes/origin/main` as `origin/main`.
fn anchor_name(reference: &str) -> &str {
    reference
        .strip_prefix("refs/strata/remotes/")
        .unwrap_or(reference)
}

/// The text a mark can hold: control characters other than tab and newline
/// become a space, and it is cut on a char boundary.
fn fit_note(note: &str) -> String {
    let clean: String = note
        .chars()
        .map(|c| {
            if (c as u32) < 0x20 && c != '\t' && c != '\n' {
                ' '
            } else {
                c
            }
        })
        .collect();
    if clean.len() <= NOTE_MAX {
        return clean;
    }
    let mut end = NOTE_MAX;
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    clean[..end].to_owned()
}

impl Refusals {
    pub(super) fn is_empty(&self) -> bool {
        self.reasons.is_empty()
    }

    /// A refusal. The first clause to name a `detail` key keeps it.
    fn add(&mut self, code: &str, detail: Value, lines: Vec<String>) {
        push_once(&mut self.reasons, code);
        push_once(&mut self.marked, code);
        if let Value::Object(found) = detail {
            for (k, v) in found {
                self.detail.entry(k).or_insert(v);
            }
        }
        self.lines.extend(lines);
    }

    /// A failed read: named in `reasons`, but no refusal.
    pub(super) fn unread(&mut self, code: &str) {
        push_once(&mut self.reasons, code);
    }

    /// The mark's note, naming no trigger, so a re-judge with the same reasons
    /// compares equal. `None` when nothing here was refused.
    pub(super) fn note(&self) -> Option<String> {
        if self.marked.is_empty() {
            return None;
        }
        let mut note = format!(
            "auto-close did not close this session: {}:",
            self.marked.join(", ")
        );
        for line in &self.lines {
            note.push('\n');
            note.push_str(line);
        }
        Some(fit_note(&note))
    }

    /// Clause 4: `items` are `Outstanding::to_json`'s. `reasons` lists each code
    /// once, the first failing item's first; `detail` names the first item and
    /// lists them all, up to the cap.
    pub(super) fn add_outstanding(&mut self, s: &SessionId, items: &[Value]) {
        let Some(first) = items.first() else { return };
        for (n, item) in items.iter().enumerate() {
            let code = text(item, "code");
            let mut detail = if n == 0 { first.clone() } else { json!({}) };
            if n == 0 {
                let shown: Vec<&Value> = items.iter().take(OUTSTANDING_SHOWN).collect();
                detail["outstanding"] = json!(shown);
                detail["more"] = json!(items.len() > OUTSTANDING_SHOWN);
            }
            self.add(code, detail, vec![outstanding_line(item, s)]);
        }
    }

    /// Clause 3.
    pub(super) fn add_upstream(&mut self, upstream: &Upstream) {
        for (repo, u) in &upstream.repos {
            if u.ahead > 0 {
                self.add(
                    "ahead",
                    json!({ "repo": repo.as_str(), "ahead": u.ahead, "base_only": u.base_only }),
                    vec![format!(
                        "{repo}: its base has {} commits main does not",
                        u.ahead
                    )],
                );
            }
            if u.unchecked {
                self.add(
                    "unchecked",
                    json!({ "repo": repo.as_str() }),
                    vec![format!("{repo}: its base could not be compared with main")],
                );
            }
        }
    }

    /// Clause 5: this call's `already_published` entries of the session's repos.
    /// A refused entry fails clause 4 first, so this comes after it.
    pub(super) fn add_refused_pushes(
        &mut self,
        group: &ReleaseGroup,
        s: &SessionId,
        already: &[Value],
    ) {
        for entry in already {
            let repo = text(entry, "repo");
            let mine = group
                .batches
                .iter()
                .any(|b| &b.session == s && b.repo.as_str() == repo);
            let push = entry.get("push").unwrap_or(&Value::Null);
            let refused =
                !push.is_null() && push.get("kind").and_then(Value::as_str) != Some("Pushed");
            if mine && refused {
                self.add(
                    "push_refused",
                    json!({ "repo": repo, "push": push }),
                    vec![format!(
                        "{repo}: the push of an earlier publish was refused; git-strata push {repo}"
                    )],
                );
            }
        }
    }

    /// Clause 6 (a), conflicts: the tracked repos of `upstream`.
    pub(super) fn add_conflicts(&mut self, s: &SessionId, upstream: &Upstream) {
        for (repo, u) in &upstream.repos {
            let stuck = u.auto.as_ref().and_then(|a| a.reason.as_deref()) == Some("conflicts");
            if u.conflicts.is_empty() && !stuck {
                continue;
            }
            let paths: Vec<&str> = u.conflicts.iter().map(|p| p.as_str()).collect();
            let mut item = json!({ "repo": repo.as_str(), "conflicts": paths });
            if stuck {
                item["auto_reason"] = json!("conflicts");
            }
            let lines = conflict_lines(&item, s);
            self.add("conflicts", item, lines);
        }
    }

    /// Clause 6 (a) and (b), proposals: `{repo, proposal, state, own, tracked}`.
    pub(super) fn add_deps(&mut self, s: &SessionId, items: &[Value]) {
        if items.is_empty() {
            return;
        }
        for (n, item) in items.iter().enumerate() {
            let mut detail = if n == 0 { item.clone() } else { json!({}) };
            if n == 0 {
                detail["proposals"] = json!(items);
            }
            self.add("deps_pending", detail, vec![deps_line(item, s)]);
        }
    }

    /// Clause 6 (c): one repo's saved `sync` conflict report.
    pub(super) fn add_report(&mut self, s: &SessionId, repo: &RepoName, paths: &[&str]) {
        if paths.is_empty() {
            return;
        }
        let item = json!({ "repo": repo.as_str(), "conflicts": paths, "report": true });
        let lines = conflict_lines(&item, s);
        self.add("conflicts", item, lines);
    }

    /// Clause 6 (d): the flat keys name the first repo, `assigned` every path.
    pub(super) fn add_assigned(&mut self, assigned: &BTreeMap<RepoName, Vec<String>>) {
        let Some((first, paths)) = assigned.iter().next() else {
            return;
        };
        let lines = assigned
            .iter()
            .flat_map(|(repo, paths)| {
                paths
                    .iter()
                    .map(move |p| format!("{repo}: {p} is assigned to this session"))
            })
            .collect();
        let all: BTreeMap<&str, &Vec<String>> =
            assigned.iter().map(|(r, p)| (r.as_str(), p)).collect();
        self.add(
            "assigned",
            json!({ "repo": first.as_str(), "paths": paths, "assigned": all }),
            lines,
        );
    }
}

fn outstanding_line(item: &Value, s: &SessionId) -> String {
    let repo = text(item, "repo");
    let oid = item
        .get("commit")
        .and_then(Value::as_str)
        .or_else(|| item.get("head").and_then(Value::as_str));
    let publish = item
        .get("publish")
        .map(Value::to_string)
        .unwrap_or_else(|| "?".into());
    if text(item, "code") == "not_merged" {
        let (from, to) = (text(item, "publish_branch"), text(item, "default_branch"));
        let what = oid.map_or_else(
            || format!("publish {publish}"),
            |c| format!("commit {}", short(c)),
        );
        return format!(
            "{repo}: {what} is on origin/{from}, not origin/{to} (publish.branch = {from}: the merge into {to} is \
             the human's; the daemon never closes this session; run git-strata rm --session {s} yourself when done)"
        );
    }
    let what = text(item, "what");
    let subject = match (what, oid) {
        ("base", Some(c)) => format!("base {}", short(c)),
        ("publish", Some(c)) => format!("publish {publish} (commit {})", short(c)),
        ("publish", None) => format!("publish {publish}"),
        ("pending", Some(c)) => format!("rebase target {}", short(c)),
        ("planned_replay", Some(c)) => format!("planned replay {}", short(c)),
        ("planned_publish", _) => format!("planned publish {publish}"),
        (other, _) => other.to_owned(),
    };
    let anchor = anchor_name(text(item, "ref"));
    if item.get("on_chain").and_then(Value::as_bool) != Some(true) {
        return format!(
            "{repo}: {subject} is not on {anchor} and no push lands it (off the publish chain; git-strata rm \
             --session {s} yourself when done)"
        );
    }
    if text(item, "chain") == "diverged" {
        return format!(
            "{repo}: {subject} is not on {anchor} (on the publish chain; git-strata push {repo}; a repo that \
             needs a confirmation: git-strata chain --repo {repo} --accept-origin replay first)"
        );
    }
    if what == "base" && item.get("tracked").and_then(Value::as_bool) == Some(false) {
        return format!("{repo}: {subject} is not on {anchor} (untracked sibling; push {repo})");
    }
    format!("{repo}: {subject} is not on {anchor} (on the publish chain; push {repo})")
}

fn deps_line(item: &Value, s: &SessionId) -> String {
    let repo = text(item, "repo");
    let id = item
        .get("proposal")
        .map(Value::to_string)
        .unwrap_or_else(|| "?".into());
    let own = item.get("own").and_then(Value::as_bool) == Some(true);
    let tracked = item.get("tracked").and_then(Value::as_bool) == Some(true);
    match (own, text(item, "state"), tracked) {
        (true, "awaiting_publish", false) => format!(
            "{repo}: deps proposal {id} is this session's own draft on an untracked repo, which nothing \
             publishes; git-strata rm --session {s} yourself when done"
        ),
        (true, "awaiting_publish", true) => format!(
            "{repo}: deps proposal {id} is this session's own draft and awaits a publish of its inputs; no \
             command cancels it, so git-strata rm --session {s} yourself if you dropped that change"
        ),
        (true, _, _) => format!(
            "{repo}: deps proposal {id} is this session's own and is pending; the human approves it"
        ),
        _ => format!("{repo}: deps proposal {id} is pending"),
    }
}

fn conflict_lines(item: &Value, s: &SessionId) -> Vec<String> {
    let repo = text(item, "repo");
    let paths = item.get("conflicts").map(strs).unwrap_or_default();
    if item.get("report").and_then(Value::as_bool) == Some(true) {
        return paths
            .iter()
            .map(|p| {
                format!(
                    "{repo}: git-strata sync left a conflict on {p}; git-strata sync --session {s} --repo {repo} \
                     reports it again"
                )
            })
            .collect();
    }
    let mut lines: Vec<String> = paths
        .iter()
        .map(|p| format!("{repo}: {p} has a conflict"))
        .collect();
    if item.get("auto_reason").is_some() {
        lines.push(format!("{repo}: the last auto-rebase stopped on conflicts"));
    }
    lines
}

fn tree_name(v: &Value) -> String {
    let path = text(v, "path");
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

fn old_tree_line(v: &Value) -> String {
    let repo = text(v, "repo");
    let name = tree_name(v);
    if v.get("changed").and_then(Value::as_bool) == Some(false) {
        format!("{repo}: pre-move tree {name} is still kept (unchanged; your own rm lets it go)")
    } else {
        format!("{repo}: pre-move tree {name} changed; tell the human")
    }
}

fn path_line(
    repo: &str,
    path: &str,
    section: &Map<String, Value>,
    untracked: bool,
    s: &SessionId,
) -> String {
    if untracked {
        return format!("{repo}: {path} not published (untracked repo: no checkpoint keeps it)");
    }
    let regenerable = section
        .get("regenerable")
        .and_then(|m| m.get(repo))
        .and_then(|v| v.get("paths"));
    if regenerable.map(strs).unwrap_or_default().contains(&path) {
        return format!("{repo}: {path} not published (rm.regenerable)");
    }
    let landing = section.get("landing").and_then(|m| m.get(repo));
    let why = landing
        .and_then(|l| l.get("why_none"))
        .and_then(Value::as_str);
    let published = landing
        .and_then(|l| l.get("published"))
        .and_then(Value::as_bool)
        == Some(true);
    // no_built_base outranks a same-repo historical match: the base is gone, so
    // the note must name the agent's own rm (design §4 / §10 row 1), not claim
    // an older publish alone explains the refusal.
    if why == Some("no_built_base") {
        let anchor = landing
            .map(|l| anchor_name(text(l, "anchor")))
            .unwrap_or("origin/main");
        return format!(
            "{repo}: {path} not published against {anchor} (the base of this session's newest publish is not built; your own git-strata rm --session {s} judges by D57)"
        );
    }
    if landing
        .and_then(|l| l.get("historical"))
        .map(strs)
        .unwrap_or_default()
        .contains(&path)
    {
        return format!(
            "{repo}: {path} not published (an older publish has this content; main's current state does not)"
        );
    }
    match why {
        Some("no_landed_publish") if published => format!(
            "{repo}: {path} is in a publish that landed as a replayed copy; git-strata sync --session {s}, then your own rm"
        ),
        _ => format!("{repo}: {path} not published"),
    }
}

/// The refusals of a `Dirty` other than a recheck's. A repo whose entries only
/// a failed read kept is no refusal: its work may well be published.
fn dirty(s: &SessionId, mut d: Map<String, Value>) -> Refusals {
    let mut errors = Map::new();
    for key in ["landed_errors", "projection_errors"] {
        if let Some(Value::Object(m)) = d.get(key) {
            errors.extend(m.clone());
        }
    }
    if let Some(Value::Object(repos)) = d.get_mut("repos") {
        repos.retain(|r, _| !errors.contains_key(r));
    }
    let repos = d
        .get("repos")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let untracked: BTreeSet<String> = d
        .get("untracked_repos")
        .map(|v| strs(v).into_iter().map(str::to_owned).collect())
        .unwrap_or_default();
    let unresolved = d
        .get("unresolved")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let old_trees = d
        .get("old_trees")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let embedded = d
        .get("embedded_repos")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut r = Refusals::default();
    if repos.is_empty()
        && untracked.is_empty()
        && unresolved.is_empty()
        && old_trees.is_empty()
        && embedded.is_empty()
    {
        r.unread("error");
        r.detail = json!({ "code": "judge_read", "errors": errors })
            .as_object()
            .cloned()
            .unwrap_or_default();
        return r;
    }
    let lines_for = |wanted_untracked: bool| -> Vec<String> {
        repos
            .iter()
            .filter(|(repo, _)| untracked.contains(*repo) == wanted_untracked)
            .flat_map(|(repo, v)| {
                let paths = v.get("paths").map(strs).unwrap_or_default();
                paths
                    .into_iter()
                    .map(|p| path_line(repo, p, &d, wanted_untracked, s))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let published_lines = lines_for(false);
    let untracked_lines = lines_for(true);
    if repos.keys().any(|repo| !untracked.contains(repo)) {
        r.add("unpublished", json!({}), published_lines);
    }
    if !untracked.is_empty() {
        r.add("untracked_unpublished", json!({}), untracked_lines);
    }
    if !old_trees.is_empty() {
        r.add(
            "kept_trees",
            json!({}),
            old_trees.iter().map(old_tree_line).collect(),
        );
    }
    if !embedded.is_empty() {
        let lines = embedded
            .iter()
            .flat_map(|(repo, v)| {
                v.get("paths")
                    .map(strs)
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |p| {
                        format!("{repo}: nested git repository {p} is covered by no publish")
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        r.add("embedded_repo", json!({}), lines);
    }
    if !unresolved.is_empty() {
        let lines = unresolved
            .iter()
            .map(|v| {
                let kind = text(v, "kind").replace('_', " ");
                match v.get("repo").and_then(Value::as_str) {
                    Some(repo) => format!("{repo}: unresolved {kind}; tell the human"),
                    None => format!("unresolved {kind}; tell the human"),
                }
            })
            .collect();
        r.add("unresolved", json!({}), lines);
    }
    if !errors.is_empty() {
        r.unread("error");
    }
    r.detail = d;
    r
}

/// A recheck's refusal. A non-empty `reopened` is clause 6's own state, found in
/// the transaction, so it reads as clause 6 does.
fn changed_during(s: &SessionId, d: Map<String, Value>) -> Refusals {
    let mut r = Refusals::default();
    let reopened = d
        .get("reopened")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !reopened.is_empty() {
        let after = d
            .get("after")
            .cloned()
            .unwrap_or_else(|| json!("checkpoint"));
        let flat = |item: &Value| {
            let mut v = item.clone();
            if let Some(m) = v.as_object_mut() {
                m.retain(|k, _| k != "code");
            }
            v["after"] = after.clone();
            v
        };
        let deps: Vec<Value> = reopened
            .iter()
            .filter(|i| text(i, "code") == "deps_pending")
            .map(&flat)
            .collect();
        for item in &reopened {
            let mut detail = flat(item);
            match text(item, "code") {
                "deps_pending" => {
                    detail["proposals"] = json!(deps);
                    r.add("deps_pending", detail, vec![deps_line(item, s)]);
                }
                other => {
                    let lines = conflict_lines(item, s);
                    r.add(other, detail, lines);
                }
            }
        }
        return r;
    }
    let mut lines: Vec<String> = Vec::new();
    if let Some(repos) = d.get("repos").and_then(Value::as_object) {
        for (repo, v) in repos {
            if let Some(why) = v.get("workspace").and_then(Value::as_str) {
                lines.push(format!("{repo}: its workspace went {why} during the close"));
            }
            let paths = v.get("paths").map(strs).unwrap_or_default();
            lines.extend(
                paths
                    .iter()
                    .map(|p| format!("{repo}: {p} changed during the close")),
            );
        }
    }
    for v in d
        .get("old_trees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        lines.push(format!(
            "{}: pre-move tree {} changed during the close",
            text(v, "repo"),
            tree_name(v)
        ));
    }
    for item in d
        .get("outstanding")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        lines.push(outstanding_line(item, s));
    }
    if d.get("pending_changed").and_then(Value::as_bool) == Some(true) {
        lines.push("a rebase target moved during the close; the next publish or push of its repos judges again".into());
    }
    r.detail = d;
    r.add("changed_during_remove", json!({}), lines);
    r
}

/// The judge's refusal, `Dirty` or `OpenFiles`, on a session still `Active`.
pub(super) fn of_refusal(s: &SessionId, e: &StrataError) -> Refusals {
    let mut d = e
        .detail
        .clone()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    d.retain(|k, _| k != "upstream");
    if e.code == Code::OpenFiles {
        let mut pids: Vec<i64> = d
            .get("open")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|o| o.get("pid").and_then(Value::as_i64))
            .collect();
        pids.sort_unstable();
        pids.dedup();
        let held = pids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        d.insert("pids".into(), json!(pids));
        let mut r = Refusals {
            detail: d,
            ..Refusals::default()
        };
        r.add(
            "open_files",
            json!({}),
            vec![format!("pids {held} hold files")],
        );
        return r;
    }
    if d.get("reason").and_then(Value::as_str) == Some("changed_during_remove") {
        return changed_during(s, d);
    }
    dirty(s, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::try_from("s-aaaabbbb".to_string()).unwrap()
    }

    fn dirty_err(detail: Value) -> StrataError {
        StrataError::new(Code::Dirty, "auto-close refused").with_detail(detail)
    }

    fn list(paths: &[&str]) -> Value {
        json!({ "paths": paths, "count": paths.len(), "truncated": false })
    }

    #[test]
    fn judge_read_is_error_not_a_refusal() {
        for key in ["landed_errors", "projection_errors"] {
            let r = of_refusal(
                &sid(),
                &dirty_err(
                    json!({ "repos": { "tool": list(&["a"]) }, key: { "tool": "git failed" } }),
                ),
            );
            assert_eq!(r.reasons, ["error"], "{key}");
            assert_eq!(r.detail["code"], "judge_read");
            assert_eq!(r.detail["errors"]["tool"], "git failed");
            assert_eq!(r.note(), None);
        }
        let r = of_refusal(
            &sid(),
            &dirty_err(json!({
                "repos": { "tool": list(&["a"]), "front": list(&["b"]) },
                "landed_errors": { "tool": "git failed" },
            })),
        );
        assert_eq!(r.reasons, ["unpublished", "error"]);
        assert_eq!(
            r.detail["repos"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["front"]
        );
        let note = r.note().unwrap();
        assert!(note.contains("front: b not published"), "{note}");
        assert!(!note.contains("tool"), "{note}");
        assert!(
            note.starts_with("auto-close did not close this session: unpublished:"),
            "{note}"
        );

        let r = of_refusal(
            &sid(),
            &dirty_err(json!({
                "repos": { "tool": list(&["a"]) },
                "landed_errors": { "tool": "git failed" },
                "old_trees": [{ "repo": "tool", "n": 3, "path": "/s/.strata-tmp-move-tool-1a", "changed": false }],
            })),
        );
        assert_eq!(r.reasons, ["kept_trees", "error"]);
    }

    #[test]
    fn reopened_items_report_their_own_codes() {
        let changed = |extra: Value| {
            let mut d =
                json!({ "reason": "changed_during_remove", "after": "checkpoint", "repos": {} });
            d.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            of_refusal(&sid(), &dirty_err(d))
        };
        let r = changed(json!({ "reopened": [{
            "repo": "tool", "code": "deps_pending", "proposal": 12, "state": "pending", "own": false, "tracked": true,
        }] }));
        assert_eq!(r.reasons, ["deps_pending"]);
        assert_eq!(r.detail["after"], "checkpoint");
        assert_eq!(r.detail["proposal"], 12);
        assert_eq!(r.detail["proposals"][0]["proposal"], 12);
        assert!(r.detail.get("code").is_none());
        assert!(r.note().is_some());

        let r = changed(json!({ "pending_changed": true }));
        assert_eq!(r.reasons, ["changed_during_remove"]);
        assert!(r.note().unwrap().contains("a rebase target moved"));

        let r = changed(
            json!({ "reopened": [{ "repo": "tool", "code": "conflicts", "conflicts": ["src/p.ts"], "report": true }] }),
        );
        assert_eq!(r.reasons, ["conflicts"]);
        assert_eq!(r.detail["report"], true);
        assert!(
            r.note()
                .unwrap()
                .contains("git-strata sync left a conflict on src/p.ts")
        );

        let r = changed(json!({ "reopened": [{
            "repo": "front", "code": "deps_pending", "proposal": 9, "state": "awaiting_publish", "own": true, "tracked": false,
        }] }));
        assert_eq!(r.reasons, ["deps_pending"]);
        assert_eq!(r.detail["tracked"], false);
        assert!(r.note().unwrap().contains("own draft on an untracked repo"));
    }

    #[test]
    fn a_dirty_with_several_sections_lists_every_code() {
        let r = of_refusal(
            &sid(),
            &dirty_err(json!({
                "repos": { "tool": list(&["gen/a.json", "src/p.ts", "src/q.ts"]), "front": list(&["x"]) },
                "untracked_repos": ["front"],
                "regenerable": { "tool": list(&["gen/a.json"]) },
                "landing": { "tool": { "historical": ["src/p.ts"], "why_none": "no_newer_publish", "published": true } },
                "old_trees": [{ "repo": "tool", "n": 1, "path": "/s/.strata-tmp-move-tool-1a", "changed": true }],
                "embedded_repos": { "tool": list(&["vendor/x"]) },
                "unresolved": [{ "kind": "restore_scene", "repo": "tool", "path": "/s/r" }],
                "upstream": { "repos": {} },
            })),
        );
        assert_eq!(
            r.reasons,
            [
                "unpublished",
                "untracked_unpublished",
                "kept_trees",
                "embedded_repo",
                "unresolved"
            ]
        );
        assert!(r.detail.get("upstream").is_none());
        let note = r.note().unwrap();
        assert!(
            note.contains("tool: gen/a.json not published (rm.regenerable)"),
            "{note}"
        );
        assert!(note.contains("tool: src/p.ts not published (an older publish has this content; main's current state does not)"), "{note}");
        assert!(note.contains("tool: src/q.ts not published\n"), "{note}");
        assert!(
            note.contains("front: x not published (untracked repo"),
            "{note}"
        );
        assert!(
            note.contains("tool: nested git repository vendor/x is covered by no publish"),
            "{note}"
        );
    }

    #[test]
    fn the_why_none_lines_name_the_agents_own_rm() {
        let s = sid();
        let landing = |why: &str, published: bool, historical: &[&str]| {
            of_refusal(
                &s,
                &dirty_err(json!({
                    "repos": { "tool": list(&["src/a.ts"]) },
                    "landing": { "tool": {
                        "why_none": why, "published": published, "anchor": "refs/strata/remotes/origin/main",
                        "historical": historical,
                    } },
                })),
            )
            .note()
            .unwrap()
        };
        assert!(landing("no_built_base", true, &[]).contains("not published against origin/main"));
        assert!(landing("no_built_base", true, &[]).contains(&format!(
            "your own git-strata rm --session {s} judges by D57"
        )));
        // Same scene as row 1 end-to-end: historical is filled, but no_built_base wins.
        let with_hist = landing("no_built_base", true, &["src/a.ts"]);
        assert!(
            with_hist.contains(&format!(
                "your own git-strata rm --session {s} judges by D57"
            )),
            "{with_hist}"
        );
        assert!(
            !with_hist.contains("an older publish has this content"),
            "{with_hist}"
        );
        assert!(
            landing("no_landed_publish", true, &[])
                .contains(&format!("git-strata sync --session {s}, then your own rm"))
        );
        assert!(landing("no_landed_publish", false, &[]).ends_with("tool: src/a.ts not published"));
    }

    #[test]
    fn open_files_names_the_pids_once() {
        let e = StrataError::new(Code::OpenFiles, "auto-close refused").with_detail(json!({
            "open": [{ "pid": 4188 }, { "pid": 4121 }, { "pid": 4188 }],
        }));
        let r = of_refusal(&sid(), &e);
        assert_eq!(r.reasons, ["open_files"]);
        assert_eq!(r.detail["pids"], json!([4121, 4188]));
        assert!(r.note().unwrap().contains("pids 4121, 4188 hold files"));
    }

    #[test]
    fn every_outstanding_item_is_listed_and_the_detail_is_capped() {
        let item = |n: u64| {
            json!({
                "code": "not_pushed", "repo": "front", "what": "publish", "commit": format!("{n:040x}"), "publish": n,
                "ref": "refs/strata/remotes/origin/main", "tracked": true, "on_chain": true, "chain": "leads",
            })
        };
        let items: Vec<Value> = (1..=25).map(item).collect();
        let mut r = Refusals::default();
        r.add_outstanding(&sid(), &items);
        assert_eq!(r.reasons, ["not_pushed"]);
        assert_eq!(r.detail["repo"], "front");
        assert_eq!(r.detail["publish"], 1);
        assert_eq!(r.detail["outstanding"].as_array().unwrap().len(), 20);
        assert_eq!(r.detail["more"], true);
        let note = r.note().unwrap();
        assert!(note.contains("front: publish 1 (commit"), "{note}");
        assert!(
            note.contains("is not on origin/main (on the publish chain; push front)"),
            "{note}"
        );
    }

    #[test]
    fn the_outstanding_lines_follow_on_chain_and_the_branch_pair() {
        let s = sid();
        let line = |extra: Value| {
            let mut v = json!({
                "code": "not_pushed", "repo": "front", "what": "publish", "commit": "9c1e".repeat(10), "publish": 41,
                "ref": "refs/strata/remotes/origin/main", "tracked": true,
            });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            outstanding_line(&v, &s)
        };
        assert!(line(json!({ "on_chain": false, "chain": "rewound" })).contains(&format!(
            "and no push lands it (off the publish chain; git-strata rm --session {s} yourself when done)"
        )));
        assert!(
            line(json!({ "on_chain": true, "chain": "diverged" }))
                .contains("git-strata chain --repo front --accept-origin replay first")
        );
        let untracked = line(
            json!({ "what": "base", "commit": null, "head": "3f9c".repeat(10), "tracked": false, "on_chain": true, "chain": "leads" }),
        );
        assert!(untracked.contains("base 3f9c"), "{untracked}");
        assert!(
            untracked.ends_with("(untracked sibling; push front)"),
            "{untracked}"
        );
        let split = line(
            json!({ "code": "not_merged", "publish_branch": "release", "default_branch": "main" }),
        );
        assert!(split.contains("is on origin/release, not origin/main (publish.branch = release: the merge into main is the human's"), "{split}");
    }

    #[test]
    fn a_failed_read_beside_a_refusal_marks_and_alone_does_not() {
        let mut r = Refusals::default();
        r.unread("upstream_unavailable");
        assert_eq!(r.note(), None);
        r.add(
            "not_pushed",
            json!({ "repo": "tool" }),
            vec!["tool: a line".into()],
        );
        assert_eq!(r.reasons, ["upstream_unavailable", "not_pushed"]);
        let note = r.note().unwrap();
        assert!(
            note.starts_with("auto-close did not close this session: not_pushed:"),
            "{note}"
        );
        assert!(!note.contains("upstream_unavailable"), "{note}");
    }

    #[test]
    fn the_note_fits_a_mark() {
        let line = format!("tool: \u{1}{}é", "x".repeat(4000));
        let mut r = Refusals::default();
        r.add("unpublished", json!({}), vec![line, "second".into()]);
        let note = r.note().unwrap();
        assert!(note.len() <= NOTE_MAX);
        assert!(note.contains("tool:  x"), "{note}");
        strata_types::validate_mark_note(Some(&note)).unwrap();
        let cut = fit_note(&format!("{}é", "x".repeat(NOTE_MAX - 1)));
        assert_eq!(cut.len(), NOTE_MAX - 1);
    }

    #[test]
    fn assigned_detail_lists_every_repo() {
        let front = RepoName::try_from("front".to_string()).unwrap();
        let tool = RepoName::try_from("tool".to_string()).unwrap();
        let assigned = BTreeMap::from([
            (front, vec!["a.ts".to_string()]),
            (tool, vec!["b.ts".to_string()]),
        ]);
        let mut r = Refusals::default();
        r.add_assigned(&assigned);
        assert_eq!(r.reasons, ["assigned"]);
        assert_eq!(r.detail["repo"], "front");
        assert_eq!(r.detail["paths"], json!(["a.ts"]));
        assert_eq!(r.detail["assigned"]["tool"], json!(["b.ts"]));
        let note = r.note().unwrap();
        assert!(
            note.contains("front: a.ts is assigned to this session"),
            "{note}"
        );
        assert!(
            note.contains("tool: b.ts is assigned to this session"),
            "{note}"
        );
    }
}

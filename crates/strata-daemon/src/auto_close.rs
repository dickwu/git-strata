//! Auto-close (p1-autoclose): after a publish or a push, a session whose work
//! has fully landed on the default branch is closed by a strict, refusable,
//! never content-losing remove. One that has not is marked done with the reasons.
//!
//! The hook runs on the request's thread with no lock held. The publish or the
//! push has already landed, so nothing here can change that outcome: every
//! failure becomes a reason in the reply.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::SystemTime;

use serde_json::{Map, Value, json};
use strata_session::RemoveReport;
use strata_store::{Lifecycle, SessionRecord};
use strata_types::{
    Code, Config, MarkByKind, MarkState, ReleaseGroup, RepoName, SessionId, StrataError, Upstream,
};

use crate::Result;
use crate::handlers::{RemoveCall, Server, git_of, repo_cfg, writing};
use crate::resolve;
use crate::startup::Core;

mod refusal;

use refusal::Refusals;

/// What called the hook, as the audit line and `last_rm` name it.
#[derive(Clone, Copy)]
enum Trigger<'t> {
    Publish { id: Option<u64> },
    Push { repo: &'t RepoName },
}

impl Trigger<'_> {
    fn audit_json(&self, in_group: bool) -> Value {
        match self {
            Trigger::Publish { id } => {
                json!({ "method": "publish", "publish": id, "repo": null, "in_group": in_group })
            }
            Trigger::Push { repo } => {
                json!({ "method": "push", "publish": null, "repo": repo.as_str(), "in_group": in_group })
            }
        }
    }

    /// What `record_rm_attempt` adds, so `status` says whose attempt it was.
    fn attempt_json(&self) -> Value {
        match self {
            Trigger::Publish { id } => json!({ "by": "auto_close", "publish": id, "repo": null }),
            Trigger::Push { repo } => {
                json!({ "by": "auto_close", "publish": null, "repo": repo.as_str() })
            }
        }
    }
}

/// How one session's sequence ended.
struct Outcome {
    closed: bool,
    /// A group session someone else removed: closed for the caller, `gone` here.
    by_other: bool,
    /// `false` for what nothing judged: `disabled`, `extended`.
    counted: bool,
    audited: bool,
    reasons: Vec<String>,
    detail: Value,
    rm: Option<RemoveReport>,
    mark: Value,
    upstream: Option<Upstream>,
}

impl Outcome {
    fn open(reasons: Vec<String>, detail: Value, mark: Value) -> Outcome {
        Outcome {
            closed: false,
            by_other: false,
            counted: true,
            audited: true,
            reasons,
            detail,
            rm: None,
            mark,
            upstream: None,
        }
    }

    fn closed(rm: RemoveReport, upstream: Option<Upstream>) -> Outcome {
        let upstream = upstream.map(|mut u| {
            u.as_of_rm = Some("before".into());
            for repo in u.repos.values_mut() {
                repo.unpublished = Some(false);
            }
            u
        });
        Outcome {
            closed: true,
            by_other: false,
            counted: true,
            audited: true,
            reasons: Vec::new(),
            detail: json!({}),
            rm: Some(rm),
            mark: json!({ "written": false }),
            upstream,
        }
    }

    /// The caller's question is "is my session gone?", so this reads as closed;
    /// what this judge did stays in the audit line.
    fn gone() -> Outcome {
        Outcome {
            closed: true,
            by_other: true,
            ..Outcome::open(Vec::new(), json!({}), json!({ "written": false }))
        }
    }

    fn disabled(repo: &RepoName) -> Outcome {
        Outcome {
            counted: false,
            audited: false,
            ..Outcome::open(
                vec!["disabled".into()],
                json!({ "repo": repo.as_str() }),
                json!({ "written": false }),
            )
        }
    }

    /// Clause 0b: a repo was added during this run, so nothing is judged and an
    /// `add` that says more work is coming does not earn a mark.
    fn extended(repos: &[&str]) -> Outcome {
        Outcome {
            counted: false,
            ..Outcome::open(
                vec!["extended".into()],
                json!({ "repos": repos }),
                json!({ "written": false, "skipped": "extended" }),
            )
        }
    }

    /// The step never reached a judgement, or a read failed. A mark records a
    /// refusal, so none is written. Every `Code::Busy` is `busy`.
    fn not_judged(e: &StrataError) -> Outcome {
        if e.code == Code::Busy {
            let reason = e
                .detail
                .as_ref()
                .and_then(|d| d.get("reason"))
                .cloned()
                .unwrap_or(Value::Null);
            return Outcome::open(
                vec!["busy".into()],
                json!({ "reason": reason }),
                json!({ "written": false, "skipped": "busy" }),
            );
        }
        tracing::warn!(code = e.code.as_str(), error = %e.message, "auto-close did not reach a judgement");
        Outcome::open(
            vec!["error".into()],
            json!({ "code": e.code.as_str() }),
            json!({ "written": false, "skipped": "error" }),
        )
    }

    fn panicked() -> Outcome {
        Outcome::open(
            vec!["error".into()],
            json!({ "code": "handler_panicked" }),
            json!({ "written": false, "skipped": "error" }),
        )
    }

    fn reply_json(&self, in_group: bool) -> Value {
        let rm = self.rm.as_ref().map_or(Value::Null, |r| {
            serde_json::to_value(r).unwrap_or(Value::Null)
        });
        let mut v = json!({
            "closed": self.closed,
            "in_group": in_group,
            "reasons": self.reasons,
            "detail": self.detail,
            "rm": rm,
            "mark": self.mark,
        });
        if self.by_other {
            v["by"] = json!("other");
        }
        v
    }
}

/// How one session's sequence ended, for the run.
enum Step {
    /// Left the set after the enumeration: no judge, no line, no entry.
    Dropped,
    Judged(Outcome),
}

/// The two fields the reply gains.
pub(crate) struct Fields {
    closed: Option<bool>,
    auto_close: Value,
    /// Group sessions that need no fresh `upstream`: the read taken before their
    /// teardown, or nothing for one someone else removed.
    pub(crate) before: BTreeMap<SessionId, Option<Upstream>>,
}

impl Fields {
    pub(crate) fn apply(self, data: &mut Value) {
        if let Some(closed) = self.closed {
            data["closed"] = json!(closed);
        }
        data["auto_close"] = self.auto_close;
    }

    fn failed(closed: Option<bool>) -> Fields {
        Fields {
            closed,
            auto_close: json!({ "closed": false, "reasons": ["error"], "rejudged": [], "sessions": {} }),
            before: BTreeMap::new(),
        }
    }
}

/// Clause 0b's baseline for a group: the manifests when the handler started. An
/// unreadable row reads as empty, so the comparison fails closed.
pub(crate) fn baselines(
    core: &Core,
    sessions: &BTreeSet<SessionId>,
) -> BTreeMap<SessionId, BTreeSet<RepoName>> {
    sessions
        .iter()
        .map(|s| {
            let repos = match core.store.session(s) {
                Ok(Some(rec)) => rec.manifest.repos.keys().cloned().collect(),
                Ok(None) => BTreeSet::new(),
                Err(e) => {
                    tracing::warn!(session = %s, error = %e.message, "auto-close could not read the manifest");
                    BTreeSet::new()
                }
            };
            (s.clone(), repos)
        })
        .collect()
}

/// A repo resolves `true` from its own key, else the global one. A repo this
/// generation lacks counts as `false`.
fn resolves(cfg: &Config, repo: &RepoName) -> bool {
    cfg.repos
        .get(repo)
        .is_some_and(|rc| rc.publish.auto_close.unwrap_or(cfg.publish.auto_close))
}

/// Clause 0: the generation this request pinned and the current one must both
/// say `true`, so only a refusal may use the newer config.
fn opted_out<'r>(
    core: &Core,
    mut repos: impl Iterator<Item = &'r RepoName>,
) -> Result<Option<RepoName>> {
    let pinned = &core.snapshot()?.config;
    let current = &core.catalog.current().config;
    Ok(repos
        .find(|r| !resolves(pinned, r) || !resolves(current, r))
        .cloned())
}

fn lifecycle_name(l: Lifecycle) -> String {
    format!("{l:?}").to_lowercase()
}

fn at_text(at: &SystemTime) -> String {
    strata_types::serde_rfc3339::format(at).unwrap_or_default()
}

fn is_done(core: &Core, s: &SessionId) -> Result<bool> {
    Ok(core
        .store
        .mark(s)?
        .is_some_and(|m| m.state == MarkState::Done))
}

/// The live done-marked sessions holding any of `repos`, in id order, with the
/// manifest clause 0b compares against. Candidates only: nothing is decided here.
fn done_marked(
    core: &Core,
    repos: &BTreeSet<RepoName>,
    not: &BTreeSet<SessionId>,
) -> Result<BTreeMap<SessionId, BTreeSet<RepoName>>> {
    let mut out = BTreeMap::new();
    if repos.is_empty() {
        return Ok(out);
    }
    for rec in core.store.sessions()? {
        let id = &rec.manifest.id;
        if rec.lifecycle == Lifecycle::Removed
            || not.contains(id)
            || !rec.manifest.repos.keys().any(|r| repos.contains(r))
            || !is_done(core, id)?
        {
            continue;
        }
        out.insert(id.clone(), rec.manifest.repos.keys().cloned().collect());
    }
    Ok(out)
}

fn publishes_default_branch(core: &Core, repo: &RepoName) -> Result<bool> {
    let rc = repo_cfg(core, repo)?;
    Ok(rc.publish.branch == rc.default_branch)
}

/// The repos this call pushed. A `Pushed` in `already_published` is an earlier
/// run's that took the error path, whose hook never ran. A split-branch repo's
/// push moves no ref the proof reads, so nothing a re-judge could find has changed.
fn pushed_repos(core: &Core, published: &[Value], already: &[Value]) -> Result<BTreeSet<RepoName>> {
    let mut out = BTreeSet::new();
    for entry in published.iter().chain(already) {
        let pushed = entry
            .get("push")
            .and_then(|p| p.get("kind"))
            .and_then(Value::as_str)
            == Some("Pushed");
        let Some(name) = entry.get("repo").and_then(Value::as_str).filter(|_| pushed) else {
            continue;
        };
        let repo = RepoName::try_from(name.to_owned())?;
        if publishes_default_branch(core, &repo)? {
            out.insert(repo);
        }
    }
    Ok(out)
}

/// A member's closed flag and reasons.
fn conjunction(members: &[(String, bool, Vec<String>)]) -> (bool, Vec<String>) {
    match members {
        [] => (false, Vec::new()),
        [(_, closed, reasons)] => (*closed, reasons.clone()),
        many => {
            let closed = many.iter().all(|(_, c, _)| *c);
            let reasons = many
                .iter()
                .flat_map(|(s, _, reasons)| reasons.iter().map(move |r| format!("{s}:{r}")))
                .collect();
            (closed, reasons)
        }
    }
}

/// One run of the hook.
struct Run<'r, 'd, 'a> {
    srv: &'r Server<'d, 'a>,
    core: &'a Core,
    trigger: Trigger<'r>,
    /// Clause 5 reads these; a push has none.
    group: Option<&'r ReleaseGroup>,
    already: &'r [Value],
    sessions: Map<String, Value>,
    rejudged: Vec<String>,
    members: Vec<(String, bool, Vec<String>)>,
    before: BTreeMap<SessionId, Option<Upstream>>,
}

impl<'r, 'd, 'a> Run<'r, 'd, 'a> {
    fn new(
        srv: &'r Server<'d, 'a>,
        trigger: Trigger<'r>,
        group: Option<&'r ReleaseGroup>,
        already: &'r [Value],
    ) -> Self {
        Run {
            srv,
            core: srv.core(),
            trigger,
            group,
            already,
            sessions: Map::new(),
            rejudged: Vec::new(),
            members: Vec::new(),
            before: BTreeMap::new(),
        }
    }

    /// A seam is a test's: an armed failure here is only logged.
    fn seam(&self, phase: &str) {
        if let Err(e) = self.srv.sessions().seam(phase) {
            tracing::debug!(phase, error = %e.message, "auto-close seam");
        }
    }

    fn judge(&mut self, s: &SessionId, baseline: &BTreeSet<RepoName>, in_group: bool) {
        let outcome = match self.sequence(s, baseline, in_group) {
            Step::Dropped => return,
            Step::Judged(o) => o,
        };
        // Step 6: with the queue place released and no lock held.
        if outcome.audited {
            self.audit(s, &outcome, in_group);
        }
        self.sessions
            .insert(s.as_str().to_owned(), outcome.reply_json(in_group));
        let member = match self.trigger {
            Trigger::Publish { .. } => in_group,
            Trigger::Push { .. } => outcome.counted,
        };
        if member {
            self.members
                .push((s.as_str().to_owned(), outcome.closed, outcome.reasons));
        }
        if !in_group && outcome.counted {
            self.rejudged.push(s.as_str().to_owned());
        }
        if in_group && outcome.closed {
            self.before.insert(s.clone(), outcome.upstream);
        }
    }

    /// Steps 1 and 2. Everything of steps 3-5 runs under the queue place.
    fn sequence(&self, s: &SessionId, baseline: &BTreeSet<RepoName>, in_group: bool) -> Step {
        if let Some(step) = self.first_read(s, in_group) {
            return step;
        }
        self.seam("auto_close.before_judge");
        let taken = catch_unwind(AssertUnwindSafe(|| {
            resolve::with_queue_not_publishing(self.core, s, || {
                Ok(self.under_place(s, baseline, in_group))
            })
        }));
        match taken {
            Ok(Ok(step)) => step,
            Ok(Err(e)) => Step::Judged(Outcome::not_judged(&e)),
            Err(_) => {
                tracing::error!(session = %s, "auto-close panicked; the publish or push stands");
                Step::Judged(Outcome::panicked())
            }
        }
    }

    /// Step 1, holding nothing. A `false` here is only an early exit, which costs
    /// an opted-out candidate no lock wait. A `Removed` row ends the sequence with
    /// no queue take, because `check_session` would fence even that row.
    fn first_read(&self, s: &SessionId, in_group: bool) -> Option<Step> {
        let rec = match self.core.store.session(s) {
            Ok(rec) => rec,
            Err(e) => return Some(Step::Judged(Outcome::not_judged(&e))),
        };
        match rec {
            Some(rec) if rec.lifecycle == Lifecycle::Active => {
                match opted_out(self.core, rec.manifest.repos.keys()) {
                    Ok(Some(repo)) => Some(Step::Judged(Outcome::disabled(&repo))),
                    Ok(None) => None,
                    Err(e) => Some(Step::Judged(Outcome::not_judged(&e))),
                }
            }
            Some(rec) if rec.lifecycle != Lifecycle::Removed => None,
            _ if in_group => Some(Step::Judged(Outcome::gone())),
            _ => Some(Step::Dropped),
        }
    }

    /// Every failure inside the place becomes an outcome: the closure never
    /// answers `Err`, so `Ok(Err)` above means it never ran.
    fn under_place(&self, s: &SessionId, baseline: &BTreeSet<RepoName>, in_group: bool) -> Step {
        self.seam("auto_close.in_closure");
        match self.reread_and_judge(s, baseline, in_group) {
            Ok(step) => step,
            Err(e) => Step::Judged(Outcome::not_judged(&e)),
        }
    }

    /// Steps 3-5.
    fn reread_and_judge(
        &self,
        s: &SessionId,
        baseline: &BTreeSet<RepoName>,
        in_group: bool,
    ) -> Result<Step> {
        let core = self.core;
        let rec = match core.store.session(s)? {
            Some(rec) if rec.lifecycle != Lifecycle::Removed => rec,
            _ if in_group => return Ok(Step::Judged(Outcome::gone())),
            _ => {
                tracing::debug!(session = %s, "auto-close: the session went after the enumeration");
                return Ok(Step::Dropped);
            }
        };
        // A `mark active` since the enumeration deleted the row under this same
        // queue: the hook must not bring back a row the agent just deleted.
        if !in_group && !is_done(core, s)? {
            tracing::debug!(session = %s, "auto-close: the mark went after the enumeration");
            return Ok(Step::Dropped);
        }
        if rec.lifecycle != Lifecycle::Active {
            let detail = json!({ "state": lifecycle_name(rec.lifecycle) });
            return Ok(Step::Judged(Outcome::open(
                vec!["lifecycle".into()],
                detail,
                json!({ "written": false }),
            )));
        }
        if let Some(repo) = opted_out(core, rec.manifest.repos.keys())? {
            return Ok(Step::Judged(Outcome::disabled(&repo)));
        }
        let grown: Vec<&str> = rec
            .manifest
            .repos
            .keys()
            .filter(|r| !baseline.contains(*r))
            .map(RepoName::as_str)
            .collect();
        if !grown.is_empty() {
            return Ok(Step::Judged(Outcome::extended(&grown)));
        }
        self.seam("auto_close.after_reread");
        let (refusals, upstream) = self.clauses(s, &rec)?;
        self.seam("auto_close.after_clauses");
        Ok(Step::Judged(match upstream {
            Some(upstream) if refusals.is_empty() => self.close(s, upstream),
            _ => self.refused(s, refusals),
        }))
    }

    /// Clauses 4, 2, 3, 5, 6, all read fresh under the place. The judge runs only
    /// when all of them pass.
    fn clauses(&self, s: &SessionId, rec: &SessionRecord) -> Result<(Refusals, Option<Upstream>)> {
        let core = self.core;
        let rows = core.store.publishes()?;
        let pending = core.store.rebase_pending(Some(s))?;
        let mut r = Refusals::default();
        let items = strata_session::outstanding(
            &core.store,
            |repo| git_of(core, repo),
            |repo| repo_cfg(core, repo),
            rec,
            &pending,
            &rows,
        )?;
        let items: Vec<Value> = items
            .iter()
            .map(strata_session::Outstanding::to_json)
            .collect();
        r.add_outstanding(s, &items);
        let upstream = self.srv.upstream_best_effort(s);
        match &upstream {
            None => r.unread("upstream_unavailable"),
            Some(u) => r.add_upstream(u),
        }
        if let Some(group) = self.group {
            r.add_refused_pushes(group, s, self.already);
        }
        if let Some(u) = &upstream {
            r.add_conflicts(s, u);
        }
        r.add_deps(s, &deps_items(core, s, rec, upstream.as_ref())?);
        for repo in rec.manifest.repos.keys() {
            let report = core.store.sync_report(s, repo)?;
            let paths: Vec<&str> = report.iter().map(|row| row.path.as_str()).collect();
            r.add_report(s, repo, &paths);
        }
        r.add_assigned(&strata_publish::assigned_to(&rows, s));
        Ok((r, upstream))
    }

    /// Step 5: the only way this module removes anything.
    fn close(&self, s: &SessionId, upstream: Upstream) -> Outcome {
        let by = self.trigger.attempt_json();
        let (removed, read) = self
            .srv
            .remove_session(s, RemoveCall::AutoClose { upstream, by });
        match removed {
            Ok(rm) => Outcome::closed(rm, read),
            Err(e) => self.after_failed_close(s, &e),
        }
    }

    /// The lifecycle first: `Code::Unknown` also answers failures before
    /// `Removing` (a failed holder scan, a failed git read), so the code alone
    /// cannot say whether anything happened.
    fn after_failed_close(&self, s: &SessionId, e: &StrataError) -> Outcome {
        let state = match self.core.store.session(s) {
            Ok(rec) => rec.map_or(Lifecycle::Removed, |r| r.lifecycle),
            Err(read) => return Outcome::not_judged(&read),
        };
        if state != Lifecycle::Active {
            let detail = json!({ "state": lifecycle_name(state), "code": e.code.as_str() });
            return Outcome::open(vec!["unknown".into()], detail, json!({ "written": false }));
        }
        match e.code {
            Code::Dirty | Code::OpenFiles => self.refused(s, refusal::of_refusal(s, e)),
            _ => Outcome::not_judged(e),
        }
    }

    /// A refusal marks; a set of failed reads alone does not.
    fn refused(&self, s: &SessionId, r: Refusals) -> Outcome {
        let mark = match r.note() {
            Some(note) => write_mark(self.core, s, &note),
            None => json!({ "written": false, "skipped": "error" }),
        };
        Outcome::open(r.reasons, Value::Object(r.detail), mark)
    }

    fn audit(&self, s: &SessionId, o: &Outcome, in_group: bool) {
        let (closed, reasons) = if o.by_other {
            (false, vec!["gone".to_owned()])
        } else {
            (o.closed, o.reasons.clone())
        };
        let rm = o.rm.as_ref().map_or(
            Value::Null,
            |r| json!({ "mode": "strict", "checkpoints": r.checkpoints, "trash": r.trash }),
        );
        self.core.sessions_audit.record(&json!({
            "at": at_text(&SystemTime::now()),
            "event": "auto_close",
            "session": s.as_str(),
            "by": { "kind": "daemon", "pid": std::process::id() },
            "auth": "none",
            "version": env!("CARGO_PKG_VERSION"),
            "trigger": self.trigger.audit_json(in_group),
            "closed": closed,
            "reasons": reasons,
            "detail": o.detail,
            "rm": rm,
            "mark": o.mark,
        }));
    }

    fn fields(self) -> Fields {
        let (closed, reasons) = conjunction(&self.members);
        let top = match self.trigger {
            Trigger::Publish { .. } => Some(closed),
            // An empty conjunction never reads as a close.
            Trigger::Push { .. } => (!self.members.is_empty()).then_some(closed),
        };
        Fields {
            closed: top,
            auto_close: json!({
                "closed": closed,
                "reasons": reasons,
                "rejudged": self.rejudged,
                "sessions": self.sessions,
            }),
            before: self.before,
        }
    }
}

fn proposal_state_name(state: &strata_deps::ProposalState) -> &'static str {
    match state {
        strata_deps::ProposalState::AwaitingPublish => "awaiting_publish",
        strata_deps::ProposalState::Pending => "pending",
        strata_deps::ProposalState::Applied => "applied",
        strata_deps::ProposalState::Superseded { .. } => "superseded",
    }
}

/// Clause 6's proposals: `upstream.deps` of the tracked repos and, with no
/// tracked filter, every open proposal of this session.
fn deps_items(
    core: &Core,
    s: &SessionId,
    rec: &SessionRecord,
    upstream: Option<&Upstream>,
) -> Result<Vec<Value>> {
    let open = strata_deps::open_proposals(&core.store, None)?;
    let own = |p: &strata_deps::Proposal| matches!(&p.origin, strata_deps::ProposalOrigin::Session { session } if session == s);
    let mut items = Vec::new();
    let mut named = Vec::new();
    for (repo, u) in upstream.into_iter().flat_map(|u| u.repos.iter()) {
        let Some(deps) = &u.deps else { continue };
        named.push(deps.proposal);
        items.push(json!({
            "repo": repo.as_str(),
            "proposal": deps.proposal,
            "state": "pending",
            "own": open.iter().any(|p| p.id == deps.proposal && own(p)),
            "tracked": true,
        }));
    }
    for p in open.iter().filter(|p| own(p) && !named.contains(&p.id)) {
        items.push(json!({
            "repo": p.repo.as_str(),
            "proposal": p.id,
            "state": proposal_state_name(&p.state),
            "own": true,
            "tracked": rec.manifest.repos.get(&p.repo).is_some_and(|w| w.tracked),
        }));
    }
    Ok(items)
}

fn mark_failed(s: &SessionId, e: &StrataError) -> Value {
    tracing::warn!(session = %s, error = %e.message, "auto-close could not write the done mark");
    json!({ "written": false, "error": { "code": e.code.as_str(), "message": e.message } })
}

/// Under the place. The daemon's own row is refreshed, so the next refusal shows
/// its current reasons, and an unchanged note writes nothing, because `at` keys
/// the once-per-mark reason. Anyone else's row stays.
fn write_mark(core: &Core, s: &SessionId, note: &str) -> Value {
    match core.store.mark(s) {
        Err(e) => mark_failed(s, &e),
        Ok(Some(m)) if m.by.kind != MarkByKind::Daemon => {
            let kind = serde_json::to_value(m.by.kind).ok();
            let kind = kind.as_ref().and_then(Value::as_str).unwrap_or("other");
            json!({ "written": false, "skipped": format!("kept_{kind}") })
        }
        Ok(Some(m)) if m.note.as_deref() == Some(note) => {
            json!({ "written": false, "skipped": "unchanged", "at": at_text(&m.at) })
        }
        Ok(_) => {
            writing();
            match core.store.set_mark(
                s,
                MarkState::Done,
                Some(note),
                MarkByKind::Daemon,
                SystemTime::now(),
            ) {
                Ok(Some(m)) => json!({ "written": true, "at": at_text(&m.at) }),
                Ok(None) => json!({ "written": false }),
                Err(e) => mark_failed(s, &e),
            }
        }
    }
}

/// A session `add` has extended is work in progress again, so the next push must
/// not judge it. Anyone else's mark stays: it is its writer's to undo.
pub(crate) fn clear_daemon_mark(core: &Core, s: &SessionId) {
    let cleared = core.store.mark(s).and_then(|m| match m {
        Some(m) if m.by.kind == MarkByKind::Daemon => core
            .store
            .set_mark(
                s,
                MarkState::Active,
                None,
                MarkByKind::Daemon,
                SystemTime::now(),
            )
            .map(|_| ()),
        _ => Ok(()),
    });
    if let Err(e) = cleared {
        tracing::warn!(session = %s, error = %e.message, "add could not clear the daemon's done mark");
    }
}

/// `publish`'s success path, after its group rebase.
pub(crate) struct PublishCall<'c> {
    pub(crate) group: &'c ReleaseGroup,
    pub(crate) baselines: &'c BTreeMap<SessionId, BTreeSet<RepoName>>,
    pub(crate) published: &'c [Value],
    pub(crate) already: &'c [Value],
}

/// The second `catch_unwind`: the enumeration and the assembly of the reply. The
/// publish's or the push's own reply stands whatever happens here.
fn settled<T>(body: impl FnOnce() -> Result<T>, failed: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::warn!(error = %e.message, "auto-close could not run");
            failed()
        }
        Err(_) => {
            tracing::error!("auto-close panicked outside a session's judgement");
            failed()
        }
    }
}

/// Set (a), the group in id order, then set (b), the done-marked sessions of the
/// repos this call pushed. A session's outcome never changes another's.
pub(crate) fn after_publish(srv: &Server<'_, '_>, call: &PublishCall<'_>) -> Fields {
    settled(
        || {
            let core = srv.core();
            let id = call
                .published
                .last()
                .and_then(|v| v.get("id"))
                .or_else(|| call.already.last().and_then(|v| v.get("publish")))
                .and_then(Value::as_u64);
            let group: BTreeSet<SessionId> = call
                .group
                .batches
                .iter()
                .map(|b| b.session.clone())
                .collect();
            let pushed = pushed_repos(core, call.published, call.already)?;
            let others = done_marked(core, &pushed, &group)?;
            let mut run = Run::new(srv, Trigger::Publish { id }, Some(call.group), call.already);
            run.seam("auto_close.after_enumerate");
            let empty = BTreeSet::new();
            for s in &group {
                run.judge(s, call.baselines.get(s).unwrap_or(&empty), true);
            }
            for (s, baseline) in &others {
                run.judge(s, baseline, false);
            }
            Ok(run.fields())
        },
        || Fields::failed(Some(false)),
    )
}

/// A push judges only done-marked sessions: a publish is a session's own "done"
/// signal, a push is not. A split-branch repo's push lands nothing on the default
/// branch, so the hook does not run. `None`: the reply gains no field.
pub(crate) fn after_push(srv: &Server<'_, '_>, repo: &RepoName) -> Option<Fields> {
    settled(
        || {
            let core = srv.core();
            if !publishes_default_branch(core, repo)? {
                return Ok(None);
            }
            let others = done_marked(core, &BTreeSet::from([repo.clone()]), &BTreeSet::new())?;
            if others.is_empty() {
                return Ok(None);
            }
            let mut run = Run::new(srv, Trigger::Push { repo }, None, &[]);
            run.seam("auto_close.after_enumerate");
            for (s, baseline) in &others {
                run.judge(s, baseline, false);
            }
            Ok((!run.sessions.is_empty()).then(|| run.fields()))
        },
        || Some(Fields::failed(None)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(code: Code, detail: Value) -> StrataError {
        StrataError::new(code, "refused").with_detail(detail)
    }

    #[test]
    fn every_busy_is_busy_and_marks_nothing() {
        for reason in [
            "publishing",
            "lock_wait",
            "pins_tightened",
            "du_cancel_timeout",
        ] {
            let o = Outcome::not_judged(&err(Code::Busy, json!({ "reason": reason })));
            assert_eq!(o.reasons, ["busy"]);
            assert_eq!(o.detail["reason"], reason);
            assert_eq!(o.mark, json!({ "written": false, "skipped": "busy" }));
            assert!(o.audited);
        }
    }

    #[test]
    fn phase_b_failure_maps_to_error() {
        let o = Outcome::not_judged(&err(Code::Unknown, json!({ "reason": "git" })));
        assert_eq!(o.reasons, ["error"]);
        assert_eq!(o.detail["code"], "Unknown");
        assert_eq!(o.mark["skipped"], "error");
        assert!(!o.closed);
    }

    #[test]
    fn a_failed_queue_take_names_its_code() {
        let o = Outcome::not_judged(&err(Code::ReadOnly, json!({ "reason": "shutting_down" })));
        assert_eq!(o.reasons, ["error"]);
        assert_eq!(o.detail["code"], "ReadOnly");
        assert_eq!(o.mark["skipped"], "error");
    }

    #[test]
    fn a_panic_is_an_error_with_a_line_and_no_mark() {
        let o = Outcome::panicked();
        assert_eq!(o.reasons, ["error"]);
        assert_eq!(o.detail["code"], "handler_panicked");
        assert_eq!(o.mark["skipped"], "error");
        assert!(o.audited);
    }

    #[test]
    fn gone_reads_closed_for_the_caller() {
        let o = Outcome::gone();
        let v = o.reply_json(true);
        assert_eq!(v["closed"], true);
        assert_eq!(v["in_group"], true);
        assert_eq!(v["by"], "other");
        assert_eq!(v["reasons"], json!([]));
        assert_eq!(v["detail"], json!({}));
        assert_eq!(v["rm"], Value::Null);
        assert_eq!(v["mark"], json!({ "written": false }));
        assert!(o.audited);
    }

    #[test]
    fn disabled_and_extended_count_for_nothing() {
        let tool = RepoName::try_from("tool".to_string()).unwrap();
        let o = Outcome::disabled(&tool);
        assert!(!o.counted && !o.audited);
        assert_eq!(o.reasons, ["disabled"]);
        assert_eq!(o.detail["repo"], "tool");
        let o = Outcome::extended(&["front"]);
        assert!(!o.counted && o.audited);
        assert_eq!(o.reasons, ["extended"]);
        assert_eq!(o.detail["repos"], json!(["front"]));
        assert_eq!(o.mark["skipped"], "extended");
    }

    #[test]
    fn the_conjunction_prefixes_only_when_several() {
        assert_eq!(conjunction(&[]), (false, Vec::new()));
        let one = [("a".to_string(), false, vec!["unpublished".to_string()])];
        assert_eq!(conjunction(&one), (false, vec!["unpublished".to_string()]));
        let two = [
            ("a".to_string(), true, Vec::new()),
            ("b".to_string(), false, vec!["unpublished".to_string()]),
        ];
        assert_eq!(
            conjunction(&two),
            (false, vec!["b:unpublished".to_string()])
        );
    }

    #[test]
    fn the_triggers_name_the_call_in_every_record() {
        let tool = RepoName::try_from("tool".to_string()).unwrap();
        let publish = Trigger::Publish { id: Some(42) };
        assert_eq!(
            publish.audit_json(true),
            json!({ "method": "publish", "publish": 42, "repo": null, "in_group": true })
        );
        assert_eq!(
            publish.attempt_json(),
            json!({ "by": "auto_close", "publish": 42, "repo": null })
        );
        let push = Trigger::Push { repo: &tool };
        assert_eq!(
            push.audit_json(false),
            json!({ "method": "push", "publish": null, "repo": "tool", "in_group": false })
        );
        assert_eq!(
            push.attempt_json(),
            json!({ "by": "auto_close", "publish": null, "repo": "tool" })
        );
    }

    /// §10 row 3 (Slice T): a session failing clauses 3 and 4 lists clause 4's
    /// code first. `clauses` adds them in that order (asserted on its source),
    /// and `Refusals` keeps the order it was given.
    #[test]
    fn reasons_follow_the_evaluation_order() {
        let src = include_str!("auto_close.rs");
        let body = &src[src.find("fn clauses(").expect("fn clauses")..];
        let body = &body[..body.find("\n    }\n").expect("end of clauses")];
        let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("{needle} in clauses"));
        assert!(at("r.add_outstanding(") < at("r.add_upstream("));

        let s = SessionId::try_from("s-abcdefgh".to_string()).unwrap();
        let base = "1".repeat(40);
        let item = json!({
            "code": "not_merged", "repo": "tool", "what": "base", "head": "2".repeat(40),
            "ref": "refs/strata/remotes/origin/main", "tracked": true,
            "publish_branch": "release", "default_branch": "main",
        });
        let upstream: Upstream = serde_json::from_value(json!({
            "clean": false,
            "repos": { "tool": {
                "ahead": 2, "following": true, "capped": false, "unchecked": false,
                "diverged": false, "base_only": [base, base], "base_head": base,
            } },
        }))
        .unwrap();
        let mut r = Refusals::default();
        r.add_outstanding(&s, &[item]);
        r.add_upstream(&upstream);
        assert_eq!(r.reasons, ["not_merged", "ahead"]);
        assert_eq!(r.detail["what"], "base", "the first clause keeps the shared keys");
        let note = r.note().unwrap();
        assert!(note.find("not_merged").unwrap() < note.find("ahead").unwrap(), "{note}");
    }
}

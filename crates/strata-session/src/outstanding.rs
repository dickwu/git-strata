//! Auto-close §2.1: every commit a close would abandon must be on the default
//! branch's tracking ref. Read-only and lock-free; its only inputs are the
//! store and git, so the trigger cannot change the answer: no reply is read.
//! It has no cap (a cap would be a hole) and reads no overlay.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;
use strata_deps::{ChainState, publish_chain};
use strata_gitio::{Git, remote_ref};
use strata_store::{
    PendingSource, PublishPhase, PublishRecord, RebasePending, RefOp, RefOutcome, SessionRecord,
    Store,
};
use strata_types::{Code, CommitOid, RepoConfig, RepoName, SessionId, WorkspaceState};

use crate::Result;

/// The git reads of the proof, so that its rule can run over a fake.
pub trait Ancestry {
    fn read_ref(&self, name: &str) -> Result<Option<CommitOid>>;
    /// A pruned commit is simply not an ancestor; any other error stays one.
    fn is_ancestor(&self, ancestor: &CommitOid, descendant: &CommitOid) -> Result<bool>;
    /// The replayed copy of `commit` on `tip`, naming `id`, `repo` and
    /// `session`; a pruned commit has no provable copy.
    fn landed_copy(
        &self,
        id: u64,
        repo: &RepoName,
        session: &SessionId,
        commit: &CommitOid,
        tip: &CommitOid,
    ) -> Result<Option<CommitOid>>;
    /// `branch`'s publish chain: its state and top. Read only for a commit off
    /// the anchor on a repo that publishes its default branch.
    fn chain(&self, branch: &str) -> Result<(ChainState, Option<CommitOid>)>;
}

impl Ancestry for Git {
    fn read_ref(&self, name: &str) -> Result<Option<CommitOid>> {
        Git::read_ref(self, name)
    }

    fn is_ancestor(&self, ancestor: &CommitOid, descendant: &CommitOid) -> Result<bool> {
        match Git::is_ancestor(self, ancestor, descendant) {
            Err(e) if e.code == Code::NoSuchCommit => Ok(false),
            other => other,
        }
    }

    fn landed_copy(
        &self,
        id: u64,
        repo: &RepoName,
        session: &SessionId,
        commit: &CommitOid,
        tip: &CommitOid,
    ) -> Result<Option<CommitOid>> {
        Git::landed_copy(self, id, repo, session, commit, tip)
    }

    fn chain(&self, branch: &str) -> Result<(ChainState, Option<CommitOid>)> {
        Ok(match publish_chain(self, branch)? {
            Some(c) => (c.state, c.top),
            None => (ChainState::Absent, None),
        })
    }
}

/// A `rebase_pending` row folded into the item it names, or a `pending` item's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PendingFold {
    pub rev: u64,
    pub source: PendingSource,
    pub reason: String,
    pub unpushed: bool,
}

/// One commit the close would abandon that is not on the anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Outstanding {
    /// `not_pushed` when the repo publishes its default branch, else `not_merged`.
    pub code: &'static str,
    pub repo: RepoName,
    /// `base`, `publish`, `pending`, `planned_replay`, `planned_publish`.
    pub what: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<CommitOid>,
    /// A `publish` item's commit; `null` when an open row names none. Other
    /// items have no such key ([`Outstanding::to_json`]).
    pub commit: Option<CommitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    #[serde(rename = "ref")]
    pub anchor: String,
    pub tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_chain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<CommitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingFold>,
}

impl Outstanding {
    /// The item as JSON. `commit` is on every `publish` item (`null` for an open
    /// row with no commit) and on no other.
    pub fn to_json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if self.what != "publish"
            && let Some(m) = v.as_object_mut()
        {
            m.remove("commit");
        }
        v
    }
}

fn chain_name(s: ChainState) -> &'static str {
    match s {
        ChainState::Absent => "absent",
        ChainState::Rewound => "rewound",
        ChainState::Equal => "equal",
        ChainState::Leads => "leads",
        ChainState::Behind => "behind",
        ChainState::Diverged => "diverged",
    }
}

fn phase_name(p: PublishPhase) -> &'static str {
    match p {
        PublishPhase::Running => "running",
        PublishPhase::Done => "done",
        PublishPhase::Unknown => "unknown",
    }
}

/// What one publish row says this session wrote in this repo.
enum RowCommit {
    /// Stopped before writing anything, or not this repo's row.
    Skip,
    /// Could not be read from the row: fails closed.
    Missing,
    Commit(CommitOid),
}

fn outcome_commit(r: &PublishRecord) -> Option<CommitOid> {
    r.outcome
        .as_ref()
        .and_then(|o| o.get("commit"))
        .and_then(Value::as_str)
        .and_then(|c| CommitOid::try_from(c.to_owned()).ok())
}

/// The reading of strata-publish's `row_commit`, over `store.ref_changes`:
/// strata-publish depends on this crate, not the other way.
fn row_commit(store: &Store, r: &PublishRecord, repo: &RepoName) -> Result<RowCommit> {
    let other_repo = r
        .outcome
        .as_ref()
        .and_then(|o| o.get("repo"))
        .and_then(Value::as_str)
        .is_some_and(|x| x != repo.as_str());
    if other_repo {
        return Ok(RowCommit::Skip);
    }
    match r.phase {
        PublishPhase::Done => Ok(outcome_commit(r).map_or(RowCommit::Skip, RowCommit::Commit)),
        PublishPhase::Running => Ok(RowCommit::Missing),
        PublishPhase::Unknown => {
            if let Some(c) = outcome_commit(r) {
                return Ok(RowCommit::Commit(c));
            }
            let written = store
                .ref_changes(r.group.id)?
                .into_iter()
                .rev()
                .find(|c| {
                    c.op == RefOp::UpdateRef && c.outcome == RefOutcome::Applied && c.repo == *repo
                })
                .map(|c| c.new);
            Ok(written.map_or(RowCommit::Missing, RowCommit::Commit))
        }
    }
}

/// One repo's reads and the fields every one of its items shares.
struct RepoProof<'a, A> {
    git: &'a A,
    cfg: &'a RepoConfig,
    session: &'a SessionId,
    repo: &'a RepoName,
    tracked: bool,
    anchor: String,
    tip: Option<CommitOid>,
    same_branch: bool,
}

impl<A: Ancestry> RepoProof<'_, A> {
    fn on_anchor(&self, c: &CommitOid) -> Result<bool> {
        match &self.tip {
            Some(t) => self.git.is_ancestor(c, t),
            None => Ok(false),
        }
    }

    fn item(&self, what: &'static str) -> Outstanding {
        Outstanding {
            code: if self.same_branch { "not_pushed" } else { "not_merged" },
            repo: self.repo.clone(),
            what,
            head: None,
            commit: None,
            publish: None,
            phase: None,
            stopped: None,
            anchor: self.anchor.clone(),
            tracked: self.tracked,
            publish_branch: (!self.same_branch).then(|| self.cfg.publish.branch.clone()),
            default_branch: (!self.same_branch).then(|| self.cfg.default_branch.clone()),
            on_chain: None,
            chain: None,
            copy: None,
            pending: None,
        }
    }

    /// `publish` lines: every row with a batch of this session and repo.
    fn publish_items(
        &self,
        store: &Store,
        rows: &[PublishRecord],
        seen: &mut BTreeSet<CommitOid>,
        items: &mut Vec<Outstanding>,
    ) -> Result<()> {
        let mut own: Vec<&PublishRecord> = rows
            .iter()
            .filter(|r| {
                r.group.batches.iter().any(|b| b.session == *self.session && b.repo == *self.repo)
            })
            .collect();
        own.sort_by_key(|r| r.group.id);
        for r in own {
            let commit = match row_commit(store, r, self.repo)? {
                RowCommit::Skip => continue,
                RowCommit::Missing => None,
                RowCommit::Commit(c) => {
                    if self.on_anchor(&c)? || self.copy_on_anchor(r.group.id, &c)? {
                        continue;
                    }
                    if !seen.insert(c.clone()) {
                        continue;
                    }
                    Some(c)
                }
            };
            let mut item = self.item("publish");
            item.commit = commit;
            item.publish = Some(r.group.id);
            item.phase = Some(phase_name(r.phase));
            item.stopped = r
                .outcome
                .as_ref()
                .and_then(|o| o.get("stopped"))
                .and_then(Value::as_str)
                .filter(|s| *s == Code::PushRefused.as_str())
                .map(str::to_owned);
            items.push(item);
        }
        Ok(())
    }

    fn copy_on_anchor(&self, id: u64, c: &CommitOid) -> Result<bool> {
        match &self.tip {
            Some(t) => Ok(self.git.landed_copy(id, self.repo, self.session, c, t)?.is_some()),
            None => Ok(false),
        }
    }

    /// The `pending`, `planned_replay` and `planned_publish` lines of the row.
    fn row_items(
        &self,
        row: &RebasePending,
        rows: &[PublishRecord],
        seen: &mut BTreeSet<CommitOid>,
        items: &mut Vec<Outstanding>,
    ) -> Result<()> {
        if let Some(head) = &row.head {
            let head = CommitOid::try_from(head.clone())?;
            if !self.on_anchor(&head)? {
                seen.insert(head.clone());
                let mut item = self.item("pending");
                item.head = Some(head);
                item.pending = Some(PendingFold {
                    rev: row.rev,
                    source: row.source,
                    reason: row.reason.clone(),
                    unpushed: row.unpushed,
                });
                items.push(item);
            }
        }
        if let Some(planned) = &row.planned_head {
            let planned = CommitOid::try_from(planned.clone())?;
            if !self.on_anchor(&planned)? && seen.insert(planned.clone()) {
                let mut item = self.item("planned_replay");
                item.head = Some(planned);
                items.push(item);
            }
        }
        if let Some(id) = row.planned_publish {
            let decided = rows
                .iter()
                .find(|r| r.group.id == id)
                .is_some_and(|r| r.phase != PublishPhase::Running);
            if !decided {
                let mut item = self.item("planned_publish");
                item.publish = Some(id);
                items.push(item);
            }
        }
        Ok(())
    }

    /// `not_pushed` only: whether the next push can land each item. The chain
    /// is read once, and only when something is off the anchor.
    fn fill_chain(&self, items: &mut [Outstanding]) -> Result<()> {
        if !self.same_branch || items.is_empty() {
            return Ok(());
        }
        let (state, top) = self.git.chain(&self.cfg.default_branch)?;
        let leading = matches!(state, ChainState::Leads | ChainState::Diverged);
        for item in items.iter_mut() {
            let commit = item.commit.clone().or_else(|| item.head.clone());
            let mut reaches = false;
            if let (Some(c), Some(t)) = (&commit, &top) {
                if self.git.is_ancestor(c, t)? {
                    reaches = true;
                } else if let Some(id) = item.publish.filter(|_| item.what == "publish")
                    && let Some(copy) = self.git.landed_copy(id, self.repo, self.session, c, t)?
                {
                    item.copy = Some(copy);
                    reaches = true;
                }
            }
            item.chain = Some(chain_name(state));
            item.on_chain = Some(reaches && leading);
        }
        Ok(())
    }
}

/// A `pending` head equal to another failing item's commit or copy goes into
/// that item, so each distinct commit is reported once.
fn fold_pending(items: &mut Vec<Outstanding>) {
    let Some(at) = items.iter().position(|i| i.what == "pending") else {
        return;
    };
    let head = items[at].head.clone();
    let names = |i: &Outstanding| {
        i.what != "pending"
            && head.is_some()
            && (i.commit == head || i.head == head || i.copy == head)
    };
    if let Some(into) = items.iter().position(names) {
        let pending = items.remove(at).pending;
        let into = if into > at { into - 1 } else { into };
        items[into].pending = pending;
    }
}

fn repo_items<A: Ancestry>(
    store: &Store,
    proof: &RepoProof<'_, A>,
    ws: &WorkspaceState,
    row: Option<&RebasePending>,
    rows: &[PublishRecord],
) -> Result<Vec<Outstanding>> {
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    if !proof.on_anchor(&ws.head)? {
        seen.insert(ws.head.clone());
        let mut item = proof.item("base");
        item.head = Some(ws.head.clone());
        items.push(item);
    }
    proof.publish_items(store, rows, &mut seen, &mut items)?;
    if let Some(row) = row {
        proof.row_items(row, rows, &mut seen, &mut items)?;
    }
    proof.fill_chain(&mut items)?;
    fold_pending(&mut items);
    Ok(items)
}

/// For every repo of the manifest, tracked or not, every commit this session's
/// close would abandon that is not on `origin/<default_branch>`: the workspace
/// base, each of its publishes (or that commit's replayed copy) and every target
/// of its `rebase_pending` row. `pending` is `store.rebase_pending(Some(id))` and
/// `rows` is `store.publishes()`, both passed in: a step that guards on what it
/// proved must prove exactly those.
pub fn outstanding<'g, A, G, C>(
    store: &Store,
    git_of: G,
    cfg_of: C,
    rec: &SessionRecord,
    pending: &[RebasePending],
    rows: &[PublishRecord],
) -> Result<Vec<Outstanding>>
where
    A: Ancestry + 'g,
    G: Fn(&RepoName) -> Result<&'g A>,
    C: Fn(&RepoName) -> Result<&'g RepoConfig>,
{
    let session = &rec.manifest.id;
    let mut out = Vec::new();
    for (repo, ws) in &rec.manifest.repos {
        let cfg = cfg_of(repo)?;
        let git = git_of(repo)?;
        let anchor = remote_ref("origin", &cfg.default_branch);
        let tip = git.read_ref(&anchor)?;
        let proof = RepoProof {
            git,
            cfg,
            session,
            repo,
            tracked: ws.tracked,
            anchor,
            tip,
            same_branch: cfg.publish.branch == cfg.default_branch,
        };
        let row = pending.iter().find(|p| p.repo == *repo);
        out.extend(repo_items(store, &proof, ws, row, rows)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use serde_json::json;
    use strata_deps::ChainState;
    use strata_store::{
        Lifecycle, PendingSource, PublishPhase, PublishRecord, RebasePending, SessionRecord, Store,
    };
    use strata_types::{
        BaseGen, Batch, CheckpointId, CommitOid, DepsSetId, PublishTarget, ReleaseGroup,
        RepoConfig, RepoName, RepoPath, SessionId, SessionManifest, TreeOid, WorkspaceState,
    };

    use super::{Ancestry, Outstanding, PendingFold, outstanding};

    fn oid(nibble: char) -> CommitOid {
        CommitOid::try_from(nibble.to_string().repeat(40)).unwrap()
    }

    fn sid() -> SessionId {
        SessionId::try_from("s-abcdefgh".to_string()).unwrap()
    }

    fn repo() -> RepoName {
        RepoName::try_from("tool".to_string()).unwrap()
    }

    fn cfg(same_branch: bool) -> RepoConfig {
        RepoConfig {
            path: PathBuf::from("/tmp/tool"),
            default_branch: "main".into(),
            deps_dirs: vec![],
            seed_caches: vec![],
            seed_files: None,
            install_inputs: vec![],
            install: None,
            checks: vec![],
            publish: PublishTarget {
                branch: if same_branch { "main".into() } else { "release".into() },
                push: true,
                confirm: false,
                auto_close: None,
            },
            deploy_on_push: false,
            pin_unpushed: true,
            check_siblings: Vec::new(),
            rm: Default::default(),
            rebase: Default::default(),
            sync: Default::default(),
        }
    }

    fn rec(head: CommitOid, tracked: bool) -> SessionRecord {
        let r = repo();
        let mut repos = BTreeMap::new();
        repos.insert(
            r.clone(),
            WorkspaceState {
                base: BaseGen {
                    repo: r,
                    tree: TreeOid::try_from("1".repeat(40)).unwrap(),
                    deps: DepsSetId::try_from("a".repeat(64)).unwrap(),
                },
                head,
                path: PathBuf::from("/tmp/ws"),
                tracked,
            },
        );
        SessionRecord {
            manifest: SessionManifest {
                id: sid(),
                ticket: None,
                agent: None,
                root: PathBuf::from("/tmp/s"),
                repos,
            },
            lifecycle: Lifecycle::Active,
            created: UNIX_EPOCH,
            removed: None,
            phase: None,
            scene: None,
            workspaces: BTreeMap::new(),
        }
    }

    fn empty_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("strata");
        let checkout = dir.path().join("co");
        std::fs::create_dir_all(&checkout).unwrap();
        let store = Store::open(&root, &[&checkout]).unwrap();
        (dir, store)
    }

    fn pending_row(
        head: Option<&str>,
        planned_head: Option<&str>,
        planned_publish: Option<u64>,
        source: PendingSource,
        unpushed: bool,
    ) -> RebasePending {
        RebasePending {
            session: sid(),
            repo: repo(),
            rev: 1,
            source,
            to_deps: None,
            from_tree: "1".repeat(40),
            from_deps: "a".repeat(64),
            head: head.map(str::to_owned),
            tree: head.map(|_| "2".repeat(40)),
            planned_head: planned_head.map(str::to_owned),
            planned_tree: planned_head.map(|_| "3".repeat(40)),
            planned_publish,
            reason: "recent_writes".into(),
            detail: None,
            first_at: UNIX_EPOCH,
            last_at: UNIX_EPOCH,
            attempts: 0,
            unpushed,
        }
    }

    fn publish_done(id: u64, commit: Option<&str>, stopped: Option<&str>) -> PublishRecord {
        let mut outcome = json!({ "repo": "tool" });
        if let Some(c) = commit {
            outcome["commit"] = json!(c);
        }
        if let Some(s) = stopped {
            outcome["stopped"] = json!(s);
        }
        PublishRecord {
            message: None,
            group: ReleaseGroup {
                id,
                batches: vec![Batch {
                    session: sid(),
                    repo: repo(),
                    checkpoint: CheckpointId::try_from(1u64).unwrap(),
                    paths: vec![RepoPath::try_from("src/p.ts".to_string()).unwrap()],
                }],
            },
            phase: PublishPhase::Done,
            started: UNIX_EPOCH,
            ended: Some(UNIX_EPOCH + Duration::from_secs(1)),
            outcome: Some(outcome),
        }
    }

    fn publish_running(id: u64) -> PublishRecord {
        PublishRecord {
            message: None,
            group: ReleaseGroup {
                id,
                batches: vec![Batch {
                    session: sid(),
                    repo: repo(),
                    checkpoint: CheckpointId::try_from(1u64).unwrap(),
                    paths: vec![],
                }],
            },
            phase: PublishPhase::Running,
            started: UNIX_EPOCH,
            ended: None,
            outcome: None,
        }
    }

    struct Fake {
        tip: Option<CommitOid>,
        /// descendant contains ancestor.
        ancestors: HashMap<(String, String), bool>,
        copies: HashMap<(u64, String), CommitOid>,
        chain: (ChainState, Option<CommitOid>),
        pruned: HashSet<String>,
    }

    impl Fake {
        fn with_tip(tip: CommitOid) -> Self {
            Self {
                tip: Some(tip.clone()),
                chain: (ChainState::Leads, Some(tip)),
                ancestors: HashMap::new(),
                copies: HashMap::new(),
                pruned: HashSet::new(),
            }
        }

        fn link(&mut self, ancestor: &CommitOid, descendant: &CommitOid) {
            self.ancestors.insert((ancestor.as_str().to_owned(), descendant.as_str().to_owned()), true);
        }
    }

    impl Ancestry for Fake {
        fn read_ref(&self, name: &str) -> crate::Result<Option<CommitOid>> {
            assert_eq!(name, "refs/strata/remotes/origin/main");
            Ok(self.tip.clone())
        }

        fn is_ancestor(&self, ancestor: &CommitOid, descendant: &CommitOid) -> crate::Result<bool> {
            // Same mapping as `Ancestry for Git`: a pruned commit is simply not an ancestor.
            if self.pruned.contains(ancestor.as_str()) || self.pruned.contains(descendant.as_str()) {
                return Ok(false);
            }
            if ancestor == descendant {
                return Ok(true);
            }
            Ok(self
                .ancestors
                .get(&(ancestor.as_str().to_owned(), descendant.as_str().to_owned()))
                .copied()
                .unwrap_or(false))
        }

        fn landed_copy(
            &self,
            id: u64,
            _repo: &RepoName,
            _session: &SessionId,
            commit: &CommitOid,
            _tip: &CommitOid,
        ) -> crate::Result<Option<CommitOid>> {
            if self.pruned.contains(commit.as_str()) {
                return Ok(None);
            }
            Ok(self.copies.get(&(id, commit.as_str().to_owned())).cloned())
        }

        fn chain(&self, branch: &str) -> crate::Result<(ChainState, Option<CommitOid>)> {
            assert_eq!(branch, "main");
            Ok(self.chain.clone())
        }
    }

    fn run(
        store: &Store,
        fake: &Fake,
        same_branch: bool,
        head: CommitOid,
        pending: &[RebasePending],
        rows: &[PublishRecord],
    ) -> Vec<Outstanding> {
        let cfg = cfg(same_branch);
        let record = rec(head, true);
        outstanding(store, |_| Ok(fake), |_| Ok(&cfg), &record, pending, rows).unwrap()
    }

    #[test]
    fn base_on_anchor_with_nothing_else_is_clear() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let fake = Fake::with_tip(tip.clone());
        let items = run(&store, &fake, true, tip, &[], &[]);
        assert!(items.is_empty(), "{items:?}");
    }

    #[test]
    fn base_off_anchor_is_not_pushed_and_fills_chain() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let head = oid('b');
        let mut fake = Fake::with_tip(tip.clone());
        fake.chain = (ChainState::Leads, Some(oid('c')));
        fake.link(&head, &oid('c'));
        let items = run(&store, &fake, true, head.clone(), &[], &[]);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].what, "base");
        assert_eq!(items[0].code, "not_pushed");
        assert_eq!(items[0].head.as_ref(), Some(&head));
        assert_eq!(items[0].anchor, "refs/strata/remotes/origin/main");
        assert_eq!(items[0].chain, Some("leads"));
        assert_eq!(items[0].on_chain, Some(true));
        assert!(items[0].publish_branch.is_none());
        let v = items[0].to_json();
        assert!(v.get("commit").is_none(), "{v}");
        assert_eq!(v["head"], json!(head.as_str()));
    }

    #[test]
    fn split_branch_codes_not_merged_and_names_both_branches() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let head = oid('b');
        let fake = Fake::with_tip(tip);
        let items = run(&store, &fake, false, head, &[], &[]);
        assert_eq!(items[0].code, "not_merged");
        assert_eq!(items[0].publish_branch.as_deref(), Some("release"));
        assert_eq!(items[0].default_branch.as_deref(), Some("main"));
        assert!(items[0].chain.is_none(), "chain is not_pushed-only");
        assert!(items[0].on_chain.is_none());
    }

    #[test]
    fn publish_off_anchor_stands_and_copy_clears_it() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let base = tip.clone();
        let commit = oid('c');
        let mut fake = Fake::with_tip(tip.clone());
        let rows = [publish_done(41, Some(commit.as_str()), None)];
        let items = run(&store, &fake, true, base.clone(), &[], &rows);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].what, "publish");
        assert_eq!(items[0].commit.as_ref(), Some(&commit));
        assert_eq!(items[0].publish, Some(41));
        assert_eq!(items[0].phase, Some("done"));
        let v = items[0].to_json();
        assert_eq!(v["commit"], json!(commit.as_str()));

        fake.copies.insert((41, commit.as_str().to_owned()), oid('d'));
        fake.link(&oid('d'), &tip);
        let items = run(&store, &fake, true, base, &[], &rows);
        assert!(items.is_empty(), "replayed copy on tip clears the publish: {items:?}");
    }

    #[test]
    fn pending_head_equal_to_publish_commit_is_folded_in() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let base = tip.clone();
        let commit = oid('c');
        let fake = Fake::with_tip(tip);
        let rows = [publish_done(7, Some(commit.as_str()), Some("PushRefused"))];
        let pending = [pending_row(Some(commit.as_str()), None, None, PendingSource::Publish, true)];
        let items = run(&store, &fake, true, base, &pending, &rows);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].what, "publish");
        assert_eq!(items[0].stopped.as_deref(), Some("PushRefused"));
        assert_eq!(
            items[0].pending,
            Some(PendingFold {
                rev: 1,
                source: PendingSource::Publish,
                reason: "recent_writes".into(),
                unpushed: true,
            })
        );
    }

    #[test]
    fn planned_replay_and_open_planned_publish_stand() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let base = tip.clone();
        let planned = oid('e');
        let fake = Fake::with_tip(tip);
        let pending = [pending_row(None, Some(planned.as_str()), Some(99), PendingSource::Replay, false)];
        let items = run(&store, &fake, true, base, &pending, &[]);
        let whats: Vec<_> = items.iter().map(|i| i.what).collect();
        assert_eq!(whats, ["planned_replay", "planned_publish"], "{items:?}");
        assert_eq!(items[0].head.as_ref(), Some(&planned));
        assert_eq!(items[1].publish, Some(99));

        // A non-Running publish row decides the planned_publish line.
        let rows = [publish_done(99, Some(oid('f').as_str()), None)];
        let items = run(&store, &fake, true, oid('a'), &pending, &rows);
        assert!(items.iter().all(|i| i.what != "planned_publish"), "{items:?}");
        assert!(items.iter().any(|i| i.what == "publish"));
    }

    #[test]
    fn running_publish_fails_closed_with_null_commit() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let fake = Fake::with_tip(tip.clone());
        let rows = [publish_running(3)];
        let items = run(&store, &fake, true, tip, &[], &rows);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].what, "publish");
        assert!(items[0].commit.is_none());
        assert_eq!(items[0].phase, Some("running"));
        assert_eq!(items[0].to_json()["commit"], json!(null));
    }

    #[test]
    fn pruned_commit_is_not_an_ancestor_and_stays_outstanding() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let head = oid('b');
        let mut fake = Fake::with_tip(tip);
        fake.pruned.insert(head.as_str().to_owned());
        let items = run(&store, &fake, true, head.clone(), &[], &[]);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].what, "base");
        assert_eq!(items[0].head.as_ref(), Some(&head));
    }

    #[test]
    fn on_chain_false_when_chain_does_not_lead() {
        let (_d, store) = empty_store();
        let tip = oid('a');
        let head = oid('b');
        let mut fake = Fake::with_tip(tip);
        // Off the anchor, but reachable from an Equal chain top: still not pushable.
        fake.chain = (ChainState::Equal, Some(oid('c')));
        fake.link(&head, &oid('c'));
        let items = run(&store, &fake, true, head, &[], &[]);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].chain, Some("equal"));
        assert_eq!(items[0].on_chain, Some(false), "equal never lands a push");
    }
}

//! Design p1-autoclose §10 row 3: auto-close clause 4 (`strata_session::outstanding`,
//! the function `auto_close::Run::clauses` calls first) over a fake `Ancestry`.
//! `reasons_follow_the_evaluation_order` lives in `src/auto_close.rs`'s unit
//! tests: `Refusals` is private to the daemon.

// Shared fixture (Slice T, design §10 row 3): a fake `Ancestry` per repo over
// the real `strata_session::outstanding`.
#[allow(dead_code)]
mod fx {
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use serde_json::{Value, json};
    use strata_deps::ChainState;
    use strata_session::{Ancestry, Outstanding, outstanding};
    use strata_store::{
        Lifecycle, PendingSource, PublishPhase, PublishRecord, RebasePending, SessionRecord, Store,
    };
    use strata_types::{
        BaseGen, Batch, CheckpointId, Code, CommitOid, DepsSetId, PublishTarget, ReleaseGroup,
        RepoConfig, RepoName, RepoPath, SessionId, SessionManifest, StrataError, TreeOid,
        WorkspaceState,
    };

    pub type R<T> = std::result::Result<T, StrataError>;

    pub fn oid(n: u64) -> CommitOid {
        CommitOid::try_from(format!("{n:040x}")).unwrap()
    }
    pub fn sid(s: &str) -> SessionId {
        SessionId::try_from(format!("s-{s}")).unwrap()
    }
    pub fn me() -> SessionId {
        sid("abcdefgh")
    }
    pub fn name(r: &str) -> RepoName {
        RepoName::try_from(r.to_string()).unwrap()
    }

    pub fn cfg(publish_branch: &str) -> RepoConfig {
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
                branch: publish_branch.into(),
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

    /// `(repo, head, tracked)` per manifest repo.
    pub fn rec(repos: &[(&str, CommitOid, bool)]) -> SessionRecord {
        let mut map = BTreeMap::new();
        for (r, head, tracked) in repos {
            map.insert(
                name(r),
                WorkspaceState {
                    base: BaseGen {
                        repo: name(r),
                        tree: TreeOid::try_from("1".repeat(40)).unwrap(),
                        deps: DepsSetId::try_from("a".repeat(64)).unwrap(),
                    },
                    head: head.clone(),
                    path: PathBuf::from(format!("/tmp/ws/{r}")),
                    tracked: *tracked,
                },
            );
        }
        SessionRecord {
            manifest: SessionManifest {
                id: me(),
                ticket: None,
                agent: None,
                root: PathBuf::from("/tmp/s"),
                repos: map,
            },
            lifecycle: Lifecycle::Active,
            created: UNIX_EPOCH,
            removed: None,
            phase: None,
            scene: None,
            workspaces: BTreeMap::new(),
        }
    }

    pub fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("co");
        std::fs::create_dir_all(&checkout).unwrap();
        let store = Store::open(&dir.path().join("strata"), &[&checkout]).unwrap();
        (dir, store)
    }

    pub fn row(
        id: u64,
        session: &SessionId,
        repo: &str,
        phase: PublishPhase,
        commit: Option<&CommitOid>,
    ) -> PublishRecord {
        let mut outcome = json!({ "repo": repo });
        if let Some(c) = commit {
            outcome["commit"] = json!(c.as_str());
        }
        PublishRecord {
            message: None,
            group: ReleaseGroup {
                id,
                batches: vec![Batch {
                    session: session.clone(),
                    repo: name(repo),
                    checkpoint: CheckpointId::try_from(1u64).unwrap(),
                    paths: vec![RepoPath::try_from("src/p.ts".to_string()).unwrap()],
                }],
            },
            phase,
            started: UNIX_EPOCH,
            ended: (phase != PublishPhase::Running).then(|| UNIX_EPOCH + Duration::from_secs(1)),
            outcome: (phase != PublishPhase::Running).then_some(outcome),
        }
    }

    pub fn pending(
        repo: &str,
        head: Option<&CommitOid>,
        planned_head: Option<&CommitOid>,
        planned_publish: Option<u64>,
        source: PendingSource,
    ) -> RebasePending {
        RebasePending {
            session: me(),
            repo: name(repo),
            rev: 3,
            source,
            to_deps: None,
            from_tree: "1".repeat(40),
            from_deps: "a".repeat(64),
            head: head.map(|c| c.as_str().to_owned()),
            tree: head.map(|_| "2".repeat(40)),
            planned_head: planned_head.map(|c| c.as_str().to_owned()),
            planned_tree: planned_head.map(|_| "3".repeat(40)),
            planned_publish,
            reason: "recent_writes".into(),
            detail: None,
            first_at: UNIX_EPOCH,
            last_at: UNIX_EPOCH,
            attempts: 0,
            unpushed: true,
        }
    }

    pub struct Fake {
        pub refs: HashMap<String, CommitOid>,
        /// A ref name whose read panics: provably never consulted.
        pub forbid: Option<String>,
        pub links: HashSet<(String, String)>,
        pub copies: HashMap<(u64, String), CommitOid>,
        pub chain: (ChainState, Option<CommitOid>),
        pub fail: Option<Code>,
    }

    impl Fake {
        /// `origin/main` at `tip`, the chain leading at `tip`.
        pub fn at(tip: &CommitOid) -> Self {
            let mut refs = HashMap::new();
            refs.insert("refs/strata/remotes/origin/main".to_owned(), tip.clone());
            Self {
                refs,
                forbid: None,
                links: HashSet::new(),
                copies: HashMap::new(),
                chain: (ChainState::Leads, Some(tip.clone())),
                fail: None,
            }
        }
        pub fn link(&mut self, ancestor: &CommitOid, descendant: &CommitOid) {
            self.links.insert((ancestor.as_str().into(), descendant.as_str().into()));
        }
    }

    impl Ancestry for Fake {
        fn read_ref(&self, n: &str) -> R<Option<CommitOid>> {
            if self.forbid.as_deref() == Some(n) {
                panic!("consulted {n}");
            }
            Ok(self.refs.get(n).cloned())
        }
        fn is_ancestor(&self, a: &CommitOid, d: &CommitOid) -> R<bool> {
            if let Some(code) = self.fail {
                return Err(StrataError::new(code, "fake git failure"));
            }
            Ok(a == d || self.links.contains(&(a.as_str().into(), d.as_str().into())))
        }
        fn landed_copy(
            &self,
            id: u64,
            _repo: &RepoName,
            _session: &SessionId,
            commit: &CommitOid,
            _tip: &CommitOid,
        ) -> R<Option<CommitOid>> {
            Ok(self.copies.get(&(id, commit.as_str().to_owned())).cloned())
        }
        fn chain(&self, branch: &str) -> R<(ChainState, Option<CommitOid>)> {
            assert_eq!(branch, "main", "the chain of the default branch");
            Ok(self.chain.clone())
        }
    }

    pub fn run(
        store: &Store,
        fakes: &[(&str, &Fake, &RepoConfig)],
        rec: &SessionRecord,
        pending: &[RebasePending],
        rows: &[PublishRecord],
    ) -> R<Vec<Outstanding>> {
        let git: HashMap<RepoName, &Fake> = fakes.iter().map(|(r, f, _)| (name(r), *f)).collect();
        let cfgs: HashMap<RepoName, &RepoConfig> =
            fakes.iter().map(|(r, _, c)| (name(r), *c)).collect();
        outstanding(store, |r| Ok(git[r]), |r| Ok(cfgs[r]), rec, pending, rows)
    }

    pub fn whats(items: &[Outstanding]) -> Vec<&'static str> {
        items.iter().map(|i| i.what).collect()
    }

    pub fn json_of(items: &[Outstanding]) -> Vec<Value> {
        items.iter().map(Outstanding::to_json).collect()
    }
}

use fx::*;
use serde_json::json;
use strata_store::PublishPhase;

#[test]
fn landed_clauses_cover_every_manifest_repo() {
    let (_d, store) = store();
    let (tip_tool, tip_front) = (oid(1), oid(2));
    let front_head = oid(3);
    let tool = Fake::at(&tip_tool);
    let mut front = Fake::at(&tip_front);
    let main = cfg("main");
    let rec = rec(&[("tool", tip_tool.clone(), true), ("front", front_head.clone(), false)]);

    let items = run(&store, &[("tool", &tool, &main), ("front", &front, &main)], &rec, &[], &[]).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    let v = items[0].to_json();
    assert_eq!(v["code"], "not_pushed");
    assert_eq!(v["repo"], "front");
    assert_eq!(v["tracked"], false);
    assert_eq!(v["what"], "base");
    assert_eq!(v["head"], json!(front_head.as_str()));

    front.link(&front_head, &tip_front);
    let items = run(&store, &[("tool", &tool, &main), ("front", &front, &main)], &rec, &[], &[]).unwrap();
    assert!(items.is_empty(), "both on origin passes: {items:?}");
}

#[test]
fn landed_clauses_anchor_on_the_default_branch() {
    let (_d, store) = store();
    let (m, rel) = (oid(10), oid(11));
    let (base, commit) = (oid(20), oid(21));
    let mut fake = Fake::at(&m);
    fake.refs.insert("refs/strata/remotes/origin/release".into(), rel.clone());
    // Both are on origin/release, neither on origin/main.
    fake.link(&base, &rel);
    fake.link(&commit, &rel);
    fake.forbid = Some("refs/strata/remotes/origin/release".into());
    let rows = [row(4, &me(), "tool", PublishPhase::Done, Some(&commit))];
    let rec = rec(&[("tool", base.clone(), true)]);

    let split = cfg("release");
    let items = run(&store, &[("tool", &fake, &split)], &rec, &[], &rows).unwrap();
    let v = json_of(&items);
    assert_eq!(whats(&items), ["base", "publish"], "{v:?}");
    for item in &v {
        assert_eq!(item["code"], "not_merged");
        assert_eq!(item["publish_branch"], "release");
        assert_eq!(item["default_branch"], "main");
        assert_eq!(item["ref"], "refs/strata/remotes/origin/main");
    }
    assert_eq!(v[0]["head"], json!(base.as_str()));
    assert_eq!(v[1]["commit"], json!(commit.as_str()));
    assert_eq!(v[1]["publish"], 4);

    fake.forbid = None;
    let same = cfg("main");
    let items = run(&store, &[("tool", &fake, &same)], &rec, &[], &rows).unwrap();
    assert_eq!(whats(&items), ["base", "publish"]);
    assert!(items.iter().all(|i| i.code == "not_pushed"), "{items:?}");
    assert!(items.iter().all(|i| i.publish_branch.is_none()));

    fake.link(&base, &m);
    fake.link(&commit, &m);
    fake.forbid = Some("refs/strata/remotes/origin/release".into());
    for c in [&split, &same] {
        let items = run(&store, &[("tool", &fake, c)], &rec, &[], &rows).unwrap();
        assert!(items.is_empty(), "on origin/main passes: {items:?}");
    }
}

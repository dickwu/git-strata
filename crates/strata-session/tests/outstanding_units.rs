//! Design p1-autoclose §10 row 3 (r5/r6): units of `outstanding` (§2.1) over a
//! fake `Ancestry`, and one over the real `Ancestry for Git`.

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
            self.links
                .insert((ancestor.as_str().into(), descendant.as_str().into()));
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

use std::collections::BTreeMap;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use fx::*;
use serde_json::json;
use strata_deps::ChainState;
use strata_store::{DepsOrigin, PendingSource, PublishPhase, RefChange, RefOp, RefOutcome};
use strata_types::{
    BaseGen, BaseManifest, Batch, Checkpoint, CheckpointReason, Code, DepsSet, DepsSetId,
    ReleaseGroup, RepoPath, SessionManifest, TreeOid, WorkspaceState,
};

#[test]
fn outstanding_reads_every_publish_row_of_the_pair() {
    let (_d, store) = store();
    let l = oid(1);
    let fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    let mut shared = row(
        9,
        &sid("zzzzzzzz"),
        "tool",
        PublishPhase::Done,
        Some(&oid(9)),
    );
    shared.group.batches.push(
        row(9, &me(), "tool", PublishPhase::Done, None)
            .group
            .batches
            .remove(0),
    );
    // Earlier calls' rows, out of id order: every one is read, none from a reply.
    let rows = [
        shared,
        row(5, &me(), "tool", PublishPhase::Done, Some(&oid(5))),
    ];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &rows).unwrap();
    assert_eq!(whats(&items), ["publish", "publish"], "{items:?}");
    assert_eq!(
        items.iter().map(|i| i.publish).collect::<Vec<_>>(),
        [Some(5), Some(9)]
    );
    assert_eq!(items[0].commit, Some(oid(5)));
    assert_eq!(items[1].commit, Some(oid(9)));
}

#[test]
fn outstanding_ignores_another_sessions_row() {
    let (_d, store) = store();
    let l = oid(1);
    let fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    let rows = [
        row(
            5,
            &sid("zzzzzzzz"),
            "tool",
            PublishPhase::Done,
            Some(&oid(5)),
        ),
        row(6, &sid("zzzzzzzz"), "tool", PublishPhase::Running, None),
        row(7, &me(), "front", PublishPhase::Done, Some(&oid(7))),
    ];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &rows).unwrap();
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn outstanding_counts_every_pending_source() {
    let (_d, store) = store();
    let l = oid(1);
    let h = oid(2);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    for source in [
        PendingSource::Publish,
        PendingSource::Replay,
        PendingSource::Main,
    ] {
        let mut fake = Fake::at(&l);
        let p = [pending("tool", Some(&h), None, None, source)];
        let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &[]).unwrap();
        assert_eq!(whats(&items), ["pending"], "{source:?}: {items:?}");
        assert_eq!(items[0].head, Some(h.clone()));
        assert_eq!(items[0].pending.as_ref().map(|p| p.source), Some(source));
        let v = items[0].to_json();
        assert!(v.get("commit").is_none(), "{v}");

        fake.link(&h, &l);
        let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &[]).unwrap();
        assert!(items.is_empty(), "{source:?} on L passes: {items:?}");
    }
}

#[test]
fn outstanding_fails_closed_on_planned_columns() {
    let (_d, store) = store();
    let l = oid(1);
    let planned = oid(2);
    let fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);

    let p = [pending(
        "tool",
        None,
        Some(&planned),
        None,
        PendingSource::Replay,
    )];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &[]).unwrap();
    assert_eq!(whats(&items), ["planned_replay"], "{items:?}");
    assert_eq!(items[0].head, Some(planned.clone()));

    // `planned_publish` naming a missing row, then a Running one (another
    // session's batch, so the row itself adds no `publish` item).
    let p = [pending(
        "tool",
        None,
        None,
        Some(77),
        PendingSource::Publish,
    )];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &[]).unwrap();
    assert_eq!(whats(&items), ["planned_publish"], "missing row: {items:?}");
    assert_eq!(items[0].publish, Some(77));
    let running = [row(
        77,
        &sid("zzzzzzzz"),
        "tool",
        PublishPhase::Running,
        None,
    )];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &running).unwrap();
    assert_eq!(whats(&items), ["planned_publish"], "running row: {items:?}");
    let done = [row(
        77,
        &sid("zzzzzzzz"),
        "tool",
        PublishPhase::Done,
        Some(&oid(3)),
    )];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &done).unwrap();
    assert!(items.is_empty(), "a decided row clears it: {items:?}");
}

#[test]
fn outstanding_fails_closed_on_an_unknown_row_without_a_commit() {
    let (_d, store) = store();
    let l = oid(1);
    let fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    // No commit in the outcome and no applied ref change in the store.
    let rows = [row(8, &me(), "tool", PublishPhase::Unknown, None)];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &rows).unwrap();
    assert_eq!(whats(&items), ["publish"], "{items:?}");
    assert_eq!(items[0].commit, None);
    assert_eq!(items[0].phase, Some("unknown"));
    assert_eq!(items[0].to_json()["commit"], json!(null));
}

#[test]
fn outstanding_skips_a_done_row_with_a_null_commit() {
    let (_d, store) = store();
    let l = oid(1);
    let fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    let rows = [row(8, &me(), "tool", PublishPhase::Done, None)];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &rows).unwrap();
    assert!(items.is_empty(), "stopped before writing: {items:?}");
}

#[test]
fn outstanding_folds_the_pending_row_into_its_publish_item() {
    let (_d, store) = store();
    let l = oid(1);
    let commit = oid(2);
    let fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    let rows = [row(8, &me(), "tool", PublishPhase::Done, Some(&commit))];
    let p = [pending(
        "tool",
        Some(&commit),
        None,
        None,
        PendingSource::Publish,
    )];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &rows).unwrap();
    assert_eq!(whats(&items), ["publish"], "{items:?}");
    let fold = items[0].pending.as_ref().expect("folded");
    assert_eq!(
        (fold.rev, fold.source, fold.unpushed),
        (3, PendingSource::Publish, true)
    );
    assert_eq!(items[0].to_json()["pending"]["reason"], "recent_writes");

    // A different head is its own item.
    let p = [pending(
        "tool",
        Some(&oid(3)),
        None,
        None,
        PendingSource::Publish,
    )];
    let items = run(&store, &[("tool", &fake, &c)], &rec, &p, &rows).unwrap();
    assert_eq!(whats(&items), ["publish", "pending"], "{items:?}");
    assert!(items[0].pending.is_none());
}

#[test]
fn outstanding_has_no_cap() {
    let (_d, store) = store();
    let l = oid(1);
    let mut fake = Fake::at(&l);
    let c = cfg("main");
    let rec = rec(&[("tool", l.clone(), true)]);
    let rows: Vec<_> = (1..=40u64)
        .map(|id| row(id, &me(), "tool", PublishPhase::Done, Some(&oid(100 + id))))
        .collect();
    for id in 2..=40u64 {
        fake.link(&oid(100 + id), &l);
    }
    let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &rows).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].publish, Some(1));
    assert_eq!(items[0].commit, Some(oid(101)));
}

#[test]
fn on_chain_follows_the_chain_state() {
    let (_d, store) = store();
    let l = oid(1);
    let h = oid(2);
    let top = oid(3);
    let c = cfg("main");
    let rec = rec(&[("tool", h.clone(), true)]);
    // (state, top, head reaches the top, on_chain)
    let cases = [
        (ChainState::Absent, None, false, false),
        (ChainState::Rewound, Some(&top), true, false),
        (ChainState::Equal, Some(&top), true, false),
        (ChainState::Behind, Some(&top), true, false),
        (ChainState::Leads, Some(&top), true, true),
        (ChainState::Leads, Some(&top), false, false),
        (ChainState::Diverged, Some(&top), true, true),
        (ChainState::Diverged, Some(&top), false, false),
    ];
    for (state, t, reaches, want) in cases {
        let mut fake = Fake::at(&l);
        fake.chain = (state, t.cloned());
        if reaches {
            fake.link(&h, &top);
        }
        let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &[]).unwrap();
        assert_eq!(items.len(), 1, "{state:?}: {items:?}");
        assert_eq!(items[0].on_chain, Some(want), "{state:?} reaches={reaches}");
        let name = format!("{state:?}").to_lowercase();
        assert_eq!(items[0].chain, Some(name.as_str()));
    }
    // The split-branch code reads no chain at all.
    let mut fake = Fake::at(&l);
    fake.chain = (ChainState::Leads, Some(top.clone()));
    let split = cfg("release");
    let items = run(&store, &[("tool", &fake, &split)], &rec, &[], &[]).unwrap();
    assert_eq!((items[0].chain, items[0].on_chain), (None, None));
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn outstanding_stands_on_a_pruned_commit() {
    let (_d, store) = store();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let tip = strata_types::CommitOid::try_from(git(root, &["rev-parse", "HEAD"])).unwrap();
    for r in [
        "refs/strata/remotes/origin/main",
        "refs/strata/publish/main",
        "refs/strata/anchor/main",
    ] {
        git(root, &["update-ref", r, tip.as_str()]);
    }
    let real = strata_gitio::Git::open(root).unwrap();
    // A publish commit that is gone from the object store.
    let gone = oid(0xdead);
    let rows = [row(8, &me(), "tool", PublishPhase::Done, Some(&gone))];
    let c = cfg("main");
    let rec = rec(&[("tool", tip.clone(), true)]);
    let items = strata_session::outstanding(&store, |_| Ok(&real), |_| Ok(&c), &rec, &[], &rows)
        .expect("a pruned commit is an item, not an Err");
    assert_eq!(whats(&items), ["publish"], "{items:?}");
    assert_eq!(items[0].commit, Some(gone.clone()));
    assert_eq!(items[0].on_chain, Some(false));

    // Any other git failure stays an Err (the hook reports `error {code}`).
    let mut fake = Fake::at(&tip);
    fake.fail = Some(Code::Unknown);
    let err = run(&store, &[("tool", &fake, &c)], &rec, &[], &rows).unwrap_err();
    assert_eq!(err.code, Code::Unknown);
}

#[test]
fn outstanding_reads_an_unknown_rows_applied_ref_change() {
    // §10 row 20 (Claude r6 nit 10): an Unknown row with no outcome.commit and
    // an applied UpdateRef in ref_changes → the item's commit is that ref's `new`.
    let (_d, store) = store();
    let tip = oid(1);
    let written = oid(42);
    let sid = me();
    let rn = name("tool");
    let tree = TreeOid::try_from("1".repeat(40)).unwrap();
    let deps = DepsSetId::try_from("a".repeat(64)).unwrap();
    let base = BaseGen {
        repo: rn.clone(),
        tree: tree.clone(),
        deps: deps.clone(),
    };
    let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);

    store
        .record_deps_set(
            &DepsSet {
                id: deps,
                repo: rn.clone(),
                inputs_hash: "0".repeat(64),
                platform: "darwin-arm64".into(),
                toolchain: "none".into(),
                dirs: vec![],
            },
            DepsOrigin::Empty,
            None,
            at(1),
        )
        .unwrap();
    store
        .record_base_gen(
            &base,
            &BaseManifest {
                tree,
                entries: BTreeMap::new(),
            },
            at(1),
        )
        .unwrap();

    let root = store.root().join("sessions").join(sid.as_str());
    let mut repos = BTreeMap::new();
    repos.insert(
        rn.clone(),
        WorkspaceState {
            base: base.clone(),
            head: tip.clone(),
            path: root.join("tool"),
            tracked: true,
        },
    );
    store
        .create_session(
            &SessionManifest {
                id: sid.clone(),
                ticket: None,
                agent: None,
                root,
                repos,
            },
            at(10),
        )
        .unwrap();
    assert!(
        store
            .set_lifecycle(
                &sid,
                strata_store::Lifecycle::Creating,
                strata_store::Lifecycle::Active,
                None,
                at(11)
            )
            .unwrap()
    );

    let cp = store
        .record_checkpoint(Checkpoint {
            id: strata_types::CheckpointId::try_from(999u64).unwrap(),
            session: sid.clone(),
            repo: rn.clone(),
            base,
            at: at(50),
            reason: CheckpointReason::Manual {
                message: "m".into(),
            },
            entries: vec![],
        })
        .unwrap();
    let group = ReleaseGroup {
        id: 0,
        batches: vec![Batch {
            session: sid.clone(),
            repo: rn.clone(),
            checkpoint: cp,
            paths: vec![RepoPath::try_from("src/p.ts".to_string()).unwrap()],
        }],
    };
    let pid = store.record_publish(&group, at(70)).unwrap();
    assert!(
        store
            .set_publish_phase(
                pid,
                PublishPhase::Running,
                PublishPhase::Unknown,
                Some(&json!({ "repo": "tool" })),
                at(80),
            )
            .unwrap()
    );
    store
        .record_ref_change(&RefChange {
            at: at(81),
            repo: rn,
            op: RefOp::UpdateRef,
            ref_name: "refs/strata/publish/main".into(),
            old: Some(tip.clone()),
            new: written.clone(),
            outcome: RefOutcome::Applied,
            publish: Some(pid),
            confirmed: None,
            detail: None,
        })
        .unwrap();

    let row = store.publish_record(pid).unwrap().unwrap();
    assert_eq!(row.phase, PublishPhase::Unknown);
    assert!(row.outcome.as_ref().unwrap().get("commit").is_none());

    let fake = Fake::at(&tip);
    let c = cfg("main");
    let rec = rec(&[("tool", tip.clone(), true)]);
    let items = run(&store, &[("tool", &fake, &c)], &rec, &[], &[row]).unwrap();
    assert_eq!(whats(&items), ["publish"], "{items:?}");
    assert_eq!(items[0].commit, Some(written));
    assert_eq!(items[0].publish, Some(pid));
    assert_eq!(items[0].phase, Some("unknown"));
}

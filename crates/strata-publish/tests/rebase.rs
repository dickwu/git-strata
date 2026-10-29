//! Task 5: the post-publish rebase, `assign` and `diff_between`
//! (publish.md §5.7–§5.9). Every remote is a local bare repository in the
//! sandbox; nothing reaches a network remote.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use strata_deps::{self as deps, Proposal, ProposalOrigin, ProposalState, ProposeArgs};
use strata_overlay::{Base, WorkspaceRef};
use strata_publish::seams::{self, FailPoint};
use strata_publish::{Published, RebaseReport, assign, diff_between, publish, rebase_session};
use strata_store::PublishPhase;
use strata_types::{BaseGen, Code, CommitOid, EntryKind, ReleaseGroup, SessionId, StrataError};
use support::*;

const CHAIN: &str = "refs/strata/publish/main";

fn fx() -> Fx {
    let fx = Fx::with(&["tool"]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    fx
}

fn run(fx: &Fx, g: &ReleaseGroup) -> strata_publish::Result<Published> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    publish(&ctx, g, None)
}

/// `files` written into a new session, checkpointed; the group selects
/// `select` (or every file).
fn session_with(fx: &Fx, files: &[(&str, &[u8])], select: Option<&[&str]>) -> (SessionId, ReleaseGroup) {
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", files);
    let c = fx.checkpoint(&s, "tool");
    let all: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
    let paths = select.unwrap_or(&all);
    (s.clone(), group(vec![batch(&s, "tool", c.id, paths)]))
}

/// The session's overlay against its pinned base, path → kind.
fn overlay(fx: &Fx, s: &SessionId) -> Vec<(String, EntryKind)> {
    let rn = repo_name("tool");
    let store = fx.store();
    let g = fx.base_of(s, "tool");
    let manifest = store.manifest(&rn, &g.tree).unwrap().expect("base manifest");
    let base = Base { dir: store.base_dir(&g), r#gen: g, manifest: Arc::new(manifest) };
    let root = fx.ws(s, "tool");
    let w = WorkspaceRef {
        session: s,
        repo: &rn,
        root: &root,
        config: &fx.config.repos[&rn],
        git: fx.git_of("tool"),
        store,
    };
    strata_overlay::reconcile_full(&w, &base)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| (e.path.as_str().to_string(), e.kind))
        .collect()
}

fn overlay_paths(fx: &Fx, s: &SessionId) -> Vec<String> {
    overlay(fx, s).into_iter().map(|(p, _)| p).collect()
}

fn report<'a>(p: &'a Published, s: &SessionId) -> &'a RebaseReport {
    &p.rebases.iter().find(|(x, _)| x == s).expect("a rebase report").1
}

fn tree_of(fx: &Fx, c: &CommitOid) -> String {
    fx.git_of("tool").tree_of(c).unwrap().as_str().to_string()
}

fn read(fx: &Fx, s: &SessionId, p: &str) -> Option<Vec<u8>> {
    fs::read(fx.ws(s, "tool").join(p)).ok()
}

fn proposals(fx: &Fx) -> Vec<Proposal> {
    deps::open_proposals(fx.store(), Some(&repo_name("tool"))).unwrap()
}

fn drafts_of(fx: &Fx, s: &SessionId) -> Vec<Proposal> {
    proposals(fx)
        .into_iter()
        .filter(|p| p.state == ProposalState::AwaitingPublish && p.origin == (ProposalOrigin::Session { session: s.clone() }))
        .collect()
}

fn new_package_json() -> Vec<u8> {
    PACKAGE_JSON.replace("1.0.87", "1.0.88").replace("^1.0.0", "^1.0.1").into_bytes()
}

/// A draft of `s` towards its workspace's current inputs, recorded the
/// way the session module records one (under the repo lock, no target).
fn draft(fx: &Fx, s: &SessionId) -> Proposal {
    let d = fx.deps_ctx("tool");
    let old = fx.base_of(s, "tool");
    let from_inputs = deps::find_set(fx.store(), &repo_name("tool"), &old.deps).unwrap().unwrap().inputs_hash;
    let (to_inputs, to) = deps::key_of(&d, &fx.ws(s, "tool")).unwrap();
    assert_ne!(to, old.deps);
    let mut got = None;
    fx.lock_write(&repo_name("tool"), || {
        got = Some(
            deps::propose(
                &d,
                ProposeArgs {
                    origin: ProposalOrigin::Session { session: s.clone() },
                    from: old.deps.clone(),
                    from_inputs: from_inputs.clone(),
                    to: to.clone(),
                    to_inputs: to_inputs.clone(),
                    tree: None,
                    head: None,
                },
            )
            .unwrap(),
        );
    })
    .unwrap();
    let p = got.unwrap();
    assert_eq!(p.state, ProposalState::AwaitingPublish);
    p
}

/// A child that keeps `f` open; killed on drop.
struct Holder(Child);

impl Holder {
    fn file(f: &Path) -> Holder {
        let h = Holder(
            Command::new("/bin/sleep").arg("300").current_dir("/").stdin(fs::File::open(f).unwrap()).spawn().unwrap(),
        );
        // lsof must see it before the publish runs.
        std::thread::sleep(Duration::from_millis(200));
        h
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn row_outcome(fx: &Fx, id: u64) -> serde_json::Value {
    fx.store().publish_record(id).unwrap().unwrap().outcome.unwrap()
}

fn pending(fx: &Fx, s: &SessionId) -> Option<strata_store::RebasePending> {
    fx.store().rebase_pending_of(s, &repo_name("tool")).unwrap()
}

// ---- tests -----------------------------------------------------------------

#[test]
fn post_publish_rebase_drops_published_paths() {
    let fx = fx();
    let (a, ga) = session_with(&fx, &[("from-a", b"alpha\n")], None);
    let (b, gb) = session_with(&fx, &[("from-b", b"bravo\n")], None);
    let pb = run(&fx, &gb).unwrap();
    assert!(matches!(report(&pb, &b), RebaseReport::Moved { .. }), "{:?}", pb.rebases);
    assert!(overlay_paths(&fx, &b).is_empty(), "{:?}", overlay(&fx, &b));
    let pa = run(&fx, &ga).unwrap();
    match report(&pa, &a) {
        RebaseReport::Moved { dropped, .. } => assert_eq!(dropped, &rps(&["from-a"])),
        r => panic!("{r:?}"),
    }
    assert_eq!(fx.base_of(&a, "tool").tree.as_str(), tree_of(&fx, &pa.commit));
    assert!(overlay_paths(&fx, &a).is_empty(), "{:?}", overlay(&fx, &a));
    assert_eq!(read(&fx, &a, "from-a").unwrap(), b"alpha\n");
    assert_eq!(read(&fx, &a, "from-b").unwrap(), b"bravo\n");
    let o = row_outcome(&fx, pa.id);
    assert_eq!(o["rebase"][0]["session"], a.as_str());
    assert_eq!(o["rebase"][0]["kind"], "Moved");
    assert_eq!(o["paths"], serde_json::json!(["from-a"]));
    assert!(fx.store().rebase_pending(None).unwrap().is_empty(), "both moves consumed their targets");
}

#[test]
fn later_edits_survive_rebase() {
    let fx = fx();
    let (s, g) = session_with(&fx, &[("src/index.ts", b"export const a = 1\n")], None);
    // Written after the selected checkpoint.
    fx.edit(&s, "tool", &[("src/index.ts", b"export const a = 2\n"), ("later.txt", b"later\n")]);
    let p = run(&fx, &g).unwrap();
    let main = fx.main("tool");
    let blob = fx.sb.git(&main, &["show", &format!("{}:src/index.ts", p.commit.as_str())]);
    assert_eq!(blob.trim_end(), "export const a = 1", "the later bytes are not in the commit");
    assert!(matches!(report(&p, &s), RebaseReport::Moved { .. }));
    assert_eq!(read(&fx, &s, "src/index.ts").unwrap(), b"export const a = 2\n");
    assert_eq!(read(&fx, &s, "later.txt").unwrap(), b"later\n");
    let paths = overlay_paths(&fx, &s);
    assert!(paths.contains(&"src/index.ts".to_string()) && paths.contains(&"later.txt".to_string()), "{paths:?}");
    let next = fx.checkpoint(&s, "tool");
    let seen: Vec<&str> = next.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(seen.contains(&"src/index.ts") && seen.contains(&"later.txt"), "{seen:?}");
}

#[test]
fn rebase_waits_when_deps_must_change() {
    let fx = fx();
    let pj = new_package_json();
    let (s, g) = session_with(&fx, &[("package.json", &pj)], None);
    let before = fx.base_of(&s, "tool");
    let p = run(&fx, &g).unwrap();
    match report(&p, &s) {
        RebaseReport::Waiting { proposal } => {
            assert_eq!(proposal.origin, ProposalOrigin::Main);
            assert_eq!(proposal.from, before.deps);
            assert_ne!(proposal.to, before.deps);
            assert_eq!(proposal.head.as_ref(), Some(&p.commit));
            assert_eq!(proposal.tree.as_ref().map(|t| t.as_str().to_string()), Some(tree_of(&fx, &p.commit)));
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(fx.base_of(&s, "tool"), before, "the session stays on its old base");
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), p.commit.as_str());
    assert_eq!(row_outcome(&fx, p.id)["rebase"][0]["kind"], "Waiting");
    let r = pending(&fx, &s).expect("a deferred target");
    assert_eq!((r.head.as_deref(), r.reason.as_str(), r.attempts), (Some(p.commit.as_str()), "deps_changed", 1));
    assert_eq!(r.detail.as_ref().unwrap()["publish"], p.id);
}

/// session.md §5.6 step 4.2 (D57): a publish that leaves the session
/// waiting on deps no longer makes `rm` Dirty for what it published.
#[test]
fn waiting_session_is_removable_without_discard() {
    let fx = fx();
    let pj = new_package_json();
    let (s, g) = session_with(&fx, &[("package.json", &pj)], None);
    let p = run(&fx, &g).unwrap();
    let id = match report(&p, &s) {
        RebaseReport::Waiting { proposal } => u64::from(proposal.id),
        r => panic!("{r:?}"),
    };
    let ss = fx.sessions();
    fx.edit(&s, "tool", &[("later.txt", b"later\n")]);
    let e = ss.remove(&s, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["repos"]["tool"]["paths"], serde_json::json!(["later.txt"]), "{d}");
    assert_eq!(d["landed"]["tool"]["publishes"], serde_json::json!([p.id]), "{d}");
    assert_eq!(d["landed"]["tool"]["proposals"], serde_json::json!([id]), "{d}");
    assert!(e.message.contains(&format!("waiting on deps proposal #{id}")), "{}", e.message);
    fs::remove_file(fx.ws(&s, "tool").join("later.txt")).unwrap();
    let r = ss.remove(&s, false, None);
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn landed_draft_promoted_under_repo_lock() {
    let fx = fx();
    let pj = new_package_json();
    let (s, g) = session_with(&fx, &[("package.json", &pj)], None);
    let d = draft(&fx, &s);
    let before = fx.base_of(&s, "tool");
    let p = run(&fx, &g).unwrap();
    match report(&p, &s) {
        RebaseReport::Waiting { proposal } => {
            assert_eq!(proposal.origin, ProposalOrigin::Session { session: s.clone() }, "{proposal:?}");
            assert_eq!(proposal.from, d.from);
            assert_eq!(proposal.to, d.to, "the new base's key is the draft's target");
            assert_eq!(proposal.head.as_ref(), Some(&p.commit));
            assert_ne!(proposal.state, ProposalState::AwaitingPublish);
        }
        r => panic!("{r:?}"),
    }
    assert!(drafts_of(&fx, &s).is_empty(), "the draft no longer waits for a publish");
    assert_eq!(fx.base_of(&s, "tool"), before);
}

#[test]
fn partial_input_publish_does_not_promote() {
    let fx = fx();
    let pj = new_package_json();
    let (s, g) = session_with(&fx, &[("package.json", &pj), ("bun.lock", b"# lock v2\n")], Some(&["package.json"]));
    let d = draft(&fx, &s);
    let p = run(&fx, &g).unwrap();
    match report(&p, &s) {
        RebaseReport::Waiting { proposal } => assert_eq!(proposal.origin, ProposalOrigin::Main, "{proposal:?}"),
        r => panic!("{r:?}"),
    }
    let left = drafts_of(&fx, &s);
    assert_eq!(left.len(), 1, "the draft stays");
    assert_eq!(left[0].id, d.id);
    assert_eq!(left[0].to, d.to);
}

#[test]
fn first_lander_keeps_exact_tree() {
    let fx = fx();
    let (a, ga) = session_with(&fx, &[("from-a", b"alpha\n")], None);
    let (b, gb) = session_with(&fx, &[("from-b", b"bravo\n")], None);
    let (pa, pb) = std::thread::scope(|sc| {
        let ha = sc.spawn(|| run(&fx, &ga).unwrap());
        let pa = ha.join().unwrap();
        let hb = sc.spawn(|| run(&fx, &gb).unwrap());
        (pa, hb.join().unwrap())
    });
    assert_eq!(fx.base_of(&a, "tool").tree.as_str(), tree_of(&fx, &pa.commit), "A is on its own commit");
    assert!(read(&fx, &a, "from-b").is_none(), "A does not see B's later file");
    assert_eq!(fx.base_of(&b, "tool").tree.as_str(), tree_of(&fx, &pb.commit));
    assert_eq!(read(&fx, &b, "from-a").unwrap(), b"alpha\n");
    assert_eq!(read(&fx, &b, "from-b").unwrap(), b"bravo\n");
    assert!(fx.git_of("tool").is_ancestor(&pa.commit, &pb.commit).unwrap());
}

#[test]
fn open_files_does_not_fail_a_landed_publish() {
    let fx = fx();
    let (s, g) = session_with(&fx, &[("src/index.ts", b"export const a = 3\n")], None);
    let before = fx.base_of(&s, "tool");
    let held = fx.ws(&s, "tool").join("node_modules/x/index.js");
    assert!(held.exists(), "the workspace has deps");
    let h = Holder::file(&held);
    let p = run(&fx, &g).expect("the publish succeeds");
    match report(&p, &s) {
        RebaseReport::OpenFiles { detail } => assert!(detail["holders"].as_array().is_some_and(|a| !a.is_empty()), "{detail}"),
        r => panic!("{r:?}"),
    }
    drop(h);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), p.commit.as_str());
    assert_eq!(fx.store().publish_record(p.id).unwrap().unwrap().phase, PublishPhase::Done);
    assert_eq!(fx.base_of(&s, "tool"), before, "the session did not move");
    assert_eq!(row_outcome(&fx, p.id)["rebase"][0]["kind"], "OpenFiles");
    let r = pending(&fx, &s).expect("a deferred target");
    assert_eq!((r.head.as_deref(), r.reason.as_str(), r.attempts), (Some(p.commit.as_str()), "open_files", 1));
    // After the process is gone a later rebase moves it; the commit is not replayed.
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let ng = BaseGen { repo: repo_name("tool"), tree: fx.git_of("tool").tree_of(&p.commit).unwrap(), deps: before.deps };
    let r = rebase_session(&ctx, s.clone(), &repo_name("tool"), ng, p.commit.clone()).unwrap();
    assert!(matches!(r, RebaseReport::Moved { .. }), "{r:?}");
    assert_eq!(fx.store().publishes().unwrap().len(), 1);
    assert!(pending(&fx, &s).is_none(), "a move onto the target consumes it");
}

#[test]
fn observe_failed_is_a_warning() {
    let fx = fx();
    fx.lock.set_wait(Duration::from_millis(100));
    let (s, g) = session_with(&fx, &[("src/index.ts", b"export const a = 4\n")], None);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    let p = std::thread::scope(|sc| {
        let lock = &fx.lock;
        let holder = sc.spawn(move || {
            if rx.recv().is_ok() {
                let guard = lock.hold("tool");
                ack_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(400));
                drop(guard);
            }
        });
        deps::test_seams::before_main_lock(Box::new(move || {
            tx.send(()).unwrap();
            ack_rx.recv().unwrap();
        }));
        let p = run(&fx, &g);
        deps::test_seams::reset();
        holder.join().unwrap();
        p
    });
    let p = p.expect("the publish succeeds");
    assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    assert_eq!(p.warnings[0].kind, strata_publish::WarningKind::ObserveFailed);
    assert_eq!(p.warnings[0].detail["code"], "Busy");
    assert_eq!(fx.store().session(&s).unwrap().unwrap().lifecycle, strata_store::Lifecycle::Active);
    assert!(matches!(report(&p, &s), RebaseReport::Moved { .. }), "the rebase still ran");
    let o = row_outcome(&fx, p.id);
    assert_eq!(o["warnings"][0]["kind"], "ObserveFailed");
}

#[test]
fn publish_does_not_build_while_holding_repo_lock() {
    let fx = fx();
    let pj = new_package_json();
    let (_s, g) = session_with(&fx, &[("package.json", &pj)], None);
    let n0 = fx.sb.install_count();
    let holds0 = fx.lock.holds().len();
    run(&fx, &g).unwrap();
    let n1 = fx.sb.install_count();
    assert!(n1 > n0, "the new base's deps set was built (outside the lock)");
    let holds: Vec<Hold> = fx.lock.holds().into_iter().skip(holds0).collect();
    assert!(!holds.is_empty());
    for n in n0 + 1..=n1 {
        let start: f64 = fx.sb.install_file("start", n).trim().parse().unwrap();
        let end: f64 = fx.sb.install_file("end", n).trim().parse().unwrap();
        for h in &holds {
            let hend = h.end.expect("released");
            assert!(end <= h.start || start >= hend, "install {n} [{start}, {end}] overlaps a hold [{}, {hend}]", h.start);
        }
    }
}

#[test]
fn failed_publish_keeps_overlay() {
    let mut fx = fx();
    let (a, ga) = session_with(&fx, &[("src/index.ts", b"export const a = 5\n")], None);
    let (b, gb) = session_with(&fx, &[("src/index.ts", b"export const a = 6\n")], None);
    run(&fx, &ga).unwrap();
    let before = (overlay(&fx, &b), fx.base_of(&b, "tool"), read(&fx, &b, "src/index.ts"));
    let e = run(&fx, &gb).unwrap_err();
    assert_eq!(e.code, Code::Conflict, "{e:?}");
    assert_eq!((overlay(&fx, &b), fx.base_of(&b, "tool"), read(&fx, &b, "src/index.ts")), before);
    let st = fx.sessions().status(&b).unwrap();
    assert!(st.workspaces[&repo_name("tool")].checkpoint.is_some());
    assert!(!overlay_paths(&fx, &b).is_empty(), "the change is still unpublished");
    // CheckFailed: same.
    fx.cfg_mut("tool").checks = vec!["/usr/bin/false".into()];
    let (c, gc) = session_with(&fx, &[("c.txt", b"charlie\n")], None);
    let before = (overlay(&fx, &c), fx.base_of(&c, "tool"));
    let e = run(&fx, &gc).unwrap_err();
    assert_eq!(e.code, Code::CheckFailed, "{e:?}");
    assert_eq!((overlay(&fx, &c), fx.base_of(&c, "tool")), before);
    assert!(pending(&fx, &b).is_none() && pending(&fx, &c).is_none(), "a publish with no commit leaves no plan");
    assert!(pending(&fx, &a).is_none());
}

#[test]
fn rebase_failure_still_reports_the_commit() {
    let fx = fx();
    let (s, g) = session_with(&fx, &[("src/index.ts", b"export const a = 7\n")], None);
    let before = fx.base_of(&s, "tool");
    seams::fail_once(fx.git_of("tool"), FailPoint::BeforeRebase);
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let c = e.detail.as_ref().unwrap()["commit"].as_str().unwrap().to_string();
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), c, "the commit is on the chain");
    let r = fx.store().publishes().unwrap().pop().unwrap();
    assert_eq!(r.phase, PublishPhase::Unknown);
    let o = r.outcome.unwrap();
    assert_eq!(o["commit"], c.as_str(), "the record keeps the commit");
    assert!(o.get("error").is_some());
    assert_eq!(fx.base_of(&s, "tool"), before);
    let r = pending(&fx, &s).expect("the committed target stays");
    assert_eq!((r.head.as_deref(), r.reason.as_str(), r.planned_publish), (Some(c.as_str()), "publish_unfinished", None));
}

#[test]
fn assign_names_resolver_and_does_not_push() {
    let fx = fx();
    let (_a, ga) = session_with(&fx, &[("src/index.ts", b"export const a = 8\n")], None);
    let (b, gb) = session_with(&fx, &[("src/index.ts", b"export const a = 9\n")], None);
    run(&fx, &ga).unwrap();
    let e = run(&fx, &gb).unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    let id = e.detail.as_ref().unwrap()["publish"].as_u64().expect("detail.publish");
    let chain = fx.read_ref("tool", CHAIN);
    let refs = fx.refs("tool");
    let root = fx.store().session(&b).unwrap().unwrap().manifest.root;
    let files = snapshot(&root);
    fx.git_of("tool").take_git_calls();
    let holds = fx.lock.holds().len();

    let bad = assign(fx.store(), id, &rp("src"), b.clone()).unwrap_err();
    assert_eq!(bad.code, Code::IsDirectory, "{bad:?}");
    let bad = assign(fx.store(), id, &rp("README"), b.clone()).unwrap_err();
    assert_eq!(bad.code, Code::NoSuchPath, "{bad:?}");
    let bad = assign(fx.store(), id + 100, &rp("src/index.ts"), b.clone()).unwrap_err();
    assert_eq!(bad.code, Code::NoSuchPath, "{bad:?}");
    assign(fx.store(), id, &rp("src/index.ts"), b.clone()).unwrap();

    let o = row_outcome(&fx, id);
    assert_eq!(o["assigned"][0]["path"], "src/index.ts");
    assert_eq!(o["assigned"][0]["session"], b.as_str());
    let calls = fx.git_of("tool").take_git_calls();
    assert!(calls.is_empty(), "assign runs no git: {calls:?}");
    assert_eq!(fx.lock.holds().len(), holds, "no repo lock");
    assert_eq!(fx.read_ref("tool", CHAIN), chain);
    assert_eq!(fx.refs("tool"), refs);
    assert_eq!(snapshot(&root), files, "no file under the session root");

    // Another session selecting the path → Conflict, reason assigned.
    let (c, gc) = session_with(&fx, &[("src/index.ts", b"export const a = 10\n")], None);
    let e = run(&fx, &gc).unwrap_err();
    assert_eq!(e.code, Code::Conflict, "{e:?}");
    assert_eq!(reason(&e), Some("assigned"));
    let d = &e.detail.as_ref().unwrap()["conflicts"][0];
    assert_eq!(d["assigned"], b.as_str());
    assert_eq!(d["session"], c.as_str());
    assert_eq!(fx.read_ref("tool", CHAIN), chain, "nothing published");
}

#[test]
fn diff_between_previews_both_sides() {
    let fx = fx();
    let (a, _) = session_with(
        &fx,
        &[("a-only.txt", b"a\n"), ("same.txt", b"same\n"), ("src/index.ts", b"export const a = 11\n")],
        None,
    );
    let (b, _) = session_with(
        &fx,
        &[("b-only.txt", b"b\n"), ("same.txt", b"same\n"), ("src/index.ts", b"export const a = 12\n")],
        None,
    );
    let refs = fx.refs("tool");
    let holds = fx.lock.holds().len();
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let diff = diff_between(&ctx, a.clone(), b.clone(), &repo_name("tool")).unwrap();
    assert_eq!(diff.only_a, rps(&["a-only.txt"]));
    assert_eq!(diff.only_b, rps(&["b-only.txt"]));
    let both: Vec<&str> = diff.both.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(both, vec!["same.txt", "src/index.ts"]);
    let same = diff.both[0].1.as_ref().unwrap();
    assert!(same.merged && same.hunks.is_empty());
    let idx = diff.both[1].1.as_ref().unwrap();
    assert!(!idx.merged);
    assert_eq!(idx.hunks.len(), 1, "{idx:?}");
    assert!(idx.hunks[0].ours.contains("11") && idx.hunks[0].theirs.contains("12"), "{idx:?}");
    assert_eq!(fx.lock.holds().len(), holds, "no repo lock");
    assert_eq!(fx.refs("tool"), refs, "no ref written");
    let e: StrataError = diff_between(&ctx, a, b, &repo_name("other")).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
}

/// Edits made after the last checkpoint (or with no checkpoint at all)
/// have a scanned BLAKE3 but no blob yet: the preview reads them from the
/// workspace instead of returning `Corrupt` (§5.9 previews current
/// content). Covers the same-base path and the projection path.
#[test]
fn diff_between_previews_uncheckpointed_edits() {
    let fx = fx();
    // a: checkpointed once, then edited again; b: never checkpointed.
    let (a, _) = session_with(&fx, &[("src/index.ts", b"export const a = 11\n")], None);
    fx.edit(&a, "tool", &[("src/index.ts", b"export const a = 21\n"), ("a-new.txt", b"fresh\n")]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("src/index.ts", b"export const a = 22\n"), ("same.txt", b"x\n")]);
    fx.edit(&a, "tool", &[("same.txt", b"x\n")]);
    let refs = fx.refs("tool");
    let holds = fx.lock.holds().len();
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let diff = diff_between(&ctx, a.clone(), b.clone(), &repo_name("tool")).unwrap();
    assert_eq!(diff.only_a, rps(&["a-new.txt"]));
    assert!(diff.only_b.is_empty(), "{diff:?}");
    let both: Vec<&str> = diff.both.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(both, vec!["same.txt", "src/index.ts"]);
    let same = diff.both[0].1.as_ref().unwrap_or_else(|e| panic!("same.txt: {e:?}"));
    assert!(same.merged && same.hunks.is_empty());
    let idx = diff.both[1].1.as_ref().unwrap_or_else(|e| panic!("src/index.ts: {e:?}"));
    assert!(!idx.merged);
    assert!(idx.hunks[0].ours.contains("21") && idx.hunks[0].theirs.contains("22"), "{idx:?}");
    assert_eq!(fx.lock.holds().len(), holds, "no repo lock");
    assert_eq!(fx.refs("tool"), refs, "no ref written");

    // Different pins: a (older base, uncheckpointed edit) is projected
    // onto c's newer base; its blobs are cache-filled first.
    fx.land_on_origin("tool", "main", &[("upstream.txt", b"up\n")], "upstream");
    fx.git_of("tool").fetch("origin", "main").unwrap();
    let c = fx.new_session(&["tool"]);
    if fx.base_of(&c, "tool") == fx.base_of(&a, "tool") {
        panic!("the fixture did not give c a newer base");
    }
    fx.edit(&c, "tool", &[("src/index.ts", b"export const a = 33\n")]);
    let ctx = fx.ctx(&d, &ss);
    let diff = diff_between(&ctx, a.clone(), c.clone(), &repo_name("tool")).unwrap();
    let idx = diff.both.iter().find(|(p, _)| p.as_str() == "src/index.ts").expect("both sides touch index.ts");
    let idx = idx.1.as_ref().unwrap_or_else(|e| panic!("projected src/index.ts: {e:?}"));
    assert!(!idx.merged);
    assert!(idx.hunks[0].ours.contains("21") && idx.hunks[0].theirs.contains("33"), "{idx:?}");
    assert!(diff.only_a.iter().any(|p| p.as_str() == "a-new.txt"), "{diff:?}");
}

/// rm-followups §1.6 rule 4: a plan left by a publish that stopped before
/// promotion is resolved from the record: Running is left alone, a
/// reachable commit is promoted (`publish_unfinished` unless the row is a
/// clean Done), no commit clears the plan.
#[test]
fn reconstruct_resolves_a_plan_from_the_publish_record() {
    let fx = fx();
    let (_a, ga) = session_with(&fx, &[("from-a", b"alpha\n")], None);
    let pa = run(&fx, &ga).unwrap();
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let sc = strata_publish::pending::Scope::of(&ctx);
    let (b, gb) = session_with(&fx, &[("from-b", b"bravo\n")], None);
    let id = fx.store().record_publish(&gb, std::time::SystemTime::now()).unwrap();
    assert_eq!(pending(&fx, &b).unwrap().planned_publish, Some(id));
    strata_publish::pending::reconstruct(sc).unwrap();
    assert_eq!(pending(&fx, &b).unwrap().planned_publish, Some(id), "a Running publish is left alone");
    let o = serde_json::json!({ "commit": pa.commit.as_str(), "error": { "code": "Unknown" } });
    assert!(fx.store().set_publish_phase(id, PublishPhase::Running, PublishPhase::Unknown, Some(&o), std::time::SystemTime::now()).unwrap());
    strata_publish::pending::reconstruct(sc).unwrap();
    let r = pending(&fx, &b).expect("promoted");
    assert_eq!((r.head.as_deref(), r.planned_publish, r.reason.as_str()), (Some(pa.commit.as_str()), None, "publish_unfinished"));
    assert_eq!(r.detail.as_ref().unwrap()["publish"], id);

    // no commit anywhere: the plan goes
    let (c, gc) = session_with(&fx, &[("from-c", b"charlie\n")], None);
    let id = fx.store().record_publish(&gc, std::time::SystemTime::now()).unwrap();
    assert!(fx.store().set_publish_phase(id, PublishPhase::Running, PublishPhase::Unknown, Some(&serde_json::json!({})), std::time::SystemTime::now()).unwrap());
    strata_publish::pending::reconstruct(sc).unwrap();
    assert!(pending(&fx, &c).is_none());
}

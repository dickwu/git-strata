//! deps.md §7, Task 8: approval targets and rollout plans.

mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use common::*;
use strata_deps::{
    Drift, Hold, Proposal, ProposalOrigin, ProposalState, ProposeArgs, RepoWriteLock, RolloutPlan,
    RolloutStep, SkipReason, Target, build, clone_into, drift, ensure, ensure_target, find_set,
    get_proposal, inputs_hash, key_of, main_tip, observe_main, plan_rollout, plan_switch, propose,
    record_rollout, test_seams,
};
use strata_store::{Lifecycle, NewProposal, ProposalDecision};
use strata_types::{
    BaseGen, BaseManifest, Code, CommitOid, DepsSet, DepsSetId, ProposalId, SessionId,
    SessionManifest, StrataError, TreeOid, WorkspaceState,
};

// ---- holder processes (Fd / Mapped / Cwd) ----

const HOLDER_ENV: &str = "STRATA_DEPS_ROLLOUT_HOLDER";

/// Holder mode: hold something, say READY, wait for stdin to close.
#[test]
fn rollout_holder() {
    let Ok(spec) = std::env::var(HOLDER_ENV) else {
        return;
    };
    let (kind, path) = spec.split_once(':').unwrap();
    let mut keep: Vec<fs::File> = Vec::new();
    match kind {
        "fd" => keep.push(fs::File::open(path).unwrap()),
        "mmap" => {
            let f = fs::File::open(path).unwrap();
            let len = f.metadata().unwrap().len() as usize;
            // SAFETY: read-only private mapping of an open file.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    f.as_raw_fd(),
                    0,
                )
            };
            assert_ne!(p, libc::MAP_FAILED);
            drop(f);
        }
        "cwd" => {}
        _ => panic!("unknown holder {kind}"),
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    std::process::exit(0);
}

/// Hold `path` by `kind` ("fd", "mmap" or "cwd") in a child process.
fn hold(kind: &str, path: &Path) -> Child {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        "rollout_holder",
        "--nocapture",
        "--test-threads=1",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if kind == "cwd" {
        c.env(HOLDER_ENV, "cwd:").current_dir(path);
    } else {
        c.env(HOLDER_ENV, format!("{kind}:{}", path.display()));
    }
    let mut child = c.spawn().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "holder exited before READY"
        );
        if line.contains("READY") {
            break;
        }
    }
    child
}

fn stop(mut c: Child) {
    drop(c.stdin.take());
    c.wait().unwrap();
}

// ---- small helpers ----

fn commit_oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.trim().to_string()).unwrap()
}
fn tree_oid(s: &str) -> TreeOid {
    TreeOid::try_from(s.trim().to_string()).unwrap()
}
fn set(c: char) -> DepsSetId {
    DepsSetId::try_from(c.to_string().repeat(64)).unwrap()
}
fn hex(c: char) -> String {
    c.to_string().repeat(64)
}
fn sid(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}
fn session(s: &SessionId) -> ProposalOrigin {
    ProposalOrigin::Session { session: s.clone() }
}

/// A package.json with `x` plus `tags` as dependencies.
fn pkg(tags: &[&str]) -> Vec<u8> {
    let mut deps = vec![r#""x": "^1.0.0""#.to_string()];
    deps.extend(tags.iter().map(|t| format!(r#""{t}": "1.0.0""#)));
    format!(
        "{{\n  \"name\": \"tool\",\n  \"dependencies\": {{ {} }}\n}}\n",
        deps.join(", ")
    )
    .into_bytes()
}

#[derive(Clone)]
struct Tip {
    tree: TreeOid,
    head: CommitOid,
    dir: PathBuf,
}

thread_local! {
    static N: Cell<u32> = const { Cell::new(0) };
}

fn tip(fx: &Fx) -> Tip {
    let n = N.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    Tip {
        tree: tree_oid(&fx.sb.tree_of(&fx.main, "HEAD")),
        head: commit_oid(&fx.sb.head(&fx.main)),
        dir: fx.tree(&format!("t{n}")),
    }
}

fn publish(fx: &Fx) {
    fx.sb.git(
        &fx.main,
        &["push", "-q", "-f", "origin", "HEAD:refs/heads/main"],
    );
    fx.git.fetch("origin", "main").unwrap();
}

fn push_oid(fx: &Fx, oid: &str) {
    fx.sb.git(
        &fx.main,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("{oid}:refs/heads/main"),
        ],
    );
    fx.git.fetch("origin", "main").unwrap();
}

fn advance(fx: &Fx, files: &[(&str, &[u8])], msg: &str) -> Tip {
    fx.sb.commit(&fx.main, files, msg);
    publish(fx);
    tip(fx)
}

fn rewind(fx: &Fx, oid: &CommitOid) {
    fx.sb
        .git(&fx.main, &["reset", "-q", "--hard", oid.as_str()]);
    publish(fx);
}

fn observe(fx: &Fx, t: &Tip) -> strata_deps::Result<Option<Proposal>> {
    observe_main(&fx.ctx(), &t.tree, &t.head, &t.dir)
}

fn key(fx: &Fx, t: &Tip) -> (String, DepsSetId) {
    key_of(&fx.ctx(), &t.dir).unwrap()
}

fn hash(fx: &Fx, dir: &Path) -> String {
    inputs_hash(&fx.cfg, &fx.git, dir).unwrap()
}

type MainRow = Option<(TreeOid, CommitOid, String, DepsSetId)>;

fn main_row(fx: &Fx) -> MainRow {
    fx.store()
        .deps_main(&fx.repo)
        .unwrap()
        .map(|m| (m.tree, m.head, m.inputs_hash, m.set))
}

fn all(fx: &Fx) -> Vec<Proposal> {
    fx.store()
        .proposals(Some(&fx.repo), false)
        .unwrap()
        .into_iter()
        .map(|r| get_proposal(fx.store(), r.id).unwrap())
        .collect()
}

fn state(fx: &Fx, id: ProposalId) -> ProposalState {
    get_proposal(fx.store(), id).unwrap().state
}

fn bg(fx: &Fx, tree: &TreeOid, deps: &DepsSetId) -> BaseGen {
    BaseGen {
        repo: fx.repo.clone(),
        tree: tree.clone(),
        deps: deps.clone(),
    }
}

/// `propose` arguments from (id, inputs) pairs.
fn pa(
    origin: ProposalOrigin,
    from: (&DepsSetId, &str),
    to: (&DepsSetId, &str),
    t: Option<&Tip>,
) -> ProposeArgs {
    ProposeArgs {
        origin,
        from: from.0.clone(),
        from_inputs: from.1.to_owned(),
        to: to.0.clone(),
        to_inputs: to.1.to_owned(),
        tree: t.map(|t| t.tree.clone()),
        head: t.map(|t| t.head.clone()),
    }
}

fn of(s: &DepsSet) -> (&DepsSetId, &str) {
    (&s.id, &s.inputs_hash)
}

fn mv(s: &SessionId, base: &BaseGen, head: &CommitOid) -> RolloutStep {
    RolloutStep::Move {
        session: s.clone(),
        new_base: base.clone(),
        head: head.clone(),
    }
}

fn sk(s: &SessionId, reason: SkipReason) -> RolloutStep {
    RolloutStep::Skip {
        session: s.clone(),
        reason,
    }
}

fn moves(plan: &RolloutPlan) -> Vec<SessionId> {
    plan.steps
        .iter()
        .filter_map(|s| match s {
            RolloutStep::Move { session, .. } => Some(session.clone()),
            RolloutStep::Skip { .. } => None,
        })
        .collect()
}

/// The base generation's directory, exported from its tree if missing.
fn ensure_base(fx: &Fx, base: &BaseGen) -> PathBuf {
    let dir = fx.store().base_dir(base);
    if !dir.exists() {
        fx.sb.archive_into(&fx.main, base.tree.as_str(), &dir);
    }
    dir
}

/// A workspace for `s` on `base`: `src` exported, `base.deps` cloned in.
/// With `register`, also a live store session.
fn ws_new(
    fx: &Fx,
    s: &SessionId,
    base: &BaseGen,
    src: &str,
    head: &CommitOid,
    register: bool,
) -> WorkspaceState {
    ensure_base(fx, base);
    let root = fx.paths.root.join("sessions").join(s.as_str());
    let path = root.join("tool");
    fx.sb.archive_into(&fx.main, src, &path);
    let set = find_set(fx.store(), &fx.repo, &base.deps).unwrap().unwrap();
    clone_into(fx.store(), &set, &path).unwrap();
    let ws = WorkspaceState {
        base: base.clone(),
        head: head.clone(),
        path,
        tracked: true,
    };
    if register {
        let at = SystemTime::now();
        let st = fx.store();
        let m = BaseManifest {
            tree: base.tree.clone(),
            entries: BTreeMap::new(),
        };
        st.record_base_gen(base, &m, at).unwrap();
        let mut repos = BTreeMap::new();
        repos.insert(fx.repo.clone(), ws.clone());
        st.create_session(
            &SessionManifest {
                id: s.clone(),
                ticket: None,
                root,
                repos,
            },
            at,
        )
        .unwrap();
        assert!(
            st.set_lifecycle(s, Lifecycle::Creating, Lifecycle::Active, None, at)
                .unwrap()
        );
    }
    ws
}

struct W {
    fx: Fx,
    t1: Tip,
    d1: DepsSet,
    b1: BaseGen,
}

fn world() -> W {
    let fx = Fx::new();
    fx.sb.bare_origin(&fx.main);
    fx.git.fetch("origin", "main").unwrap();
    let t1 = tip(&fx);
    let d1 = ensure(&fx.ctx(), &t1.dir).unwrap();
    let b1 = bg(&fx, &t1.tree, &d1.id);
    W { fx, t1, d1, b1 }
}

impl W {
    fn ws(&self, s: &SessionId, register: bool) -> WorkspaceState {
        ws_new(
            &self.fx,
            s,
            &self.b1,
            self.t1.head.as_str(),
            &self.t1.head,
            register,
        )
    }

    /// Main moves to `files`; its set is sealed; a Main Pending targets it.
    fn main_pending(&self, files: &[(&str, &[u8])]) -> (Tip, DepsSet, Proposal) {
        let fx = &self.fx;
        let t2 = advance(fx, files, "main change");
        let d2 = ensure(&fx.ctx(), &t2.dir).unwrap();
        let p = propose(
            &fx.ctx(),
            pa(ProposalOrigin::Main, of(&self.d1), of(&d2), Some(&t2)),
        )
        .unwrap();
        assert_eq!(p.state, ProposalState::Pending);
        (t2, d2, p)
    }
}

fn no_such(e: &StrataError, id: ProposalId, st: &ProposalState) {
    assert_eq!(e.code, Code::NoSuchProposal, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["proposal"], serde_json::json!(id.get()), "{e:?}");
    assert_eq!(d["state"], serde_json::to_value(st).unwrap(), "{e:?}");
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap()
}

// ---- tests ----

#[test]
fn rollout_skips_session_with_open_files() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let (t2, d2, p) = w.main_pending(&[("package.json", &pkg(&["y"]))]);
    let target = bg(fx, &t2.tree, &d2.id);
    let (s1, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let ws1 = w.ws(&s1, false);
    let ws2 = w.ws(&s2, false);
    let file = ws1.path.join("node_modules/x/index.js");
    for (kind, held, how) in [
        ("fd", file.clone(), Hold::Fd),
        ("mmap", file.clone(), Hold::Mapped),
        ("cwd", ws1.path.join("node_modules/x"), Hold::Cwd),
    ] {
        let child = hold(kind, &held);
        let nm1 = snapshot(&ws1.path.join("node_modules"));
        let (rows, mrow) = (all(fx), main_row(fx));
        let plan = plan_rollout(
            &ctx,
            p.id,
            &[(s1.clone(), ws1.clone()), (s2.clone(), ws2.clone())],
            true,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2, "{plan:#?}");
        match &plan.steps[0] {
            RolloutStep::Skip {
                session,
                reason: SkipReason::OpenFiles { files },
            } => {
                assert_eq!(session, &s1);
                assert_eq!(files.len(), 1, "{kind}: {files:#?}");
                assert_eq!(files[0].pid, child.id() as i32);
                assert_eq!(files[0].path, canon(&held));
                assert_eq!(files[0].hold, how);
            }
            other => panic!("{kind}: {other:#?}"),
        }
        assert_eq!(plan.steps[1], mv(&s2, &target, &t2.head));
        // The mover is handed S2 only; S1's workspace is untouched.
        let moved = moves(&plan);
        assert_eq!(moved, vec![s2.clone()]);
        assert_eq!(snapshot(&ws1.path.join("node_modules")), nm1);
        let after = record_rollout(fx.store(), &plan, &moved).unwrap();
        assert_eq!(after.state, ProposalState::Pending);
        assert_eq!((all(fx), main_row(fx)), (rows, mrow));
        stop(child);
        let plan = plan_rollout(&ctx, p.id, &[(s1.clone(), ws1.clone())], false).unwrap();
        assert_eq!(plan.steps, vec![mv(&s1, &target, &t2.head)], "{kind}");
    }
}

#[test]
fn rollout_moves_base_and_deps_atomically() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let (t2, d2, p) = w.main_pending(&[("package.json", &pkg(&["y"]))]);
    let target = bg(fx, &t2.tree, &d2.id);
    let (s1, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let (ws1, ws2) = (w.ws(&s1, false), w.ws(&s2, false));
    let plan = plan_rollout(
        &ctx,
        p.id,
        &[(s1.clone(), ws1.clone()), (s2.clone(), ws2.clone())],
        true,
    )
    .unwrap();
    assert_eq!(
        (plan.proposal, &plan.repo, plan.complete),
        (p.id, &fx.repo, true)
    );
    assert_eq!(
        plan.steps,
        vec![mv(&s1, &target, &t2.head), mv(&s2, &target, &t2.head)]
    );
    // Tree and head agree: the head's tree is the target tree.
    assert_eq!(fx.sb.tree_of(&fx.main, t2.head.as_str()), t2.tree.as_str());
    let j = serde_json::to_value(&plan).unwrap();
    assert_eq!(j["steps"][0]["kind"], "Move");
    assert_eq!(j["steps"][0]["new_base"]["deps"], d2.id.as_str());
    // Each Move goes to the mover once; S2's move fails.
    assert_eq!(moves(&plan), vec![s1.clone(), s2.clone()]);
    let after = record_rollout(fx.store(), &plan, std::slice::from_ref(&s1)).unwrap();
    assert_eq!(after.state, ProposalState::Pending);
    // Approve again: S1 is there, S2 moves; now the plan completes.
    let ws1b = WorkspaceState {
        base: target.clone(),
        head: t2.head.clone(),
        ..ws1.clone()
    };
    let plan = plan_rollout(
        &ctx,
        p.id,
        &[(s1.clone(), ws1b), (s2.clone(), ws2.clone())],
        true,
    )
    .unwrap();
    assert_eq!(
        plan.steps,
        vec![
            sk(&s1, SkipReason::AlreadyThere),
            mv(&s2, &target, &t2.head)
        ]
    );
    let done = record_rollout(fx.store(), &plan, std::slice::from_ref(&s2)).unwrap();
    assert_eq!(done.state, ProposalState::Applied);
    let e = plan_rollout(&ctx, p.id, &[(s2, ws2)], true).unwrap_err();
    no_such(&e, p.id, &ProposalState::Applied);
}

#[test]
fn proposing_session_may_use_new_deps_before_approval() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let (s, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let ws = w.ws(&s, true);
    let ws2 = w.ws(&s2, true);
    fs::write(ws.path.join("package.json"), pkg(&["y"])).unwrap();
    assert!(drift(&fx.cfg, &fx.git, &ws.path, &w.d1).unwrap().is_some());
    let base_dir = fx.store().base_dir(&w.b1);
    let dn = build(&ctx, &base_dir, Some(&ws.path)).unwrap();
    assert_eq!(dn.inputs_hash, hash(fx, &ws.path));
    let p = propose(&ctx, pa(session(&s), of(&w.d1), of(&dn), None)).unwrap();
    assert_eq!(p.state, ProposalState::AwaitingPublish);

    let plan = plan_switch(&ctx, p.id, &s, &ws).unwrap();
    assert!(!plan.complete);
    assert_eq!(
        plan.steps,
        vec![mv(&s, &bg(fx, &w.t1.tree, &dn.id), &ws.head)]
    );
    let e = plan_switch(&ctx, p.id, &s2, &ws2).unwrap_err();
    no_such(&e, p.id, &ProposalState::AwaitingPublish);
    assert_eq!(reason(&e), Some("not_origin"));
    let e = plan_rollout(&ctx, p.id, &[(s2.clone(), ws2.clone())], true).unwrap_err();
    no_such(&e, p.id, &ProposalState::AwaitingPublish);

    // Something holds S's deps dir open.
    let child = hold("fd", &ws.path.join("node_modules/x/index.js"));
    let plan = plan_switch(&ctx, p.id, &s, &ws).unwrap();
    assert!(
        matches!(
            &plan.steps[..],
            [RolloutStep::Skip {
                reason: SkipReason::OpenFiles { .. },
                ..
            }]
        ),
        "{plan:#?}"
    );
    stop(child);

    // S edits its lockfile again: the found inputs are no longer `to`'s.
    let lock = fs::read(ws.path.join("bun.lock")).unwrap();
    fs::write(ws.path.join("bun.lock"), b"changed\n").unwrap();
    let plan = plan_switch(&ctx, p.id, &s, &ws).unwrap();
    let found = hash(fx, &ws.path);
    assert_ne!(found, dn.inputs_hash);
    assert_eq!(
        plan.steps,
        vec![sk(
            &s,
            SkipReason::Drift {
                drift: Drift {
                    pinned: dn.id.clone(),
                    pinned_inputs: dn.inputs_hash.clone(),
                    found_inputs: found,
                }
            }
        )]
    );
    fs::write(ws.path.join("bun.lock"), lock).unwrap();

    // S publishes: the landing promotes the draft; the others may follow.
    let t2 = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
    assert_eq!(key(fx, &t2), (dn.inputs_hash.clone(), dn.id.clone()));
    let pp = propose(&ctx, pa(session(&s), of(&w.d1), of(&dn), Some(&t2))).unwrap();
    assert_eq!(
        (pp.state.clone(), pp.origin.clone()),
        (ProposalState::Pending, session(&s))
    );
    let plan = plan_rollout(&ctx, pp.id, &[(s2.clone(), ws2)], true).unwrap();
    assert_eq!(
        plan.steps,
        vec![mv(&s2, &bg(fx, &t2.tree, &dn.id), &t2.head)]
    );
}

#[test]
fn plan_switch_on_pending_moves_base_and_deps() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let (s, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let ws = w.ws(&s, true);
    let ws2 = w.ws(&s2, true);
    fs::write(ws.path.join("package.json"), pkg(&["y"])).unwrap();
    let dn = build(&ctx, &fx.store().base_dir(&w.b1), Some(&ws.path)).unwrap();
    propose(&ctx, pa(session(&s), of(&w.d1), of(&dn), None)).unwrap();
    let t2 = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
    let p = propose(&ctx, pa(session(&s), of(&w.d1), of(&dn), Some(&t2))).unwrap();
    assert_eq!(p.state, ProposalState::Pending);
    let target = bg(fx, &t2.tree, &dn.id);

    let plan = plan_switch(&ctx, p.id, &s, &ws).unwrap();
    assert!(!plan.complete);
    assert_eq!(plan.steps, vec![mv(&s, &target, &t2.head)]);
    let after = record_rollout(fx.store(), &plan, std::slice::from_ref(&s)).unwrap();
    assert_eq!(after.state, ProposalState::Pending);
    let e = plan_switch(&ctx, p.id, &s2, &ws2).unwrap_err();
    assert_eq!(e.code, Code::NoSuchProposal);
    assert_eq!(reason(&e), Some("not_origin"));
    let plan = plan_rollout(&ctx, p.id, &[(s.clone(), ws), (s2.clone(), ws2)], true).unwrap();
    assert_eq!(
        plan.steps,
        vec![mv(&s, &target, &t2.head), mv(&s2, &target, &t2.head)]
    );
}

#[test]
fn plan_rollout_skip_reasons_in_order() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let (t2, d2, p) = w.main_pending(&[("bun.lock", b"lock v2\n")]);
    let b2 = bg(fx, &t2.tree, &d2.id);
    let n = |c: char| sid(&format!("session{c}"));
    // a: untracked
    let mut wa = w.ws(&n('a'), false);
    wa.tracked = false;
    // b: already on `to`
    let wb = ws_new(fx, &n('b'), &b2, t2.head.as_str(), &t2.head, false);
    // c: its own open draft with other inputs, and its own changes (also holds a file)
    let wc = w.ws(&n('c'), true);
    fs::write(wc.path.join("package.json"), pkg(&["z"])).unwrap();
    let qc = propose(
        &ctx,
        pa(session(&n('c')), of(&w.d1), (&set('c'), &hex('c')), None),
    )
    .unwrap();
    // d: its own draft has this proposal's inputs; it matches them
    let wd = w.ws(&n('d'), true);
    fs::write(wd.path.join("bun.lock"), b"lock v2\n").unwrap();
    propose(
        &ctx,
        pa(
            session(&n('d')),
            of(&w.d1),
            (&set('d'), &d2.inputs_hash),
            None,
        ),
    )
    .unwrap();
    // e: drift (also holds a file)
    let we = w.ws(&n('e'), false);
    fs::write(we.path.join("bun.lock"), b"other\n").unwrap();
    // f: changed exactly to `to`'s inputs
    let wf = w.ws(&n('f'), false);
    fs::write(wf.path.join("bun.lock"), b"lock v2\n").unwrap();
    // g: open files
    let wg = w.ws(&n('g'), false);
    // h: the scan fails
    let wh = w.ws(&n('h'), false);
    let injected = StrataError::new(Code::Unknown, "injected scan failure");
    test_seams::fail_open_files_under(&wh.path, injected.clone());
    // i: plain
    let wi = w.ws(&n('i'), false);
    // j: untracked and already there: untracked wins
    let mut wj = wb.clone();
    wj.tracked = false;
    let holders: Vec<Child> = [&wc, &we, &wg]
        .iter()
        .map(|ws| hold("fd", &ws.path.join("node_modules/x/index.js")))
        .collect();

    let sessions: Vec<(SessionId, WorkspaceState)> = vec![
        (n('a'), wa.clone()),
        (n('b'), wb.clone()),
        (n('c'), wc.clone()),
        (n('d'), wd.clone()),
        (n('e'), we.clone()),
        (n('f'), wf.clone()),
        (n('g'), wg.clone()),
        (n('h'), wh.clone()),
        (n('i'), wi.clone()),
        (n('j'), wj.clone()),
    ];
    let snaps = |v: &[(SessionId, WorkspaceState)]| -> Vec<Vec<Entry>> {
        v.iter().map(|(_, ws)| snapshot(&ws.path)).collect()
    };
    let before = (all(fx), main_row(fx), snaps(&sessions));
    let plan = plan_rollout(&ctx, p.id, &sessions, true).unwrap();
    let gpath = canon(&wg.path.join("node_modules/x/index.js"));
    let want = vec![
        sk(&n('a'), SkipReason::Untracked),
        sk(&n('b'), SkipReason::AlreadyThere),
        sk(&n('c'), SkipReason::OwnChange { proposal: qc.id }),
        mv(&n('d'), &b2, &t2.head),
        sk(
            &n('e'),
            SkipReason::Drift {
                drift: Drift {
                    pinned: w.d1.id.clone(),
                    pinned_inputs: w.d1.inputs_hash.clone(),
                    found_inputs: hash(fx, &we.path),
                },
            },
        ),
        mv(&n('f'), &b2, &t2.head),
        // compared below without the command name
        sk(&n('g'), SkipReason::OpenFiles { files: vec![] }),
        sk(
            &n('h'),
            SkipReason::Error {
                error: injected.clone(),
            },
        ),
        mv(&n('i'), &b2, &t2.head),
        sk(&n('j'), SkipReason::Untracked),
    ];
    assert_eq!(plan.steps.len(), want.len());
    for (i, (got, want)) in plan.steps.iter().zip(&want).enumerate() {
        if i == 6 {
            match got {
                RolloutStep::Skip {
                    reason: SkipReason::OpenFiles { files },
                    ..
                } => {
                    assert_eq!(files.len(), 1, "{files:#?}");
                    assert_eq!(
                        (files[0].pid, &files[0].path),
                        (holders[2].id() as i32, &gpath)
                    );
                }
                other => panic!("{other:#?}"),
            }
        } else {
            assert_eq!(got, want, "step {i}");
        }
    }
    // Read-only: no store row, no workspace byte changed.
    assert_eq!((all(fx), main_row(fx), snaps(&sessions)), before);
    for c in holders {
        stop(c);
    }
    test_seams::reset();

    // `to` not sealed: past the first two checks, everything is NotReady.
    let p2 = propose(
        &ctx,
        pa(
            ProposalOrigin::Main,
            of(&w.d1),
            (&set('f'), &d2.inputs_hash),
            Some(&t2),
        ),
    )
    .unwrap();
    assert_eq!(state(fx, p.id), ProposalState::Superseded { by: p2.id });
    let mut wb2 = wb.clone();
    wb2.base.deps = set('f');
    let plan = plan_rollout(
        &ctx,
        p2.id,
        &[
            (n('a'), wa),
            (n('b'), wb2),
            (n('i'), wi),
            (n('f'), wf),
            (n('h'), wh),
        ],
        true,
    )
    .unwrap();
    assert_eq!(
        plan.steps,
        vec![
            sk(&n('a'), SkipReason::Untracked),
            sk(&n('b'), SkipReason::AlreadyThere),
            sk(&n('i'), SkipReason::NotReady),
            sk(&n('f'), SkipReason::NotReady),
            sk(&n('h'), SkipReason::NotReady),
        ]
    );
    let e = plan_rollout(&ctx, p.id, &[], true).unwrap_err();
    no_such(&e, p.id, &ProposalState::Superseded { by: p2.id });
}

#[test]
fn plan_rollout_own_change_compares_inputs_not_ids() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let tc = advance(fx, &[("package.json", &pkg(&["c"]))], "c");
    let dc = ensure(&ctx, &tc.dir).unwrap();
    let ta = advance(fx, &[("package.json", &pkg(&["a"]))], "a");
    let da = ensure(&ctx, &ta.dir).unwrap(); // bun 1.3.3
    let (s1, s2, s3, s4) = (
        sid("aaaaaaab"),
        sid("aaaaaaac"),
        sid("aaaaaaad"),
        sid("aaaaaaae"),
    );
    // S1: switched itself to D_a with its draft.
    let ws1 = ws_new(
        fx,
        &s1,
        &bg(fx, &w.t1.tree, &da.id),
        w.t1.head.as_str(),
        &w.t1.head,
        true,
    );
    fs::write(ws1.path.join("package.json"), pkg(&["a"])).unwrap();
    let q1 = propose(&ctx, pa(session(&s1), of(&w.d1), of(&da), None)).unwrap();
    // S2: its own other change B.
    let ws2 = w.ws(&s2, true);
    fs::write(ws2.path.join("package.json"), pkg(&["b"])).unwrap();
    let q2 = propose(
        &ctx,
        pa(
            session(&s2),
            of(&w.d1),
            (&set('b'), &hash(fx, &ws2.path)),
            None,
        ),
    )
    .unwrap();
    // S3: C's draft is still open, but its base already has C.
    let ws3 = ws_new(
        fx,
        &s3,
        &bg(fx, &tc.tree, &dc.id),
        tc.head.as_str(),
        &tc.head,
        true,
    );
    let q3 = propose(&ctx, pa(session(&s3), of(&w.d1), of(&dc), None)).unwrap();
    // S4: a draft for D, but the workspace has A.
    let ws4 = w.ws(&s4, true);
    fs::write(ws4.path.join("package.json"), pkg(&["a"])).unwrap();
    let q4 = propose(
        &ctx,
        pa(session(&s4), of(&w.d1), (&set('d'), &hex('d')), None),
    )
    .unwrap();

    fx.sb.set_bun_version("1.3.4");
    let da2 = ensure(&ctx, &ta.dir).unwrap();
    assert_ne!(da2.id, da.id);
    assert_eq!(da2.inputs_hash, da.inputs_hash);
    let p = propose(&ctx, pa(ProposalOrigin::Main, of(&dc), of(&da2), Some(&ta))).unwrap();
    let target = bg(fx, &ta.tree, &da2.id);
    let plan = plan_rollout(
        &ctx,
        p.id,
        &[
            (s1.clone(), ws1),
            (s2.clone(), ws2),
            (s3.clone(), ws3),
            (s4.clone(), ws4),
        ],
        true,
    )
    .unwrap();
    assert_eq!(
        plan.steps,
        vec![
            mv(&s1, &target, &ta.head),
            sk(&s2, SkipReason::OwnChange { proposal: q2.id }),
            mv(&s3, &target, &ta.head),
            mv(&s4, &target, &ta.head),
        ]
    );
    for q in [q1, q2, q3, q4] {
        assert_eq!(state(fx, q.id), ProposalState::AwaitingPublish);
    }
}

fn landing(step2: bool) {
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let (s1, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let mut ws1 = w.ws(&s1, true);
    let mut ws2 = w.ws(&s2, true);
    fs::write(ws1.path.join("package.json"), pkg(&["x2"])).unwrap();
    fs::write(ws2.path.join("package.json"), pkg(&["x2"])).unwrap();
    let dx = build(&ctx, &fx.store().base_dir(&w.b1), Some(&ws1.path)).unwrap();
    let q1 = propose(&ctx, pa(session(&s1), of(&w.d1), of(&dx), None)).unwrap();
    let plan = plan_switch(&ctx, q1.id, &s1, &ws1).unwrap();
    let b1x = bg(fx, &w.t1.tree, &dx.id);
    assert_eq!(plan.steps, vec![mv(&s1, &b1x, &ws1.head)]);
    ensure_base(fx, &b1x);
    ws1.base = b1x;
    let q2 = propose(&ctx, pa(session(&s2), of(&w.d1), of(&dx), None)).unwrap();
    // Someone else's y is already on main.
    let ty = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
    let (yi, yid) = key(fx, &ty);
    let py = propose(
        &ctx,
        pa(ProposalOrigin::Main, of(&w.d1), (&yid, &yi), Some(&ty)),
    )
    .unwrap();
    // S1 lands x2 merged with y.
    let t3 = advance(fx, &[("package.json", &pkg(&["x2", "y"]))], "land x2");
    let (xyi, xyid) = key(fx, &t3);
    let t4_files = pkg(&["x2", "y", "w"]);
    if step2 {
        let mut got = None;
        fx.lock
            .with_write(&fx.repo, &mut || {
                got = Some(propose(
                    &ctx,
                    pa(
                        session(&s1),
                        (&q1.from, &q1.from_inputs),
                        (&xyid, &xyi),
                        Some(&t3),
                    ),
                )?);
                Ok(())
            })
            .unwrap()
            .unwrap();
        let q1b = got.unwrap();
        assert_eq!(
            (q1b.state.clone(), q1b.origin.clone()),
            (ProposalState::Pending, session(&s1))
        );
        assert_eq!(state(fx, q1.id), ProposalState::Superseded { by: q1b.id });
        assert_eq!(state(fx, py.id), ProposalState::Superseded { by: q1b.id });
        assert!(
            !all(fx)
                .iter()
                .any(|q| q.origin == session(&s1) && q.state == ProposalState::AwaitingPublish)
        );
        let dxy = ensure(&ctx, &t3.dir).unwrap();
        assert_eq!(dxy.id, xyid);
        let t3b = bg(fx, &t3.tree, &dxy.id);
        let plan = plan_rollout(
            &ctx,
            q1b.id,
            &[(s1.clone(), ws1.clone()), (s2.clone(), ws2.clone())],
            true,
        )
        .unwrap();
        assert_eq!(
            plan.steps,
            vec![
                mv(&s1, &t3b, &t3.head),
                sk(&s2, SkipReason::OwnChange { proposal: q2.id })
            ]
        );
        // S2 brings its workspace to x2+y: it may move.
        fs::write(ws2.path.join("package.json"), pkg(&["x2", "y"])).unwrap();
        let plan = plan_rollout(&ctx, q1b.id, &[(s2.clone(), ws2.clone())], true).unwrap();
        assert_eq!(plan.steps, vec![mv(&s2, &t3b, &t3.head)]);
        fs::write(ws2.path.join("package.json"), pkg(&["x2"])).unwrap();
        // Main moves on.
        let t4 = advance(fx, &[("package.json", &t4_files)], "add w");
        let dw = ensure(&ctx, &t4.dir).unwrap();
        let p4 = propose(&ctx, pa(ProposalOrigin::Main, of(&dxy), of(&dw), Some(&t4))).unwrap();
        let plan = plan_rollout(&ctx, p4.id, &[(s1.clone(), ws1.clone())], true).unwrap();
        assert_eq!(
            plan.steps,
            vec![mv(&s1, &bg(fx, &t4.tree, &dw.id), &t4.head)]
        );
    } else {
        // Without step 2, Q1 stays open and keeps S1 out.
        let p3 = propose(
            &ctx,
            pa(ProposalOrigin::Main, of(&dx), (&xyid, &xyi), Some(&t3)),
        )
        .unwrap();
        let dxy = ensure(&ctx, &t3.dir).unwrap();
        let plan = plan_rollout(&ctx, p3.id, &[(s1.clone(), ws1.clone())], true).unwrap();
        assert_eq!(
            plan.steps,
            vec![sk(&s1, SkipReason::OwnChange { proposal: q1.id })]
        );
        let t4 = advance(fx, &[("package.json", &t4_files)], "add w");
        let dw = ensure(&ctx, &t4.dir).unwrap();
        let p4 = propose(&ctx, pa(ProposalOrigin::Main, of(&dxy), of(&dw), Some(&t4))).unwrap();
        let plan = plan_rollout(&ctx, p4.id, &[(s1.clone(), ws1.clone())], true).unwrap();
        assert_eq!(
            plan.steps,
            vec![sk(&s1, SkipReason::OwnChange { proposal: q1.id })]
        );
        assert_eq!(state(fx, q1.id), ProposalState::AwaitingPublish);
    }
    let _ = &mut ws2;
}

#[test]
fn landed_draft_frees_proposer_across_merged_landing() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    landing(true);
    landing(false);
}

#[test]
fn record_rollout_applies_only_complete_successful_plans() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let t1 = &w.t1;
    let p = propose(
        &ctx,
        pa(
            ProposalOrigin::Main,
            (&set('a'), &hex('a')),
            (&set('b'), &hex('b')),
            Some(t1),
        ),
    )
    .unwrap();
    let (s1, s2, s3) = (sid("aaaaaaab"), sid("aaaaaaac"), sid("aaaaaaad"));
    let base = bg(fx, &t1.tree, &set('b'));
    let plan = |complete: bool, last: RolloutStep| RolloutPlan {
        proposal: p.id,
        repo: fx.repo.clone(),
        complete,
        steps: vec![
            mv(&s1, &base, &t1.head),
            sk(&s2, SkipReason::Untracked),
            sk(&s3, SkipReason::AlreadyThere),
            last,
        ],
    };
    let ok = sk(&s3, SkipReason::AlreadyThere);
    let moved = std::slice::from_ref(&s1);
    let drift = Drift {
        pinned: set('a'),
        pinned_inputs: hex('a'),
        found_inputs: hex('e'),
    };
    let bad = [
        (plan(false, ok.clone()), moved.to_vec()),
        (plan(true, ok.clone()), vec![]),
        (plan(true, sk(&s3, SkipReason::NotReady)), moved.to_vec()),
        (
            plan(true, sk(&s3, SkipReason::OwnChange { proposal: p.id })),
            moved.to_vec(),
        ),
        (
            plan(true, sk(&s3, SkipReason::Drift { drift })),
            moved.to_vec(),
        ),
        (
            plan(true, sk(&s3, SkipReason::OpenFiles { files: vec![] })),
            moved.to_vec(),
        ),
        (
            plan(
                true,
                sk(
                    &s3,
                    SkipReason::Error {
                        error: StrataError::new(Code::Unknown, "x"),
                    },
                ),
            ),
            moved.to_vec(),
        ),
        (plan(true, mv(&s2, &base, &t1.head)), moved.to_vec()),
    ];
    for (pl, mvd) in &bad {
        let got = record_rollout(fx.store(), pl, mvd).unwrap();
        assert_eq!(got.state, ProposalState::Pending, "{pl:#?}");
    }
    let done = record_rollout(fx.store(), &plan(true, ok.clone()), moved).unwrap();
    assert_eq!(done.state, ProposalState::Applied);
    assert_eq!(
        record_rollout(fx.store(), &plan(true, ok.clone()), moved).unwrap(),
        done
    );
    // Superseded proposals are returned as they are.
    let p2 = propose(
        &ctx,
        pa(
            ProposalOrigin::Main,
            (&set('a'), &hex('a')),
            (&set('c'), &hex('c')),
            Some(t1),
        ),
    )
    .unwrap();
    let p3 = propose(
        &ctx,
        pa(
            ProposalOrigin::Main,
            (&set('a'), &hex('a')),
            (&set('d'), &hex('d')),
            Some(t1),
        ),
    )
    .unwrap();
    let mut pl = plan(true, ok);
    pl.proposal = p2.id;
    let got = record_rollout(fx.store(), &pl, moved).unwrap();
    assert_eq!(got.state, ProposalState::Superseded { by: p3.id });
    assert_eq!(state(fx, p3.id), ProposalState::Pending);
}

#[test]
fn approve_of_non_pending_proposal_is_no_such_proposal() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let t1 = &w.t1;
    let s = sid("aaaaaaab");
    let missing = ProposalId::try_from(9999u64).unwrap();
    let e = plan_rollout(&ctx, missing, &[], true).unwrap_err();
    assert_eq!(e.code, Code::NoSuchProposal);
    assert_eq!(
        ensure_target(&ctx, missing, &t1.dir).unwrap_err().code,
        Code::NoSuchProposal
    );
    assert_eq!(
        get_proposal(fx.store(), missing).unwrap_err().code,
        Code::NoSuchProposal
    );

    let a = (&set('a'), hex('a'));
    let draft = propose(
        &ctx,
        pa(session(&s), (a.0, &a.1), (&set('b'), &hex('b')), None),
    )
    .unwrap();
    let p1 = propose(
        &ctx,
        pa(
            ProposalOrigin::Main,
            (a.0, &a.1),
            (&set('c'), &hex('c')),
            Some(t1),
        ),
    )
    .unwrap();
    let p2 = propose(
        &ctx,
        pa(
            ProposalOrigin::Main,
            (a.0, &a.1),
            (&set('d'), &hex('d')),
            Some(t1),
        ),
    )
    .unwrap();
    assert!(
        fx.store()
            .decide_proposal(p2.id, ProposalDecision::Applied, SystemTime::now())
            .unwrap()
    );
    let cases = [
        (draft.id, ProposalState::AwaitingPublish),
        (p1.id, ProposalState::Superseded { by: p2.id }),
        (p2.id, ProposalState::Applied),
    ];
    for (id, st) in cases {
        assert_eq!(state(fx, id), st);
        no_such(&plan_rollout(&ctx, id, &[], true).unwrap_err(), id, &st);
        no_such(&ensure_target(&ctx, id, &t1.dir).unwrap_err(), id, &st);
    }
}

#[test]
fn approve_files_replacement_when_sealed_id_differs() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    let t2 = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
    let (yi, ya) = key(fx, &t2); // under bun 1.3.3
    let p = propose(
        &ctx,
        pa(ProposalOrigin::Main, of(&w.d1), (&ya, &yi), Some(&t2)),
    )
    .unwrap();
    fx.sb.set_bun_version("1.3.4");
    let (n0, e0) = (fx.sb.install_count(), fx.lock.enters());

    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let probe = |what: &'static str| {
        let (log, st, sd) = (log.clone(), fx.store_arc(), fx.sb.path("bun-state"));
        move || {
            let n: u32 = fs::read_to_string(sd.join("count"))
                .map(|s| s.trim().parse().unwrap())
                .unwrap_or(0);
            let rows = st.proposals(None, false).unwrap().len();
            let ended = sd.join(format!("end.{n}")).exists();
            log.lock()
                .unwrap()
                .push(format!("{what} rows={rows} installs={n} ended={ended}"));
        }
    };
    fx.lock.on_enter(probe("enter"));
    fx.lock.on_exit(probe("exit"));
    let got = ensure_target(&ctx, p.id, &t2.dir).unwrap();
    fx.lock.on_enter(|| {});
    fx.lock.on_exit(|| {});
    let Target::Replaced {
        superseded,
        replacement: q,
    } = got
    else {
        panic!("{got:#?}")
    };
    assert_eq!(superseded, p.id);
    assert_eq!(fx.sb.install_count(), n0 + 1);
    assert_eq!(fx.lock.enters(), e0 + 2);
    let n1 = n0 + 1;
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            format!("enter rows=1 installs={n1} ended=true"),
            format!("exit rows=1 installs={n1} ended=true"),
            format!("enter rows=1 installs={n1} ended=true"),
            format!("exit rows=2 installs={n1} ended=true"),
        ]
    );
    let (_, yb) = key(fx, &t2);
    assert_ne!(yb, ya);
    assert!(find_set(fx.store(), &fx.repo, &yb).unwrap().is_some());
    assert_eq!(q.state, ProposalState::Pending);
    assert_eq!((&q.to, &q.to_inputs), (&yb, &p.to_inputs));
    assert_eq!(
        (&q.origin, &q.from, &q.from_inputs, &q.tree, &q.head),
        (&p.origin, &p.from, &p.from_inputs, &p.tree, &p.head)
    );
    assert_eq!(get_proposal(fx.store(), q.id).unwrap(), q);
    assert_eq!(state(fx, p.id), ProposalState::Superseded { by: q.id });
    no_such(
        &plan_rollout(&ctx, p.id, &[], true).unwrap_err(),
        p.id,
        &ProposalState::Superseded { by: q.id },
    );
    // `to` sealed: Ready at once, no install.
    assert_eq!(
        ensure_target(&ctx, q.id, &t2.dir).unwrap(),
        Target::Ready {
            proposal: q.clone()
        }
    );
    assert_eq!(fx.sb.install_count(), n1);

    // A new Pending supersedes P while its install runs: no replacement.
    fx.sb.set_bun_version("1.3.5");
    let p5 = propose(
        &ctx,
        pa(ProposalOrigin::Main, of(&w.d1), (&set('e'), &yi), Some(&t2)),
    )
    .unwrap();
    let hooked = Arc::new(Mutex::new(None::<ProposalId>));
    {
        let (st, hooked, new) = (
            fx.store_arc(),
            hooked.clone(),
            NewProposal {
                repo: fx.repo.clone(),
                origin: None,
                from: w.d1.id.clone(),
                from_inputs: w.d1.inputs_hash.clone(),
                to: set('7'),
                to_inputs: yi.clone(),
                tree: Some(t2.tree.clone()),
                head: Some(t2.head.clone()),
            },
        );
        let id5 = p5.id;
        test_seams::after_attempt_begins(Box::new(move || {
            let mut h = hooked.lock().unwrap();
            if h.is_none() {
                *h = st
                    .record_proposal(&new, Some(id5), SystemTime::now())
                    .unwrap();
            }
        }));
    }
    let rows = all(fx).len();
    let e = ensure_target(&ctx, p5.id, &t2.dir).unwrap_err();
    test_seams::reset();
    let by = hooked.lock().unwrap().expect("the hook filed a Pending");
    no_such(&e, p5.id, &ProposalState::Superseded { by });
    assert_eq!(all(fx).len(), rows + 1);
    let (_, y5) = key(fx, &t2);
    assert!(find_set(fx.store(), &fx.repo, &y5).unwrap().is_some());
    assert!(!all(fx).iter().any(|r| r.to == y5));

    // Step 4's lock is busy: Busy, no replacement (the set is sealed).
    fx.sb.set_bun_version("1.3.6");
    let p6 = propose(
        &ctx,
        pa(ProposalOrigin::Main, of(&w.d1), (&set('6'), &yi), Some(&t2)),
    )
    .unwrap();
    let d6 = ensure(&ctx, &t2.dir).unwrap();
    let (rows, e6) = (all(fx).len(), fx.lock.enters());
    fx.lock.set_busy(1);
    let e = ensure_target(&ctx, p6.id, &t2.dir).unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert_eq!(fx.lock.enters(), e6);
    assert_eq!(all(fx).len(), rows);
    assert_eq!(state(fx, p6.id), ProposalState::Pending);
    assert!(find_set(fx.store(), &fx.repo, &d6.id).unwrap().is_some());
}

#[test]
fn approve_refuses_pending_whose_head_left_main() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    // Main moved without being observed.
    {
        let w = world();
        let fx = &w.fx;
        let ctx = fx.ctx();
        assert_eq!(observe(fx, &w.t1).unwrap(), None);
        let t2 = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
        let d2 = ensure(&ctx, &t2.dir).unwrap();
        let p = observe(fx, &t2).unwrap().unwrap();
        assert_eq!(p.to, d2.id);
        let (s1, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
        let (ws1, ws2) = (w.ws(&s1, false), w.ws(&s2, false));
        let sessions = [(s1.clone(), ws1.clone()), (s2.clone(), ws2.clone())];
        let target = bg(fx, &t2.tree, &d2.id);
        let plan = plan_rollout(&ctx, p.id, &sessions, true).unwrap();
        assert_eq!(
            plan.steps,
            vec![mv(&s1, &target, &t2.head), mv(&s2, &target, &t2.head)]
        );
        let t3 = advance(fx, &[("README.md", b"readme\n")], "readme");
        let before = (all(fx), main_row(fx));
        let e = plan_rollout(&ctx, p.id, &sessions, true).unwrap_err();
        assert_eq!(e.code, Code::HeadMoved);
        assert_eq!(reason(&e), Some("main_moved"));
        assert_eq!(detail_str(&e, "head"), Some(t2.head.as_str()));
        assert_eq!(detail_str(&e, "tip"), Some(t3.head.as_str()));
        assert_eq!(
            e.detail.as_ref().unwrap()["proposal"],
            serde_json::json!(p.id.get())
        );
        assert_eq!((all(fx), main_row(fx)), before);
        let p2 = observe(fx, &t3).unwrap().unwrap();
        assert_eq!(
            (&p2.to, &p2.tree, &p2.head),
            (&d2.id, &Some(t3.tree.clone()), &Some(t3.head.clone()))
        );
        assert_eq!(state(fx, p.id), ProposalState::Superseded { by: p2.id });
        assert_eq!(
            plan_rollout(&ctx, p.id, &sessions, true).unwrap_err().code,
            Code::NoSuchProposal
        );
        let plan = plan_rollout(&ctx, p2.id, &sessions[..1], true).unwrap();
        assert_eq!(
            plan.steps,
            vec![mv(&s1, &bg(fx, &t3.tree, &d2.id), &t3.head)]
        );
    }
    // An unpushed publish chain is main's tip until origin rewinds.
    {
        let w = world();
        let fx = &w.fx;
        let ctx = fx.ctx();
        let h0 = w.t1.head.clone();
        fx.sb
            .commit(&fx.main, &[("README.md", b"readme\n")], "readme");
        publish(fx);
        let h1 = fx.sb.head(&fx.main);
        fx.sb
            .commit(&fx.main, &[("package.json", &pkg(&["y"]))], "add y");
        let t2 = tip(fx);
        fx.sb.git(
            &fx.main,
            &["update-ref", "refs/strata/publish/main", t2.head.as_str()],
        );
        fx.sb.git(
            &fx.main,
            &["update-ref", "refs/strata/anchor/main", h1.trim()],
        );
        assert_eq!(main_tip(&fx.cfg, &fx.git).unwrap(), Some(t2.head.clone()));
        let d2 = ensure(&ctx, &t2.dir).unwrap();
        let p = propose(
            &ctx,
            pa(ProposalOrigin::Main, of(&w.d1), of(&d2), Some(&t2)),
        )
        .unwrap();
        let s1 = sid("aaaaaaab");
        let sessions = [(s1.clone(), w.ws(&s1, false))];
        let plan = plan_rollout(&ctx, p.id, &sessions, true).unwrap();
        assert_eq!(
            plan.steps,
            vec![mv(&s1, &bg(fx, &t2.tree, &d2.id), &t2.head)]
        );
        push_oid(fx, h0.as_str());
        let e = plan_rollout(&ctx, p.id, &sessions, true).unwrap_err();
        assert_eq!(e.code, Code::HeadMoved);
        assert_eq!(detail_str(&e, "tip"), Some(h0.as_str()));
        assert_eq!(state(fx, p.id), ProposalState::Pending);
    }
    // The proposer's own Pending refuses too; its draft does not.
    {
        let w = world();
        let fx = &w.fx;
        let ctx = fx.ctx();
        let s = sid("aaaaaaab");
        let ws = w.ws(&s, true);
        fs::write(ws.path.join("package.json"), pkg(&["y"])).unwrap();
        let dy = build(&ctx, &fx.store().base_dir(&w.b1), Some(&ws.path)).unwrap();
        propose(&ctx, pa(session(&s), of(&w.d1), of(&dy), None)).unwrap();
        let t2 = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
        let qp = propose(&ctx, pa(session(&s), of(&w.d1), of(&dy), Some(&t2))).unwrap();
        assert_eq!(qp.state, ProposalState::Pending);
        advance(fx, &[("README.md", b"readme\n")], "readme");
        let e = plan_switch(&ctx, qp.id, &s, &ws).unwrap_err();
        assert_eq!(e.code, Code::HeadMoved);
        assert_eq!(reason(&e), Some("main_moved"));
        fs::write(ws.path.join("package.json"), pkg(&["y", "z"])).unwrap();
        let dz = build(&ctx, &fx.store().base_dir(&w.b1), Some(&ws.path)).unwrap();
        let r = propose(&ctx, pa(session(&s), of(&w.d1), of(&dz), None)).unwrap();
        assert_eq!(r.state, ProposalState::AwaitingPublish);
        let plan = plan_switch(&ctx, r.id, &s, &ws).unwrap();
        assert_eq!(
            plan.steps,
            vec![mv(&s, &bg(fx, &w.t1.tree, &dz.id), &ws.head)]
        );
    }
}

#[test]
fn observe_main_retarget_keeps_session_origin() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    assert_eq!(observe(fx, &w.t1).unwrap(), None);
    let (s, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let ws = w.ws(&s, true);
    let ws2 = w.ws(&s2, true);
    fs::write(ws.path.join("package.json"), pkg(&["y"])).unwrap();
    let dn = build(&ctx, &fx.store().base_dir(&w.b1), Some(&ws.path)).unwrap();
    propose(&ctx, pa(session(&s), of(&w.d1), of(&dn), None)).unwrap();
    let t2 = advance(fx, &[("package.json", &pkg(&["y"]))], "add y");
    assert_eq!(key(fx, &t2).1, dn.id);
    let p = propose(&ctx, pa(session(&s), of(&w.d1), of(&dn), Some(&t2))).unwrap();
    assert_eq!(p.state, ProposalState::Pending);
    let rows = all(fx).len();
    assert_eq!(observe(fx, &t2).unwrap().map(|q| q.id), Some(p.id));
    assert_eq!(all(fx).len(), rows);
    assert_eq!(
        main_row(fx),
        Some((
            t2.tree.clone(),
            t2.head.clone(),
            dn.inputs_hash.clone(),
            dn.id.clone()
        ))
    );

    let t3 = advance(fx, &[("README.md", b"readme\n")], "readme");
    let p2 = observe(fx, &t3).unwrap().unwrap();
    assert_eq!(p2.origin, session(&s));
    assert_eq!(
        (&p2.from, &p2.from_inputs, &p2.to, &p2.to_inputs),
        (&p.from, &p.from_inputs, &p.to, &p.to_inputs)
    );
    assert_eq!(
        (&p2.tree, &p2.head),
        (&Some(t3.tree.clone()), &Some(t3.head.clone()))
    );
    assert_eq!(state(fx, p.id), ProposalState::Superseded { by: p2.id });
    assert_eq!(
        main_row(fx),
        Some((
            t3.tree.clone(),
            t3.head.clone(),
            dn.inputs_hash.clone(),
            dn.id.clone()
        ))
    );
    let plan = plan_switch(&ctx, p2.id, &s, &ws).unwrap();
    assert_eq!(
        plan.steps,
        vec![mv(&s, &bg(fx, &t3.tree, &dn.id), &t3.head)]
    );
    let e = plan_switch(&ctx, p2.id, &s2, &ws2).unwrap_err();
    assert_eq!(e.code, Code::NoSuchProposal);
    assert_eq!(reason(&e), Some("not_origin"));

    // Main is forced back to H2.
    rewind(fx, &t2.head);
    let t2b = tip(fx);
    let p3 = observe(fx, &t2b).unwrap().unwrap();
    assert_eq!(p3.origin, session(&s));
    assert_eq!(
        (&p3.tree, &p3.head),
        (&Some(t2.tree.clone()), &Some(t2.head.clone()))
    );
    let plan = plan_switch(&ctx, p3.id, &s, &ws).unwrap();
    assert_eq!(
        plan.steps,
        vec![mv(&s, &bg(fx, &t2.tree, &dn.id), &t2.head)]
    );
    let rows = all(fx).len();
    assert_eq!(observe(fx, &t2b).unwrap(), None);
    assert_eq!(all(fx).len(), rows);
}

#[test]
fn participant_publish_keeps_landed_proposers_pending() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        return;
    }
    let w = world();
    let fx = &w.fx;
    let ctx = fx.ctx();
    assert_eq!(observe(fx, &w.t1).unwrap(), None);
    let (s1, s2) = (sid("aaaaaaab"), sid("aaaaaaac"));
    let ws1 = w.ws(&s1, true);
    let _ws2 = w.ws(&s2, true);
    fs::write(ws1.path.join("package.json"), pkg(&["x2"])).unwrap();
    let dx = build(&ctx, &fx.store().base_dir(&w.b1), Some(&ws1.path)).unwrap();
    let q1 = propose(&ctx, pa(session(&s1), of(&w.d1), of(&dx), None)).unwrap();

    // S1 publishes: steps 2 and 4 of the landing under one lock.
    let t2 = advance(fx, &[("package.json", &pkg(&["x2"]))], "add x2");
    assert_eq!(key(fx, &t2).1, dx.id);
    let mut got = (None, None, 0);
    fx.lock
        .with_write(&fx.repo, &mut || {
            let a = propose(
                &ctx,
                pa(
                    session(&s1),
                    (&q1.from, &q1.from_inputs),
                    of(&dx),
                    Some(&t2),
                ),
            )?;
            let n = all(fx).len();
            let b = propose(
                &ctx,
                pa(ProposalOrigin::Main, of(&w.d1), of(&dx), Some(&t2)),
            )?;
            got = (Some(a), Some(b), n);
            Ok(())
        })
        .unwrap()
        .unwrap();
    let (a, b, n) = (got.0.unwrap(), got.1.unwrap(), got.2);
    assert_eq!(a.state, ProposalState::Pending);
    assert_eq!(b.id, a.id);
    assert_eq!(all(fx).len(), n);

    // S2 publishes an unrelated change: step 4 only.
    let t3 = advance(fx, &[("README.md", b"readme\n")], "readme");
    assert_eq!(key(fx, &t3).1, dx.id);
    let mut got = (None, None);
    fx.lock
        .with_write(&fx.repo, &mut || {
            let c = propose(
                &ctx,
                pa(ProposalOrigin::Main, of(&w.d1), of(&dx), Some(&t3)),
            )?;
            let c2 = propose(
                &ctx,
                pa(ProposalOrigin::Main, of(&w.d1), of(&dx), Some(&t3)),
            )?;
            got = (Some(c), Some(c2));
            Ok(())
        })
        .unwrap()
        .unwrap();
    let (c, c2) = (got.0.unwrap(), got.1.unwrap());
    assert_eq!(c.origin, session(&s1));
    assert_eq!((&c.from, &c.from_inputs), (&q1.from, &q1.from_inputs));
    assert_eq!(
        (&c.tree, &c.head),
        (&Some(t3.tree.clone()), &Some(t3.head.clone()))
    );
    assert_eq!(state(fx, a.id), ProposalState::Superseded { by: c.id });
    assert_eq!(c2.id, c.id);
    let rows = all(fx).len();
    let plan = plan_switch(&ctx, c.id, &s1, &ws1).unwrap();
    assert_eq!(
        plan.steps,
        vec![mv(&s1, &bg(fx, &t3.tree, &dx.id), &t3.head)]
    );
    assert_eq!(observe(fx, &t3).unwrap().map(|q| q.id), Some(c.id));
    assert_eq!(all(fx).len(), rows);
    assert_eq!(
        main_row(fx),
        Some((
            t3.tree.clone(),
            t3.head.clone(),
            dx.inputs_hash.clone(),
            dx.id.clone()
        ))
    );
}

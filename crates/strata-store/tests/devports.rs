//! store.md §4.4a: dev-port leases (allocation at create/add, ensure,
//! reassign, release at Removed, gc).

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use strata_store::*;
use strata_types::{BaseGen, BaseManifest, Code, CommitOid, DepsSet, DepsSetId, RepoName, SessionId, SessionManifest, TreeOid, WorkspaceState};
use support::*;

fn repo(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}
fn sess(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}
fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}
fn g(r: &str) -> BaseGen {
    BaseGen { repo: repo(r), tree: TreeOid::try_from("1".repeat(40)).unwrap(), deps: DepsSetId::try_from("a".repeat(64)).unwrap() }
}

fn setup() -> (Env, Store) {
    let env = env();
    let s = env.open();
    for r in ["app", "lib"] {
        let set = DepsSet { id: g(r).deps, repo: repo(r), inputs_hash: "0".repeat(64), platform: "darwin-arm64".into(), toolchain: "none".into(), dirs: vec![] };
        s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
        s.record_base_gen(&g(r), &BaseManifest { tree: g(r).tree, entries: BTreeMap::new() }, t(1)).unwrap();
    }
    (env, s)
}

fn sroot(env: &Env, id: &SessionId) -> PathBuf {
    env.root.join("sessions").join(id.as_str())
}

fn state(env: &Env, id: &SessionId, r: &str) -> WorkspaceState {
    WorkspaceState { base: g(r), head: CommitOid::try_from("2".repeat(40)).unwrap(), path: sroot(env, id).join(r), tracked: false }
}

fn manifest(env: &Env, id: &SessionId, repos: &[&str]) -> SessionManifest {
    let repos = repos.iter().map(|r| (repo(r), state(env, id, r))).collect();
    SessionManifest { id: id.clone(), ticket: None, agent: None, root: sroot(env, id), repos }
}

fn active(env: &Env, s: &Store, id: &SessionId, repos: &[&str]) {
    s.create_session(&manifest(env, id, repos), t(10)).unwrap();
    assert!(s.set_lifecycle(id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
}

fn policy(blocks: &[u16], ok: impl Fn(u16) -> bool + Send + Sync + 'static) -> Option<DevPortsPolicy> {
    Some(DevPortsPolicy { blocks: blocks.to_vec(), port_ok: Arc::new(ok) })
}

fn leases(s: &Store) -> Vec<(String, String, u16)> {
    s.dev_port_leases(None).unwrap().unwrap().into_iter().map(|l| (l.session.as_str().to_string(), l.repo.as_str().to_string(), l.base)).collect()
}

fn base(s: &Store, id: &SessionId, r: &str) -> Option<u16> {
    s.dev_port_leases(Some(id)).unwrap().unwrap().into_iter().find(|l| l.repo.as_str() == r).map(|l| l.base)
}

#[test]
fn without_a_policy_nothing_is_leased_and_ensure_is_unsupported() {
    let (env, s) = setup();
    let id = sess("nopolicy");
    active(&env, &s, &id, &["app"]);
    assert!(leases(&s).is_empty());
    assert_reason(&s.ensure_dev_ports(&id, &repo("app")).unwrap_err(), Code::Unsupported, "no_policy");
}

#[test]
fn new_and_add_lease_distinct_blocks_that_pass_the_probe() {
    let (env, s) = setup();
    // 20004's block has a busy port: never chosen
    s.set_dev_ports_policy(policy(&[20000, 20004, 20008, 20012], |p| p != 20006));
    let id = sess("aaaaaaaa");
    active(&env, &s, &id, &["app"]);
    let mut add = BTreeMap::new();
    add.insert(repo("lib"), state(&env, &id, "lib"));
    s.add_workspaces(&id, &add, &BTreeMap::new()).unwrap();
    let (a, l) = (base(&s, &id, "app").unwrap(), base(&s, &id, "lib").unwrap());
    assert_ne!(a, l);
    assert!(![a, l].contains(&20004));
    // ensure is idempotent and stable
    let e = s.ensure_dev_ports(&id, &repo("app")).unwrap();
    assert_eq!((e.base, e.web(), e.connector(), e.connector_mcp(), e.aux()), (a, a, a + 1, a + 2, a + 3));
    assert_eq!(base(&s, &id, "app"), Some(a));
    // a second session never overlaps
    let other = sess("bbbbbbbb");
    active(&env, &s, &other, &["app"]);
    let o = base(&s, &other, "app").unwrap();
    assert!(![a, l, 20004].contains(&o));
}

#[test]
fn exhaustion_never_blocks_new_and_ensure_reports_it() {
    let (env, s) = setup();
    s.set_dev_ports_policy(policy(&[20000], |_| true));
    let id = sess("cccccccc");
    active(&env, &s, &id, &["app", "lib"]);
    let got = leases(&s);
    assert_eq!(got.len(), 1, "{got:?}");
    let unleased = if got[0].1 == "app" { "lib" } else { "app" };
    let e = s.ensure_dev_ports(&id, &repo(unleased)).unwrap_err();
    assert_reason(&e, Code::Busy, "ports_exhausted");
    // reassign with nowhere to go keeps the old lease
    let leased = got[0].1.clone();
    assert_reason(&s.reassign_dev_ports(&id, &repo(&leased)).unwrap_err(), Code::Busy, "ports_exhausted");
    assert_eq!(base(&s, &id, &leased), Some(20000));
}

#[test]
fn reassign_moves_to_a_later_block_and_frees_the_old_one() {
    let (env, s) = setup();
    s.set_dev_ports_policy(policy(&[20000, 20004, 20008], |_| true));
    let id = sess("dddddddd");
    active(&env, &s, &id, &["app"]);
    let old = base(&s, &id, "app").unwrap();
    let new = s.reassign_dev_ports(&id, &repo("app")).unwrap().base;
    assert_ne!(old, new);
    assert_eq!(base(&s, &id, "app"), Some(new));
    assert_eq!(leases(&s).len(), 1);
}

#[test]
fn ensure_needs_an_active_session_and_removal_releases_for_good() {
    let (env, s) = setup();
    s.set_dev_ports_policy(policy(&[20000, 20004, 20008, 20012], |_| true));
    // creating: not active
    let c = sess("creating");
    s.create_session(&manifest(&env, &c, &["app"]), t(10)).unwrap();
    assert_reason(&s.ensure_dev_ports(&c, &repo("app")).unwrap_err(), Code::NoSuchSession, "not_active");
    // unknown: keeps its lease, gets no new one
    assert!(s.set_lifecycle(&c, Lifecycle::Creating, Lifecycle::Unknown, Some(&json!({"step": 1})), t(12)).unwrap());
    assert!(base(&s, &c, "app").is_some());
    assert_reason(&s.ensure_dev_ports(&c, &repo("app")).unwrap_err(), Code::InvalidInput, "session_unknown");
    // a repo the session lacks
    let a = sess("activeee");
    active(&env, &s, &a, &["app"]);
    assert_code(&s.ensure_dev_ports(&a, &repo("lib")).unwrap_err(), Code::NoSuchRepo);
    assert_code(&s.ensure_dev_ports(&sess("nosuchxx"), &repo("app")).unwrap_err(), Code::NoSuchSession);
    // removing keeps, removed releases; nothing resurrects it
    assert!(s.set_lifecycle(&a, Lifecycle::Active, Lifecycle::Removing, None, t(20)).unwrap());
    assert!(base(&s, &a, "app").is_some());
    assert_reason(&s.ensure_dev_ports(&a, &repo("app")).unwrap_err(), Code::NoSuchSession, "not_active");
    assert!(s.set_lifecycle(&a, Lifecycle::Removing, Lifecycle::Removed, None, t(21)).unwrap());
    assert_eq!(base(&s, &a, "app"), None);
    assert_reason(&s.reassign_dev_ports(&a, &repo("app")).unwrap_err(), Code::NoSuchSession, "not_active");
}

#[test]
fn read_only_handles_cannot_lease() {
    let (env, s) = setup();
    s.set_dev_ports_policy(policy(&[20000], |_| true));
    let id = sess("eeeeeeee");
    active(&env, &s, &id, &["app"]);
    let ro = Store::open_read_only(&env.root).unwrap();
    assert_eq!(ro.dev_port_leases(Some(&id)).unwrap().unwrap().len(), 1);
    assert_code(&ro.ensure_dev_ports(&id, &repo("app")).unwrap_err(), Code::ReadOnly);
}

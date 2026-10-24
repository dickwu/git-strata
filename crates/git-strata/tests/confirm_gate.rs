//! cli.md §8 Task 2: TTY before confirmation; nothing reaches the socket
//! when a confirm-class command runs without a terminal.

mod support;

use serde_json::{Value, json};
use support::{FakeDaemon, Home, RepoSpec, git};

fn guarded_home() -> Home {
    let h = Home::new();
    h.write_config(&[RepoSpec { name: "app", push: true, confirm: true, deploy_on_push: true }]);
    h
}

fn refuse_all(req: &Value) -> Value {
    json!({ "id": req["id"], "ok": false, "error": { "rpc": "Failed", "message": "the test daemon answers nothing" } })
}

#[test]
fn confirm_commands_refuse_non_tty() {
    let h = guarded_home();
    let d = FakeDaemon::start(&h.socket(), refuse_all);
    let cases: &[&[&str]] = &[
        &["deps", "approve", "1"],
        &["push", "app"],
        &["publish", "--from", "s-aaaaaaaa:a.txt"],
        &["chain", "--repo", "app", "--restore"],
        &["chain", "--repo", "app", "--accept-origin", "replay"],
        &["resolve", "--session", "s-aaaaaaaa", "--repo", "app", "--accept"],
        &["config", "relax", "app"],
        &["rm", "--session", "s-aaaaaaaa", "--discard"],
    ];
    for args in cases {
        let r = h.run(args);
        assert_eq!(r.err_code(), "NotTty", "{args:?}: {r:?}");
        assert_eq!(r.reason(), "not_tty", "{args:?}");
        assert!(r.stderr.is_empty() || !r.stderr.contains("confirm"), "{args:?} showed a prompt: {r:?}");
    }
    // Publish without --from on a non-TTY is no_batch, not a prompt.
    let r = h.run(&["publish"]);
    assert_eq!(r.err_code(), "InvalidInput");
    assert_eq!(r.reason(), "no_batch");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(d.connections(), 0, "a refused command connected: {:?}", d.requests());
}

fn superseded(req: &Value) -> Value {
    assert_eq!(req["method"], "deps_approve");
    json!({
        "id": req["id"],
        "ok": false,
        "error": {
            "code": "NoSuchProposal",
            "message": "proposal 1 is superseded",
            "detail": {
                "proposal": 1,
                "state": { "kind": "Superseded", "by": 2 },
                "replacement": { "id": 2, "repo": "app", "state": { "kind": "Pending" } }
            }
        }
    })
}

#[test]
fn superseded_proposal_is_not_auto_approved() {
    let h = guarded_home();
    let d = FakeDaemon::start(&h.socket(), superseded);
    let r = h.run_tty(&["deps", "approve", "1"]);
    assert_eq!(r.err_code(), "NoSuchProposal", "{r:?}");
    assert!(r.stderr.contains("superseded by proposal 2"), "{r:?}");
    assert!(r.stderr.contains("replacement"), "{r:?}");
    let reqs = d.requests();
    assert_eq!(reqs.len(), 1, "approved again: {reqs:?}");
    assert_eq!(reqs[0]["method"], "deps_approve");
    assert_eq!(reqs[0]["params"]["proposal"], 1);
    assert_eq!(reqs[0]["params"]["sessions"], Value::Null);
}

#[test]
fn resolve_and_chain_require_an_explicit_choice() {
    let h = guarded_home();
    let d = FakeDaemon::start(&h.socket(), refuse_all);
    for (args, reason) in [
        (vec!["resolve", "--session", "s-aaaaaaaa", "--repo", "app"], "no_choice"),
        (vec!["resolve", "--session", "s-aaaaaaaa", "--repo", "app", "--abandon", "--accept"], "both_choices"),
        (vec!["chain", "--repo", "app"], "no_choice"),
        (vec!["chain", "--repo", "app", "--accept-origin", "drop", "--restore"], "both_choices"),
    ] {
        // Even on a terminal: the choice is checked first.
        let r = h.run_tty(&args);
        assert_eq!(r.err_code(), "InvalidInput", "{args:?}: {r:?}");
        assert_eq!(r.reason(), reason, "{args:?}");
        let r = h.run(&args);
        assert_eq!(r.reason(), reason, "{args:?}");
    }
    // A diverged chain cannot be restored: not_ancestor before any push.
    let repo = h.repo_dir("app");
    git(&repo, h.path(), &["init", "-q", "-b", "main"]);
    let tree = git(&repo, h.path(), &["write-tree"]);
    let a = git(&repo, h.path(), &["commit-tree", &tree, "-m", "a"]);
    let b = git(&repo, h.path(), &["commit-tree", &tree, "-p", &a, "-m", "origin b"]);
    let c = git(&repo, h.path(), &["commit-tree", &tree, "-p", &a, "-m", "local c"]);
    git(&repo, h.path(), &["update-ref", "refs/strata/remotes/origin/main", &b]);
    git(&repo, h.path(), &["update-ref", "refs/strata/publish/main", &c]);
    git(&repo, h.path(), &["update-ref", "refs/strata/anchor/main", &a]);
    let r = h.run_tty(&["chain", "--repo", "app", "--restore"]);
    assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
    assert_eq!(r.reason(), "not_ancestor", "{r:?}");
    let refs = git(&repo, h.path(), &["for-each-ref", "--format=%(refname) %(objectname)"]);
    assert!(refs.contains(&format!("refs/strata/publish/main {c}")), "{refs}");
    assert!(refs.contains(&format!("refs/strata/remotes/origin/main {b}")), "{refs}");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(d.connections(), 0, "{:?}", d.requests());
}

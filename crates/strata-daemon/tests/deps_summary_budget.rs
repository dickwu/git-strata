//! Legacy summary attribution is best-effort and shares one budget per RPC.
mod support;

use serde_json::json;
use std::time::{Duration, SystemTime};
use strata_daemon::{
    handlers::{Server, Trackers},
    rpc::{Method, Params, Request},
    startup::Daemon,
};
use strata_types::{DepsSetId, SessionId};
use support::*;

#[test]
fn legacy_summary_ancestry_is_bounded_per_deps_list() {
    let fx = Dfx::with(&["a", "b", "c"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let repos = [repo_name("a"), repo_name("b"), repo_name("c")];
    let original = d.sessions.new(None, &repos).unwrap().manifest;
    for i in 0..10 {
        let mut m = original.clone();
        m.id = SessionId::try_from(format!("s-summary{}", char::from(b'a' + i as u8))).unwrap();
        m.root = m.root.with_file_name(m.id.as_str());
        for (repo, ws) in &mut m.repos {
            ws.path = m.root.join(repo.as_str());
            ws.head = core.gits[repo]
                .commit_tree(&ws.base.tree, &ws.head, &format!("legacy {i}\n"))
                .unwrap();
        }
        core.store
            .create_session(&m, SystemTime::now() + Duration::from_secs(i + 1))
            .unwrap();
    }
    for repo in &repos {
        let ws = &original.repos[repo];
        core.store
            .record_proposal(
                &strata_store::NewProposal {
                    repo: repo.clone(),
                    origin: None,
                    from: ws.base.deps.clone(),
                    from_inputs: "a".repeat(64),
                    from_head: (repo.as_str() == "c").then(|| ws.head.clone()),
                    to: DepsSetId::try_from("b".repeat(64)).unwrap(),
                    to_inputs: "b".repeat(64),
                    tree: Some(ws.base.tree.clone()),
                    head: Some(ws.head.clone()),
                },
                None,
                SystemTime::now(),
            )
            .unwrap()
            .unwrap();
        core.gits[repo].take_git_calls();
    }
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    for _ in 0..2 {
        let reply = server
            .handle(Request {
                id: json!(1),
                method: Method::DepsList,
                params: Params::parse(Method::DepsList, json!({"repo":null})).unwrap(),
            })
            .0;
        assert_eq!(reply["ok"], true, "{reply}");
        let rows = reply["data"]["proposals"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .find(|p| p["repo"] == "c")
                .unwrap()
                .get("changes")
                .is_some(),
            "recorded comparison heads bypass the exhausted legacy budget"
        );
        assert_eq!(
            rows.iter().filter(|p| p.get("changes").is_some()).count(),
            2,
            "exhausted attribution must omit enrichment, not use a partial base: {reply}"
        );
        let ancestry_calls: usize = repos
            .iter()
            .map(|repo| {
                core.gits[repo]
                    .take_git_calls()
                    .iter()
                    .filter(|c| c.args.iter().any(|a| a == "--is-ancestor"))
                    .count()
            })
            .sum();
        assert!(
            ancestry_calls <= 16,
            "unbounded legacy ancestry: {ancestry_calls}"
        );
    }
}

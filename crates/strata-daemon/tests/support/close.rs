//! What the auto-close tests share, over `Server::handle` and `Dfx`.

use serde_json::{Value, json};
use strata_daemon::handlers::Server;
use strata_daemon::rpc::{Method, Params, Request};
use strata_daemon::startup::Core;
use strata_types::{CommitOid, SessionId, SessionMark};

use super::{Dfx, batch, checkpoint, group, repo_name};

/// A fixture with auto-close on, as a test opts in: before `prepare`.
pub fn opted_in(names: &[&str]) -> Dfx {
    let mut fx = Dfx::with(names);
    fx.config.publish.auto_close = true;
    fx.write_config();
    fx
}

pub fn call(server: &Server<'_, '_>, method: &str, params: Value) -> Value {
    let method = Method::from_name(method).unwrap();
    server
        .handle(Request {
            id: json!(1),
            method,
            params: Params::parse(method, params).unwrap(),
        })
        .0
}

pub fn ok(reply: Value) -> Value {
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"].clone()
}

/// `new {repos}` with `only` absent: every other configured repo joins untracked.
pub fn new_in(server: &Server<'_, '_>, repos: &[&str]) -> SessionId {
    let data = ok(call(
        server,
        "new",
        json!({ "ticket": null, "repos": repos }),
    ));
    serde_json::from_value(data["session"]["manifest"]["id"].clone()).unwrap()
}

pub fn new(server: &Server<'_, '_>) -> SessionId {
    new_in(server, &["tool"])
}

/// `new {repos, only: true}`: only those repos, no untracked siblings.
pub fn new_only(server: &Server<'_, '_>, repos: &[&str]) -> SessionId {
    let data = ok(call(
        server,
        "new",
        json!({ "ticket": null, "repos": repos, "only": true }),
    ));
    serde_json::from_value(data["session"]["manifest"]["id"].clone()).unwrap()
}

/// A checkpoint of `repo`, then the `publish` of `paths`: the whole reply,
/// because a refusal is what some tests assert.
pub fn publish(
    server: &Server<'_, '_>,
    core: &Core,
    s: &SessionId,
    repo: &str,
    paths: &[&str],
) -> Value {
    let cp = checkpoint(core, s, repo);
    call(
        server,
        "publish",
        json!({ "group": group(vec![batch(s, repo, cp.id, paths)]), "shown": {} }),
    )
}

/// A commit on `repo`'s main and on the daemon's tracking ref of it, with no fetch.
pub fn advance(fx: &Dfx, core: &Core, repo: &str, message: &str) -> CommitOid {
    let main = fx.main(repo);
    let head = CommitOid::try_from(fx.sb.commit(&main, &[], message)).unwrap();
    let git = &core.gits[&repo_name(repo)];
    let name = strata_gitio::remote_ref("origin", "main");
    git.ref_transaction(&[strata_gitio::RefUpdate::set(
        &name,
        head.clone(),
        git.read_ref(&name).unwrap(),
    )])
    .unwrap();
    head
}

/// Every line of `sessions-audit.jsonl`, in order; none when the file is absent.
pub fn audit_lines(fx: &Dfx) -> Vec<Value> {
    std::fs::read_to_string(fx.config_dir.join("sessions-audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

pub fn mark_of(core: &Core, s: &SessionId) -> Option<SessionMark> {
    core.store.mark(s).unwrap()
}

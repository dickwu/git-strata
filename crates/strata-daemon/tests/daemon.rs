//! daemon.md §8 Task 6: end to end over the socket, and the read-only path
//! when no daemon listens (§6, §7). The daemon is served in process (a
//! binary built with test seams refuses to start, Task 3); the workspace is
//! edited by separate `sh` processes.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use strata_daemon::client;
use strata_daemon::handlers;
use strata_daemon::locks::Locks;
use strata_daemon::rpc::Method;
use strata_daemon::startup::{self, Core, Daemon, DaemonBefore};
use strata_gitio::Git;
use strata_overlay::{Base, Overlay, WorkspaceRef};
use strata_search::{GrepOpts, OverlaySource, Search, Which};
use strata_session::{SessionState, Sessions};
use strata_store::{PublishPhase, Store};
use strata_types::{CheckpointReason, Code, RepoName, RepoPath, SessionId};
use support::*;

/// Serve `d` with every handler in a scoped thread, run `f`, stop.
fn serving<R>(d: &Daemon<'_>, f: impl FnOnce() -> R) -> R {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(d, &listener, &ext));
        // Stop even when `f` panics, or the scope would wait forever.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// One request on its own connection; the whole reply object.
fn call(socket: &Path, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let line = json!({ "id": 7, "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut r = BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

fn ok(v: Value) -> Value {
    assert_eq!(v["ok"], json!(true), "{v:#}");
    v["data"].clone()
}

fn new_session(socket: &Path) -> SessionId {
    let d = ok(call(socket, "new", json!({ "ticket": null, "repos": ["tool"] })));
    serde_json::from_value(d["session"]["manifest"]["id"].clone()).unwrap()
}

fn reason_json(r: CheckpointReason) -> Value {
    serde_json::to_value(r).unwrap()
}

/// Run `script` with `/bin/sh` in `dir`: another process edits the files.
fn external(dir: &Path, script: &str) {
    let st = Command::new("/bin/sh").arg("-c").arg(script).current_dir(dir).status().unwrap();
    assert!(st.success(), "{script}");
}

/// Every regular file and symlink under `dir` (relative path → content or
/// link target), skipping `.git`, the deps dir, the seed cache and
/// `.DS_Store`. Empty directories are not content (as in git): restoring
/// a deletion may prune its emptied parent, the oracle keeps it.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if [".git", "node_modules", ".cache"].contains(&rel.as_str()) || rel.ends_with(".DS_Store") {
                continue;
            }
            let m = fs::symlink_metadata(&p).unwrap();
            if m.file_type().is_symlink() {
                out.insert(rel, format!("-> {}", fs::read_link(&p).unwrap().display()).into_bytes());
            } else if m.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

const EDIT: &str = "printf 'export const v = 2\\n' > src/index.ts
printf 'new file\\n' > src/new.ts
rm patches/a.patch
mkdir -p docs/deep && printf '# notes\\nneedle here\\n' > docs/deep/x.md
mkdir -p node_modules/x && printf 'ignored\\n' > node_modules/x/extra.js";

const TRASH: &str = "printf 'clobbered\\n' > src/index.ts
rm src/new.ts
rm -r docs
printf 'junk\\n' > junk.txt
printf 'patch again\\n' > patches/a.patch";

#[test]
fn e2e_checkpoint_restore_matches_oracle() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        external(&root, EDIT);

        // The oracle: the same edits applied to a plain export of HEAD.
        let oracle = fx.sb.path("oracle");
        fx.sb.archive_into(&fx.main("tool"), "HEAD", &oracle);
        external(&oracle, EDIT);
        assert_eq!(tree(&root), tree(&oracle), "the workspace starts as the oracle");

        let cp = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::ToolCall { id: "call-1".into() }),
            "paths": [],
        })));
        assert_eq!(cp["recorded"], json!(true), "{cp:#}");
        let cp_id = cp["checkpoint"]["id"].clone();

        // Search sees the workspace as the overlay says.
        let g = ok(call(&sock, "grep", json!({ "pattern": "needle", "session": id, "all": false, "ignore_case": false })));
        let hits = g["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "{g:#}");
        assert_eq!(hits[0]["path"], "docs/deep/x.md");
        assert_eq!(hits[0]["line"], 2);
        let f = ok(call(&sock, "files", json!({ "session": id, "repo": "tool", "glob": "src/*.ts" })));
        assert_eq!(f["paths"], json!(["src/index.ts", "src/new.ts"]), "{f:#}");

        external(&root, TRASH);
        assert_ne!(tree(&root), tree(&oracle));
        let r = ok(call(&sock, "restore", json!({ "session": id, "repo": "tool", "checkpoint": cp_id, "paths": null })));
        assert!(r["conflicts"].as_array().is_none_or(|c| c.is_empty()), "{r:#}");
        assert_eq!(tree(&root), tree(&oracle), "restore matches the independent oracle");
        // The deps dir is not the overlay's: the restore left it alone.
        assert_eq!(fs::read(root.join("node_modules/x/extra.js")).unwrap(), b"ignored\n");
    });
    drop(d);
    core.close().unwrap();
}

/// A cold overlay for the cli's direct read (no daemon, no tracker).
struct Cold<'a> {
    store: &'a Store,
    config: &'a strata_types::Config,
    gits: &'a BTreeMap<RepoName, Git>,
}

impl OverlaySource for Cold<'_> {
    fn last(&self, session: &SessionId, repo: &RepoName) -> Option<Overlay> {
        let rec = self.store.session(session).ok()??;
        let w = rec.manifest.repos.get(repo)?;
        let m = self.store.manifest(repo, &w.base.tree).ok()??;
        let base = Base { dir: self.store.base_dir(&w.base), r#gen: w.base.clone(), manifest: std::sync::Arc::new(m) };
        let wref = WorkspaceRef { session, repo, root: &w.path, config: &self.config.repos[repo], git: &self.gits[repo], store: self.store };
        strata_overlay::reconcile_full(&wref, &base).ok()
    }

    fn needs_full(&self, _: &SessionId, _: &RepoName) -> bool {
        true
    }
}

#[test]
fn cli_readonly_commands_work_without_daemon() {
    let fx = Dfx::new();
    let sock = fx.socket();
    let id = {
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let id = serving(&d, || {
            let id = new_session(&sock);
            external(&ws(&core, &id, "tool"), EDIT);
            id
        });
        drop(d);
        core.close().unwrap();
        id
    };
    assert!(!sock.exists(), "the stopped daemon unlinked its socket");

    // Write methods go through the daemon: none listening → Busy (cli.md D2).
    assert!(client::needs_daemon(Method::Publish) && client::needs_daemon(Method::Push));
    for (m, p) in [("publish", json!({ "group": { "id": 0, "batches": [] } })), ("push", json!({ "repo": "tool" }))] {
        let e = client::call(&sock, m, p).unwrap_err();
        assert_eq!(e.code, Code::Busy, "{m}: {e:?}");
        assert_eq!(reason(&e), Some("daemon_down"), "{m}");
    }

    // Read-only methods: the cli reads directly.
    for m in [Method::Status, Method::Grep, Method::Files] {
        assert!(!client::needs_daemon(m), "{m:?}");
    }
    let store = Store::open_read_only(&fx.config.paths.root).unwrap();
    let env = fx.sb.git_env();
    let gits: BTreeMap<RepoName, Git> =
        fx.config.repos.iter().map(|(n, c)| (n.clone(), Git::open_with_env(&c.path, &env).unwrap())).collect();
    let locks = Locks::new(fx.config.repos.keys().cloned(), Duration::from_millis(100));
    let ienv = fx.sb.install_env();
    let sessions = Sessions::open(&fx.config, &gits, &store, &locks, &ienv, &DaemonBefore).unwrap();

    let st = sessions.status(&id).unwrap();
    assert_eq!(st.state, SessionState::Active);
    let status = json!({ "sessions": [serde_json::to_value(&st).unwrap()] });
    assert_eq!(status["sessions"][0]["manifest"]["id"], json!(id));

    let cold = Cold { store: &store, config: &fx.config, gits: &gits };
    let search = Search::open(&fx.config, &sessions, &cold);
    let view = search.attach(&id, &repo_name("tool")).unwrap();
    let hits = search.grep(Which::One(view.clone()), "needle", &GrepOpts { case_insensitive: false }).unwrap();
    let grep = json!({ "hits": hits.iter().map(|h| json!({ "path": h.path.as_str(), "line": h.line })).collect::<Vec<_>>() });
    assert_eq!(grep["hits"], json!([{ "path": "docs/deep/x.md", "line": 2 }]));
    let files = search.files(view.clone(), "src/*.ts").unwrap();
    let files = json!(files.iter().map(|p| p.as_str()).collect::<Vec<_>>());
    assert_eq!(files, json!(["src/index.ts", "src/new.ts"]));
    search.detach(view).unwrap();
    drop(search);
    assert!(!sock.exists(), "nothing started a daemon");
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn lost_rpc_reply_is_not_replayed() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        external(&ws(&core, &id, "tool"), "printf 'one\\n' > src/one.ts");
        let count = || core.store.checkpoints(&id, None).unwrap().len();
        assert_eq!(count(), 0);

        // The client sends a write and goes away before the reply.
        {
            let mut s = UnixStream::connect(&sock).unwrap();
            let line = json!({ "id": 1, "method": "checkpoint", "params": {
                "session": id, "repo": "tool",
                "reason": reason_json(CheckpointReason::Manual { message: "lost".into() }),
                "paths": [],
            }})
            .to_string();
            s.write_all(format!("{line}\n").as_bytes()).unwrap();
            s.shutdown(std::net::Shutdown::Both).unwrap();
        }
        wait_for("the checkpoint to land", || count() == 1);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(count(), 1, "the write ran once");

        // Another client only sees the current state; nothing is replayed.
        let st = ok(call(&sock, "status", json!({ "cwd": null, "session": id.as_str(), "all": false })));
        let cps = st["sessions"][0]["checkpoints"].as_array().unwrap();
        assert_eq!(cps.len(), 1, "{st:#}");
        assert_eq!(cps[0]["reason"], reason_json(CheckpointReason::Manual { message: "lost".into() }));
        assert!(st["sessions"][0]["in_flight"].is_null());
        assert_eq!(st["sessions"][0]["queued"], 0);
        let dst = ok(call(&sock, "daemon_status", json!({})));
        assert_eq!(dst["holds"], json!([]));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(count(), 1, "still once after other clients came and went");
    });
    drop(d);
    core.close().unwrap();
}

fn push_calls(core: &Core) -> usize {
    core.gits.values().flat_map(|g| g.take_git_calls()).filter(|c| c.args.iter().any(|a| a == "push" || a == "send-pack")).count()
}

fn unknown_ids(v: &Value) -> Vec<u64> {
    v["warnings"].as_array().unwrap().iter().filter_map(|w| w.get("UnknownPublish").map(|u| u["id"].as_u64().unwrap())).collect()
}

fn origin_dir(fx: &Dfx) -> PathBuf {
    fx.sb.path("tool-origin.git")
}

fn origin_main(fx: &Dfx) -> String {
    fx.sb.git(&origin_dir(fx), &["rev-parse", "refs/heads/main"])
}

#[test]
fn failure_keeps_scene_no_replay() {
    let fx = Dfx::new();
    let hook = origin_dir(&fx).join("hooks/pre-receive");
    // Review P0-2: a definite refusal now closes its row; an unknown
    // outcome comes from a push that outlasts its timeout.
    fs::write(&hook, "#!/bin/sh\nsleep 2\nexit 1\n").unwrap();
    let t0 = std::time::Instant::now();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let origin = origin_main(&fx);
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    let (id, commit, unknown) = {
        let core = fx.prepare().unwrap();
        strata_publish::seams::set_push_timeout(&core.gits[&repo_name("tool")], Some(Duration::from_millis(300)));
        let d = Daemon::start(&core).unwrap();
        let r = serving(&d, || {
            let id = new_session(&sock);
            let root = ws(&core, &id, "tool");
            external(&root, "printf 'export const a = 4\\n' > src/index.ts");
            let cp = ok(call(&sock, "checkpoint", json!({
                "session": id, "repo": "tool",
                "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
            })));
            let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
            let g = group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]);
            push_calls(&core);

            let r = call(&sock, "publish", json!({ "group": g }));
            assert_eq!(r["ok"], json!(false), "{r:#}");
            assert_eq!(r["error"]["code"], "Unknown", "{r:#}");
            assert_eq!(r["error"]["detail"]["kind"], "Unknown", "{r:#}");
            assert_eq!(push_calls(&core), 1, "pushed once");

            // The scene stays: the chain commit, the row, the workspace.
            let commit = core.gits[&repo_name("tool")].read_ref(&chain).unwrap().expect("chain commit");
            assert_eq!(origin_main(&fx), origin);
            let rows = core.store.open_publishes().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].phase, PublishPhase::Unknown);
            let unknown = rows[0].group.id;
            assert_eq!(fs::read(root.join("src/index.ts")).unwrap(), b"export const a = 4\n");
            let dst = ok(call(&sock, "daemon_status", json!({})));
            assert_eq!(unknown_ids(&dst), vec![unknown], "{dst:#}");

            // Other requests do not replay it; the same publish is refused.
            ok(call(&sock, "status", json!({ "cwd": null, "session": null, "all": true })));
            let again = call(&sock, "publish", json!({ "group": g }));
            assert_eq!(again["error"]["code"], "Busy", "{again:#}");
            assert_eq!(again["error"]["detail"]["reason"], "publish_unknown");
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(push_calls(&core), 0, "nothing pushed again");
            (id, commit, unknown)
        });
        drop(d);
        core.close().unwrap();
        r
    };

    // A restart reports it again and still does not push.
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    assert_eq!(unknown_ids(&d.daemon_status(false).unwrap()), vec![unknown]);
    assert_eq!(push_calls(&core), 0);
    assert_eq!(core.gits[&repo_name("tool")].read_ref(&chain).unwrap(), Some(commit));
    assert_eq!(origin_main(&fx), origin);
    assert_eq!(core.store.session(&id).unwrap().unwrap().manifest.id, id);
    drop(d);
    core.close().unwrap();
    strata_publish::seams::set_push_timeout(&strata_gitio::Git::open(&fx.main("tool")).unwrap(), None);
    std::thread::sleep(Duration::from_secs(3).saturating_sub(t0.elapsed()));
}

/// Review H2: a confirmed publish is bound to the chain top the cli showed
/// (`shown`). A stale plan (it showed no chain, a publish has landed since)
/// is `NotConfirmed` (`chain_moved`) before any staging: no new commit, no
/// push. The current plan publishes.
#[test]
fn confirmed_publish_is_bound_to_the_shown_chain_top() {
    use strata_confirm::seams::Verdict;
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.confirm = true;
    fx.write_config();
    let (c, log, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass(), Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        let publish = |text: &str, shown: Value| {
            external(&root, &format!("printf '{text}\\n' > src/index.ts"));
            let cp = ok(call(&sock, "checkpoint", json!({
                "session": id, "repo": "tool",
                "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
            })));
            let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
            let g = group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]);
            call(&sock, "publish", json!({ "group": g, "shown": { "tool": shown } }))
        };
        let git = &core.gits[&repo_name("tool")];
        assert_eq!(git.read_ref(&chain).unwrap(), None);
        let first = ok(publish("export const a = 1", Value::Null));
        let x = git.read_ref(&chain).unwrap().expect("chain commit");
        assert_eq!(first["published"][0]["commit"], json!(x));
        assert_eq!(origin_main(&fx), x.as_str());
        push_calls(&core);

        // The same stale plan again: refused under the lock.
        let r = publish("export const a = 2", Value::Null);
        assert_eq!(r["error"]["code"], "NotConfirmed", "{r:#}");
        assert_eq!(r["error"]["detail"]["reason"], "chain_moved", "{r:#}");
        assert_eq!(r["error"]["detail"]["now"], json!(x), "{r:#}");
        assert_eq!(git.read_ref(&chain).unwrap(), Some(x.clone()), "no new commit");
        assert_eq!(push_calls(&core), 0, "nothing pushed");

        // The plan as it is now: published.
        let r = ok(publish("export const a = 3", json!(x)));
        let y = git.read_ref(&chain).unwrap().expect("chain commit");
        assert_ne!(y, x);
        assert_eq!(r["published"][0]["commit"], json!(y));
        assert_eq!(origin_main(&fx), y.as_str());
        assert_eq!(log.shown().len(), 3);
    });
    drop(d);
    core.close().unwrap();
}

/// Review-3 T1: `plan` (the guarded repos the cli's TTY gate covered).
/// A guarded repo the publish would commit but that is not in it is
/// `NotConfirmed` `plan_changed` before any prompt: no row, no commit, no
/// push. With the repo planned, the same publish prompts and lands.
#[test]
fn publish_refuses_a_guarded_repo_outside_the_plan_before_prompting() {
    use strata_confirm::seams::Verdict;
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.confirm = true;
    fx.write_config();
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        external(&root, "printf 'export const a = 7\\n' > src/index.ts");
        let cp = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
        })));
        let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
        let g = group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]);
        let git = &core.gits[&repo_name("tool")];
        let origin = origin_main(&fx);

        let r = call(&sock, "publish", json!({ "group": g, "shown": {}, "plan": [] }));
        assert_eq!(r["error"]["code"], "NotConfirmed", "{r:#}");
        assert_eq!(r["error"]["detail"]["reason"], "plan_changed", "{r:#}");
        assert_eq!(r["error"]["detail"]["repo"], "tool", "{r:#}");
        assert_eq!(log.shown().len(), 0, "prompted outside the plan");
        assert!(core.store.publishes().unwrap().is_empty(), "a publish row was written");
        assert_eq!(git.read_ref(&chain).unwrap(), None, "a commit was written");
        assert_eq!(origin_main(&fx), origin);
        assert_eq!(push_calls(&core), 0);

        let r = ok(call(&sock, "publish", json!({ "group": g, "shown": { "tool": null }, "plan": ["tool"] })));
        let x = git.read_ref(&chain).unwrap().expect("chain commit");
        assert_eq!(r["published"][0]["commit"], json!(x));
        assert_eq!(log.shown().len(), 1);
    });
    drop(d);
    core.close().unwrap();
}

/// Review H4 (kept by design, the agent loop made to work): a two-repo
/// publish where the second repo conflicts with upstream. The `Conflict`
/// says which repos already landed in this call (`published_before`) and
/// which repo stopped, with its paths. The agent fixes its file,
/// checkpoints, and re-runs the very same publish: the repo that landed is
/// reported as already published (no second commit, no second push), the
/// fixed repo publishes.
#[test]
fn publish_conflict_fix_rerun_succeeds_without_duplicates() {
    let fx = Dfx::with(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    let origin = |r: &str| fx.sb.git(&fx.sb.path(&format!("{r}-origin.git")), &["rev-parse", "refs/heads/main"]);
    serving(&d, || {
        let s = ok(call(&sock, "new", json!({ "ticket": null, "repos": ["front", "tool"] })));
        let id: SessionId = serde_json::from_value(s["session"]["manifest"]["id"].clone()).unwrap();
        external(&ws(&core, &id, "front"), "printf 'export const f = 1\\n' > src/index.ts");
        external(&ws(&core, &id, "tool"), "printf 'export const t = 1\\n' > src/index.ts && printf 'export const o = 1\\n' > src/other.ts");
        // Upstream edits the same line of tool's file.
        let main = fx.main("tool");
        fx.sb.commit(&main, &[("src/index.ts", b"export const t = 9\n")], "upstream");
        fx.sb.git(&main, &["push", "-q", "origin", "main"]);
        let upstream = origin("tool");

        // What the cli does: a BeforePublish checkpoint per (session, repo), then publish.
        let run = || {
            let mut batches = Vec::new();
            for (repo, paths) in [("front", vec!["src/index.ts"]), ("tool", vec!["src/index.ts", "src/other.ts"])] {
                let cp = ok(call(&sock, "checkpoint", json!({
                    "session": id, "repo": repo, "reason": { "kind": "BeforePublish" }, "paths": paths,
                })));
                batches.push(json!({ "session": id, "repo": repo, "checkpoint": cp["checkpoint"]["id"], "paths": paths }));
            }
            call(&sock, "publish", json!({ "group": { "id": 0, "batches": batches } }))
        };
        let r = run();
        assert_eq!(r["error"]["code"], "Conflict", "{r:#}");
        let det = &r["error"]["detail"];
        assert_eq!(det["repo"], "tool", "{r:#}");
        assert_eq!(det["published_before"][0]["repo"], "front", "{r:#}");
        let landed = core.gits[&repo_name("front")].read_ref(&chain).unwrap().expect("front committed");
        assert_eq!(det["published_before"][0]["commit"], json!(landed), "{r:#}");
        assert_eq!(origin("front"), landed.as_str(), "front pushed");
        let paths: Vec<&str> = det["conflicts"].as_array().unwrap().iter().filter_map(|c| c["path"].as_str()).collect();
        assert_eq!(paths, vec!["src/index.ts"], "{r:#}");
        assert_eq!(origin("tool"), upstream, "tool not pushed");

        // The agent takes upstream's line and re-runs the same publish.
        external(&ws(&core, &id, "tool"), "printf 'export const t = 9\\n' > src/index.ts");
        push_calls(&core);
        let r = ok(run());
        assert_eq!(core.gits[&repo_name("front")].read_ref(&chain).unwrap(), Some(landed.clone()), "no second front commit");
        assert_eq!(origin("front"), landed.as_str());
        assert_eq!(r["already_published"][0]["repo"], "front", "{r:#}");
        assert_eq!(r["published"].as_array().unwrap().len(), 1, "{r:#}");
        assert_eq!(r["published"][0]["repo"], "tool", "{r:#}");
        let t = core.gits[&repo_name("tool")].read_ref(&chain).unwrap().expect("tool committed");
        assert_eq!(origin("tool"), t.as_str());
        assert_eq!(fx.sb.git(&main, &["rev-parse", &format!("{t}^")]), upstream, "one commit on upstream");
        assert_eq!(fx.sb.git(&main, &["show", &format!("{t}:src/other.ts")]), "export const o = 1");
        assert_eq!(push_calls(&core), 1, "only tool pushed");
    });
    drop(d);
    core.close().unwrap();
}

/// Review-2 N6: front lands, then tool's confirmation is cancelled (an
/// error before tool's publish is even called): the error still reports
/// front in `published_before`. The re-run with a passed confirmation
/// reports front as already published and publishes tool. Review-4 F2: a
/// re-run whose plan left tool out is refused `plan_changed` before any
/// prompt, and that error too reports front in `already_published`.
#[test]
fn publish_confirmation_cancelled_after_a_repo_landed_keeps_it_in_the_envelope() {
    use strata_confirm::seams::Verdict;
    let mut fx = Dfx::with(&["front", "tool"]);
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.confirm = true;
    fx.write_config();
    let (c, log, _) = fx.confirmer(vec![Verdict::la(-2), Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    serving(&d, || {
        let s = ok(call(&sock, "new", json!({ "ticket": null, "repos": ["front", "tool"] })));
        let id: SessionId = serde_json::from_value(s["session"]["manifest"]["id"].clone()).unwrap();
        external(&ws(&core, &id, "front"), "printf 'export const f = 1\\n' > src/index.ts");
        external(&ws(&core, &id, "tool"), "printf 'export const t = 1\\n' > src/index.ts");
        let run_plan = |shown: Value, plan: Option<Value>| {
            let mut batches = Vec::new();
            for repo in ["front", "tool"] {
                let cp = ok(call(&sock, "checkpoint", json!({
                    "session": id, "repo": repo, "reason": { "kind": "BeforePublish" }, "paths": ["src/index.ts"],
                })));
                batches.push(json!({ "session": id, "repo": repo, "checkpoint": cp["checkpoint"]["id"], "paths": ["src/index.ts"] }));
            }
            let mut req = json!({ "group": { "id": 0, "batches": batches }, "shown": { "tool": shown } });
            if let Some(p) = plan {
                req["plan"] = p;
            }
            call(&sock, "publish", req)
        };
        let run = |shown: Value| run_plan(shown, None);
        let r = run(Value::Null);
        assert!(r["error"].is_object(), "{r:#}");
        let det = &r["error"]["detail"];
        let landed = core.gits[&repo_name("front")].read_ref(&chain).unwrap().expect("front committed");
        assert_eq!(det["repo"], "tool", "{r:#}");
        assert_eq!(det["published_before"][0]["repo"], "front", "{r:#}");
        assert_eq!(det["published_before"][0]["commit"], json!(landed), "{r:#}");
        assert_eq!(core.gits[&repo_name("tool")].read_ref(&chain).unwrap(), None, "tool not committed");

        // Review-4 F2: the plan left tool out → plan_changed, front still reported.
        let shown_before = log.shown().len();
        let r = run_plan(Value::Null, Some(json!([])));
        assert_eq!(r["error"]["code"], "NotConfirmed", "{r:#}");
        let det = &r["error"]["detail"];
        assert_eq!(det["reason"], "plan_changed", "{r:#}");
        assert_eq!(det["repo"], "tool", "{r:#}");
        assert_eq!(det["already_published"][0]["repo"], "front", "{r:#}");
        assert_eq!(det["already_published"][0]["commit"], json!(landed), "{r:#}");
        assert!(det.get("published_before").is_none(), "nothing landed in this call: {r:#}");
        assert_eq!(log.shown().len(), shown_before, "no prompt");
        assert_eq!(core.gits[&repo_name("tool")].read_ref(&chain).unwrap(), None, "tool not committed");

        let r = ok(run(Value::Null));
        assert_eq!(r["already_published"][0]["repo"], "front", "{r:#}");
        assert_eq!(r["already_published"][0]["commit"], json!(landed), "{r:#}");
        assert_eq!(r["published"][0]["repo"], "tool", "{r:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// Review-2 N2: `publish_plan` names the repos a publish of the group would
/// still commit and those that already landed, and writes nothing, so the
/// cli gates TTY / confirmation on the remaining repos only.
#[test]
fn publish_plan_names_the_remaining_repos_and_writes_nothing() {
    use strata_confirm::seams::Verdict;
    let mut fx = Dfx::with(&["front", "tool"]);
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.confirm = true;
    fx.write_config();
    let (c, log, _) = fx.confirmer(vec![Verdict::la(-2)]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    serving(&d, || {
        let s = ok(call(&sock, "new", json!({ "ticket": null, "repos": ["front", "tool"] })));
        let id: SessionId = serde_json::from_value(s["session"]["manifest"]["id"].clone()).unwrap();
        external(&ws(&core, &id, "front"), "printf 'export const f = 1\\n' > src/index.ts");
        external(&ws(&core, &id, "tool"), "printf 'export const t = 1\\n' > src/index.ts");
        let group = || {
            let mut batches = Vec::new();
            for repo in ["front", "tool"] {
                let cp = ok(call(&sock, "checkpoint", json!({
                    "session": id, "repo": repo, "reason": { "kind": "BeforePublish" }, "paths": ["src/index.ts"],
                })));
                batches.push(json!({ "session": id, "repo": repo, "checkpoint": cp["checkpoint"]["id"], "paths": ["src/index.ts"] }));
            }
            json!({ "id": 0, "batches": batches })
        };
        let plan = ok(call(&sock, "publish_plan", json!({ "group": group() })));
        assert_eq!(plan, json!({ "repos": ["front", "tool"], "already_published": [] }));
        assert!(core.store.publishes().unwrap().is_empty(), "the plan wrote a publish row");

        // front lands, tool's confirmation is cancelled.
        let r = call(&sock, "publish", json!({ "group": group(), "shown": { "tool": null } }));
        assert_eq!(r["error"]["detail"]["repo"], "tool", "{r:#}");
        let landed = core.gits[&repo_name("front")].read_ref(&chain).unwrap().expect("front committed");
        let rows = core.store.publishes().unwrap().len();
        let prompts = log.shown().len();

        let plan = ok(call(&sock, "publish_plan", json!({ "group": group() })));
        assert_eq!(plan["repos"], json!(["tool"]), "{plan:#}");
        assert_eq!(plan["already_published"][0]["repo"], "front", "{plan:#}");
        assert_eq!(plan["already_published"][0]["commit"], json!(landed), "{plan:#}");
        assert_eq!(core.store.publishes().unwrap().len(), rows, "the plan wrote a publish row");
        assert_eq!(log.shown().len(), prompts, "the plan prompted");
        assert_eq!(core.gits[&repo_name("tool")].read_ref(&chain).unwrap(), None, "tool committed");
        assert_eq!(core.gits[&repo_name("front")].read_ref(&chain).unwrap(), Some(landed));
        let e = call(&sock, "publish_plan", json!({ "group": { "id": 0, "batches": [] } }));
        assert_eq!(e["error"]["code"], "InvalidInput", "{e:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// Review-3 T3: the documented limit of `publish_plan` (daemon.md §3,
/// skills reference). A guarded repo whose session stayed on its old base
/// after its content landed (the publish's rebase is `Waiting` on a deps
/// change) is still in `repos`: the checkpoint still selects the landed
/// content, so the cli would still need a TTY and a confirmation for it,
/// although staging would find nothing new.
#[test]
fn publish_plan_keeps_a_waiting_sessions_landed_guarded_repo() {
    use strata_confirm::seams::Verdict;
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.confirm = true;
    fx.write_config();
    let (c, _log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        external(&ws(&core, &id, "tool"), "sed -i '' 's/1.0.87/1.0.88/; s/\\^1.0.0/^1.0.1/' package.json");
        let group = || {
            let cp = ok(call(&sock, "checkpoint", json!({
                "session": id, "repo": "tool", "reason": { "kind": "BeforePublish" }, "paths": ["package.json"],
            })));
            json!({ "id": 0, "batches": [
                { "session": id, "repo": "tool", "checkpoint": cp["checkpoint"]["id"], "paths": ["package.json"] },
            ] })
        };
        let r = ok(call(&sock, "publish", json!({ "group": group(), "shown": { "tool": null } })));
        assert_eq!(r["published"][0]["rebases"][0]["report"]["kind"], "Waiting", "{r:#}");
        let plan = ok(call(&sock, "publish_plan", json!({ "group": group() })));
        assert_eq!(plan, json!({ "repos": ["tool"], "already_published": [] }), "{plan:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// Review-2 N1: re-run filtering is per selector and by content. Each
/// call is what the cli does (a BeforePublish checkpoint, then publish).
fn publish_selectors(sock: &Path, id: &SessionId, repo: &str, paths: &[&str]) -> Value {
    let cp = ok(call(sock, "checkpoint", json!({
        "session": id, "repo": repo, "reason": { "kind": "BeforePublish" }, "paths": paths,
    })));
    call(sock, "publish", json!({ "group": { "id": 0, "batches": [
        { "session": id, "repo": repo, "checkpoint": cp["checkpoint"]["id"], "paths": paths },
    ] } }))
}

/// Review-2 N3: a failure after a definite push (the post-publish rebase,
/// or closing the row) still reports the landed repo with its push outcome
/// `Pushed` and the checks it ran: the envelope never loses a landed result.
#[test]
fn publish_failure_after_a_definite_push_reports_the_pushed_commit() {
    use strata_publish::seams::{FailPoint, fail_once};
    // A fresh daemon per point: an `Unknown` row blocks the next publish.
    for (n, at) in [(1, FailPoint::BeforeRebase), (2, FailPoint::BeforeDone)] {
        let fx = Dfx::new();
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        let git = core.gits[&repo_name("tool")].clone();
        serving(&d, || {
            let id = new_session(&sock);
            external(&ws(&core, &id, "tool"), &format!("printf 'export const a = {n}\\n' > src/index.ts"));
            fail_once(&git, at);
            let r = publish_selectors(&sock, &id, "tool", &["src/index.ts"]);
            assert_eq!(r["error"]["code"], "Unknown", "{at:?}: {r:#}");
            let l = &r["error"]["detail"]["landed"];
            let tip = git.read_ref(&strata_publish::chain_ref("main")).unwrap().unwrap();
            assert_eq!(l["commit"], json!(tip), "{at:?}: {r:#}");
            assert_eq!(l["push"]["kind"], "Pushed", "{at:?}: {r:#}");
            assert!(l["checks_ran"].is_u64(), "{at:?}: {r:#}");
        });
    }
}

/// Review-2 N1: a directory selector that landed, then new edits under it,
/// then the very same publish again: the edits are committed (the old
/// filter saw no entry literally named `src` and skipped the repo). A third
/// identical run with nothing new is `already_published`, no commit.
#[test]
fn publish_rerun_of_a_directory_selector_commits_new_edits() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    let git = &core.gits[&repo_name("tool")];
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        external(&root, "printf 'export const a = 1\\n' > src/index.ts");
        let r = ok(publish_selectors(&sock, &id, "tool", &["src"]));
        assert_eq!(r["published"].as_array().unwrap().len(), 1, "{r:#}");
        let first = git.read_ref(&chain).unwrap().expect("first commit");

        external(&root, "printf 'export const a = 2\\n' > src/index.ts && printf 'export const n = 1\\n' > src/new.ts");
        let r = ok(publish_selectors(&sock, &id, "tool", &["src"]));
        assert_eq!(r["published"].as_array().unwrap().len(), 1, "the new edits publish: {r:#}");
        assert_eq!(r["already_published"], json!([]), "{r:#}");
        let second = git.read_ref(&chain).unwrap().expect("second commit");
        assert_ne!(second, first, "a second commit");
        let tree = git.tree_of(&second).unwrap();
        let show = |p: &str| git.blob_bytes(&tree, &RepoPath::try_from(p.to_string()).unwrap()).unwrap().map(|b| b.bytes);
        assert_eq!(show("src/index.ts").as_deref(), Some(&b"export const a = 2\n"[..]));
        assert_eq!(show("src/new.ts").as_deref(), Some(&b"export const n = 1\n"[..]));

        let r = ok(publish_selectors(&sock, &id, "tool", &["src"]));
        assert_eq!(r["published"], json!([]), "{r:#}");
        assert_eq!(r["already_published"][0]["commit"], json!(second), "{r:#}");
        assert_eq!(git.read_ref(&chain).unwrap(), Some(second), "no third commit");
    });
    drop(d);
    core.close().unwrap();
}

/// Review P1-8: the newest `Done` row of a selector names a commit that is
/// no longer in the repository (replayed off the chain, then pruned by git
/// gc). Re-publishing that unchanged selector must not fail with
/// `NoSuchCommit`; the selector stays and stage answers as usual.
#[test]
fn publish_rerun_tolerates_a_pruned_landed_commit() {
    use strata_store::PublishPhase;
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        external(&root, "printf 'export const a = 7\\n' > src/index.ts");
        let r = ok(publish_selectors(&sock, &id, "tool", &["src/index.ts"]));
        assert_eq!(r["published"].as_array().unwrap().len(), 1, "{r:#}");
        let done = core.store.publishes().unwrap().into_iter().rev().find(|p| p.phase == PublishPhase::Done).expect("a done row");
        let mut group = done.group.clone();
        group.id = 0;
        let fake = core.store.record_publish(&group, std::time::SystemTime::now()).unwrap();
        let gone = json!({ "commit": "0123456789abcdef0123456789abcdef01234567", "paths": ["src/index.ts"], "push": null });
        assert!(core.store.set_publish_phase(fake, PublishPhase::Running, PublishPhase::Done, Some(&gone), std::time::SystemTime::now()).unwrap());

        let r = publish_selectors(&sock, &id, "tool", &["src/index.ts"]);
        assert_ne!(r["error"]["code"], "NoSuchCommit", "{r:#}");
        let plan = call(&sock, "publish_plan", json!({ "group": { "id": 0, "batches": group.batches } }));
        assert_ne!(plan["error"]["code"], "NoSuchCommit", "{plan:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// Review-2 N1: a landed batch {index, other}; only index is edited; the
/// same publish again commits index and does not stop at `NoMatch` for the
/// unchanged other (its content equals what landed).
#[test]
fn publish_rerun_of_a_partly_changed_batch_commits_the_changed_path() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    let git = &core.gits[&repo_name("tool")];
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        external(&root, "printf 'export const a = 1\\n' > src/index.ts && printf 'export const o = 1\\n' > src/other.ts");
        let sel = ["src/index.ts", "src/other.ts"];
        ok(publish_selectors(&sock, &id, "tool", &sel));
        let first = git.read_ref(&chain).unwrap().expect("first commit");

        external(&root, "printf 'export const a = 2\\n' > src/index.ts");
        let r = publish_selectors(&sock, &id, "tool", &sel);
        let r = ok(r);
        assert_eq!(r["published"].as_array().unwrap().len(), 1, "{r:#}");
        let second = git.read_ref(&chain).unwrap().expect("second commit");
        assert_ne!(second, first);
        let (t1, t2) = (git.tree_of(&first).unwrap(), git.tree_of(&second).unwrap());
        let at = |t: &strata_types::TreeOid, p: &str| git.blob_bytes(t, &RepoPath::try_from(p.to_string()).unwrap()).unwrap().map(|b| b.bytes);
        assert_eq!(at(&t2, "src/index.ts").as_deref(), Some(&b"export const a = 2\n"[..]));
        assert_eq!(at(&t2, "src/other.ts"), at(&t1, "src/other.ts"), "other unchanged");
    });
    drop(d);
    core.close().unwrap();
}

/// Review M1 (§5.9, I12): the running daemon reconciles on its own. With a
/// short interval: a pass scans the tracked workspace of a session that
/// never checkpointed (step 13 watchers, then a full walk), finds a file
/// no request told it about, writes no checkpoint row; a session whose
/// queue is held is skipped and listed, not waited for; gc runs under the
/// repo lock; and stopping does not wait for the next interval.
#[test]
fn reconcile_loop_rescans_skips_busy_and_stops() {
    let mut fx = Dfx::new();
    fx.reconcile_every = Duration::from_millis(150);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let tool = RepoName::try_from("tool".to_owned()).unwrap();
    let mut stop_took = Duration::ZERO;
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(&d, &listener, &ext));
        let status = || ok(call(&sock, "daemon_status", json!({})))["reconcile"].clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let id = new_session(&sock);
            let sid = json!(id.as_str());
            let cps = core.store.checkpoints(&id, Some(&tool)).unwrap().len();
            fs::write(ws(&core, &id, "tool").join("src/missed.ts"), "missed\n").unwrap();
            wait_for("a pass that finds src/missed.ts", || {
                status()["last"]["scanned"].as_array().into_iter().flatten().any(|e| {
                    e["session"] == sid && e["changed"].as_array().is_some_and(|c| c.contains(&json!("src/missed.ts")))
                })
            });
            assert_eq!(core.store.checkpoints(&id, Some(&tool)).unwrap().len(), cps, "a pass writes no checkpoint");
            let gc = status()["last"]["gc"].clone();
            assert!(gc.as_array().unwrap().iter().all(|g| g["ok"] == json!(true)), "{gc:#}");

            core.locks
                .with_sessions(std::slice::from_ref(&id), || {
                    let n = status()["runs"].as_u64().unwrap();
                    wait_for("a pass that skips the busy session", || {
                        let st = status();
                        st["runs"].as_u64().unwrap() > n
                            && st["last"]["skipped"].as_array().unwrap().iter().any(|e| e["session"] == sid)
                            && !st["last"]["scanned"].as_array().unwrap().iter().any(|e| e["session"] == sid)
                    });
                    Ok(())
                })
                .unwrap();
        }));
        let t0 = Instant::now();
        d.request_stop();
        h.join().unwrap().unwrap();
        stop_took = t0.elapsed();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    });
    assert!(stop_took < Duration::from_secs(5), "stop waited {stop_took:?}");
    drop(d);
    core.close().unwrap();
}

/// Review M2: grep / files see the current workspace, not the overlay the
/// tracker cached at the last checkpoint. After a checkpoint: a new file,
/// a deleted file, a rename and an ignore-rule change, none checkpointed.
#[test]
fn search_after_checkpoint_sees_uncheckpointed_changes() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        let cp = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::ToolCall { id: "c1".into() }), "paths": [],
        })));
        assert_eq!(cp["recorded"], json!(true), "{cp:#}");
        let files = |glob: &str| ok(call(&sock, "files", json!({ "session": id, "repo": "tool", "glob": glob })))["paths"].clone();
        let grep = |pat: &str| {
            let g = ok(call(&sock, "grep", json!({ "pattern": pat, "session": id, "all": false, "ignore_case": false })));
            g["hits"].as_array().unwrap().iter().map(|h| h["path"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        };
        let before = files("src/*.ts");
        assert_eq!(before, json!(["src/index.ts"]), "{before}");

        fs::write(root.join("src/fresh.ts"), "uniqueneedle\n").unwrap();
        assert_eq!(grep("uniqueneedle"), vec!["src/fresh.ts".to_string()], "a new file");
        assert_eq!(files("src/*.ts"), json!(["src/fresh.ts", "src/index.ts"]));

        fs::rename(root.join("src/fresh.ts"), root.join("src/moved.ts")).unwrap();
        assert_eq!(grep("uniqueneedle"), vec!["src/moved.ts".to_string()], "a rename");

        fs::remove_file(root.join("src/index.ts")).unwrap();
        assert_eq!(files("src/*.ts"), json!(["src/moved.ts"]), "a deletion");

        fs::write(root.join(".gitignore"), "src/moved.ts\n").unwrap();
        assert_eq!(grep("uniqueneedle"), Vec::<String>::new(), "an ignore rule");

        // Search refreshed the Tracker; the next checkpoint still records
        // what changed since the last checkpoint row.
        let cp2 = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::ToolCall { id: "c2".into() }), "paths": [],
        })));
        assert_eq!(cp2["recorded"], json!(true), "{cp2:#}");
        let paths: Vec<&str> = cp2["checkpoint"]["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
        assert!(paths.contains(&"src/index.ts") && paths.contains(&".gitignore"), "{paths:?}");
    });
    drop(d);
    core.close().unwrap();
}

/// Review M5: `publishing` is not a lock (daemon.md §3, D14): publish
/// releases the session queue and the repo lock after its commit/push and
/// takes them again per rebase step. An `rm` — plain or `--discard` — that
/// arrives in that gap waits for the flag, holding nothing, instead of
/// removing the session under the publish; it then runs, and the publish's
/// rebase of that session is intact.
#[test]
fn rm_waits_for_publishing_in_the_lock_free_gap() {
    use strata_confirm::seams::Verdict;
    use strata_publish::seams::{PublishFault, pause};
    let mut fx = Dfx::new();
    fx.lock_wait = Duration::from_secs(20);
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let git = &core.gits[&repo_name("tool")];
    serving(&d, || {
        for (k, discard) in [(2, false), (3, true)] {
            let id = new_session(&sock);
            let root = ws(&core, &id, "tool");
            external(&root, &format!("printf 'export const a = {k}\\n' > src/index.ts"));
            let cp = ok(call(&sock, "checkpoint", json!({
                "session": id, "repo": "tool",
                "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
            })));
            let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
            let g = group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]);
            let gap = pause(git, PublishFault::PauseBeforeRebase);
            std::thread::scope(|sc| {
                let publish = sc.spawn(|| call(&sock, "publish", json!({ "group": g })));
                gap.wait_until_paused();
                assert_eq!(core.locks.in_flight(&id), None, "the gap holds no queue place");
                let rm = sc.spawn(|| call(&sock, "rm", json!({ "session": id, "discard": discard })));
                std::thread::sleep(Duration::from_millis(400));
                assert!(!rm.is_finished(), "rm waits for publishing: {:#}", if rm.is_finished() { rm.join().unwrap() } else { Value::Null });
                gap.release();
                let p = ok(publish.join().unwrap());
                let mine = p["published"][0]["rebases"].as_array().unwrap().iter().find(|r| r["session"] == json!(id)).cloned();
                assert_eq!(mine.as_ref().map(|r| &r["report"]["kind"]), Some(&json!("Moved")), "rebase intact: {p:#}");
                ok(rm.join().unwrap());
            });
            assert!(core.store.session(&id).unwrap().is_none_or(|r| r.removed.is_some()), "rm ran after the publish");
        }
    });
    assert_eq!(log.shown().len(), 1, "the discard prompted once");
}

/// Review M6: a standalone `push` whose outcome is unknown (the origin's
/// hook outlasts the push timeout) keeps its staging scene with a
/// `push.json` record and reports `UnknownPush` — in this daemon and again
/// after a restart. A later definite push of the chain settles it.
///
/// Review-3 R4: the record names the repository identity and origin's push
/// URL. A definite push after origin's push URL was pointed at another
/// bare remote B does not settle it; a definite push to the recorded
/// destination does.
#[test]
fn standalone_push_unknown_keeps_scene_and_reports_across_restart() {
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.push = false;
    fx.write_config();
    let sock = fx.socket();
    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    let scenes = |v: &Value| -> Vec<Value> {
        v["warnings"].as_array().unwrap().iter().filter_map(|w| w.get("UnknownPush").cloned()).collect()
    };
    let t0;
    let (tip, scene) = {
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let git = core.gits[&repo_name("tool")].clone();
        let r = serving(&d, || {
            let id = new_session(&sock);
            external(&ws(&core, &id, "tool"), "printf 'export const a = 7\\n' > src/index.ts");
            let cp = ok(call(&sock, "checkpoint", json!({
                "session": id, "repo": "tool",
                "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
            })));
            let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
            ok(call(&sock, "publish", json!({ "group": group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]) })));
            fs::write(&hook, "#!/bin/sh\nsleep 2\nexit 1\n").unwrap();
            fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            strata_publish::seams::set_push_timeout(&git, Some(Duration::from_millis(300)));
            let r = call(&sock, "push", json!({ "repo": "tool" }));
            strata_publish::seams::set_push_timeout(&git, None);
            assert_eq!(r["error"]["code"], "Unknown", "{r:#}");
            let scene = PathBuf::from(r["error"]["detail"]["scene"].as_str().expect("detail.scene"));
            assert!(scene.join("push.json").is_file(), "the attempt record is kept");
            let rec: Value = serde_json::from_str(&fs::read_to_string(scene.join("push.json")).unwrap()).unwrap();
            assert_eq!(rec["repo_id"], json!(git.repo_id().get().to_string_lossy()), "{rec:#}");
            assert_eq!(rec["push_url"], json!(git.push_url("origin").unwrap()), "{rec:#}");
            let st = ok(call(&sock, "daemon_status", json!({ "verify": false })));
            let tip = git.read_ref(&strata_publish::chain_ref("main")).unwrap().unwrap();
            assert_eq!(scenes(&st), vec![json!({ "repo": "tool", "commit": tip, "scene": scene })], "{st:#}");
            (tip, scene)
        });
        t0 = Instant::now();
        drop(d);
        core.close().unwrap();
        r
    };
    // The hook's push ends; then a restart reports the kept scene again.
    std::thread::sleep(Duration::from_secs(3).saturating_sub(t0.elapsed()));
    fs::remove_file(&hook).unwrap();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    serving(&d, || {
        let st = ok(call(&sock, "daemon_status", json!({ "verify": false })));
        assert_eq!(scenes(&st), vec![json!({ "repo": "tool", "commit": tip, "scene": scene })], "{st:#}");
        // Review-3 R4: the same repo, remote and branch, another destination.
        let tool = fx.config.repos[&repo_name("tool")].path.clone();
        let b = fx.sb.path("tool-b.git");
        fx.sb.git(&fx.sb.root, &["clone", "-q", "--bare", hook.parent().unwrap().parent().unwrap().to_str().unwrap(), b.to_str().unwrap()]);
        fx.sb.git(&tool, &["config", "remote.origin.pushurl", b.to_str().unwrap()]);
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        assert_eq!(r["push"]["kind"], "Pushed", "{r:#}");
        assert_eq!(fx.sb.git(&b, &["rev-parse", "refs/heads/main"]), tip.as_str(), "pushed to B");
        let st = ok(call(&sock, "daemon_status", json!({ "verify": false })));
        assert_eq!(scenes(&st), vec![json!({ "repo": "tool", "commit": tip, "scene": scene })], "{st:#}");
        assert!(scene.exists(), "a push to another destination does not settle the scene");
        // Back to the recorded destination (B's main mirrored to origin, so
        // origin is not rewound): the next chain commit is pushed to origin
        // and settles the scene.
        fx.sb.git(&tool, &["config", "--unset", "remote.origin.pushurl"]);
        let origin = hook.parent().unwrap().parent().unwrap().to_str().unwrap().to_string();
        fx.sb.git(&b, &["push", "-q", &origin, "main"]);
        let id = new_session(&sock);
        external(&ws(&core, &id, "tool"), "printf 'export const a = 8\\n' > src/index.ts");
        let cp = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::Manual { message: "m2".into() }), "paths": [],
        })));
        let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
        ok(call(&sock, "publish", json!({ "group": group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]) })));
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        assert_eq!(r["push"]["kind"], "Pushed", "{r:#}");
        let st = ok(call(&sock, "daemon_status", json!({ "verify": false })));
        assert!(scenes(&st).is_empty(), "{st:#}");
        assert!(!scene.exists(), "a definite push of the chain settles the scene");
    });
}

/// Review P1-13 (D34): the push scene record is fsynced before anything is
/// pushed. When that fsync fails the push is refused, origin is untouched
/// and no scene is left behind.
#[test]
fn standalone_push_needs_a_durable_scene_record() {
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.push = false;
    fx.write_config();
    let sock = fx.socket();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let tool = fx.config.repos[&repo_name("tool")].path.clone();
    let a_url = fx.sb.git(&tool, &["config", "remote.origin.url"]).trim().to_string();
    let main_at = || fx.sb.git(&fx.sb.root, &["--git-dir", &a_url, "rev-parse", "refs/heads/main"]).trim().to_string();
    let before = main_at();
    let root = fx.config.paths.root.clone();
    serving(&d, || {
        let id = new_session(&sock);
        external(&ws(&core, &id, "tool"), "printf 'export const a = 11\\n' > src/index.ts");
        let cp = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
        })));
        let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
        ok(call(&sock, "publish", json!({ "group": group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]) })));
        strata_daemon::seams::set_scene_sync_fails(&root, true);
        let r = call(&sock, "push", json!({ "repo": "tool" }));
        strata_daemon::seams::set_scene_sync_fails(&root, false);
        assert!(r.get("error").is_some(), "{r:#}");
        assert_eq!(main_at(), before, "nothing was pushed");
        let left: Vec<_> = fs::read_dir(root.join("daemon/push")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "no scene is left: {left:?}");
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        assert_eq!(r["push"]["kind"], "Pushed", "{r:#}");
        assert_ne!(main_at(), before);
    });
    drop(d);
    core.close().unwrap();
}

/// Review-5 N1: a standalone `push` whose recorded URL A gets an exact
/// `insteadOf` rule while it stages (git would send A to B) is refused
/// before anything is sent (`url-rewritten`): A and B are untouched and a
/// kept scene for A is not settled.
#[test]
fn standalone_push_refuses_a_url_rewritten_while_staging() {
    let mut fx = Dfx::new();
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.push = false;
    fx.write_config();
    let sock = fx.socket();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let git = core.gits[&repo_name("tool")].clone();
    let tool = fx.config.repos[&repo_name("tool")].path.clone();
    let a_url = fx.sb.git(&tool, &["config", "remote.origin.url"]).trim().to_string();
    let b = fx.sb.path("tool-b.git");
    fx.sb.git(&fx.sb.root, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]);
    let main_at = |dir: &str| fx.sb.git(&fx.sb.root, &["--git-dir", dir, "rev-parse", "refs/heads/main"]).trim().to_string();
    let before = main_at(&a_url);
    serving(&d, || {
        let id = new_session(&sock);
        external(&ws(&core, &id, "tool"), "printf 'export const a = 9\\n' > src/index.ts");
        let cp = ok(call(&sock, "checkpoint", json!({
            "session": id, "repo": "tool",
            "reason": reason_json(CheckpointReason::Manual { message: "m".into() }), "paths": [],
        })));
        let cp_id = serde_json::from_value(cp["checkpoint"]["id"].clone()).unwrap();
        ok(call(&sock, "publish", json!({ "group": group(vec![batch(&id, "tool", cp_id, &["src/index.ts"])]) })));
        let tip = git.read_ref(&strata_publish::chain_ref("main")).unwrap().unwrap();
        let old = fx.config.paths.root.join("daemon/push/1-1");
        fs::create_dir_all(&old).unwrap();
        let body = json!({
            "repo": "tool", "commit": tip, "remote": "origin", "branch": "main",
            "repo_id": git.repo_id().get().to_string_lossy(), "push_url": a_url,
        });
        fs::write(old.join("push.json"), body.to_string()).unwrap();
        let staging = git.pause_export_before_rename();
        std::thread::scope(|sc| {
            let t = sc.spawn(|| call(&sock, "push", json!({ "repo": "tool" })));
            let start = Instant::now();
            while !staging.is_paused() && !t.is_finished() && start.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(staging.is_paused(), "push reached the staging pause: {:?}", t.is_finished());
            fx.sb.git(&tool, &["config", &format!("url.{}.insteadOf", b.display()), &a_url]);
            staging.release();
            let r = t.join().unwrap();
            assert_eq!(r["error"]["code"], "InvalidInput", "{r:#}");
            assert_eq!(r["error"]["detail"]["reason"], "url-rewritten", "{r:#}");
        });
        assert_eq!(main_at(&a_url), before, "nothing sent to A");
        assert_eq!(main_at(b.to_str().unwrap()), before, "nothing sent to B");
        assert!(old.exists(), "the scene for A is not settled");
    });
    drop(d);
    core.close().unwrap();
}

/// §5.12: `du` answers from the cache, sizes a new session in the
/// background, records it at new/rm; the global `gc` empties old,
/// unreferenced store/trash entries and keeps young ones.
#[test]
fn du_sizes_sessions_records_new_and_rm_and_gc_empties_trash() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let root = fx.config.paths.root.clone();
    let trash = root.join("store/trash");
    fs::create_dir_all(&trash).unwrap();
    let old = trash.join(format!("1000-{}", "0".repeat(32)));
    fs::create_dir_all(old.join("x")).unwrap();
    fs::write(old.join("x/f"), vec![7u8; 100_000]).unwrap();
    serving(&d, || {
        let id = new_session(&sock);
        let wait_idle = || {
            wait_for("du pass", || {
                let r = ok(call(&sock, "du", json!({})));
                !r["scan"]["running"].as_bool().unwrap() && r["scan"]["queued"] == json!(0) && r["store"]["all"]["pending"] == json!(0)
            })
        };
        ok(call(&sock, "du", json!({ "refresh": true })));
        wait_idle();
        let r = ok(call(&sock, "du", json!({})));
        let list = r["sessions"]["list"].as_array().unwrap();
        let s = list.iter().find(|s| s["session"] == json!(id.as_str())).unwrap_or_else(|| panic!("{r:#}"));
        assert!(s["logical"].as_u64().unwrap() > 0, "{s}");
        assert!(s["as_of"].is_string());
        assert!(s["create_ms"].is_u64(), "the daemon times its own `new`: {s}");
        assert_eq!(r["savings"]["cumulative"]["timed"], json!(1), "{r:#}");
        let t = &r["store"]["trash"];
        assert!(t["eligible"]["entries"].as_u64().unwrap() >= 1, "{t}");
        assert!(t["eligible"]["reclaim_max"].as_u64().unwrap() >= 100_000, "{t}");
        assert_eq!(t["gc"], json!("git-strata gc"));
        assert_eq!(r["savings"]["cumulative"]["sessions"], json!(1), "{r:#}");

        ok(call(&sock, "rm", json!({ "session": id, "discard": false })));
        let r = ok(call(&sock, "du", json!({})));
        assert!(r["sessions"]["list"].as_array().unwrap().iter().all(|s| s["session"] != json!(id.as_str())), "{r:#}");
        assert_eq!(r["savings"]["cumulative"]["removed"], json!(1), "{r:#}");

        let g = ok(call(&sock, "gc", json!({ "repo": null })));
        assert!(g["trash"]["removed"].as_u64().unwrap() >= 1, "{g:#}");
        assert!(!old.exists());
        let g = ok(call(&sock, "gc", json!({ "repo": "tool" })));
        assert_eq!(g["trash"], Value::Null, "a repo gc leaves trash alone");
    });
    drop(d);
    core.close().unwrap();
}

/// D33 (review GS-09): `gc --store --dry-run` previews and writes nothing;
/// `gc --store` runs the store-wide gc with the trash protection; `store`
/// takes no repo and `dry_run` needs `store`.
#[test]
fn store_gc_is_opt_in_and_previews_first() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let root = fx.config.paths.root.clone();
    let trash = root.join("store/trash");
    fs::create_dir_all(&trash).unwrap();
    let old = trash.join(format!("1000-{}", "0".repeat(32)));
    fs::create_dir_all(old.join("x")).unwrap();
    serving(&d, || {
        let r = call(&sock, "gc", json!({ "repo": "tool", "store": true }));
        assert_eq!(r["error"]["detail"]["reason"], json!("store_with_repo"), "{r:#}");
        let r = call(&sock, "gc", json!({ "repo": null, "dry_run": true }));
        assert_eq!(r["error"]["detail"]["reason"], json!("dry_run_without_store"), "{r:#}");

        let runs = core.store.last_gc_report(None).unwrap();
        let p = ok(call(&sock, "gc", json!({ "repo": null, "store": true, "dry_run": true })));
        assert_eq!(p["dry_run"], json!(true), "{p:#}");
        assert!(p["preview"]["trash_eligible"].as_u64().unwrap() >= 1, "{p:#}");
        assert_eq!(p["preview"]["keep_secs"], json!(30 * 24 * 3600), "{p:#}");
        assert!(old.exists(), "a dry run deletes nothing");
        assert_eq!(core.store.last_gc_report(None).unwrap(), runs, "a dry run records no gc run");

        let g = ok(call(&sock, "gc", json!({ "repo": null, "store": true })));
        assert!(g["store"]["trash_removed"].as_u64().unwrap() >= 1, "{g:#}");
        assert!(g["store"]["error"].is_null(), "{g:#}");
        assert!(!old.exists());
        assert!(core.store.last_gc_report(None).unwrap().is_some(), "the store-wide run is recorded");
    });
    drop(d);
    core.close().unwrap();
}

/// session.md D51/D52: `new` with `only` lays out the named repo alone; a
/// call on a repo the session lacks names `git-strata add`; `add` gives the
/// session that repo, and the same call then works.
#[test]
fn slim_new_then_add_over_the_socket() {
    let fx = Dfx::with(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let s = ok(call(&sock, "new", json!({ "ticket": null, "repos": ["tool"], "only": true })));
        let repos = s["session"]["manifest"]["repos"].as_object().unwrap();
        assert_eq!(repos.keys().collect::<Vec<_>>(), ["tool"], "{s:#}");
        let id: SessionId = serde_json::from_value(s["session"]["manifest"]["id"].clone()).unwrap();

        let r = call(&sock, "files", json!({ "session": id, "repo": "front", "glob": "*" }));
        assert_eq!(r["ok"], json!(false), "{r:#}");
        assert_eq!(r["error"]["code"], "NoSuchRepo", "{r:#}");
        assert_eq!(r["error"]["detail"]["reason"], "not_in_session", "{r:#}");
        assert_eq!(r["error"]["detail"]["fix"], json!(format!("git-strata add --session {id} --repo front")), "{r:#}");

        let a = ok(call(&sock, "add", json!({ "session": id, "repos": ["front"] })));
        assert!(a["report"]["added"]["front"]["tracked"].as_bool().unwrap(), "{a:#}");
        assert_eq!(a["anchor_unknown"], json!([]), "{a:#}");
        let f = ok(call(&sock, "files", json!({ "session": id, "repo": "front", "glob": "src/*.ts" })));
        assert_eq!(f["paths"], json!(["src/index.ts"]), "{f:#}");
        let again = ok(call(&sock, "add", json!({ "session": id, "repos": ["front"] })));
        assert_eq!(again["report"]["added"], json!({}), "{again:#}");
        assert!(again["report"]["already"]["front"].is_object(), "{again:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// agent-id: `daemon_status.features` lists `new.agent`; `new` stores the
/// agent and `status` shows it as `manifest.agent`; absent or null stores
/// none; a bad name is InvalidInput reason `agent` and creates nothing; a
/// non-string is an rpc `Failed`.
#[test]
fn new_records_the_agent_and_daemon_status_lists_the_feature() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let dst = ok(call(&sock, "daemon_status", json!({ "verify": false })));
        assert!(dst["features"].as_array().unwrap().contains(&json!("new.agent")), "{dst:#}");

        let s = ok(call(&sock, "new", json!({ "ticket": null, "repos": ["tool"], "agent": "claude-code" })));
        assert_eq!(s["session"]["manifest"]["agent"], "claude-code", "{s:#}");
        let id = s["session"]["manifest"]["id"].as_str().unwrap().to_string();
        let st = ok(call(&sock, "status", json!({ "cwd": null, "session": id, "all": false })));
        assert_eq!(st["sessions"][0]["status"]["manifest"]["agent"], "claude-code", "{st:#}");

        for p in [json!({ "ticket": null, "repos": ["tool"] }), json!({ "ticket": null, "repos": ["tool"], "agent": null })] {
            let s = ok(call(&sock, "new", p));
            assert!(s["session"]["manifest"].get("agent").is_none(), "absent, not null: {s:#}");
        }
        let count = || ok(call(&sock, "status", json!({ "cwd": null, "session": null, "all": true })))["sessions"].as_array().unwrap().len();
        assert_eq!(count(), 3);

        for bad in ["", "bad name", "-x", "é"] {
            let r = call(&sock, "new", json!({ "ticket": null, "repos": ["tool"], "agent": bad }));
            assert_eq!(r["error"]["code"], "InvalidInput", "{bad:?}: {r:#}");
            assert_eq!(r["error"]["detail"]["reason"], "agent", "{bad:?}: {r:#}");
        }
        let r = call(&sock, "new", json!({ "ticket": null, "repos": ["tool"], "agent": 5 }));
        assert_eq!(r["ok"], json!(false), "{r:#}");
        assert_eq!(r["error"]["rpc"], "Failed", "{r:#}");
        assert_eq!(count(), 3, "refused requests create nothing");
    });
    drop(d);
    core.close().unwrap();
}

/// dev-ports: `daemon_status.features` lists `session.ports`; `new` leases
/// a block; `ports_ensure` returns it unchanged; `ports_reassign` moves it;
/// after `rm` the lease is gone and neither RPC brings it back.
#[test]
fn new_leases_dev_ports_and_ensure_reassign_rm_follow_the_lifecycle() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let dst = ok(call(&sock, "daemon_status", json!({ "verify": false })));
        assert!(dst["features"].as_array().unwrap().contains(&json!("session.ports")), "{dst:#}");
        let id = new_session(&sock);
        let leased = core.store.dev_port_leases(Some(&id)).unwrap().unwrap();
        assert_eq!(leased.len(), 1, "{leased:?}");
        let base = leased[0].base;
        assert!((20000..=29996).contains(&base) && base % 4 == 0, "{base}");

        let p = json!({ "session": id.as_str(), "repo": "tool" });
        let e = ok(call(&sock, "ports_ensure", p.clone()));
        assert_eq!(e["base"], json!(base), "{e:#}");
        assert_eq!(e["ports"], json!({ "web": base, "connector": base + 1, "connector_mcp": base + 2, "aux": base + 3 }));
        let r = ok(call(&sock, "ports_reassign", p.clone()));
        assert_ne!(r["base"], json!(base), "{r:#}");
        assert_eq!(ok(call(&sock, "ports_ensure", p.clone()))["base"], r["base"]);

        let r = call(&sock, "ports_ensure", json!({ "session": id.as_str(), "repo": "nope" }));
        assert_eq!(r["error"]["code"], "NoSuchRepo", "{r:#}");
        let r = call(&sock, "ports_ensure", json!({ "session": id.as_str(), "repo": "tool", "extra": 1 }));
        assert_eq!(r["error"]["rpc"], "Failed", "{r:#}");

        ok(call(&sock, "rm", json!({ "session": id.as_str(), "discard": false })));
        assert_eq!(core.store.dev_port_leases(Some(&id)).unwrap().unwrap(), vec![]);
        for m in ["ports_ensure", "ports_reassign"] {
            let r = call(&sock, m, p.clone());
            assert_eq!(r["error"]["code"], "NoSuchSession", "{m}: {r:#}");
        }
        assert_eq!(core.store.dev_port_leases(Some(&id)).unwrap().unwrap(), vec![]);
    });
    drop(d);
    core.close().unwrap();
}

/// Review P1-3: a finished connection leaves nothing behind (the clone the
/// daemon keeps for the stop goes with it), and connections past
/// `MAX_CONNS` wait instead of each taking a thread.
#[test]
fn finished_connections_release_their_descriptors() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        for _ in 0..40 {
            ok(call(&sock, "daemon_status", json!({})));
        }
        let t0 = std::time::Instant::now();
        while d.open_connections() > 0 && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(d.open_connections(), 0, "closed connections are dropped");
        // Idle connections count while open and are capped.
        // Connect in steps so the listen backlog never overflows.
        let mut idle: Vec<UnixStream> = Vec::new();
        for i in 0..startup::MAX_CONNS + 5 {
            idle.push(UnixStream::connect(&sock).unwrap());
            let want = (i + 1).min(startup::MAX_CONNS);
            let t0 = std::time::Instant::now();
            while d.open_connections() < want && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(d.open_connections(), startup::MAX_CONNS, "no more than MAX_CONNS served at once");
        drop(idle);
        let t0 = std::time::Instant::now();
        while d.open_connections() > 0 && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(d.open_connections(), 0);
        // The daemon still answers.
        ok(call(&sock, "daemon_status", json!({})));
    });
    drop(d);
    core.close().unwrap();
}

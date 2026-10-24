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
use strata_types::{CheckpointReason, Code, RepoName, SessionId};
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
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let origin = origin_main(&fx);
    let sock = fx.socket();
    let chain = strata_publish::chain_ref("main");
    let (id, commit, unknown) = {
        let core = fx.prepare().unwrap();
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
            assert_eq!(r["error"]["detail"]["kind"], "PushRefused", "{r:#}");
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

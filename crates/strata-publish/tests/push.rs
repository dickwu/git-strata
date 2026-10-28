//! Task 4: commit, push and unknown outcomes (publish.md §5.2 steps 6–9,
//! §5.6, §6). Every remote is a local bare repository in the sandbox.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use strata_deps::{ChainState, publish_chain};
use strata_publish::seams::{self, FailPoint, PublishFault};
use strata_publish::{Published, publish, push};
use strata_store::PublishPhase;
use strata_types::{Action, Code, CommitOid, ConfirmToken, PushOutcome, ReleaseGroup, SessionId, StrataError};
use support::*;

const CHAIN: &str = "refs/strata/publish/main";
const ANCHOR: &str = "refs/strata/anchor/main";
const TRACK: &str = "refs/strata/remotes/origin/main";

fn fx() -> Fx {
    let fx = Fx::with(&["tool"]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    fx
}

/// One session editing `file` in tool, checkpointed, as a group.
fn edit(fx: &Fx, file: &str, content: &[u8]) -> (SessionId, ReleaseGroup) {
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[(file, content)]);
    let c = fx.checkpoint(&s, "tool");
    (s.clone(), group(vec![batch(&s, "tool", c.id, &[file])]))
}

fn run(fx: &Fx, g: &ReleaseGroup, confirm: Option<ConfirmToken>) -> strata_publish::Result<Published> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    publish(&ctx, g, confirm)
}

fn token(action: Action) -> ConfirmToken {
    ConfirmToken { action, issued: Instant::now(), nonce: 7 }
}

/// Review P1-15: the approval names origin's push URL as read now.
fn deploy(fx: &Fx, branch: &str) -> Action {
    deploy_at(branch, &fx.git_of("tool").push_url("origin").unwrap())
}

fn deploy_at(branch: &str, url: &str) -> Action {
    Action::PushDeploy { repo: repo_name("tool"), branch: branch.into(), url: url.into() }
}

/// Every git call since the last take: `"<subcommand> <argv…> cwd=… <GIT_* env>"`.
fn calls(fx: &Fx) -> Vec<String> {
    fx.git_of("tool")
        .take_git_calls()
        .into_iter()
        .map(|c| {
            let args: Vec<String> = c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
            let mut sub = String::new();
            let mut skip = false;
            for a in &args {
                if skip {
                    skip = false;
                } else if a == "-c" || a == "-C" {
                    skip = true;
                } else if !a.starts_with('-') {
                    sub = a.clone();
                    break;
                }
            }
            let mut s = vec![sub];
            s.extend(args);
            s.push(format!("cwd={}", c.cwd.display()));
            for (k, v) in &c.env {
                if k.to_string_lossy().starts_with("GIT_") {
                    s.push(v.to_string_lossy().into_owned());
                }
            }
            s.join(" ")
        })
        .collect()
}

fn ran(calls: &[String], sub: &str) -> usize {
    calls.iter().filter(|c| c.split(' ').next() == Some(sub)).count()
}

fn touched_layout(calls: &[String]) -> bool {
    calls.iter().any(|c| c.contains(".strata-publish-"))
}

fn layouts(fx: &Fx, s: &SessionId) -> Vec<PathBuf> {
    let root = fx.store().session(s).unwrap().unwrap().manifest.root;
    let mut v: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".strata-publish-"))
        .collect();
    v.sort();
    v
}

fn row(fx: &Fx, id: u64) -> strata_store::PublishRecord {
    fx.store().publish_record(id).unwrap().expect("publish row")
}

fn id_of(e: &StrataError) -> Option<u64> {
    e.detail.as_ref().and_then(|d| d.get("publish")).and_then(|v| v.as_u64())
}

fn last_row(fx: &Fx) -> strata_store::PublishRecord {
    fx.store().publishes().unwrap().pop().expect("a publish row")
}

fn commit_of(e: &StrataError) -> String {
    e.detail.as_ref().unwrap()["commit"].as_str().expect("detail.commit").to_string()
}

fn object_exists(fx: &Fx, oid: &str) -> bool {
    fx.sb.git_output(&fx.main("tool"), &["cat-file", "-e", &format!("{oid}^{{commit}}")]).status.success()
}

fn origin_dir(fx: &Fx) -> PathBuf {
    fx.sb.root.join("tool-origin.git")
}

fn hook(fx: &Fx, body: &str) {
    let p = origin_dir(fx).join("hooks/pre-receive");
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fresh staging for `commit` (export + attach) for the standalone push.
fn staging_for(fx: &Fx, commit: &str) -> PathBuf {
    let g = fx.git_of("tool");
    let dir = fx.sb.root.join(format!("pushstage-{}", random_tag()));
    fs::create_dir_all(&dir).unwrap();
    let c = CommitOid::try_from(commit.to_string()).unwrap();
    let wt = dir.join("tool");
    g.export_tree(&g.tree_of(&c).unwrap(), &wt).unwrap();
    g.attach_staging(&wt, &dir.join(".git-tool"), &c).unwrap();
    wt
}

fn state(fx: &Fx) -> ChainState {
    publish_chain(fx.git_of("tool"), "main").unwrap().unwrap().state
}

#[test]
fn commit_cas_failure_is_not_retried() {
    let mut fx = fx();
    fx.cfg_mut("tool").publish.push = false;
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 1\n");
    let origin = fx.read_ref("tool", TRACK).unwrap();
    calls(&fx);
    let p = seams::pause(fx.git_of("tool"), PublishFault::PauseAfterCommit);
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| run(&fx, &g, None));
        p.wait_until_paused();
        // Someone else moves the chain between read and compare-and-swap.
        fx.sb.git(&fx.main("tool"), &["update-ref", CHAIN, &origin]);
        p.release();
        h.join().unwrap()
    });
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["reason"], "head_moved");
    assert_eq!(d["head_moved"]["expected"], serde_json::Value::Null);
    assert_eq!(d["head_moved"]["actual"], origin.as_str());
    let c = commit_of(&e);
    assert!(object_exists(&fx, &c), "the commit object stays");
    let cs = calls(&fx);
    assert_eq!(ran(&cs, "commit-tree"), 1, "not retried: {cs:#?}");
    assert_eq!(ran(&cs, "push"), 0);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), origin, "the chain keeps the other writer's value");
    let r = last_row(&fx);
    assert_eq!(r.phase, PublishPhase::Unknown);
    assert_eq!(r.outcome.as_ref().unwrap()["commit"], c.as_str());
}

#[test]
fn confirm_without_a_matching_token_stops_before_push() {
    let mut fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, "src/index.ts", b"export const a = 2\n");
    let bad = [
        None,
        Some(token(deploy(&fx, "other"))),
        Some(token(Action::PushDeploy { repo: repo_name("front"), branch: "main".into(), url: fx.git_of("tool").push_url("origin").unwrap() })),
        Some(token(Action::RemoveDiscard { session: s.clone() })),
    ];
    for (deploy_on_push, confirm) in [(true, true), (false, true)] {
        fx.cfg_mut("tool").deploy_on_push = deploy_on_push;
        fx.cfg_mut("tool").publish.confirm = confirm;
        for t in bad.iter() {
            let t = t.as_ref().map(|t| token(t.action.clone()));
            calls(&fx);
            let e = run(&fx, &g, t).unwrap_err();
            assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
            let cs = calls(&fx);
            assert!(!touched_layout(&cs), "no staging: {cs:#?}");
            assert_eq!(ran(&cs, "commit-tree"), 0);
            assert_eq!(ran(&cs, "push"), 0);
            assert!(layouts(&fx, &s).is_empty());
            assert_eq!(fx.read_ref("tool", CHAIN), None);
            assert_eq!(fx.origin_head("tool", "main"), origin);
            let r = last_row(&fx);
            assert_eq!(r.phase, PublishPhase::Done);
            assert_eq!(r.outcome.as_ref().unwrap()["stopped"], "NotConfirmed");
        }
    }
    // The matching action pushes.
    let p = run(&fx, &g, Some(token(deploy(&fx, "main")))).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    let changes = fx.store().ref_changes(p.id).unwrap();
    let pushed = changes.iter().find(|c| c.op == strata_store::RefOp::Push).expect("push recorded");
    assert_eq!(pushed.confirmed, Some(deploy(&fx, "main")));
}

/// Review P1-15: the approval names origin's push URL. When origin's push
/// URL under the lock is not the approved one (it was changed after the
/// prompt), publish and push are `NotConfirmed` (`remote_changed`, both
/// URLs in the detail) before any staging, and nothing reaches either
/// remote. With the approved URL restored, the same kind of token pushes.
#[test]
fn a_confirmed_push_is_bound_to_the_approved_url() {
    let mut fx = fx();
    fx.cfg_mut("tool").publish.confirm = true;
    let origin = fx.origin_head("tool", "main");
    let approved = fx.git_of("tool").push_url("origin").unwrap();
    let other = fx.sb.root.join("elsewhere.git");
    fx.sb.git(&fx.sb.root, &["init", "-q", "--bare", other.to_str().unwrap()]);
    let main = fx.main("tool");
    fx.sb.git(&main, &["config", "remote.origin.pushurl", other.to_str().unwrap()]);
    let (s, g) = edit(&fx, "src/index.ts", b"export const a = 9\n");
    calls(&fx);
    let e = run(&fx, &g, Some(token(deploy_at("main", &approved)))).unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
    let d = e.detail.clone().unwrap();
    assert_eq!(d["reason"], "remote_changed", "{e:?}");
    assert_eq!(d["confirmed"], approved.as_str());
    assert_eq!(d["now"], other.to_str().unwrap());
    let cs = calls(&fx);
    assert!(!touched_layout(&cs), "no staging: {cs:#?}");
    assert_eq!(ran(&cs, "push"), 0);
    assert!(layouts(&fx, &s).is_empty());
    assert_eq!(fx.read_ref("tool", CHAIN), None);
    assert_eq!(fx.origin_head("tool", "main"), origin);
    assert_eq!(fx.sb.git(&fx.sb.root, &["--git-dir", other.to_str().unwrap(), "for-each-ref"]), "", "nothing reached the other remote");
    // A token for the URL as it is now pushes there (the human approved it).
    fx.sb.git(&main, &["config", "--unset", "remote.origin.pushurl"]);
    let p = run(&fx, &g, Some(token(deploy_at("main", &approved)))).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
}

#[test]
fn deploy_on_push_without_confirm_pushes_headless() {
    // 2026-10-26: deploy_on_push is information; publish.confirm alone gates.
    let mut fx = fx();
    fx.cfg_mut("tool").deploy_on_push = true;
    fx.cfg_mut("tool").publish.confirm = false;
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 8\n");
    let p = run(&fx, &g, None).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    let changes = fx.store().ref_changes(p.id).unwrap();
    let pushed = changes.iter().find(|c| c.op == strata_store::RefOp::Push).expect("push recorded");
    assert_eq!(pushed.confirmed, None);
}

#[test]
fn push_timeout_keeps_chain_reports_unknown() {
    let mut fx = fx();
    let t0 = Instant::now();
    hook(&fx, "sleep 2\nexit 1");
    seams::set_push_timeout(fx.git_of("tool"), Some(Duration::from_millis(300)));
    let origin = fx.origin_head("tool", "main");
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 3\n");
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let c = commit_of(&e);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), c, "the chain stays");
    let r = last_row(&fx);
    assert_eq!(r.phase, PublishPhase::Unknown);
    let id = r.group.id;
    seams::set_push_timeout(fx.git_of("tool"), None);
    // Restart: the row is reported, nothing is pushed again.
    fx.close_store();
    fx.reopen_store();
    fx.reopen_gits();
    let open = fx.store().open_publishes().unwrap();
    assert!(open.iter().any(|r| r.group.id == id && r.phase == PublishPhase::Unknown));
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Busy);
    assert_eq!(reason(&e), Some("publish_unknown"));
    let cs = calls(&fx);
    assert_eq!(ran(&cs, "push"), 0, "{cs:#?}");
    assert_eq!(ran(&cs, "commit-tree"), 0);
    assert_eq!(fx.origin_head("tool", "main"), origin);
    // Let the hook finish before the sandbox goes.
    std::thread::sleep(Duration::from_secs(3).saturating_sub(t0.elapsed()));
}

#[test]
fn push_refused_keeps_local_chain() {
    let fx = fx();
    hook(&fx, "echo 'denied by policy' >&2\nexit 1");
    let origin = fx.origin_head("tool", "main");
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 4\n");
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["kind"], "PushRefused");
    assert!(d["message"].as_str().unwrap().contains("denied by policy"), "{d}");
    let c = commit_of(&e);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), c);
    assert_eq!(fx.read_ref("tool", TRACK).unwrap(), origin);
    assert_eq!(fx.origin_head("tool", "main"), origin);
    // Review P0-2: a definite refusal closes the row (`Done`, stopped
    // `PushRefused`), so it does not block the repo's next publish.
    let r = last_row(&fx);
    assert_eq!(r.phase, PublishPhase::Done);
    let o = r.outcome.unwrap();
    assert_eq!(o["stopped"], "PushRefused", "{o}");
    assert_eq!(o["commit"], c.as_str(), "{o}");
    assert_eq!(o["push"]["kind"], "Refused", "{o}");
    assert_eq!(state(&fx), ChainState::Leads);

    // The standalone push has written nothing yet: PushRefused.
    let st = staging_for(&fx, &c);
    let cfg = fx.config.repos[&repo_name("tool")].clone();
    let e = push(fx.git_of("tool"), &cfg, &repo_name("tool"), &st, None).unwrap_err();
    assert_eq!(e.code, Code::PushRefused, "{e:?}");
    assert!(detail_str(&e, "message").unwrap().contains("denied by policy"));
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), c);
    fs::remove_file(origin_dir(&fx).join("hooks/pre-receive")).unwrap();
    assert_eq!(push(fx.git_of("tool"), &cfg, &repo_name("tool"), &st, None).unwrap(), PushOutcome::Pushed);
    assert_eq!(fx.origin_head("tool", "main"), c);
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), c);
}

#[test]
fn push_false_leaves_chain_local() {
    let mut fx = fx();
    fx.cfg_mut("tool").publish.push = false;
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, "src/index.ts", b"export const a = 5\n");
    calls(&fx);
    let p = run(&fx, &g, None).unwrap();
    assert_eq!(p.push, None);
    assert_eq!(p.checks_ran, 0);
    assert_eq!(ran(&calls(&fx), "push"), 0);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), p.commit.as_str());
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), origin, "B was origin's tip");
    assert_eq!(fx.origin_head("tool", "main"), origin);
    assert_eq!(state(&fx), ChainState::Leads);
    let r = row(&fx, p.id);
    assert_eq!(r.phase, PublishPhase::Done);
    let o = r.outcome.unwrap();
    assert_eq!(o["push"], serde_json::Value::Null);
    assert_eq!(o["checks_ran"], 0);
    assert_eq!(o["commit"], p.commit.as_str());
    assert_eq!(o["repo"], "tool");
    assert!(o.get("stopped").is_none());
    assert!(layouts(&fx, &s).is_empty(), "the layout is removed on success");

    // A later push is its own call; it checks the confirmation first.
    let st = staging_for(&fx, p.commit.as_str());
    let mut cfg = fx.config.repos[&repo_name("tool")].clone();
    cfg.deploy_on_push = true;
    cfg.publish.confirm = true;
    calls(&fx);
    let e = push(fx.git_of("tool"), &cfg, &repo_name("tool"), &st, None).unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed);
    assert_eq!(ran(&calls(&fx), "push"), 0);
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), origin);
    let out = push(fx.git_of("tool"), &cfg, &repo_name("tool"), &st, Some(token(deploy(&fx, "main")))).unwrap();
    assert_eq!(out, PushOutcome::Pushed);
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), p.commit.as_str());
    assert_eq!(fx.read_ref("tool", TRACK).unwrap(), p.commit.as_str());
    // Nothing leads any more: NoCommit.
    let e = push(fx.git_of("tool"), &cfg, &repo_name("tool"), &st, Some(token(deploy(&fx, "main")))).unwrap_err();
    assert_eq!(e.code, Code::NoCommit);
}

#[test]
fn pushed_sets_anchor_to_pushed_commit() {
    let fx = fx();
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 6\n");
    let p = run(&fx, &g, None).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    let c1 = p.commit.as_str().to_string();
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), c1);
    assert_eq!(fx.read_ref("tool", TRACK).unwrap(), c1);
    assert_eq!(fx.origin_head("tool", "main"), c1);
    assert_eq!(state(&fx), ChainState::Equal);

    // The remote accepts; the call has not recorded yet; then the record
    // cannot commit (the tracking ref moved under it): not Pushed.
    let (_s2, g2) = edit(&fx, "src/b.ts", b"export const b = 1\n");
    let initial = fx.sb.git(&fx.main("tool"), &["rev-parse", &format!("{c1}^")]).trim().to_string();
    let pz = seams::pause(fx.git_of("tool"), PublishFault::PauseBeforePushRecorded);
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| run(&fx, &g2, None));
        pz.wait_until_paused();
        fx.sb.git(&fx.main("tool"), &["update-ref", TRACK, &initial]);
        pz.release();
        h.join().unwrap()
    });
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let c2 = commit_of(&e);
    assert_eq!(fx.origin_head("tool", "main"), c2, "the remote has it");
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), c1, "the anchor was not written on its own");
    assert_ne!(state(&fx), ChainState::Leads);
    // The remote is later rewound below the anchor: the next fetch reads
    // Rewound, not Leads, and the anchor is not moved to origin.
    fx.sb.git(&origin_dir(&fx), &["update-ref", "refs/heads/main", &initial]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    assert_eq!(state(&fx), ChainState::Rewound);
    assert_eq!(strata_publish::advance_anchor(fx.git_of("tool"), "main").unwrap(), None);
    assert_eq!(fx.read_ref("tool", ANCHOR).unwrap(), c1);
}

#[test]
fn no_flag_or_env_bypass() {
    let mut fx = fx();
    fx.cfg_mut("tool").deploy_on_push = true;
    fx.cfg_mut("tool").publish.confirm = true;
    let g = fx.git_of("tool");
    for k in ["STRATA_PUBLISH", "STRATA_CONFIRM", "STRATA_NO_CONFIRM", "STRATA_YES", "CI"] {
        g.set_env(k, Some(std::ffi::OsStr::new("1")));
    }
    let origin = fx.origin_head("tool", "main");
    let (_s, gr) = edit(&fx, "src/index.ts", b"export const a = 7\n");
    calls(&fx);
    let e = run(&fx, &gr, None).unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
    let cs = calls(&fx);
    assert_eq!(ran(&cs, "push"), 0);
    assert_eq!(ran(&cs, "commit-tree"), 0);
    assert_eq!(fx.origin_head("tool", "main"), origin);
    // Publish reads no environment variable and no flag of its own.
    for src in [
        include_str!("../src/lib.rs"),
        include_str!("../src/push.rs"),
        include_str!("../src/chain.rs"),
        include_str!("../src/stage.rs"),
        include_str!("../src/check.rs"),
    ] {
        // (check.rs lists the inherited names only to scrub them from the
        // child's environment; no value is read.)
        assert!(!src.contains("env::var(") && !src.contains("env::var_os("), "publish must not read the environment");
    }
}

#[test]
fn injected_failure_after_commit_is_unknown() {
    for at in [FailPoint::BeforeRefs, FailPoint::BeforePush, FailPoint::BeforeDone] {
        let fx = fx();
        let origin = fx.origin_head("tool", "main");
        let (s, g) = edit(&fx, "src/index.ts", b"export const a = 8\n");
        let ws_before = fs::read(fx.ws(&s, "tool").join("src/index.ts")).unwrap();
        let base_before = fx.base_of(&s, "tool");
        seams::fail_once(fx.git_of("tool"), at);
        calls(&fx);
        let e = run(&fx, &g, None).unwrap_err();
        assert_eq!(e.code, Code::Unknown, "{at:?}: {e:?}");
        let c = commit_of(&e);
        assert!(object_exists(&fx, &c), "{at:?}: the commit object stays");
        let cs = calls(&fx);
        assert_eq!(ran(&cs, "commit-tree"), 1);
        match at {
            FailPoint::BeforeRefs => {
                assert_eq!(fx.read_ref("tool", CHAIN), None);
                assert_eq!(ran(&cs, "push"), 0);
                assert_eq!(fx.origin_head("tool", "main"), origin);
            }
            FailPoint::BeforePush => {
                assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), c);
                assert_eq!(ran(&cs, "push"), 0);
                assert_eq!(fx.origin_head("tool", "main"), origin);
            }
            FailPoint::BeforeDone => {
                assert_eq!(fx.origin_head("tool", "main"), c);
                assert_eq!(layouts(&fx, &s).len(), 1, "the scene stays");
            }
            FailPoint::BeforeRebase | FailPoint::SiblingAnchor | FailPoint::ColdInstall | FailPoint::ColdInstallPanic => {
                unreachable!()
            }
        }
        let r = last_row(&fx);
        assert_eq!(r.phase, PublishPhase::Unknown, "{at:?}");
        assert_eq!(r.outcome.as_ref().unwrap()["commit"], c.as_str());
        assert_eq!(fs::read(fx.ws(&s, "tool").join("src/index.ts")).unwrap(), ws_before, "overlay untouched");
        if at == FailPoint::BeforeDone {
            // The rebase (§5.8) ran before the last step.
            assert_eq!(fx.base_of(&s, "tool").tree, fx.git_of("tool").tree_of(&strata_types::CommitOid::try_from(c.clone()).unwrap()).unwrap());
        } else {
            assert_eq!(fx.base_of(&s, "tool"), base_before);
        }
    }
}

#[test]
fn second_attempt_does_not_overwrite() {
    let fx = fx();
    let (s, g) = edit(&fx, "src/index.ts", b"export const a = 9\n");
    // An orphaned Running row in the same scope.
    let orphan = fx.store().record_publish(&g, std::time::SystemTime::now()).unwrap();
    calls(&fx);
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert_eq!(reason(&e), Some("publish_open"));
    assert_eq!(id_of(&e), Some(orphan));
    assert_eq!(row(&fx, orphan).phase, PublishPhase::Running);
    assert_eq!(row(&fx, orphan).outcome, None);
    assert_eq!(fx.store().publishes().unwrap().len(), 1, "no new row");
    assert_eq!(ran(&calls(&fx), "commit-tree"), 0);
    let _ = s;

    // An Unknown row blocks every publish of the repo; its outcome stays.
    let mark = serde_json::json!({ "repo": "tool", "note": "original" });
    assert!(fx
        .store()
        .set_publish_phase(orphan, PublishPhase::Running, PublishPhase::Unknown, Some(&mark), std::time::SystemTime::now())
        .unwrap());
    let (_s2, g2) = edit(&fx, "src/other.ts", b"export const o = 1\n");
    for gr in [&g, &g2] {
        let e = run(&fx, gr, None).unwrap_err();
        assert_eq!(e.code, Code::Busy);
        assert_eq!(reason(&e), Some("publish_unknown"));
    }
    assert_eq!(row(&fx, orphan).outcome, Some(mark));
    assert_eq!(fx.store().publishes().unwrap().len(), 1);
    assert_eq!(ran(&calls(&fx), "commit-tree"), 0);
}

#[test]
fn running_row_of_another_session_does_not_block() {
    let fx = fx();
    let (_s1, g1) = edit(&fx, "src/index.ts", b"export const a = 10\n");
    let other = fx.store().record_publish(&g1, std::time::SystemTime::now()).unwrap();
    let (_s2, g2) = edit(&fx, "src/other.ts", b"export const o = 2\n");
    let p = run(&fx, &g2, None).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    assert_eq!(row(&fx, other).phase, PublishPhase::Running);
}

#[test]
fn uncertain_cleanup_stays_unknown() {
    let mut fx = fx();
    let (s, g) = edit(&fx, "src/index.ts", b"export const a = 11\n");
    let root = fx.store().session(&s).unwrap().unwrap().manifest.root;
    let st = fx.sb.state.display().to_string();
    // The check leaves a directory that is swapped for a symlink while it
    // is being removed, so the removal cannot finish.
    fx.sb.write_program("leaves", &format!("#!/bin/sh\nmkdir -p inner/x\npwd > '{st}/leaves.pwd'\nexit 2\n"));
    fx.cfg_mut("tool").checks = vec![fx.sb.bin.join("leaves").display().to_string()];
    let pwd = fx.sb.state.join("leaves.pwd");
    strata_apfs::seams::set_before_open(Some(Box::new(move |_, name| {
        let Ok(dir) = fs::read_to_string(&pwd) else { return };
        let p = Path::new(dir.trim()).join("inner");
        if name == "inner" && p.is_dir() && !p.is_symlink() {
            fs::rename(&p, p.with_file_name("inner.gone")).unwrap();
            std::os::unix::fs::symlink("/nonexistent", &p).unwrap();
        }
    })));
    calls(&fx);
    let r = run(&fx, &g, None);
    strata_apfs::seams::set_before_open(None);
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["cause"]["code"], "CheckFailed", "{d}");
    assert_eq!(d["cause"]["detail"]["exit_code"], 2);
    let left = layouts(&fx, &s);
    assert_eq!(left.len(), 1, "the directory stays");
    assert_eq!(ran(&calls(&fx), "commit-tree"), 0);
    assert_eq!(fx.read_ref("tool", CHAIN), None);
    let r = last_row(&fx);
    assert_eq!(r.phase, PublishPhase::Unknown);
    assert!(r.outcome.as_ref().unwrap().get("stopped").is_none());
    let _ = root;
    for p in left {
        let _ = fs::remove_dir_all(p);
    }
}

#[test]
fn restart_reports_unknown_never_replays() {
    let mut fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 12\n");
    seams::fail_once(fx.git_of("tool"), FailPoint::BeforePush);
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown);
    let c = commit_of(&e);
    let id = last_row(&fx).group.id;
    fx.close_store();
    fx.reopen_store();
    fx.reopen_gits();
    let open = fx.store().open_publishes().unwrap();
    let r = open.iter().find(|r| r.group.id == id).expect("reported");
    assert_eq!(r.phase, PublishPhase::Unknown);
    assert_eq!(r.outcome.as_ref().unwrap()["commit"], c.as_str());
    let cs = calls(&fx);
    assert_eq!(ran(&cs, "push") + ran(&cs, "commit-tree") + ran(&cs, "update-ref"), 0, "opening writes nothing");
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Busy);
    assert_eq!(reason(&e), Some("publish_unknown"));
    let cs = calls(&fx);
    assert_eq!(ran(&cs, "push"), 0, "{cs:#?}");
    assert_eq!(ran(&cs, "commit-tree"), 0);
    assert_eq!(fx.origin_head("tool", "main"), origin);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), c);
    assert_eq!(fx.store().publishes().unwrap().len(), 1);
}

#[test]
fn pre_write_wait_is_busy_post_write_wait_is_not() {
    let mut fx = fx();
    fx.cfg_mut("tool").publish.push = false;
    fx.lock.set_wait(Duration::from_millis(200));
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 13\n");
    {
        let _held = fx.lock.hold("tool");
        calls(&fx);
        let e = run(&fx, &g, None).unwrap_err();
        assert_eq!(e.code, Code::Busy, "{e:?}");
        assert_eq!(ran(&calls(&fx), "commit-tree"), 0);
        assert_eq!(fx.read_ref("tool", CHAIN), None);
        let r = last_row(&fx);
        assert_eq!(r.phase, PublishPhase::Done);
        assert_eq!(r.outcome.as_ref().unwrap()["stopped"], "Busy");
    }
    // After commit_tree, a holder keeps the lock for longer than the bound:
    // the call waits it out.
    let p = seams::pause(fx.git_of("tool"), PublishFault::PauseAfterCommit);
    let hold_for = Duration::from_millis(1000);
    let (r, got, done) = std::thread::scope(|sc| {
        let h = sc.spawn(|| {
            let r = run(&fx, &g, None);
            (r, Instant::now())
        });
        p.wait_until_paused();
        let (tx, rx) = std::sync::mpsc::channel();
        let lock = &fx.lock;
        let holder = sc.spawn(move || {
            let guard = lock.hold("tool");
            tx.send(Instant::now()).unwrap();
            std::thread::sleep(hold_for);
            drop(guard);
        });
        std::thread::sleep(Duration::from_millis(100));
        p.release();
        let got = rx.recv().unwrap();
        holder.join().unwrap();
        let (r, done) = h.join().unwrap();
        (r, got, done)
    });
    let p = r.unwrap();
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), p.commit.as_str());
    assert!(done >= got + hold_for - Duration::from_millis(50), "the post-write wait outlasted the bound");
    assert_eq!(row(&fx, p.id).phase, PublishPhase::Done);
}

#[test]
fn removed_session_is_nosuchsession() {
    let fx = fx();
    let (s, g) = edit(&fx, "src/index.ts", b"export const a = 14\n");
    fx.sessions().remove(&s, true, Some(token(Action::RemoveDiscard { session: s.clone() }))).unwrap();
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    assert!(fx.store().publishes().unwrap().is_empty(), "no new row");
}

#[test]
fn publish_refuses_rewound_and_diverged_chain() {
    let mut fx = fx();
    fx.cfg_mut("tool").publish.push = false;
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 15\n");
    let p = run(&fx, &g, None).unwrap();
    // Someone else lands on origin: the local chain diverges.
    fx.land_on_origin("tool", "main", &[("README", b"other\n")], "other");
    let (_s2, g2) = edit(&fx, "src/b.ts", b"export const b = 2\n");
    calls(&fx);
    let e = run(&fx, &g2, None).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("chain_diverged"));
    assert_eq!(ran(&calls(&fx), "commit-tree"), 0);
    assert_eq!(fx.read_ref("tool", CHAIN).unwrap(), p.commit.as_str());
    let r = last_row(&fx);
    assert_eq!(r.phase, PublishPhase::Done);
    assert_eq!(r.outcome.as_ref().unwrap()["stopped"], "InvalidInput");

    // Rewound: the anchor is not on origin. Refused outside the lock, no row.
    let rows = fx.store().publishes().unwrap().len();
    fx.sb.git(&fx.main("tool"), &["update-ref", ANCHOR, p.commit.as_str()]);
    let e = run(&fx, &g2, None).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("chain_rewound"));
    assert_eq!(fx.store().publishes().unwrap().len(), rows);
}

/// Review-3 R3: a failure in the chain-ref transaction (after the object
/// exists, before any push) still reports `push: null` and the checks that
/// ran.
#[test]
fn chain_ref_failure_reports_checks_ran_and_no_push() {
    let mut fx = fx();
    fx.cfg_mut("tool").checks = vec!["/usr/bin/true".into(), "/usr/bin/true".into()];
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 31\n");
    seams::fail_once(fx.git_of("tool"), FailPoint::BeforeRefs);
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert!(object_exists(&fx, &commit_of(&e)));
    assert_eq!(d["checks_ran"], 2, "{d}");
    assert!(d.get("push").is_some_and(serde_json::Value::is_null), "explicit push: null: {d}");
    assert_eq!(fx.read_ref("tool", CHAIN), None);
}

/// Review-3 T2: a remote refusal whose row cannot be closed reports the
/// recording failure (`row_moved`), not a plain refusal; the refusal stays
/// in `detail.cause` and the result fields (commit, push, checks) travel on.
#[test]
fn refusal_with_a_moved_row_keeps_the_recording_error_and_the_result() {
    let mut fx = fx();
    fx.cfg_mut("tool").checks = vec!["/usr/bin/true".into()];
    hook(&fx, "echo 'denied by policy' >&2\nexit 1");
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 32\n");
    let p = seams::pause(fx.git_of("tool"), PublishFault::PauseAfterCommit);
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| run(&fx, &g, None));
        p.wait_until_paused();
        // Someone else moves the Running row while the publish is paused.
        let id = last_row(&fx).group.id;
        let mark = serde_json::json!({ "repo": "tool", "note": "moved" });
        assert!(fx
            .store()
            .set_publish_phase(id, PublishPhase::Running, PublishPhase::Unknown, Some(&mark), std::time::SystemTime::now())
            .unwrap());
        p.release();
        h.join().unwrap()
    });
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["reason"], "row_moved", "{d}");
    assert!(d.get("kind").is_none(), "the outer error is not the refusal: {d}");
    assert_eq!(d["cause"]["detail"]["kind"], "PushRefused", "{d}");
    assert_eq!(d["push"]["kind"], "Refused", "{d}");
    assert_eq!(d["checks_ran"], 1, "{d}");
    assert_eq!(d["commit"].as_str().unwrap(), fx.read_ref("tool", CHAIN).unwrap());
    assert_eq!(last_row(&fx).outcome.unwrap()["note"], "moved", "the other writer's outcome stays");
}

/// Review P0-2: a refused publish does not block the next one; the next
/// publish pushes the whole chain (the refused commit and its own).
#[test]
fn refused_publish_does_not_block_the_next() {
    let fx = fx();
    hook(&fx, "echo 'denied by policy' >&2\nexit 1");
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 41\n");
    let e = run(&fx, &g, None).unwrap_err();
    let first = commit_of(&e);
    fs::remove_file(origin_dir(&fx).join("hooks/pre-receive")).unwrap();
    let (_s2, g2) = edit(&fx, "src/other.ts", b"export const b = 41\n");
    let p = run(&fx, &g2, None).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    assert!(fx.git_of("tool").is_ancestor(&strata_types::CommitOid::try_from(first).unwrap(), &p.commit).unwrap());
}

/// Review P0-2: an `Unknown` row whose commit is now on the remote is
/// reconciled (`Done`, `reconciled`) by the next publish of the repo; one
/// whose commit is not on the remote keeps refusing. Nothing is replayed.
#[test]
fn unknown_row_is_reconciled_when_the_remote_has_the_commit() {
    let fx = fx();
    let t0 = Instant::now();
    hook(&fx, "sleep 2\nexit 1");
    seams::set_push_timeout(fx.git_of("tool"), Some(Duration::from_millis(300)));
    let (_s, g) = edit(&fx, "src/index.ts", b"export const a = 42\n");
    let e = run(&fx, &g, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let c = commit_of(&e);
    let id = last_row(&fx).group.id;
    assert_eq!(row(&fx, id).phase, PublishPhase::Unknown);
    seams::set_push_timeout(fx.git_of("tool"), None);
    std::thread::sleep(Duration::from_secs(3).saturating_sub(t0.elapsed()));
    fs::remove_file(origin_dir(&fx).join("hooks/pre-receive")).unwrap();

    // Not on the remote yet: still refused, still Unknown.
    let (_s2, g2) = edit(&fx, "src/other.ts", b"export const b = 42\n");
    let e = run(&fx, &g2, None).unwrap_err();
    assert_eq!(reason(&e), Some("publish_unknown"), "{e:?}");
    assert_eq!(row(&fx, id).phase, PublishPhase::Unknown);

    // The human pushes the chain; the next publish reconciles and runs.
    let st = staging_for(&fx, &c);
    let cfg = fx.config.repos[&repo_name("tool")].clone();
    assert_eq!(push(fx.git_of("tool"), &cfg, &repo_name("tool"), &st, None).unwrap(), PushOutcome::Pushed);
    let p = run(&fx, &g2, None).unwrap();
    assert_eq!(p.push, Some(PushOutcome::Pushed));
    let r = row(&fx, id);
    assert_eq!(r.phase, PublishPhase::Done);
    let o = r.outcome.unwrap();
    assert_eq!(o["commit"], c.as_str(), "{o}");
    assert_eq!(o["push"]["kind"], "Pushed", "{o}");
    assert!(o["reconciled"]["remote"].is_string(), "{o}");
}

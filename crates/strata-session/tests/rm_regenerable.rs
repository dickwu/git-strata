//! rm-followups §1.8 (B2): `rm.regenerable` entries of a tracked, healthy
//! workspace do not block `rm`; they are checkpointed and listed.

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use strata_session::{NewReport, Sessions};
use strata_store::Lifecycle;
use strata_types::{CheckpointReason, Code, SessionId, StrataError};
use support::*;

fn ws(rep: &NewReport, repo: &str) -> PathBuf {
    rep.manifest.repos[&repo_name(repo)].path.clone()
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail.as_ref().unwrap().as_object().unwrap()
}

fn paths(v: &Value) -> Vec<String> {
    let mut p: Vec<String> = v["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_owned()).collect();
    p.sort();
    p
}

fn lifecycle(fx: &Fx, id: &SessionId) -> Lifecycle {
    fx.store().session(id).unwrap().unwrap().lifecycle
}

fn set(s: &Sessions<'_>, repo: &str, list: &[&str]) {
    s.set_regenerable(&repo_name(repo), list.iter().map(|p| p.to_string()).collect());
}

#[test]
fn regenerable_does_not_block_rm_and_is_checkpointed() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    set(&s, "tool", &["gen/**", "!gen/keep.json", "src/index.ts"]);

    // Added (gen/a.json) and Modified (src/index.ts) regenerable files only.
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let w = ws(&rep, "tool");
    fx.sb.write(&w, "gen/a.json", b"{}\n");
    fs::write(w.join("src/index.ts"), b"regenerated\n").unwrap();
    let r = s.remove(&id, false, None).unwrap();
    assert!(!r.discarded);
    let mut got: Vec<String> = r.regenerable[&repo_name("tool")].iter().map(|p| p.as_str().to_owned()).collect();
    got.sort();
    assert_eq!(got, vec!["gen/a.json", "src/index.ts"]);
    let cp = fx.store().checkpoint(&id, r.checkpoints[&repo_name("tool")]).unwrap();
    assert_eq!(cp.reason, CheckpointReason::BeforeRemove);
    let mut cp_paths: Vec<&str> = cp.entries.iter().map(|e| e.path.as_str()).collect();
    cp_paths.sort();
    assert_eq!(cp_paths, vec!["gen/a.json", "src/index.ts"], "the checkpoint keeps them");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed);

    // A negated path still blocks; detail lists both kinds.
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let w = ws(&rep, "tool");
    fx.sb.write(&w, "gen/a.json", b"{}\n");
    fx.sb.write(&w, "gen/keep.json", b"{}\n");
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(paths(&detail(&e)["repos"]["tool"]), vec!["gen/keep.json"]);
    assert_eq!(paths(&detail(&e)["regenerable"]["tool"]), vec!["gen/a.json"]);
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Active);
}

#[test]
fn regenerable_never_matches_deleted_or_install_inputs() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    // Every file of the project matches; inputs and deletions still block.
    set(&s, "tool", &["**", "!nothing"]);
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let w = ws(&rep, "tool");
    fs::write(w.join("bun.lock"), b"lock v2\n").unwrap();
    fs::write(w.join("patches/a.patch"), b"patch b\n").unwrap();
    fs::remove_file(w.join("src/index.ts")).unwrap();
    fx.sb.write(&w, "src/new.ts", b"new\n");
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(paths(&detail(&e)["repos"]["tool"]), vec!["bun.lock", "patches/a.patch", "src/index.ts"]);
    assert_eq!(paths(&detail(&e)["regenerable"]["tool"]), vec!["src/new.ts"]);
}

#[test]
fn regenerable_ignored_for_untracked_repo() {
    let fx = Fx::new();
    let s = fx.sessions();
    set(&s, "front", &["src/**"]);
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    fx.sb.write(&ws(&rep, "front"), "src/stray.ts", b"stray\n");
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(detail(&e)["untracked_repos"], serde_json::json!(["front"]));
    assert_eq!(paths(&detail(&e)["repos"]["front"]), vec!["src/stray.ts"]);
    assert!(detail(&e).get("regenerable").is_none(), "{e:?}");
}

#[test]
fn regenerable_ignored_for_unhealthy_workspace() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    set(&s, "tool", &["gen/**"]);
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let w = ws(&rep, "tool");
    fx.sb.write(&w, "gen/a.json", b"{}\n");
    // The workspace's .git link is gone: unhealthy (missing).
    let dotgit = w.join(".git");
    if dotgit.is_dir() {
        fs::remove_dir_all(&dotgit).unwrap();
    } else {
        fs::remove_file(&dotgit).unwrap();
    }
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let kinds: Vec<&str> =
        detail(&e)["unresolved"].as_array().unwrap().iter().map(|u| u["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["workspace_missing"]);
    assert!(detail(&e).get("regenerable").is_none(), "{e:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Active);
}

#[test]
fn regenerable_empty_list_changes_nothing() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    fx.sb.write(&ws(&rep, "tool"), "gen/a.json", b"{}\n");
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(paths(&detail(&e)["repos"]["tool"]), vec!["gen/a.json"]);
    assert!(detail(&e).get("regenerable").is_none());
}

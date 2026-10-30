//! session.md D51 (`new` with only the named repos), D52 (`add`), D53
//! (fetch only the session's repos; the fetch window), D54 (caches cloned
//! behind the workspace clones).

mod support;

use std::fs;
use std::time::Duration;

use strata_session::{ADD_STAGING_PREFIX, Layout, SessionSeam};
use strata_types::{Code, StrataError};
use support::*;

fn push(fx: &Fx, repo: &str, files: &[(&str, &[u8])], msg: &str) -> String {
    let main = fx.main(repo);
    let c = fx.sb.commit(&main, files, msg);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    c
}

fn break_origin(fx: &Fx, repo: &str) {
    fx.sb.git(&fx.main(repo), &["remote", "set-url", "origin", "/nonexistent/origin.git"]);
}

#[test]
fn new_only_lays_out_and_fetches_just_the_named_repos() {
    let fx = Fx::new();
    // front's origin is gone: the full layout must fetch it and fails, the
    // slim one never touches it.
    break_origin(&fx, "front");
    let s = fx.sessions();
    assert!(s.new(None, &[fx.rn("tool")]).is_err());
    let rep = s.new_with(None, &[fx.rn("tool")], Layout::Only).unwrap();
    let id = rep.manifest.id.clone();
    assert_eq!(rep.manifest.repos.keys().cloned().collect::<Vec<_>>(), vec![fx.rn("tool")]);
    assert!(rep.manifest.repos[&fx.rn("tool")].tracked);
    assert!(rep.manifest.root.join("tool/.git").exists());
    assert!(!rep.manifest.root.join("front").exists());
    assert!(fx.pins("tool").contains(id.as_str()));
    assert!(!fx.pins("front").contains(id.as_str()));
    let rec = fx.store().session(&id).unwrap().unwrap();
    assert_eq!(rec.manifest.repos.len(), 1);
}

#[test]
fn new_only_and_default_layout_differ_only_in_members() {
    let fx = Fx::new();
    let s = fx.sessions();
    let full = s.new(None, &[fx.rn("tool")]).unwrap();
    assert_eq!(full.manifest.repos.len(), 2);
    assert!(!full.manifest.repos[&fx.rn("front")].tracked);
    let slim = s.new_with(None, &[fx.rn("front"), fx.rn("tool")], Layout::Only).unwrap();
    assert_eq!(slim.manifest.repos.len(), 2);
    assert!(slim.manifest.repos.values().all(|w| w.tracked));
}

#[test]
fn add_builds_a_tracked_workspace_with_caches_and_is_idempotent() {
    let mut fx = Fx::new();
    let main = fx.main("front");
    fx.cfg_mut("front").seed_caches = rps(&[".next/cache"]);
    push(&fx, "front", &[(".gitignore", b"node_modules/\n.next/\n")], "ignore");
    fx.sb.write(&main, ".next/cache/c.json", b"{}");
    let s = fx.sessions();
    let rep = s.new_with(None, &[fx.rn("tool")], Layout::Only).unwrap();
    let id = rep.manifest.id.clone();

    let a = s.add(&id, &[fx.rn("front")]).unwrap();
    assert_eq!(a.session, id);
    assert!(a.already.is_empty());
    let w = &a.added[&fx.rn("front")];
    assert!(w.tracked);
    assert_eq!(w.path, rep.manifest.root.join("front"));
    assert_eq!(fs::read(w.path.join(".next/cache/c.json")).unwrap(), b"{}");
    // A real worktree on the session's shadow repo, at the pinned head.
    assert_eq!(fx.sb.git(&w.path, &["rev-parse", "HEAD"]).trim(), w.head.as_str());
    assert!(fx.pins("front").contains(id.as_str()));
    let rec = fx.store().session(&id).unwrap().unwrap();
    assert_eq!(rec.manifest.repos.len(), 2);
    assert_eq!(&rec.manifest.repos[&fx.rn("front")], w);
    // No staging left behind.
    let names: Vec<String> = fs::read_dir(&rep.manifest.root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|n| n.starts_with(ADD_STAGING_PREFIX)), "{names:?}");

    // Again: nothing to do.
    let b = s.add(&id, &[fx.rn("front"), fx.rn("tool")]).unwrap();
    assert!(b.added.is_empty());
    assert_eq!(b.already.len(), 2);

    // The session still removes cleanly with the added repo.
    s.remove(&id, false, None).unwrap();
    assert!(!fx.pins("front").contains(id.as_str()));
}

#[test]
fn add_refuses_before_any_write() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new_with(None, &[fx.rn("tool")], Layout::Only).unwrap();
    let id = rep.manifest.id.clone();
    let e = s.add(&id, &[]).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    let e = s.add(&sid("s-zzzzzzzz"), &[fx.rn("front")]).unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession);
    let e = s.add(&id, &[repo_name("nope")]).unwrap_err();
    assert_eq!(e.code, Code::NoSuchRepo);
    // A leftover in the way.
    fs::create_dir(rep.manifest.root.join("front")).unwrap();
    let e = s.add(&id, &[fx.rn("front")]).unwrap_err();
    assert_eq!(e.code, Code::Exists);
    assert_eq!(reason(&e), Some("add_leftover"));
    fs::remove_dir(rep.manifest.root.join("front")).unwrap();
    assert_eq!(fx.store().session(&id).unwrap().unwrap().manifest.repos.len(), 1);
    // Not active.
    s.remove(&id, false, None).unwrap();
    let e = s.add(&id, &[fx.rn("front")]).unwrap_err();
    assert!(matches!(e.code, Code::InvalidInput | Code::NoSuchSession), "{e:?}");
}

#[test]
fn a_failed_add_records_nothing_unpins_and_names_what_it_kept() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new_with(None, &[fx.rn("tool")], Layout::Only).unwrap();
    let id = rep.manifest.id.clone();
    s.arm(SessionSeam::FailAt { phase: "add.build@front".into(), errno: libc::EIO });
    let e = s.add(&id, &[fx.rn("front")]).unwrap_err();
    assert!(!matches!(e.code, Code::Exists | Code::NoSuchRepo), "{e:?}");
    assert_eq!(fx.store().session(&id).unwrap().unwrap().manifest.repos.len(), 1);
    assert!(!fx.pins("front").contains(id.as_str()));
    // The seam fires before building, so nothing is kept; a later add works.
    let a = s.add(&id, &[fx.rn("front")]).unwrap();
    assert_eq!(a.added.len(), 1);
}

#[test]
fn the_fetch_window_reuses_a_recent_fetch_only_when_set() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let tool = fx.rn("tool");
    let first = s.new_with(None, std::slice::from_ref(&tool), Layout::Only).unwrap();
    // Default window 0: every new fetches.
    let c1 = push(&fx, "tool", &[("a.txt", b"1")], "one");
    let second = s.new_with(None, std::slice::from_ref(&tool), Layout::Only).unwrap();
    assert_eq!(second.manifest.repos[&tool].head.as_str(), c1);
    assert_ne!(first.manifest.repos[&tool].head, second.manifest.repos[&tool].head);
    // A window: the next new reuses that fetch and sees the old tip.
    s.set_fetch_window(Duration::from_secs(600));
    let third = s.new_with(None, std::slice::from_ref(&tool), Layout::Only).unwrap();
    let _c2 = push(&fx, "tool", &[("a.txt", b"2")], "two");
    let fourth = s.new_with(None, std::slice::from_ref(&tool), Layout::Only).unwrap();
    assert_eq!(fourth.manifest.repos[&tool].head, third.manifest.repos[&tool].head);
    // Back to 0: fetched again.
    s.set_fetch_window(Duration::ZERO);
    let fifth = s.new_with(None, std::slice::from_ref(&tool), Layout::Only).unwrap();
    assert_ne!(fifth.manifest.repos[&tool].head, fourth.manifest.repos[&tool].head);
}

#[test]
fn not_in_session_names_the_add_command() {
    let e = StrataError::not_in_session("s-abcdefgh", "front");
    assert_eq!(e.code, Code::NoSuchRepo);
    assert!(e.message.contains("git-strata add --session s-abcdefgh --repo front"), "{}", e.message);
    assert_eq!(reason(&e), Some("not_in_session"));
    assert_eq!(detail_str(&e, "fix"), Some("git-strata add --session s-abcdefgh --repo front"));
}

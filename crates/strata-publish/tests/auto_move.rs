//! rm-followups §1.4 (sub-step 2.2b): the `Auto` post-publish move's gates.
//! G0 state, G1 open files, G2 quiet window, G3 read-only projection with
//! `rm.regenerable`. Not wired into publish yet; called directly.

mod support;

use std::fs;
use std::process::{Child, Command};
use std::time::Duration;

use strata_publish::{MovePolicy, Published, RebaseReport, publish, rebase_session_with};
use strata_types::{BaseGen, ReleaseGroup, SessionId};
use support::*;

fn fx(quiet: u64, regenerable: &[&str]) -> Fx {
    let mut fx = Fx::with(&["tool"]);
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.rebase.quiet_secs = quiet;
    rc.rm.regenerable = regenerable.iter().map(|s| s.to_string()).collect();
    fx.git_of("tool").fetch("origin", "main").unwrap();
    fx
}

fn run(fx: &Fx, g: &ReleaseGroup) -> Published {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    publish(&fx.ctx(&d, &ss), g, None).unwrap()
}

fn session_with(fx: &Fx, files: &[(&str, &[u8])]) -> (SessionId, ReleaseGroup) {
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", files);
    let c = fx.checkpoint(&s, "tool");
    let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
    (s.clone(), group(vec![batch(&s, "tool", c.id, &paths)]))
}

/// A publishes `a_files`; B (edited with `b_files` before) is moved `Auto`
/// onto A's commit.
fn auto(fx: &Fx, b: &SessionId, p: &Published) -> RebaseReport {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let old = fx.base_of(b, "tool");
    let ng = BaseGen { repo: repo_name("tool"), tree: fx.git_of("tool").tree_of(&p.commit).unwrap(), deps: old.deps };
    rebase_session_with(&ctx, b.clone(), &repo_name("tool"), ng, p.commit.clone(), MovePolicy::Auto).unwrap()
}

fn reason(r: &RebaseReport) -> &str {
    match r {
        RebaseReport::Deferred { reason, .. } => reason,
        r => panic!("not deferred: {r:?}"),
    }
}

struct Holder(Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn auto_defers_recent_writes_inside_the_quiet_window() {
    let fx = fx(120, &[]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("b.txt", b"bravo\n")]);
    let (_a, ga) = session_with(&fx, &[("a.txt", b"alpha\n")]);
    let p = run(&fx, &ga);
    let before = fx.base_of(&b, "tool");
    let r = auto(&fx, &b, &p);
    assert_eq!(reason(&r), "recent_writes", "{r:?}");
    let RebaseReport::Deferred { detail, .. } = &r else { unreachable!() };
    assert_eq!(detail["quiet_secs"], 120, "{detail}");
    assert_eq!(fx.base_of(&b, "tool"), before, "a deferred attempt leaves the workspace alone");
}

#[test]
fn auto_moves_an_idle_workspace() {
    let fx = fx(0, &[]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("b.txt", b"bravo\n")]);
    let (_a, ga) = session_with(&fx, &[("a.txt", b"alpha\n")]);
    let p = run(&fx, &ga);
    std::thread::sleep(Duration::from_millis(20));
    let r = auto(&fx, &b, &p);
    assert!(matches!(r, RebaseReport::Moved { .. }), "{r:?}");
    assert_eq!(fs::read(fx.ws(&b, "tool").join("a.txt")).unwrap(), b"alpha\n");
    assert_eq!(fs::read(fx.ws(&b, "tool").join("b.txt")).unwrap(), b"bravo\n");
}

#[test]
fn auto_defers_open_files_anywhere_in_the_workspace() {
    let fx = fx(0, &[]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("b.txt", b"bravo\n")]);
    let (_a, ga) = session_with(&fx, &[("a.txt", b"alpha\n")]);
    let p = run(&fx, &ga);
    // A shell whose cwd is the workspace root (not a deps directory).
    let h = Holder(Command::new("/bin/sleep").arg("300").current_dir(fx.ws(&b, "tool")).spawn().unwrap());
    std::thread::sleep(Duration::from_millis(200));
    let r = auto(&fx, &b, &p);
    assert_eq!(reason(&r), "open_files", "{r:?}");
    let RebaseReport::Deferred { detail, .. } = &r else { unreachable!() };
    assert_eq!(detail["holders"][0]["pid"], h.0.id() as i64, "{detail}");
    drop(h);
}

#[test]
fn auto_defers_a_conflict_outside_regenerable() {
    let fx = fx(0, &[]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("shared.txt", b"bravo\n")]);
    let (_a, ga) = session_with(&fx, &[("shared.txt", b"alpha\n")]);
    let p = run(&fx, &ga);
    let before = fx.base_of(&b, "tool");
    let r = auto(&fx, &b, &p);
    assert_eq!(reason(&r), "conflicts", "{r:?}");
    let RebaseReport::Deferred { detail, .. } = &r else { unreachable!() };
    assert_eq!(detail["paths"][0], "shared.txt", "{detail}");
    assert_eq!(fx.base_of(&b, "tool"), before);
    assert_eq!(fs::read(fx.ws(&b, "tool").join("shared.txt")).unwrap(), b"bravo\n");
}

#[test]
fn regenerable_conflict_moves_and_keeps_session_bytes() {
    let fx = fx(0, &["shared.txt"]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("shared.txt", b"bravo\n")]);
    let (_a, ga) = session_with(&fx, &[("shared.txt", b"alpha\n"), ("a.txt", b"alpha\n")]);
    let p = run(&fx, &ga);
    let r = auto(&fx, &b, &p);
    match &r {
        RebaseReport::Moved { regenerable_kept, .. } => assert_eq!(regenerable_kept, &vec![rp("shared.txt")]),
        r => panic!("{r:?}"),
    }
    assert_eq!(fs::read(fx.ws(&b, "tool").join("shared.txt")).unwrap(), b"bravo\n", "session bytes kept");
    assert_eq!(fs::read(fx.ws(&b, "tool").join("a.txt")).unwrap(), b"alpha\n", "moved");
}

#[test]
fn explicit_policy_ignores_the_gates() {
    let fx = fx(120, &[]);
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("b.txt", b"bravo\n")]);
    let (_a, ga) = session_with(&fx, &[("a.txt", b"alpha\n")]);
    let p = run(&fx, &ga);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let old = fx.base_of(&b, "tool");
    let ng = BaseGen { repo: repo_name("tool"), tree: fx.git_of("tool").tree_of(&p.commit).unwrap(), deps: old.deps };
    let r = rebase_session_with(&ctx, b.clone(), &repo_name("tool"), ng, p.commit.clone(), MovePolicy::Explicit).unwrap();
    assert!(matches!(r, RebaseReport::Moved { .. }), "{r:?}");
}

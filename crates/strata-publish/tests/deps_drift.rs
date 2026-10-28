//! deps-drift r8 §3: the checks run with the deps of the staged install
//! inputs. A publish whose inputs have no sealed set installs one (R1 closes
//! `Drift`/`deps_install_needed`, R2 publishes), a sealed set is
//! substituted, and either way the publish only warns.

mod support;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use strata_publish::seams::{self, FailPoint, PublishFault};
use strata_publish::{Published, WarningKind, publish, stage};
use strata_store::PublishPhase;
use strata_types::{Code, CommitOid, ReleaseGroup, SessionId, StrataError};
use support::*;

const ORIGIN: &str = "refs/strata/remotes/origin/main";

/// The install also records the inputs it saw, so a check can tell which
/// set it runs with.
const MARKED_INSTALL: &str = r#"mkdir -p node_modules/x node_modules/.bin || exit 1
echo 'module.exports = 1' > node_modules/x/index.js
echo '{"name":"x","version":"1.0.0"}' > node_modules/x/package.json
ln -sf ../x/index.js node_modules/.bin/x
cat package.json bun.lock > node_modules/inputs.txt
echo "installed"
"#;

const PKG2: &str = r#"{
  "name": "tool",
  "version": "1.0.87",
  "dependencies": {
    "x": "^1.0.0",
    "y": "^2.0.0"
  }
}
"#;

/// A fixture whose checks compare the deps' marker with the staged inputs.
fn fx() -> Fx {
    let mut fx = Fx::with(&["tool"]);
    fx.sb.set_install_script(MARKED_INSTALL);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    let st = fx.sb.state.display().to_string();
    fx.sb.write_program(
        "same-deps",
        &format!(
            "#!/bin/sh\necho \"$PWD\" >> '{st}/checks.log'\ncat package.json bun.lock | cmp -s - node_modules/inputs.txt || {{ echo 'deps do not match the inputs'; exit 9; }}\n"
        ),
    );
    fx.cfg_mut("tool").checks = vec![fx.sb.bin.join("same-deps").display().to_string()];
    fx
}

fn select(fx: &Fx, s: &SessionId, files: &[(&str, &[u8])]) -> ReleaseGroup {
    fx.edit(s, "tool", files);
    let c = fx.checkpoint(s, "tool");
    let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
    group(vec![batch(s, "tool", c.id, &paths)])
}

fn run(fx: &Fx, g: &ReleaseGroup) -> strata_publish::Result<Published> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    publish(&ctx, g, None)
}

fn rows(fx: &Fx) -> Vec<strata_store::PublishRecord> {
    fx.store().publishes().unwrap()
}

fn outcome(r: &strata_store::PublishRecord) -> &Value {
    r.outcome.as_ref().expect("an outcome")
}

fn snapshot_gone(fx: &Fx, row: u64) {
    let p = strata_deps::snapshot_dir(&fx.config.paths, &repo_name("tool"), row);
    assert!(!p.exists(), "snapshot {} left", p.display());
    let inputs = fx.config.paths.deps().join("tool").join(".inputs");
    if inputs.exists() {
        assert_eq!(fs::read_dir(&inputs).unwrap().count(), 0, "no snapshot left");
    }
}

fn warning(p: &Published, kind: WarningKind) -> &Value {
    &p.warnings.iter().find(|w| w.kind == kind).unwrap_or_else(|| panic!("no {kind:?} in {:?}", p.warnings)).detail
}

fn detail(e: &StrataError) -> &Value {
    e.detail.as_ref().expect("detail")
}

fn marker(fx: &Fx, s: &SessionId) -> String {
    fs::read_to_string(fx.ws(s, "tool").join("node_modules/inputs.txt")).unwrap()
}

fn initial_inputs() -> String {
    format!("{PACKAGE_JSON}lock v1\n")
}

/// Test 1: changed inputs, nothing sealed → cold install, R1 `Drift`, R2
/// publishes with the installed set, `DepsReinstalled`.
#[test]
fn unswitched_inputs_are_installed_and_published() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let pin = fx.base_of(&s, "tool").deps;
    let installs = fx.sb.install_count();
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    let p = run(&fx, &g).expect("publish");
    assert_eq!(fx.sb.install_count(), installs + 1, "one cold install");
    let all = rows(&fx);
    assert_eq!(all.len(), 2, "{all:?}");
    let (r1, r2) = (&all[0], &all[1]);
    assert_eq!(r1.phase, PublishPhase::Done);
    assert_eq!(outcome(r1)["stopped"], "Drift", "{:?}", r1.outcome);
    assert_eq!(outcome(r1)["deps"]["reason"], "deps_install_needed", "{:?}", r1.outcome);
    assert_eq!(r2.phase, PublishPhase::Done);
    assert_eq!(p.id, r2.group.id);
    assert!(outcome(r2)["commit"].is_string(), "{:?}", r2.outcome);
    let w = warning(&p, WarningKind::DepsReinstalled);
    assert_eq!(w["cause"], "unswitched", "{w}");
    assert_eq!(w["first_row"], r1.group.id, "{w}");
    assert_eq!(w["pinned"], pin.as_str(), "{w}");
    assert_ne!(w["used"], pin.as_str(), "{w}");
    assert!(w["fix"].as_str().unwrap().contains("--switch"), "{w}");
    assert!(w["message"].as_str().unwrap().contains("install inputs differ"), "{w}");
    // The checks saw the new set; the workspace kept its own.
    let log = fs::read_to_string(fx.sb.state.join("checks.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "the checks ran once: {log}");
    assert_eq!(marker(&fx, &s), initial_inputs());
    assert_eq!(fx.base_of(&s, "tool").deps, pin, "the session's pin is unchanged");
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    snapshot_gone(&fx, r1.group.id);
}

/// Tests 2 and 4: the same inputs again (main already has them, the session
/// only edits source) → the sealed set is substituted; nothing installs.
#[test]
fn sealed_set_is_substituted_without_install() {
    let fx = fx();
    let a = fx.new_session(&["tool"]);
    let b = fx.new_session(&["tool"]);
    run(&fx, &select(&fx, &a, &[("package.json", PKG2.as_bytes())])).expect("first publish");
    let installs = fx.sb.install_count();
    let before = rows(&fx).len();
    let p = run(&fx, &select(&fx, &b, &[("src/index.ts", b"export const b = 1\n")])).expect("second publish");
    assert_eq!(fx.sb.install_count(), installs, "no install");
    assert_eq!(rows(&fx).len(), before + 1, "one row");
    let w = warning(&p, WarningKind::DepsSubstituted);
    assert_eq!(w["cause"], "base_moved", "{w}");
    assert!(w.get("fix").is_none(), "{w}");
    assert!(w.get("after_install").is_none(), "{w}");
}

/// Test 4 (no record): main moved its inputs outside strata → cold install
/// and `DepsReinstalled` with `base_moved`, not `selection`.
#[test]
fn moved_base_without_a_set_installs() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    fx.land_on_origin("tool", "main", &[("bun.lock", b"lock v2\n")], "lock");
    let installs = fx.sb.install_count();
    let p = run(&fx, &select(&fx, &s, &[("src/index.ts", b"export const c = 1\n")])).expect("publish");
    assert_eq!(fx.sb.install_count(), installs + 1);
    let w = warning(&p, WarningKind::DepsReinstalled);
    assert_eq!(w["cause"], "base_moved", "{w}");
}

/// Test 5: a failed cold install keeps its code; R1 stays `Done`; a retry
/// meets `previous_attempt` and does not install again.
#[test]
fn failed_cold_install_returns_its_own_error() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let origin = fx.origin_head("tool", "main");
    fx.sb.set_install_script("echo broken; exit 1\n");
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    let installs = fx.sb.install_count();
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = detail(&e);
    assert_eq!(d["reason"], "install_failed", "{d}");
    assert_eq!(d["stage"], "deps_install", "{d}");
    let all = rows(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(d["publish"], all[0].group.id, "{d}");
    assert!(d["deps_drift"]["cause"].is_string(), "{d}");
    assert_eq!(all[0].phase, PublishPhase::Done);
    assert_eq!(outcome(&all[0])["stopped"], "Drift");
    assert!(fx.store().open_publishes().unwrap().is_empty(), "no Unknown row");
    assert_eq!(fx.origin_head("tool", "main"), origin, "nothing pushed");
    assert_eq!(fx.sb.install_count(), installs + 1);
    snapshot_gone(&fx, all[0].group.id);
    let again = run(&fx, &g).unwrap_err();
    assert_eq!(again.code, Code::Unknown, "{again:?}");
    assert_eq!(detail(&again)["reason"], "previous_attempt", "{again:?}");
    assert_eq!(fx.sb.install_count(), installs + 1, "no second install");
    assert!(fx.store().open_publishes().unwrap().is_empty());
}

/// Test 7: main's inputs move again during the cold install → R2
/// `Drift`/`deps_drift`; install ran once.
#[test]
fn inputs_moving_during_the_install_is_drift() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    let installs = fx.sb.install_count();
    let pause = seams::pause(fx.git_of("tool"), PublishFault::PauseBeforeColdInstall);
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| run(&fx, &g));
        pause.wait_until_paused();
        fx.land_on_origin("tool", "main", &[("bun.lock", b"lock v3\n")], "lock");
        pause.release();
        h.join().unwrap()
    });
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::Drift, "{e:?}");
    let d = detail(&e);
    assert_eq!(d["reason"], "deps_drift", "{d}");
    assert!(d["install"]["set"].is_string(), "{d}");
    assert_eq!(fx.sb.install_count(), installs + 1);
    let all = rows(&fx);
    assert_eq!(all.len(), 2);
    for r in &all {
        assert_eq!(r.phase, PublishPhase::Done);
        assert_eq!(outcome(r)["stopped"], "Drift");
    }
    assert_eq!(outcome(&all[1])["deps"]["reason"], "deps_drift");
    snapshot_gone(&fx, all[0].group.id);
}

/// Test 9: the cold install dies (seam) → no `Running`/`Unknown` row.
#[test]
fn interrupted_cold_install_leaves_no_open_row() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    seams::fail_once(fx.git_of("tool"), FailPoint::ColdInstall);
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(detail(&e)["stage"], "deps_install", "{e:?}");
    assert!(fx.store().open_publishes().unwrap().is_empty());
    snapshot_gone(&fx, rows(&fx)[0].group.id);
    // Nothing was recorded for the set, so the next publish installs.
    run(&fx, &g).expect("retry");
}

/// Codex impl-r1 B2: a panic in the cold install (the daemon catches
/// handler panics) still removes R1's snapshot.
#[test]
fn panicking_cold_install_removes_the_snapshot() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    seams::fail_once(fx.git_of("tool"), FailPoint::ColdInstallPanic);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&fx, &g)));
    assert!(r.is_err(), "the injected panic unwinds");
    let r1 = rows(&fx).remove(0);
    assert_eq!(r1.phase, PublishPhase::Done);
    snapshot_gone(&fx, r1.group.id);
    run(&fx, &g).expect("retry");
}

/// Codex impl-r1 B2: a panic while the snapshot is built (under the locks)
/// leaves no snapshot behind.
#[test]
fn panicking_snapshot_build_removes_the_snapshot() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    strata_deps::test_seams::after_snapshot_clone(Box::new(|_, _| panic!("injected snapshot panic")));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&fx, &g)));
    strata_deps::test_seams::reset();
    assert!(r.is_err(), "the injected panic unwinds");
    snapshot_gone(&fx, rows(&fx)[0].group.id);
}

/// Test 11: no checks → no selection, no install, one row, no warning.
#[test]
fn no_checks_skip_selection() {
    let mut fx = fx();
    fx.cfg_mut("tool").checks = vec![];
    let s = fx.new_session(&["tool"]);
    let installs = fx.sb.install_count();
    let p = run(&fx, &select(&fx, &s, &[("package.json", PKG2.as_bytes())])).expect("publish");
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    // No cold install; the post-publish rebase builds main's new set
    // (base.rs step 5), as it always has.
    assert!(fx.sb.install_count() <= installs + 1);
    assert_eq!(rows(&fx).len(), 1);
    snapshot_gone(&fx, p.id);
}

/// Test 12: a package.json `version` bump is not an input change.
#[test]
fn package_version_bump_is_not_drift() {
    let mut fx = fx();
    fx.cfg_mut("tool").checks = vec![fx.sb.bin.join("same-deps").display().to_string().replace("same-deps", "true-check")];
    fx.sb.write_program("true-check", "#!/bin/sh\nexit 0\n");
    let s = fx.new_session(&["tool"]);
    let installs = fx.sb.install_count();
    let bumped = PACKAGE_JSON.replace("1.0.87", "1.0.88");
    let p = run(&fx, &select(&fx, &s, &[("package.json", bumped.as_bytes())])).expect("publish");
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    assert_eq!(fx.sb.install_count(), installs);
    assert_eq!(rows(&fx).len(), 1);
}

/// Test 14: the standalone stage keeps P even when a set could be
/// substituted.
#[test]
fn standalone_stage_keeps_the_pinned_set() {
    let fx = fx();
    let a = fx.new_session(&["tool"]);
    let b = fx.new_session(&["tool"]);
    run(&fx, &select(&fx, &a, &[("package.json", PKG2.as_bytes())])).expect("seal the set");
    let installs = fx.sb.install_count();
    fx.git_of("tool").fetch("origin", "main").unwrap();
    let base = CommitOid::try_from(fx.read_ref("tool", ORIGIN).unwrap()).unwrap();
    let g = select(&fx, &b, &[("src/index.ts", b"export const s = 1\n")]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let st = stage(&ctx, &g, &base).expect("stage");
    assert!(st.deps_warning.is_none());
    assert_eq!(fs::read_to_string(st.worktree.join("node_modules/inputs.txt")).unwrap(), initial_inputs());
    assert_eq!(fx.sb.install_count(), installs);
}

/// Test 13: R1's layout cannot be removed → `Unknown` row, snapshot gone,
/// no cold install.
#[test]
fn stuck_layout_is_unknown_and_does_not_install() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes()), ("inner/x", b"x\n")]);
    let installs = fx.sb.install_count();
    let done = Arc::new(AtomicBool::new(false));
    let once = done.clone();
    let root = fx.store().session(&s).unwrap().unwrap().manifest.root;
    let scan = root.clone();
    // Only once R1's input snapshot exists: the removal after the selection.
    let inputs = fx.config.paths.deps().join("tool").join(".inputs");
    strata_apfs::seams::set_before_open(Some(Box::new(move |_, name| {
        if name != "inner" || once.load(Ordering::SeqCst) {
            return;
        }
        if fs::read_dir(&inputs).map(|mut d| d.next().is_none()).unwrap_or(true) {
            return;
        }
        let Some(p) = layout_inner(&scan) else { return };
        once.store(true, Ordering::SeqCst);
        fs::rename(&p, p.with_file_name("inner.gone")).unwrap();
        std::os::unix::fs::symlink("/nonexistent", &p).unwrap();
    })));
    let r = run(&fx, &g);
    strata_apfs::seams::set_before_open(None);
    let e = r.unwrap_err();
    assert!(done.load(Ordering::SeqCst), "the seam fired");
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let all = rows(&fx);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].phase, PublishPhase::Unknown);
    assert_eq!(fx.sb.install_count(), installs, "no cold install");
    snapshot_gone(&fx, all[0].group.id);
    for e in fs::read_dir(root).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with(".strata-publish-") {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// The real `inner` directory of a layout under the session root `root`.
fn layout_inner(root: &Path) -> Option<std::path::PathBuf> {
    for e in fs::read_dir(root).ok()?.flatten() {
        if !e.file_name().to_string_lossy().starts_with(".strata-publish-") {
            continue;
        }
        let mut stack = vec![(e.path(), 0)];
        while let Some((d, depth)) = stack.pop() {
            for c in fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = c.path();
                let ft = c.file_type().ok()?;
                if c.file_name() == "inner" && ft.is_dir() {
                    return Some(p);
                }
                if ft.is_dir() && depth < 3 {
                    stack.push((p, depth + 1));
                }
            }
        }
    }
    None
}

/// Test 20: the selection fails (snapshot hashes wrong) → R1 `Done`,
/// `Drift`/`deps_select_failed`; the next publish is not blocked.
#[test]
fn selection_failure_is_drift_and_does_not_block() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    strata_deps::test_seams::snapshot_hash_wrong();
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::Drift, "{e:?}");
    let d = detail(&e);
    assert_eq!(d["reason"], "deps_select_failed", "{d}");
    assert_eq!(d["cause"]["code"], "Unknown", "{d}");
    let r1 = rows(&fx).remove(0);
    assert_eq!(r1.phase, PublishPhase::Done);
    assert_eq!(outcome(&r1)["stopped"], "Drift");
    snapshot_gone(&fx, r1.group.id);
    run(&fx, &g).expect("the next publish proceeds");
}

/// Test 21: the snapshot's tree is swapped for a symlink into the
/// workspace → `NotDirectory` (stage `deps_select`), nothing written there.
#[test]
fn snapshot_symlink_swap_is_refused() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    let ws = fx.ws(&s, "tool");
    let target = ws.join("patches");
    let before: Vec<_> = fs::read_dir(&target).unwrap().flatten().map(|e| e.file_name()).collect();
    strata_deps::test_seams::after_snapshot_clone(Box::new(move |snap, _| {
        let t = snap.join("tree");
        let _ = fs::remove_dir_all(&t);
        std::os::unix::fs::symlink(&target, &t).unwrap();
    }));
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(detail(&e)["stage"], "deps_select", "{e:?}");
    let r1 = rows(&fx).remove(0);
    assert_eq!(r1.phase, PublishPhase::Done);
    assert_eq!(outcome(&r1)["stopped"], "NotDirectory");
    let after: Vec<_> = fs::read_dir(ws.join("patches")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(before, after, "nothing written into the workspace");
    snapshot_gone(&fx, r1.group.id);
}

/// Test 23: a substituted publish whose check fails carries the warning.
#[test]
fn check_failure_after_substitution_carries_the_warning() {
    let mut fx = fx();
    let a = fx.new_session(&["tool"]);
    let b = fx.new_session(&["tool"]);
    run(&fx, &select(&fx, &a, &[("package.json", PKG2.as_bytes())])).expect("seal the set");
    let checks = fx.cfg_mut("tool").checks.clone();
    fx.sb.write_program("fails", "#!/bin/sh\nexit 4\n");
    fx.cfg_mut("tool").checks = [checks, vec![fx.sb.bin.join("fails").display().to_string()]].concat();
    let e = run(&fx, &select(&fx, &b, &[("src/index.ts", b"export const f = 1\n")])).unwrap_err();
    assert_eq!(e.code, Code::CheckFailed, "{e:?}");
    let w = &detail(&e)["deps_warning"];
    assert_eq!(w["kind"], "DepsSubstituted", "{w}");
    assert!(w["detail"]["message"].is_string(), "{w}");
}

/// Test 15: the install program is gone and the inputs changed →
/// `NoSuchPath` as is, at the selection, with no install.
#[test]
fn missing_toolchain_is_reported_as_is() {
    let fx = fx();
    let s = fx.new_session(&["tool"]);
    let g = select(&fx, &s, &[("package.json", PKG2.as_bytes())]);
    let installs = fx.sb.install_count();
    fs::rename(fx.sb.bin.join("bun"), fx.sb.bin.join("bun.away")).unwrap();
    let r = run(&fx, &g);
    fs::rename(fx.sb.bin.join("bun.away"), fx.sb.bin.join("bun")).unwrap();
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    assert_eq!(detail(&e)["stage"], "deps_select", "{e:?}");
    assert_eq!(fx.sb.install_count(), installs);
    let r1 = rows(&fx).remove(0);
    assert_eq!(r1.phase, PublishPhase::Done);
    assert_eq!(outcome(&r1)["stopped"], "NoSuchPath");
    let root = fx.store().session(&s).unwrap().unwrap().manifest.root;
    let left = fs::read_dir(root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".strata-publish-")).count();
    assert_eq!(left, 0, "the layout is removed");
}

/// Test 24: P was built with `deps_dirs = ["gen"]`, the config now says
/// `["vendor"]`, and the session writes `gen/source.rs` with changed inputs
/// → `Drift`/`deps_select_failed`/`tracked_overlap`, no install.
#[test]
fn tracked_overlap_with_the_pinned_dirs_is_drift() {
    let mut fx = fx();
    fx.sb.set_install_script(&format!("{MARKED_INSTALL}mkdir -p gen && echo generated > gen/out.rs\n"));
    fx.cfg_mut("tool").deps_dirs = rps(&["node_modules", "gen"]);
    let s = fx.new_session(&["tool"]);
    assert!(fx.ws(&s, "tool").join("gen/out.rs").exists(), "P has gen");
    fx.cfg_mut("tool").deps_dirs = rps(&["node_modules", "vendor"]);
    let installs = fx.sb.install_count();
    let g = select(&fx, &s, &[("gen/source.rs", b"pub fn a() {}\n"), ("package.json", PKG2.as_bytes())]);
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::Drift, "{e:?}");
    let d = detail(&e);
    assert_eq!(d["reason"], "deps_select_failed", "{d}");
    assert_eq!(d["sub"], "tracked_overlap", "{d}");
    assert_eq!(d["dirs"], serde_json::json!(["gen"]), "{d}");
    assert_eq!(fx.sb.install_count(), installs, "no install");
    let r1 = rows(&fx).remove(0);
    assert_eq!(r1.phase, PublishPhase::Done);
    assert_eq!(outcome(&r1)["stopped"], "Drift");
}

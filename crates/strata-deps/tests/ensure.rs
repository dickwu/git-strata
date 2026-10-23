//! deps.md §7, Task 5: `ensure` and the empty set.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier};

use common::*;
use strata_deps::{
    DepsConfigKey, build, clone_into, empty_inputs_hash, ensure, inputs_hash, platform, set_id,
    test_seams, toolchain,
};
use strata_store::{AttemptKind, AttemptState, DepsAttempt, DepsOrigin};
use strata_types::{Code, DepsSet, DepsSetId};

#[test]
fn worker() {
    match worker_mode().as_deref() {
        None => {}
        Some("sock") => sock_worker(),
        Some(m) => panic!("unknown worker mode {m}"),
    }
}

fn attempts(fx: &Fx) -> Vec<DepsAttempt> {
    fx.store().deps_attempts(Some(&fx.repo)).unwrap()
}

fn expected_id(fx: &Fx, tree: &Path) -> DepsSetId {
    set_id(
        &inputs_hash(&fx.cfg, &fx.git, tree).unwrap(),
        &platform(),
        &toolchain(&fx.cfg, &fx.env).unwrap(),
        &DepsConfigKey::of(&fx.cfg).unwrap(),
    )
}

fn origin(fx: &Fx, set: &DepsSet) -> DepsOrigin {
    fx.store()
        .deps_set(&fx.repo, &set.id)
        .unwrap()
        .unwrap()
        .origin
}

fn kinds(fx: &Fx) -> Vec<(AttemptKind, AttemptState)> {
    attempts(fx).iter().map(|a| (a.kind, a.state)).collect()
}

#[test]
fn ensure_prefers_sealed_then_adopt_then_build() {
    // Already sealed: no clone, no install, no attempt.
    let fx = Fx::new();
    let tree = fx.tree("a");
    let built = build(&fx.ctx(), &tree, None).unwrap();
    assert_eq!(fx.sb.install_count(), 1);
    let n = attempts(&fx).len();
    fx.main_deps();
    let set = ensure(&fx.ctx(), &tree).unwrap();
    assert_eq!(set, built);
    assert_eq!(set.id, expected_id(&fx, &tree));
    assert_eq!(fx.sb.install_count(), 1);
    assert_eq!(attempts(&fx).len(), n);
    assert!(fx.staging_dirs().is_empty());

    // Not sealed, main matches: adopted, no install.
    let fx = Fx::new();
    fx.main_deps();
    let tree = fx.tree("a");
    let set = ensure(&fx.ctx(), &tree).unwrap();
    assert_eq!(set.id, expected_id(&fx, &tree));
    assert_eq!(fx.sb.install_count(), 0);
    assert_eq!(origin(&fx, &set), DepsOrigin::Adopt);
    assert_eq!(kinds(&fx), vec![(AttemptKind::Adopt, AttemptState::Sealed)]);

    // Every adoption refusal falls back to one cold install.
    for case in [
        "inputs", "missing", "file", "symlink", "socket", "fifo", "special",
    ] {
        let fx = Fx::new();
        let nm = fx.main.join("node_modules");
        match case {
            "inputs" => {
                fx.main_deps();
                fs::write(fx.main.join("bun.lock"), b"lock v1\nlocal edit\n").unwrap();
            }
            "missing" => {}
            "file" => fs::write(&nm, b"x").unwrap(),
            "symlink" => {
                fx.main_deps();
                fs::rename(&nm, fx.main.join("real_nm")).unwrap();
                std::os::unix::fs::symlink("real_nm", &nm).unwrap();
            }
            "socket" => make_socket(&fx.main, "node_modules"),
            "fifo" => mkfifo(&nm),
            _ => {
                fx.main_deps();
                mkfifo(&nm.join("a/f"));
            }
        }
        let tree = fx.tree("a");
        let set = ensure(&fx.ctx(), &tree).unwrap_or_else(|e| panic!("{case}: {e:?}"));
        assert_eq!(set.id, expected_id(&fx, &tree), "{case}");
        assert_eq!(fx.sb.install_count(), 1, "{case}");
        assert_eq!(origin(&fx, &set), DepsOrigin::Build, "{case}");
        assert!(
            fx.store()
                .deps_dir(&fx.repo, &set.id)
                .join("node_modules/x/index.js")
                .is_file()
        );
    }

    // Main changes during every clone: three adoptions, then a cold install.
    test_seams::reset();
    let fx = Fx::new();
    fx.main_deps();
    let tree = fx.tree("a");
    let f = fx.main.join("node_modules/a/index.js");
    let n = Arc::new(AtomicU32::new(0));
    let n2 = n.clone();
    test_seams::after_attempt_begins(Box::new(move || {
        let i = n2.fetch_add(1, Ordering::SeqCst);
        fs::write(&f, format!("module.exports = {i}\n")).unwrap();
    }));
    let r = ensure(&fx.ctx(), &tree);
    test_seams::reset();
    let set = r.unwrap();
    assert_eq!(set.id, expected_id(&fx, &tree));
    assert_eq!(fx.sb.install_count(), 1);
    assert_eq!(origin(&fx, &set), DepsOrigin::Build);
    let atts = attempts(&fx);
    assert_eq!(atts.len(), 4);
    for a in &atts[..3] {
        assert_eq!(
            (a.kind, a.state),
            (AttemptKind::Adopt, AttemptState::Refused)
        );
        let e = a.error.as_ref().expect("refusal recorded");
        assert_eq!(reason(e), Some("changed_during_clone"));
    }
    assert_eq!(
        (atts[3].kind, atts[3].state),
        (AttemptKind::Build, AttemptState::Sealed)
    );
}

#[test]
fn ensure_refuses_input_missing_from_base_tree() {
    let mut fx = Fx::new();
    fx.main_deps();
    fs::write(fx.main.join("bunfig.toml"), b"[install]\n").unwrap();
    fx.cfg.install_inputs = rps(&["package.json", "bun.lock", "patches", "bunfig.toml"]);
    let tree = fx.tree("a");
    let e = ensure(&fx.ctx(), &tree).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["repo"], "tool");
    assert_eq!(d["field"], "install_inputs");
    assert_eq!(d["index"], 3);
    assert_eq!(d["rule"], "tracked_in_base");
    assert_eq!(d["path"], "bunfig.toml");
    assert!(!fx.repo_dir().join(".locks").exists());
    assert!(attempts(&fx).is_empty());
    assert!(fx.staging_dirs().is_empty());
    assert_eq!(fx.sb.install_count(), 0);
}

#[test]
fn concurrent_ensure_builds_once() {
    let fx = Fx::new();
    fx.sb
        .set_install_script(&format!("sleep 1\n{DEFAULT_INSTALL}"));
    let tree = fx.tree("a");
    let gate = Barrier::new(2);
    let (a, b) = std::thread::scope(|s| {
        let run = || {
            gate.wait();
            ensure(&fx.ctx(), &tree)
        };
        let t1 = s.spawn(run);
        let t2 = s.spawn(run);
        (t1.join().unwrap(), t2.join().unwrap())
    });
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a, b);
    assert_eq!(fx.sb.install_count(), 1);
    let atts = attempts(&fx);
    assert_eq!(atts.len(), 1, "{atts:?}");
    assert_eq!(
        (atts[0].kind, atts[0].state),
        (AttemptKind::Build, AttemptState::Sealed)
    );
    assert_eq!(atts[0].set, a.id);
    assert_eq!(origin(&fx, &a), DepsOrigin::Build);
}

/// A `bun` that records any invocation, `--version` included.
fn forbid_commands(fx: &Fx) -> std::path::PathBuf {
    let mark = fx.sb.state.join("ran");
    fx.sb.write_program(
        "bun",
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", mark.display()),
    );
    mark
}

fn empty_id() -> DepsSetId {
    set_id(
        &empty_inputs_hash(),
        &platform(),
        "",
        &DepsConfigKey::empty(),
    )
}

#[test]
fn empty_config_yields_empty_set() {
    let mut fx = Fx::new();
    fx.cfg.deps_dirs.clear();
    let mark = forbid_commands(&fx);
    let tree = fx.tree("a");
    let set = ensure(&fx.ctx(), &tree).unwrap();
    assert!(set.dirs.is_empty());
    assert_eq!(set.toolchain, "");
    assert_eq!(set.inputs_hash, empty_inputs_hash());
    assert_eq!(set.id, empty_id());
    assert!(!mark.exists(), "no command ran");
    let dir = fx.store().deps_dir(&fx.repo, &set.id);
    assert!(dir.is_dir());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    assert_eq!(mode_of(&dir), 0o555);
    assert_eq!(origin(&fx, &set), DepsOrigin::Empty);
    assert!(attempts(&fx).is_empty());
    assert!(!fx.repo_dir().join(".locks").exists());
    assert!(fx.staging_dirs().is_empty());
    assert_eq!(fx.lock.enters(), 1);

    // Again: same id, still one record, no further lock entry.
    let again = ensure(&fx.ctx(), &tree).unwrap();
    assert_eq!(again, set);
    assert_eq!(fx.lock.enters(), 1);
    let dst = fx.sb.root.join("dst");
    fs::create_dir_all(&dst).unwrap();
    clone_into(fx.store(), &set, &dst).unwrap();
    assert_eq!(fs::read_dir(&dst).unwrap().count(), 0);

    // Two threads at once.
    let mut fx = Fx::new();
    fx.cfg.deps_dirs.clear();
    let tree = fx.tree("a");
    let gate = Barrier::new(2);
    let (a, b) = std::thread::scope(|s| {
        let run = || {
            gate.wait();
            ensure(&fx.ctx(), &tree)
        };
        let t1 = s.spawn(run);
        let t2 = s.spawn(run);
        (t1.join().unwrap(), t2.join().unwrap())
    });
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a, b);
    assert_eq!(a.id, empty_id());
    assert_eq!(origin(&fx, &a), DepsOrigin::Empty);
    assert!(attempts(&fx).is_empty());
    let entries: Vec<_> = fs::read_dir(fx.repo_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from(a.id.as_str())]);
}

#[test]
fn empty_set_registers_leftover_empty_dir() {
    let mut fx = Fx::new();
    fx.cfg.deps_dirs.clear();
    let id = empty_id();
    let dir = fx.store().deps_dir(&fx.repo, &id);
    fs::create_dir_all(&dir).unwrap();
    let tree = fx.tree("a");
    let set = ensure(&fx.ctx(), &tree).unwrap();
    assert_eq!(set.id, id);
    assert_eq!(origin(&fx, &set), DepsOrigin::Empty);
    assert_eq!(mode_of(&dir), 0o555);
    assert_eq!(fx.lock.enters(), 1);
    assert!(attempts(&fx).is_empty());

    // A leftover with a file in it is an orphan: untouched, unrecorded.
    let mut fx = Fx::new();
    fx.cfg.deps_dirs.clear();
    let dir = fx.store().deps_dir(&fx.repo, &id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("stray"), b"left").unwrap();
    let mode = mode_of(&dir);
    let tree = fx.tree("a");
    let e = ensure(&fx.ctx(), &tree).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e), Some("orphan"));
    assert_eq!(detail_str(&e, "path").map(Path::new), Some(dir.as_path()));
    assert_eq!(fs::read(dir.join("stray")).unwrap(), b"left");
    assert_eq!(mode_of(&dir), mode);
    assert_eq!(fx.store().deps_set(&fx.repo, &id).unwrap(), None);
    assert!(attempts(&fx).is_empty());
}

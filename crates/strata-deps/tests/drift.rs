//! deps.md §7, Task 6: drift.

mod common;

use std::fs;

use common::*;
use strata_deps::{build, clone_into, drift, inputs_hash};

#[test]
fn deps_dir_writes_are_not_drift() {
    let fx = Fx::new();
    let tree = fx.tree("a");
    let set = build(&fx.ctx(), &tree, None).unwrap();
    let sealed = fx.store().deps_dir(&fx.repo, &set.id);
    let before = snapshot(&sealed);
    let ws = fx.tree("ws");
    clone_into(fx.store(), &set, &ws).unwrap();
    assert_eq!(drift(&fx.cfg, &fx.git, &ws, &set).unwrap(), None);

    // dev-link writes, a new .bin entry, a removed package.
    let nm = ws.join("node_modules");
    fs::create_dir_all(nm.join("@inter/hr-core/src")).unwrap();
    fs::write(nm.join("@inter/hr-core/src/a.ts"), b"export const a = 1\n").unwrap();
    fs::write(nm.join(".bin/extra"), b"#!/bin/sh\n").unwrap();
    fs::remove_dir_all(nm.join("x")).unwrap();
    assert_eq!(drift(&fx.cfg, &fx.git, &ws, &set).unwrap(), None);
    assert_eq!(snapshot(&sealed), before);

    // An install input change is drift.
    fs::write(
        ws.join("package.json"),
        PACKAGE_JSON.replace("\"x\": \"^1.0.0\"", "\"x\": \"^2.0.0\""),
    )
    .unwrap();
    let d = drift(&fx.cfg, &fx.git, &ws, &set).unwrap().expect("drift");
    assert_eq!(d.pinned, set.id);
    assert_eq!(d.found_inputs, inputs_hash(&fx.cfg, &fx.git, &ws).unwrap());
}

#[test]
fn drift_compares_install_inputs_only() {
    let fx = Fx::new();
    let tree = fx.tree("a");
    let set = build(&fx.ctx(), &tree, None).unwrap();
    let ws = fx.tree("ws");
    clone_into(fx.store(), &set, &ws).unwrap();

    fs::write(ws.join("bun.lock"), b"lock v2\n").unwrap();
    let d = drift(&fx.cfg, &fx.git, &ws, &set).unwrap().expect("drift");
    assert_eq!(d.pinned, set.id);
    assert_eq!(d.pinned_inputs, set.inputs_hash);
    assert_eq!(d.found_inputs, inputs_hash(&fx.cfg, &fx.git, &ws).unwrap());
    assert_ne!(d.found_inputs, d.pinned_inputs);

    // A new toolchain with the same inputs is not drift.
    fs::write(ws.join("bun.lock"), b"lock v1\n").unwrap();
    fx.sb.set_bun_version("1.3.4");
    assert_eq!(drift(&fx.cfg, &fx.git, &ws, &set).unwrap(), None);

    // A main tip export (no .git) agrees with a checkout of the same content.
    fx.sb
        .commit(&fx.main, &[("bun.lock", b"lock v3\n")], "lock");
    let export = fx.tree("tip");
    assert!(!export.join(".git").exists());
    let on_export = drift(&fx.cfg, &fx.git, &export, &set).unwrap();
    let on_main = drift(&fx.cfg, &fx.git, &fx.main, &set).unwrap();
    assert!(on_export.is_some());
    assert_eq!(on_export, on_main);
    assert_eq!(drift(&fx.cfg, &fx.git, &tree, &set).unwrap(), None);
    assert_eq!(drift(&fx.cfg, &fx.git, &ws, &set).unwrap(), None);
}

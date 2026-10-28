//! deps-drift design r8 §2.2 step 4: publish's install-input snapshot.

mod common;

use std::fs;
use std::os::unix::fs::symlink;

use common::*;
use strata_deps::test_seams;
use strata_deps::{
    build, clean_snapshots, hash_records, input_records, inputs_hash, key_of, remove_snapshot,
    snapshot_dir, snapshot_inputs, snapshot_tree,
};
use strata_types::Code;

fn tree_with_inputs(fx: &Fx) -> std::path::PathBuf {
    let tree = fx.tree_tagged("staged", "y");
    fs::write(tree.join("bun.lock"), "lock v1\n").unwrap();
    fs::create_dir_all(tree.join("patches/sub")).unwrap();
    fs::write(tree.join("patches/sub/a.patch"), "diff\n").unwrap();
    symlink("sub/a.patch", tree.join("patches/link.patch")).unwrap();
    fs::write(tree.join("README.md"), "not an input\n").unwrap();
    tree
}

/// Test 6: the snapshot hashes like the staged tree, holds only the install
/// inputs, and a build from it seals the set the staged tree keys to.
#[test]
fn snapshot_hashes_like_the_staged_tree_and_builds_its_set() {
    test_seams::reset();
    let fx = Fx::new();
    let tree = tree_with_inputs(&fx);
    let ctx = fx.ctx();
    let h = inputs_hash(&fx.cfg, &fx.git, &tree).unwrap();
    let snap = snapshot_inputs(&ctx, &tree, 7, &h).unwrap();
    assert_eq!(snap, snapshot_dir(&fx.paths, &fx.repo, 7));
    let st = snapshot_tree(&fx.paths, &fx.repo, 7);
    assert_eq!(inputs_hash(&fx.cfg, &fx.git, &st).unwrap(), h);
    assert!(st.join("patches/sub/a.patch").is_file());
    assert_eq!(fs::read_link(st.join("patches/link.patch")).unwrap(), std::path::Path::new("sub/a.patch"));
    assert!(!st.join("README.md").exists());
    assert!(!snap.join(".slot").exists());
    assert_eq!(
        input_records(&fx.cfg, &fx.git, &st).unwrap(),
        input_records(&fx.cfg, &fx.git, &tree).unwrap()
    );

    let base = fx.tree("base");
    let (_, want) = key_of(&ctx, &tree).unwrap();
    let set = build(&ctx, &base, Some(&st)).unwrap();
    assert_eq!(set.id, want);
    assert_eq!(set.inputs_hash, h);

    assert!(remove_snapshot(&fx.paths, &fx.repo, 7));
    assert!(!snap.exists());
    assert!(remove_snapshot(&fx.paths, &fx.repo, 7), "absent counts as removed");
}

#[test]
fn snapshot_hash_mismatch_is_unknown_and_leaves_nothing() {
    test_seams::reset();
    let fx = Fx::new();
    let tree = tree_with_inputs(&fx);
    let h = inputs_hash(&fx.cfg, &fx.git, &tree).unwrap();
    test_seams::snapshot_hash_wrong();
    let e = snapshot_inputs(&fx.ctx(), &tree, 3, &h).unwrap_err();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(e.detail.as_ref().unwrap()["reason"], "inputs_snapshot");
    assert!(!snapshot_dir(&fx.paths, &fx.repo, 3).exists());
}

/// A member swapped for a symlink between the clone and the rename: the
/// snapshot is not written through it, and the snapshot is removed.
#[test]
fn snapshot_never_writes_through_a_swapped_parent() {
    test_seams::reset();
    let fx = Fx::new();
    let tree = tree_with_inputs(&fx);
    let h = inputs_hash(&fx.cfg, &fx.git, &tree).unwrap();
    let outside = fx.sb.path("outside");
    fs::create_dir_all(&outside).unwrap();
    let out2 = outside.clone();
    test_seams::after_snapshot_clone(Box::new(move |snap, _dst| {
        let t = snap.join("tree");
        fs::remove_dir(&t).ok();
        symlink(&out2, &t).unwrap();
    }));
    let r = snapshot_inputs(&fx.ctx(), &tree, 4, &h);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "nothing written outside");
    assert!(!snapshot_dir(&fx.paths, &fx.repo, 4).exists());
}

#[test]
fn clean_snapshots_removes_every_left_snapshot() {
    test_seams::reset();
    let fx = Fx::new();
    let tree = tree_with_inputs(&fx);
    let h = inputs_hash(&fx.cfg, &fx.git, &tree).unwrap();
    snapshot_inputs(&fx.ctx(), &tree, 1, &h).unwrap();
    snapshot_inputs(&fx.ctx(), &tree, 2, &h).unwrap();
    assert!(clean_snapshots(&fx.paths).is_empty());
    assert!(!snapshot_dir(&fx.paths, &fx.repo, 1).exists());
    assert!(!snapshot_dir(&fx.paths, &fx.repo, 2).exists());
}

/// Test 16: splitting `inputs_hash` into records did not change it.
#[test]
fn inputs_hash_of_fixed_inputs_is_unchanged() {
    test_seams::reset();
    let fx = Fx::new();
    let dir = fx.sb.path("fixed");
    fs::create_dir_all(dir.join("patches")).unwrap();
    fs::write(dir.join("package.json"), r#"{"name":"tool","dependencies":{"x":"^1.0.0"}}"#).unwrap();
    fs::write(dir.join("patches/a.patch"), "diff\n").unwrap();
    symlink("a.patch", dir.join("patches/b.patch")).unwrap();
    let h = inputs_hash(&fx.cfg, &fx.git, &dir).unwrap();
    assert_eq!(hash_records(&input_records(&fx.cfg, &fx.git, &dir).unwrap()), h);
    assert_eq!(h, "d7f9be33ae1fa05d066bf07f2f039afa0051bcb49c8adb2ff68416606db9a858");
}

/// Codex impl-r1 B1: a symlinked `deps/<repo>` is never followed, by the
/// startup sweep or by `remove_snapshot`.
#[test]
fn snapshot_removal_never_follows_a_repo_link() {
    test_seams::reset();
    let fx = Fx::new();
    let outside = fx.sb.path("outside");
    fs::create_dir_all(outside.join(".inputs/9")).unwrap();
    fs::write(outside.join(".inputs/9/sentinel"), "keep\n").unwrap();
    fs::create_dir_all(fx.paths.deps()).unwrap();
    symlink(&outside, fx.paths.deps().join("alias")).unwrap();
    assert!(clean_snapshots(&fx.paths).is_empty());
    let alias = strata_types::RepoName::try_from("alias".to_owned()).unwrap();
    assert!(!remove_snapshot(&fx.paths, &alias, 9), "a link is refused, not removed through");
    assert!(outside.join(".inputs/9/sentinel").exists(), "nothing outside the store is removed");
}

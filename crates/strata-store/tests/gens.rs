//! store.md §7, Task 4: base generations with their manifests.

mod support;

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use strata_store::*;
use strata_types::{BaseGen, BaseManifest, BlobId, Code, DepsSet, DepsSetId, ManifestEntry, RepoName, RepoPath, TreeOid};
use support::*;

fn repo(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}
fn set_id(c: char) -> DepsSetId {
    DepsSetId::try_from(c.to_string().repeat(64)).unwrap()
}
fn tree(c: char) -> TreeOid {
    TreeOid::try_from(c.to_string().repeat(40)).unwrap()
}
fn blob(c: char) -> BlobId {
    BlobId::try_from(c.to_string().repeat(64)).unwrap()
}
fn t(s: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(s)
}
fn gen_(r: &str, tr: char, d: char) -> BaseGen {
    BaseGen { repo: repo(r), tree: tree(tr), deps: set_id(d) }
}
fn manifest(tr: char, entries: &[(&str, char, u32, u64)]) -> BaseManifest {
    BaseManifest {
        tree: tree(tr),
        entries: entries
            .iter()
            .map(|(p, b, m, s)| (RepoPath::try_from(p.to_string()).unwrap(), ManifestEntry { blob: blob(*b), mode: *m, size: *s }))
            .collect::<BTreeMap<_, _>>(),
    }
}
fn register_deps(s: &Store, r: &str, d: char) {
    let set = DepsSet {
        id: set_id(d),
        repo: repo(r),
        inputs_hash: "0".repeat(64),
        platform: "darwin-arm64".into(),
        toolchain: "none".into(),
        dirs: vec![],
    };
    s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
}
fn counts(env: &Env) -> (i64, i64, i64) {
    let (c, _) = raw(&env.db(), false);
    let q = |t: &str| c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0)).unwrap();
    (q("base_trees"), q("manifest_entries"), q("base_gens"))
}
fn full() -> BaseManifest {
    manifest('1', &[("README.md", 'a', 0o100644, 10), ("bin/run", 'b', 0o100755, 20), ("link", 'c', 0o120000, 7), ("src/lib.rs", 'd', 0o100644, 0)])
}

#[test]
fn generation_rows_register_and_read_back() {
    let env = env();
    let s = env.open();
    register_deps(&s, "app", 'a');
    register_deps(&s, "app", 'b');
    let g = gen_("app", '1', 'a');
    assert!(!s.has_base_gen(&g).unwrap());
    s.record_base_gen(&g, &full(), t(5)).unwrap();
    assert!(s.has_base_gen(&g).unwrap());
    assert_eq!(s.manifest(&repo("app"), &tree('1')).unwrap(), Some(full()));
    assert_eq!(counts(&env), (1, 4, 1));

    // the same again writes nothing
    s.record_base_gen(&g, &full(), t(6)).unwrap();
    assert_eq!(counts(&env), (1, 4, 1));

    // a second generation of the same tree reuses the manifest
    let g2 = gen_("app", '1', 'b');
    s.record_base_gen(&g2, &full(), t(7)).unwrap();
    assert!(s.has_base_gen(&g2).unwrap());
    assert_eq!(counts(&env), (1, 4, 2));

    // a different manifest for the same (repo, tree) → manifest_mismatch, first difference
    let cases: Vec<(BaseManifest, &str)> = vec![
        (manifest('1', &[("README.md", 'a', 0o100644, 10), ("bin/run", 'b', 0o100644, 20), ("link", 'c', 0o120000, 7), ("src/lib.rs", 'd', 0o100644, 0)]), "bin/run"),
        (manifest('1', &[("README.md", 'a', 0o100644, 10), ("bin/run", 'b', 0o100755, 20), ("link", 'e', 0o120000, 7), ("src/lib.rs", 'd', 0o100644, 0)]), "link"),
        (manifest('1', &[("README.md", 'a', 0o100644, 11), ("bin/run", 'b', 0o100755, 20), ("link", 'c', 0o120000, 7), ("src/lib.rs", 'd', 0o100644, 0)]), "README.md"),
        (manifest('1', &[("README.md", 'a', 0o100644, 10), ("bin/run", 'b', 0o100755, 20), ("link", 'c', 0o120000, 7)]), "src/lib.rs"),
        (manifest('1', &[("README.md", 'a', 0o100644, 10), ("bin/run", 'b', 0o100755, 20), ("link", 'c', 0o120000, 7), ("src/lib.rs", 'd', 0o100644, 0), ("z", 'a', 0o100644, 1)]), "z"),
        (manifest('1', &[("README.md", 'a', 0o100644, 10), ("bin/rum", 'b', 0o100755, 20), ("link", 'c', 0o120000, 7), ("src/lib.rs", 'd', 0o100644, 0)]), "bin/rum"),
    ];
    register_deps(&s, "app", 'c');
    for (m, path) in cases {
        let e = s.record_base_gen(&gen_("app", '1', 'c'), &m, t(8)).unwrap_err();
        assert_reason(&e, Code::InvalidInput, "manifest_mismatch");
        assert_eq!(detail(&e, "path"), Some(&serde_json::json!(path)));
    }
    assert!(!s.has_base_gen(&gen_("app", '1', 'c')).unwrap());
    assert_eq!(counts(&env), (1, 4, 2));
    assert_eq!(s.manifest(&repo("app"), &tree('1')).unwrap(), Some(full()));

    // bad mode is refused before anything is written
    let e = s.record_base_gen(&gen_("app", '2', 'a'), &manifest('2', &[("x", 'a', 0o100600, 1)]), t(9)).unwrap_err();
    assert_code(&e, Code::InvalidInput);
    assert_eq!(counts(&env), (1, 4, 2));

    // survives a reopen
    s.close().unwrap();
    let s = env.open();
    assert!(s.has_base_gen(&g).unwrap() && s.has_base_gen(&g2).unwrap());
    assert_eq!(s.manifest(&repo("app"), &tree('1')).unwrap(), Some(full()));
}

#[test]
fn manifest_absent_and_empty_are_distinct() {
    let env = env();
    let s = env.open();
    assert_eq!(s.manifest(&repo("app"), &tree('e')).unwrap(), None);
    register_deps(&s, "app", 'a');
    register_deps(&s, "lib", 'a');
    s.record_base_gen(&gen_("app", 'e', 'a'), &manifest('e', &[]), t(1)).unwrap();
    assert_eq!(s.manifest(&repo("app"), &tree('e')).unwrap(), Some(manifest('e', &[])));
    assert_eq!(s.manifest(&repo("lib"), &tree('e')).unwrap(), None);
    assert_eq!(counts(&env), (1, 0, 1));
    // the same tree in another repo has its own manifest
    let lib = manifest('e', &[("only-in-lib", 'f', 0o100644, 3)]);
    s.record_base_gen(&gen_("lib", 'e', 'a'), &lib, t(2)).unwrap();
    assert_eq!(s.manifest(&repo("lib"), &tree('e')).unwrap(), Some(lib));
    assert_eq!(s.manifest(&repo("app"), &tree('e')).unwrap(), Some(manifest('e', &[])));
    // registering the empty tree again is a no-op; a non-empty one for it is a mismatch
    s.record_base_gen(&gen_("app", 'e', 'a'), &manifest('e', &[]), t(3)).unwrap();
    let e = s.record_base_gen(&gen_("app", 'e', 'a'), &manifest('e', &[("x", 'a', 0o100644, 1)]), t(3)).unwrap_err();
    assert_reason(&e, Code::InvalidInput, "manifest_mismatch");
    assert_eq!(detail(&e, "path"), Some(&serde_json::json!("x")));
    assert_eq!(counts(&env), (2, 1, 2));
}

#[test]
fn base_gen_requires_registered_deps() {
    let env = env();
    let s = env.open();
    let e = s.record_base_gen(&gen_("app", '1', 'a'), &full(), t(1)).unwrap_err();
    assert_reason(&e, Code::InvalidInput, "deps_unregistered");
    assert_eq!(counts(&env), (0, 0, 0));
    // registered in another repo is not enough
    register_deps(&s, "lib", 'a');
    let e = s.record_base_gen(&gen_("app", '1', 'a'), &full(), t(1)).unwrap_err();
    assert_reason(&e, Code::InvalidInput, "deps_unregistered");
    assert_eq!(counts(&env), (0, 0, 0));
    // tree mismatch
    let e = s.record_base_gen(&gen_("lib", '2', 'a'), &full(), t(1)).unwrap_err();
    assert_code(&e, Code::InvalidInput);
    assert_eq!(detail(&e, "field"), Some(&serde_json::json!("tree")));
    assert_eq!(counts(&env), (0, 0, 0));
    assert!(s.manifest(&repo("lib"), &tree('2')).unwrap().is_none());
    // a repo without dependency dirs uses the Empty set
    register_deps(&s, "app", 'a');
    s.record_base_gen(&gen_("app", '1', 'a'), &full(), t(2)).unwrap();
    assert!(s.has_base_gen(&gen_("app", '1', 'a')).unwrap());
    assert_eq!(s.deps_set(&repo("app"), &set_id('a')).unwrap().unwrap().set.dirs, Vec::<RepoPath>::new());
}

#[test]
fn generation_paths_derive_from_keys() {
    let env = env();
    let s = env.open();
    let g = gen_("app", '1', 'a');
    let root = env.root.clone();
    assert_eq!(s.base_dir(&g), root.join("bases/app").join("1".repeat(40)).join("a".repeat(64)));
    assert_eq!(s.deps_dir(&repo("app"), &set_id('a')), root.join("deps/app").join("a".repeat(64)));
    assert_eq!(s.deps_staging_dir(&repo("app"), 7), root.join("deps/app/.staging/7"));
    assert_ne!(s.base_dir(&g), s.base_dir(&gen_("app", '1', 'b')));
    assert_ne!(s.base_dir(&g), s.base_dir(&gen_("lib", '1', 'a')));
    // paths are pure functions of the keys: nothing is created
    assert!(!s.base_dir(&g).exists());
    assert!(!s.deps_dir(&repo("app"), &set_id('a')).exists());
}

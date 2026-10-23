//! deps.md §7, Task 4: adopting the main checkout's deps dirs.

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use strata_deps::{
    adopt, build, clone_into, find_set, inputs_hash, key_of, nested_seed_caches, test_seams,
};
use strata_store::{AttemptKind, AttemptState};
use strata_types::Code;

#[test]
fn worker() {
    match worker_mode().as_deref() {
        None => {}
        Some("sock") => sock_worker(),
        Some(m) => panic!("unknown worker mode {m}"),
    }
}

fn no_trace(fx: &Fx) {
    assert!(fx.store().deps_attempts(Some(&fx.repo)).unwrap().is_empty());
    assert!(fx.staging_dirs().is_empty());
}

fn one_refused(fx: &Fx) {
    let atts = fx.store().deps_attempts(Some(&fx.repo)).unwrap();
    assert_eq!(atts.len(), 1);
    assert_eq!(
        (atts[0].kind, atts[0].state),
        (AttemptKind::Adopt, AttemptState::Refused)
    );
    assert!(fx.staging_dirs().is_empty(), "staging retired");
}

#[test]
fn adopt_matches_inputs_hash_else_refuses() {
    // Success: main differs from the base only in package.json's version.
    let fx = Fx::new();
    fx.main_deps();
    fs::write(
        fx.main.join("package.json"),
        PACKAGE_JSON.replace("1.0.87", "1.0.88"),
    )
    .unwrap();
    let tree = fx.tree("a");
    let before = apfs_walk(&fx.main, "node_modules");
    let set = adopt(&fx.ctx(), &tree).unwrap();
    assert_eq!(
        set.inputs_hash,
        inputs_hash(&fx.cfg, &fx.git, &tree).unwrap()
    );
    assert_eq!(set.id, key_of(&fx.ctx(), &tree).unwrap().1);
    assert_eq!(set.dirs, rps(&["node_modules"]));
    let dir = fx.store().deps_dir(&fx.repo, &set.id);
    let (s, m) = (
        dir.join("node_modules/a/index.js"),
        fx.main.join("node_modules/a/index.js"),
    );
    assert_eq!(fs::read(&s).unwrap(), fs::read(&m).unwrap());
    assert_ne!(
        fs::metadata(&s).unwrap().ino(),
        fs::metadata(&m).unwrap().ino()
    );
    assert_eq!(
        fs::read_link(dir.join("node_modules/.bin/a")).unwrap(),
        Path::new("../a/index.js")
    );
    assert_eq!(mode_of(&dir), 0o555);
    assert_eq!(apfs_walk(&fx.main, "node_modules"), before);
    let atts = fx.store().deps_attempts(Some(&fx.repo)).unwrap();
    assert_eq!(atts.len(), 1);
    assert_eq!(
        (atts[0].kind, atts[0].state),
        (AttemptKind::Adopt, AttemptState::Sealed)
    );
    assert_eq!(fx.sb.install_count(), 0);
    assert!(fx.staging_dirs().is_empty());

    // An uncommitted bun.lock change in main: Drift(inputs), nothing written.
    let fx = Fx::new();
    fx.main_deps();
    let tree = fx.tree("a");
    fs::write(fx.main.join("bun.lock"), b"lock v1\nlocal edit\n").unwrap();
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    assert_eq!(e.code, Code::Drift, "{e:?}");
    assert_eq!(reason(&e), Some("inputs"));
    let (h, id) = key_of(&fx.ctx(), &tree).unwrap();
    assert_eq!(detail_str(&e, "base_inputs"), Some(h.as_str()));
    assert_eq!(
        detail_str(&e, "main_inputs"),
        Some(inputs_hash(&fx.cfg, &fx.git, &fx.main).unwrap().as_str())
    );
    no_trace(&fx);
    assert_eq!(find_set(fx.store(), &fx.repo, &id).unwrap(), None);
    assert!(!fx.lock_path(&id).exists());

    // No node_modules in main.
    let fx = Fx::new();
    let tree = fx.tree("a");
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    no_trace(&fx);

    // The repo write lock stays busy at seal time.
    let fx = Fx::new();
    fx.main_deps();
    let tree = fx.tree("a");
    fx.lock.set_busy(1);
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    one_refused(&fx);
    let id = key_of(&fx.ctx(), &tree).unwrap().1;
    assert!(!fx.store().deps_dir(&fx.repo, &id).exists());

    // sidecar is a symlink out of the repository.
    let mut fx = Fx::new();
    fx.main_deps();
    fx.cfg.deps_dirs = rps(&["node_modules", "sidecar/node_modules"]);
    let outside = fx.sb.root.join("outside");
    fs::create_dir_all(outside.join("node_modules/q")).unwrap();
    fs::write(outside.join("node_modules/q/i.js"), b"q").unwrap();
    std::os::unix::fs::symlink(&outside, fx.main.join("sidecar")).unwrap();
    let tree = fx.tree("a");
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    no_trace(&fx);
    assert_eq!(snapshot(&outside).len(), 3);

    // node_modules is a regular file, a socket or a FIFO with no writer.
    for kind in ["file", "socket", "fifo"] {
        let fx = Fx::new();
        let nm = fx.main.join("node_modules");
        match kind {
            "file" => fs::write(&nm, b"x").unwrap(),
            "socket" => make_socket(&fx.main, "node_modules"),
            _ => mkfifo(&nm),
        }
        let tree = fx.tree("a");
        let start = Instant::now();
        let e = adopt(&fx.ctx(), &tree).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(1), "{kind}");
        assert_eq!(e.code, Code::NotDirectory, "{kind}: {e:?}");
        no_trace(&fx);
    }

    // Replaced by a socket after the attempt begins, before the clone.
    test_seams::reset();
    let fx = Fx::new();
    fx.main_deps();
    let tree = fx.tree("a");
    let main = fx.main.clone();
    test_seams::after_attempt_begins(Box::new(move || {
        fs::rename(main.join("node_modules"), main.join("node_modules.old")).unwrap();
        make_socket(&main, "node_modules");
    }));
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    test_seams::reset();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    one_refused(&fx);
}

#[test]
fn adopt_refuses_when_main_changes_during_clone() {
    test_seams::reset();
    let fx = Fx::new();
    fx.main_deps();
    let tree = fx.tree("a");
    let target = fx.main.join("node_modules/a/index.js");
    let t = target.clone();
    test_seams::after_attempt_begins(Box::new(move || {
        fs::write(&t, b"module.exports = 'changed'\n").unwrap()
    }));
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    test_seams::reset();
    assert_eq!(e.code, Code::Drift, "{e:?}");
    assert_eq!(reason(&e), Some("changed_during_clone"));
    assert_eq!(detail_str(&e, "path"), Some("node_modules/a/index.js"));
    let id = key_of(&fx.ctx(), &tree).unwrap().1;
    assert_eq!(find_set(fx.store(), &fx.repo, &id).unwrap(), None);
    one_refused(&fx);
    assert_eq!(fs::read(&target).unwrap(), b"module.exports = 'changed'\n");

    // Without the hook: adopted.
    let set = adopt(&fx.ctx(), &tree).unwrap();
    assert_eq!(set.id, id);
    let dir = fx.store().deps_dir(&fx.repo, &id);
    assert_eq!(
        fs::read(dir.join("node_modules/a/index.js")).unwrap(),
        b"module.exports = 'changed'\n"
    );

    // A change inside a nested seed cache is not a change of the deps dirs.
    let mut fx = Fx::new();
    fx.cfg.seed_caches = rps(&[".cache", "node_modules/.cache"]);
    fx.main_deps();
    let cache = fx.main.join("node_modules/.cache/babel");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("x.json"), b"1").unwrap();
    let tree = fx.tree("a");
    let c = cache.join("x.json");
    test_seams::after_attempt_begins(Box::new(move || fs::write(&c, b"22").unwrap()));
    let r = adopt(&fx.ctx(), &tree);
    test_seams::reset();
    let set = r.unwrap();
    assert!(
        !fx.store()
            .deps_dir(&fx.repo, &set.id)
            .join("node_modules/.cache")
            .exists()
    );
}

#[test]
fn adopt_refuses_special_files_in_main() {
    let fx = Fx::new();
    fx.main_deps();
    mkfifo(&fx.main.join("node_modules/a/f"));
    let tree = fx.tree("a");
    let before = apfs_walk(&fx.main, "node_modules");
    let start = Instant::now();
    let e = adopt(&fx.ctx(), &tree).unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(e.code, Code::Drift, "{e:?}");
    assert_eq!(reason(&e), Some("special_file"));
    assert_eq!(detail_str(&e, "path"), Some("node_modules/a/f"));
    no_trace(&fx);
    assert_eq!(apfs_walk(&fx.main, "node_modules"), before);
}

fn sidecar_fx() -> Fx {
    let mut fx = Fx::new();
    fx.sb.commit(
        &fx.main,
        &[("sidecar/vendor/cloakbrowser/index.js", b"vendored\n")],
        "vendor",
    );
    fx.cfg.deps_dirs = rps(&["node_modules", "sidecar/node_modules"]);
    fx
}

fn sidecar_links(root: &Path) {
    let nm = root.join("node_modules");
    fs::create_dir_all(nm.join(".bin")).unwrap();
    fs::create_dir_all(nm.join("typescript/bin")).unwrap();
    fs::write(nm.join("typescript/bin/tsc"), b"tsc").unwrap();
    std::os::unix::fs::symlink("../typescript/bin/tsc", nm.join(".bin/tsc")).unwrap();
    fs::create_dir_all(root.join("sidecar/node_modules")).unwrap();
    std::os::unix::fs::symlink(
        "../vendor/cloakbrowser",
        root.join("sidecar/node_modules/cloakbrowser"),
    )
    .unwrap();
}

const SIDECAR_INSTALL: &str = r#"mkdir -p node_modules/.bin node_modules/typescript/bin sidecar/node_modules || exit 1
echo tsc > node_modules/typescript/bin/tsc
ln -s ../typescript/bin/tsc node_modules/.bin/tsc
ln -s ../vendor/cloakbrowser sidecar/node_modules/cloakbrowser
"#;

#[test]
fn symlinks_inside_repo_root_are_sealed() {
    let fx = sidecar_fx();
    sidecar_links(&fx.main);
    let tree = fx.tree("a");
    let set = adopt(&fx.ctx(), &tree).unwrap();
    assert_eq!(set.dirs, rps(&["node_modules", "sidecar/node_modules"]));
    let dir = fx.store().deps_dir(&fx.repo, &set.id);
    assert_eq!(
        fs::read_link(dir.join("node_modules/.bin/tsc")).unwrap(),
        Path::new("../typescript/bin/tsc")
    );
    let cb = dir.join("sidecar/node_modules/cloakbrowser");
    assert_eq!(
        fs::read_link(&cb).unwrap(),
        Path::new("../vendor/cloakbrowser")
    );
    assert!(!cb.exists(), "dangling inside the sealed set");
    let base = fx.tree("base");
    clone_into(fx.store(), &set, &base).unwrap();
    assert_eq!(
        fs::canonicalize(base.join("sidecar/node_modules/cloakbrowser")).unwrap(),
        fs::canonicalize(base.join("sidecar/vendor/cloakbrowser")).unwrap()
    );
    assert_eq!(
        fs::read(base.join("sidecar/node_modules/cloakbrowser/index.js")).unwrap(),
        b"vendored\n"
    );

    // Links leaving the repository root.
    for (name, target) in [("abs", "/tmp/elsewhere"), ("up", "../../../z")] {
        let fx = sidecar_fx();
        sidecar_links(&fx.main);
        fs::create_dir_all(fx.main.join("node_modules/x")).unwrap();
        std::os::unix::fs::symlink(target, fx.main.join("node_modules/x").join(name)).unwrap();
        let tree = fx.tree("a");
        let e = adopt(&fx.ctx(), &tree).unwrap_err();
        assert_eq!(e.code, Code::Drift, "{name}: {e:?}");
        assert_eq!(reason(&e), Some("symlink_escapes"));
        assert_eq!(
            detail_str(&e, "path"),
            Some(format!("node_modules/x/{name}").as_str())
        );
        no_trace(&fx);
    }

    // Cold installs: the same rule.
    for (name, target) in [("abs", "/tmp/elsewhere"), ("up", "../../../z")] {
        let fx = sidecar_fx();
        fx.sb.set_install_script(&format!(
            "{SIDECAR_INSTALL}mkdir -p node_modules/x\nln -s {target} node_modules/x/{name}\n"
        ));
        let tree = fx.tree("a");
        let e = build(&fx.ctx(), &tree, None).unwrap_err();
        assert_eq!(e.code, Code::Unknown, "{name}: {e:?}");
        assert_eq!(reason(&e), Some("symlink_escapes"));
        let staging = PathBuf::from(detail_str(&e, "staging").unwrap());
        assert!(staging.join("tree").is_dir(), "scene kept");
    }
    let fx = sidecar_fx();
    fx.sb.set_install_script(SIDECAR_INSTALL);
    let tree = fx.tree("a");
    let set = build(&fx.ctx(), &tree, None).unwrap();
    let dir = fx.store().deps_dir(&fx.repo, &set.id);
    assert_eq!(
        fs::read_link(dir.join("sidecar/node_modules/cloakbrowser")).unwrap(),
        Path::new("../vendor/cloakbrowser")
    );
}

#[test]
fn seed_cache_is_cloned_not_sealed() {
    let mut fx = Fx::new();
    fx.cfg.seed_caches = rps(&["node_modules/.cache", ".next/cache"]);
    assert_eq!(nested_seed_caches(&fx.cfg), rps(&["node_modules/.cache"]));
    fx.main_deps();
    let cache = fx.main.join("node_modules/.cache/babel");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("x.json"), b"{}").unwrap();
    let x = rp("node_modules/.cache/babel/x.json");
    let gen_before = strata_apfs::attrs_of(&fx.main, &x).unwrap().gen_count;
    let tree = fx.tree("a");
    let set = adopt(&fx.ctx(), &tree).unwrap();
    let dir = fx.store().deps_dir(&fx.repo, &set.id);
    assert!(dir.join("node_modules/a/index.js").is_file());
    assert!(!dir.join("node_modules/.cache").exists());
    assert_eq!(fs::read(cache.join("x.json")).unwrap(), b"{}");
    assert_eq!(
        strata_apfs::attrs_of(&fx.main, &x).unwrap().gen_count,
        gen_before
    );
    let dst = fx.sb.root.join("dst");
    fs::create_dir_all(&dst).unwrap();
    clone_into(fx.store(), &set, &dst).unwrap();
    assert!(dst.join("node_modules/a").is_dir());
    assert!(!dst.join("node_modules/.cache").exists());

    // Cold install output: the nested cache is not sealed either.
    let mut fx = Fx::new();
    fx.cfg.seed_caches = rps(&["node_modules/.cache", ".next/cache"]);
    fx.sb.set_install_script(&format!("{DEFAULT_INSTALL}mkdir -p node_modules/.cache/babel\necho 1 > node_modules/.cache/babel/x.json\n"));
    let tree = fx.tree("a");
    let set = build(&fx.ctx(), &tree, None).unwrap();
    let dir = fx.store().deps_dir(&fx.repo, &set.id);
    assert!(dir.join("node_modules/x/index.js").is_file());
    assert!(!dir.join("node_modules/.cache").exists());
}

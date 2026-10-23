//! Task 1: install-input hash and deps set id (deps.md §7).

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use strata_deps::{DepsConfigKey, inputs_hash, is_input, platform, set_id, toolchain};
use strata_types::{Code, RepoConfig};

fn hash(sb: &Sandbox, cfg: &RepoConfig, git: &strata_gitio::Git, root: &Path) -> String {
    let _ = sb;
    inputs_hash(cfg, git, root).unwrap_or_else(|e| panic!("inputs_hash {}: {e:?}", root.display()))
}

/// A sandbox with a main checkout (for `Git`) and a config naming `inputs`.
fn setup(inputs: &[&str]) -> (Sandbox, std::path::PathBuf, strata_gitio::Git, RepoConfig) {
    let sb = Sandbox::new();
    let main = sb.init_project("main");
    let git = sb.open(&main);
    let mut cfg = sb.config(&main);
    cfg.install_inputs = rps(inputs);
    (sb, main, git, cfg)
}

#[test]
fn inputs_hash_ignores_manifest_version() {
    let (sb, _main, git, cfg) = setup(&["package.json", "composer.json", "package-lock.json"]);
    let dir = sb.path("export");
    fs::create_dir_all(&dir).unwrap();
    let put = |rel: &str, s: &str| fs::write(dir.join(rel), s).unwrap();

    put("package.json", r#"{"name":"tool","version":"1.0.87","dependencies":{"x":"^1.0.0"}}"#);
    put("composer.json", r#"{"name":"a/b","version":"1.0.87","require":{"php":">=8"}}"#);
    put(
        "package-lock.json",
        r#"{"name":"tool","version":"1.0.87","lockfileVersion":3,"packages":{"":{"name":"tool","version":"1.0.87"},"node_modules/x":{"version":"1.0.0"}}}"#,
    );
    let h0 = hash(&sb, &cfg, &git, &dir);

    put("package.json", r#"{"name":"tool","version":"1.0.88","dependencies":{"x":"^1.0.0"}}"#);
    assert_eq!(hash(&sb, &cfg, &git, &dir), h0, "package.json version bump");
    put("composer.json", r#"{"name":"a/b","version":"1.0.88","require":{"php":">=8"}}"#);
    assert_eq!(hash(&sb, &cfg, &git, &dir), h0, "composer.json version bump");
    put(
        "package-lock.json",
        r#"{"name":"tool","version":"1.0.88","lockfileVersion":3,"packages":{"":{"name":"tool","version":"1.0.88"},"node_modules/x":{"version":"1.0.0"}}}"#,
    );
    assert_eq!(hash(&sb, &cfg, &git, &dir), h0, "lock top-level and packages[\"\"] version");

    // A jq-style rewrite: indentation, key order, trailing newline.
    put("package.json", "{\n    \"dependencies\": {\n        \"x\": \"^1.0.0\"\n    },\n    \"version\": \"9.9.9\",\n    \"name\": \"tool\"\n}\n");
    assert_eq!(hash(&sb, &cfg, &git, &dir), h0, "jq rewrite");

    put("package.json", r#"{"name":"tool","version":"1.0.88","dependencies":{"x":"^1.0.0","y":"^2.0.0"}}"#);
    let h_dep = hash(&sb, &cfg, &git, &dir);
    assert_ne!(h_dep, h0, "added dependency");
    put("package.json", r#"{"name":"tool","version":"1.0.88","dependencies":{"x":"^1.0.0"}}"#);
    assert_eq!(hash(&sb, &cfg, &git, &dir), h0);

    put(
        "package-lock.json",
        r#"{"name":"tool","version":"1.0.88","lockfileVersion":3,"packages":{"":{"name":"tool","version":"1.0.88"},"node_modules/x":{"version":"1.0.1"}}}"#,
    );
    assert_ne!(hash(&sb, &cfg, &git, &dir), h0, "a dependency's version");

    // Unparsable / non-object JSON: raw bytes, no error.
    put("package.json", "{not json");
    let a = hash(&sb, &cfg, &git, &dir);
    put("package.json", "{not json ");
    let b = hash(&sb, &cfg, &git, &dir);
    assert_ne!(a, b, "raw bytes count");
    put("package.json", "[1,2,3]");
    let c = hash(&sb, &cfg, &git, &dir);
    put("package.json", "[1, 2, 3]");
    assert_ne!(c, hash(&sb, &cfg, &git, &dir), "non-object JSON is raw bytes");
}

#[test]
fn inputs_hash_covers_files_dirs_and_absence() {
    let (sb, main, git, cfg) = setup(&["package.json", "bun.lock", "patches", "sidecar"]);
    let h0 = hash(&sb, &cfg, &git, &main);

    // An export (no .git) with the same content hashes the same.
    let export = sb.path("export");
    sb.archive_into(&main, "HEAD", &export);
    assert_eq!(hash(&sb, &cfg, &git, &export), h0, "export == worktree");

    // A one-byte change to bun.lock.
    fs::write(main.join("bun.lock"), b"lock v2\n").unwrap();
    assert_ne!(hash(&sb, &cfg, &git, &main), h0, "bun.lock byte");
    fs::write(main.join("bun.lock"), b"lock v1\n").unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &main), h0);

    // An ignored .DS_Store inside an input dir does not count.
    fs::write(main.join("patches/.DS_Store"), b"junk").unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &main), h0, "ignored .DS_Store");

    // Mode-only change does not count.
    fs::set_permissions(main.join("bun.lock"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &main), h0, "mode only");

    // An untracked, not-ignored patch counts.
    fs::write(main.join("patches/new.patch"), b"new\n").unwrap();
    let h_patch = hash(&sb, &cfg, &git, &main);
    assert_ne!(h_patch, h0, "untracked patch");
    fs::remove_file(main.join("patches/new.patch")).unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &main), h0);

    // `sidecar` is missing: no error. Present differs from missing; an empty
    // dir is the same as missing.
    fs::create_dir(main.join("sidecar")).unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &main), h0, "empty dir == missing");
    fs::write(main.join("sidecar/x"), b"x").unwrap();
    assert_ne!(hash(&sb, &cfg, &git, &main), h0, "present != missing");
    fs::remove_dir_all(main.join("sidecar")).unwrap();

    fs::remove_file(main.join("bun.lock")).unwrap();
    let h_missing = hash(&sb, &cfg, &git, &main);
    assert_ne!(h_missing, h0, "missing bun.lock");
    fs::write(main.join("bun.lock"), b"").unwrap();
    assert_ne!(hash(&sb, &cfg, &git, &main), h_missing, "empty file != missing");
}

#[test]
fn inputs_hash_never_follows_symlinks() {
    let (sb, _main, git, cfg) = setup(&["package.json", "bun.lock", "sidecar/package.json"]);
    let dir = sb.path("ws");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("package.json"), PACKAGE_JSON).unwrap();
    let outside = sb.path("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("lock-a"), b"a\n").unwrap();
    fs::write(outside.join("lock-b"), b"a\n").unwrap();
    symlink(outside.join("lock-a"), dir.join("bun.lock")).unwrap();

    let h0 = hash(&sb, &cfg, &git, &dir);
    fs::write(outside.join("lock-a"), b"changed\n").unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &dir), h0, "target content does not count");

    fs::remove_file(dir.join("bun.lock")).unwrap();
    symlink(outside.join("lock-b"), dir.join("bun.lock")).unwrap();
    let h1 = hash(&sb, &cfg, &git, &dir);
    assert_ne!(h1, h0, "link target string counts");

    // An intermediate symlink: sidecar -> outside dir with a package.json.
    fs::write(outside.join("package.json"), PACKAGE_JSON).unwrap();
    symlink(&outside, dir.join("sidecar")).unwrap();
    let h_link = hash(&sb, &cfg, &git, &dir);
    fs::remove_file(dir.join("sidecar")).unwrap();
    assert_eq!(hash(&sb, &cfg, &git, &dir), h_link, "sidecar/package.json behind a link is Absent");
    symlink(&outside, dir.join("sidecar")).unwrap();

    // An unreadable target still hashes (only the link is read).
    fs::set_permissions(outside.join("lock-b"), fs::Permissions::from_mode(0o000)).unwrap();
    let r = inputs_hash(&cfg, &git, &dir);
    fs::set_permissions(outside.join("lock-b"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(r.unwrap(), h1);
}

fn mkfifo(p: &Path) {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0, "mkfifo");
}

#[test]
fn inputs_hash_rejects_special_files() {
    let (sb, _main, git, cfg) = setup(&["package.json", "bun.lock", "patches"]);
    let dir = sb.path("ws");
    fs::create_dir_all(dir.join("patches")).unwrap();
    fs::write(dir.join("package.json"), PACKAGE_JSON).unwrap();
    mkfifo(&dir.join("bun.lock"));

    let t = Instant::now();
    let e = inputs_hash(&cfg, &git, &dir).unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(1), "must not block on the FIFO");
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert_eq!(e.detail.as_ref().and_then(|d| d["path"].as_str()), Some("bun.lock"), "{e:?}");

    fs::remove_file(dir.join("bun.lock")).unwrap();
    fs::write(dir.join("bun.lock"), b"lock\n").unwrap();
    inputs_hash(&cfg, &git, &dir).unwrap();

    mkfifo(&dir.join("patches/p.fifo"));
    let t = Instant::now();
    let e = inputs_hash(&cfg, &git, &dir).unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(1));
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert_eq!(e.detail.as_ref().and_then(|d| d["path"].as_str()), Some("patches/p.fifo"));
    fs::remove_file(dir.join("patches/p.fifo")).unwrap();

    let _sock = std::os::unix::net::UnixListener::bind(dir.join("patches/s.sock")).unwrap();
    let e = inputs_hash(&cfg, &git, &dir).unwrap_err();
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert_eq!(e.detail.as_ref().and_then(|d| d["path"].as_str()), Some("patches/s.sock"));
}

const CHILD_ENV: &str = "STRATA_DEPS_SET_ID_CHILD";

fn sample_id() -> String {
    let key = DepsConfigKey {
        deps_dirs: rps(&["node_modules", "web/node_modules"]),
        nested_seed_caches: vec![],
        install: vec!["bun".into(), "install".into()],
    };
    set_id("abc", "arm64-apple-darwin", "bun 1.3.3", &key).as_str().to_string()
}

#[test]
fn set_id_is_deterministic_and_unambiguous() {
    if std::env::var_os(CHILD_ENV).is_some() {
        println!("\nSET_ID={}", sample_id());
        return;
    }
    let id = sample_id();
    assert_eq!(id.len(), 64);
    assert!(id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{id}");

    // Same id in another process.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "set_id_is_deterministic_and_unambiguous", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let child = stdout
        .lines()
        .find_map(|l| l.split_once("SET_ID=").map(|(_, r)| r.trim()))
        .expect("child id");
    assert_eq!(child, id);

    let base = DepsConfigKey {
        deps_dirs: rps(&["node_modules", "web/node_modules"]),
        nested_seed_caches: vec![],
        install: vec!["bun".into(), "install".into()],
    };
    let id_of = |h: &str, p: &str, t: &str, k: &DepsConfigKey| set_id(h, p, t, k).as_str().to_string();
    let e = DepsConfigKey::empty();
    assert_ne!(id_of("ab", "c", "", &e), id_of("a", "bc", "", &e), "length prefixes");
    assert_ne!(id_of("abc", "arm64-apple-darwin", "bun 1.3.4", &base), id, "toolchain");
    assert_ne!(id_of("abc", "x86_64-apple-darwin", "bun 1.3.3", &base), id, "platform");

    let mut reordered = base.clone();
    reordered.deps_dirs.reverse();
    assert_eq!(id_of("abc", "arm64-apple-darwin", "bun 1.3.3", &reordered), id, "deps_dirs order");
    let mut extra = base.clone();
    extra.deps_dirs.push(rp("api/node_modules"));
    assert_ne!(id_of("abc", "arm64-apple-darwin", "bun 1.3.3", &extra), id, "extra deps dir");
    let mut frozen = base.clone();
    frozen.install.push("--frozen-lockfile".into());
    assert_ne!(id_of("abc", "arm64-apple-darwin", "bun 1.3.3", &frozen), id, "install argv");
    let mut nested = base.clone();
    nested.nested_seed_caches.push(rp("node_modules/.cache"));
    assert_ne!(id_of("abc", "arm64-apple-darwin", "bun 1.3.3", &nested), id, "nested seed cache");

    // DepsConfigKey::of
    let sb = Sandbox::new();
    let mut cfg = sb.config(&sb.root);
    cfg.deps_dirs = rps(&["web/node_modules", "node_modules"]);
    cfg.seed_caches = rps(&[".cache", "node_modules/.cache", "elsewhere/.cache"]);
    let k = DepsConfigKey::of(&cfg).unwrap();
    assert_eq!(k.deps_dirs, rps(&["node_modules", "web/node_modules"]));
    assert_eq!(k.nested_seed_caches, rps(&["node_modules/.cache"]));
    assert_eq!(k.install, vec!["bun", "install", "--frozen-lockfile"]);
    cfg.install = Some("bun 'install".into());
    assert_eq!(DepsConfigKey::of(&cfg).unwrap_err().code, Code::InvalidInput);

    // Fixed constants: the empty inputs hash and the empty-everything id.
    let empty_hash = strata_deps::empty_inputs_hash();
    let (_sb2, _main, git, mut cfg2) = setup(&[]);
    cfg2.install_inputs.clear();
    assert_eq!(inputs_hash(&cfg2, &git, &sb.root).unwrap(), empty_hash);
    assert_eq!(empty_hash, EMPTY_INPUTS_HASH);
    assert_eq!(set_id(&empty_hash, "", "", &DepsConfigKey::empty()).as_str(), EMPTY_SET_ID);
    assert!(platform().ends_with("-apple-darwin"));
}

const EMPTY_INPUTS_HASH: &str = "71ce361f2ac2b0acf249941307a1d87bafd2960c5808f0a27c9ccd6759bd138b";
const EMPTY_SET_ID: &str = "ee45535b20313741a01ac686734df8f390cedd8f76863530707ac71595d421b3";

#[test]
fn toolchain_is_install_program_version() {
    let sb = Sandbox::new();
    let cfg = sb.config(&sb.root);
    let env = sb.install_env();
    assert_eq!(toolchain(&cfg, &env).unwrap(), "bun 1.3.3");
    assert_eq!(toolchain(&cfg, &env).unwrap(), "bun 1.3.3", "cached");

    // Replacing the program (new inode, size, mtime) invalidates the cache.
    sb.set_bun_version("1.3.4");
    assert_eq!(toolchain(&cfg, &env).unwrap(), "bun 1.3.4");

    // stderr fallback.
    sb.write_program("bun", "#!/bin/sh\necho '' ; echo '  bun 9 (stderr) ' >&2\n");
    let t = toolchain(&cfg, &env).unwrap();
    assert_eq!(t, "bun bun 9 (stderr)");

    // No install: empty.
    let mut none = cfg.clone();
    none.install = None;
    assert_eq!(toolchain(&none, &env).unwrap(), "");

    // Missing program.
    let mut missing = cfg.clone();
    missing.install = Some("nosuchprog install".into());
    let e = toolchain(&missing, &env).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    let d = e.detail.unwrap();
    assert_eq!(d["program"], "nosuchprog");
    assert_eq!(d["path"], env.path.as_str());
}

#[test]
fn is_input_matches_entries_and_members() {
    let sb = Sandbox::new();
    let mut cfg = sb.config(&sb.root);
    cfg.install_inputs = rps(&["package.json", "bun.lock", "patches"]);
    for p in ["package.json", "patches", "patches/a.patch", "patches/sub/b.patch", "bun.lock"] {
        assert!(is_input(&cfg, &rp(p)), "{p}");
    }
    for p in ["patches2/a.patch", "sub/package.json", "bun.lockb", "node_modules/x/package.json"] {
        assert!(!is_input(&cfg, &rp(p)), "{p}");
    }
    cfg.install_inputs.clear();
    for p in ["package.json", "patches/a.patch"] {
        assert!(!is_input(&cfg, &rp(p)), "{p}");
    }
}

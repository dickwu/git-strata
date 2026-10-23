//! deps.md §7, Task 3: sealed generations, the build lock and cold installs.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use common::*;
use strata_deps::{
    BuildKind, BuildState, Ctx, InstallEnv, attempts, build, clone_into, find_set, key_of,
    test_seams,
};
use strata_store::{AttemptKind, AttemptState, DepsOrigin, Store};
use strata_types::{Code, Paths};

const SLEEPY_INSTALL: &str = r#"touch "$STATE/ready.$n"
sleep 60
"#;

fn install_with(extra: &str) -> String {
    format!("{DEFAULT_INSTALL}{extra}")
}

fn store_attempts(fx: &Fx) -> Vec<strata_store::DepsAttempt> {
    fx.store().deps_attempts(Some(&fx.repo)).unwrap()
}

fn count_tree(dir: &Path) -> (u64, u64) {
    let (mut files, mut dirs) = (0, 0);
    for e in snapshot(dir) {
        match e.1.as_str() {
            "file" => files += 1,
            "dir" => dirs += 1,
            _ => {}
        }
    }
    (files, dirs)
}

/// Worker processes (killed builder, foreign lock holder).
#[test]
fn worker() {
    let Some(mode) = worker_mode() else { return };
    let var = |k: &str| PathBuf::from(std::env::var_os(k).unwrap_or_else(|| panic!("{k} not set")));
    match mode.as_str() {
        "build" => {
            let root = var("STRATA_ROOT");
            let main = var("STRATA_MAIN");
            let home = var("STRATA_HOME");
            let xdg = home.join(".config");
            let store = Store::open(&root, &[&main]).unwrap();
            let git = strata_gitio::Git::open_with_env(
                &main,
                &[
                    ("HOME", Some(home.as_os_str())),
                    ("XDG_CONFIG_HOME", Some(xdg.as_os_str())),
                    ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
                ],
            )
            .unwrap();
            let cfg = bun_config(&main);
            let env = InstallEnv {
                path: format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    var("STRATA_BIN").display()
                ),
                timeout: Duration::from_secs(120),
            };
            let paths = Paths { root };
            let repo = repo_name("tool");
            let lock = RecLock::default();
            let ctx = Ctx {
                store: &store,
                paths: &paths,
                repo: &repo,
                cfg: &cfg,
                git: &git,
                env: &env,
                locks: &lock,
            };
            let r = build(&ctx, &var("STRATA_TREE"), None);
            eprintln!("worker build returned {r:?}");
            std::process::exit(0);
        }
        "hold" => {
            let lock = var("STRATA_LOCK");
            fs::create_dir_all(lock.parent().unwrap()).unwrap();
            let f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&lock)
                .unwrap();
            // SAFETY: flock on our own descriptor.
            assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
            fs::write(var("STRATA_READY"), b"1").unwrap();
            let ms: u64 = std::env::var("STRATA_HOLD_MS").unwrap().parse().unwrap();
            std::thread::sleep(Duration::from_millis(ms));
            let t = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            fs::write(var("STRATA_RELEASED"), format!("{t:.6}")).unwrap();
            drop(f);
            std::process::exit(0);
        }
        other => panic!("unknown worker mode {other}"),
    }
}

fn spawn_build_worker(fx: &Fx, tree: &Path) -> std::process::Child {
    spawn_worker(
        "worker",
        "build",
        &[
            ("STRATA_ROOT", fx.paths.root.as_os_str()),
            ("STRATA_MAIN", fx.main.as_os_str()),
            ("STRATA_HOME", fx.sb.home.as_os_str()),
            ("STRATA_BIN", fx.sb.bin.as_os_str()),
            ("STRATA_TREE", tree.as_os_str()),
        ],
    )
}

#[test]
fn build_seals_install_output_without_seed_caches() {
    test_seams::reset();
    let mut fx = Fx::new();
    fx.cfg.deps_dirs = rps(&["node_modules", "public/pdfium"]);
    fx.cfg.seed_caches = rps(&[".cache", "node_modules/.cache"]);
    fx.sb.set_install_script(&install_with(
        "mkdir -p node_modules/.cache public/pdfium\necho y > node_modules/.cache/y\necho z > public/pdfium/z.wasm\n",
    ));
    // SAFETY: a variable no other code reads; set before any install runs.
    unsafe { std::env::set_var("GIT_DEPS_TEST_MARKER", "1") };
    let tree = fx.tree("a");
    let ctx = fx.ctx();
    let (h, id) = key_of(&ctx, &tree).unwrap();
    let set = build(&ctx, &tree, None).unwrap();
    assert_eq!(set.id, id);
    assert_eq!(set.dirs, rps(&["node_modules", "public/pdfium"]));
    assert_eq!(set.inputs_hash, h);
    assert_eq!(
        set.inputs_hash,
        strata_deps::inputs_hash(&fx.cfg, &fx.git, &tree).unwrap()
    );
    assert_eq!(set.toolchain, "bun 1.3.3");

    let dir = fx.store().deps_dir(&fx.repo, &id);
    assert!(dir.join("node_modules/x/index.js").is_file());
    assert!(dir.join("public/pdfium/z.wasm").is_file());
    assert!(!dir.join("node_modules/.cache").exists());
    assert_eq!(mode_of(&dir), 0o555);
    assert_eq!(mode_of(&dir.join("node_modules")), 0o755);

    let atts = store_attempts(&fx);
    assert_eq!(atts.len(), 1);
    let a = &atts[0];
    assert_eq!(
        (a.kind, a.state),
        (AttemptKind::Build, AttemptState::Sealed)
    );
    assert_eq!(fx.sb.install_count(), 1);
    let staging = fx.store().deps_staging_dir(&fx.repo, a.id);
    assert_eq!(
        fx.sb.install_file("cwd", 1).trim(),
        staging.join("tree").to_str().unwrap()
    );
    assert!(!staging.exists(), "staging retired");
    assert!(fx.trash_count() >= 1, "staging went to the trash");
    let env = fx.sb.install_file("env", 1);
    assert!(
        env.lines().any(|l| l == format!("PATH={}", fx.env.path)),
        "{env}"
    );
    assert!(!env.lines().any(|l| l.starts_with("GIT_")), "{env}");
    assert_eq!(
        fx.sb.install_file("args", 1).trim(),
        "install --frozen-lockfile"
    );
    let null = std::process::Command::new("stat")
        .args(["-f", "%Z", "/dev/null"])
        .output()
        .unwrap();
    assert_eq!(
        fx.sb.install_file("stdin", 1).trim(),
        String::from_utf8_lossy(&null.stdout).trim()
    );

    assert_eq!(
        find_set(fx.store(), &fx.repo, &id).unwrap(),
        Some(set.clone())
    );
    let rec = fx.store().deps_set(&fx.repo, &id).unwrap().unwrap();
    assert_eq!(rec.origin, DepsOrigin::Build);
    let (files, dirs) = count_tree(&dir);
    assert_eq!(test_seams::sync_counts(), (files, dirs, 1));
    assert!(
        attempts(fx.store(), &fx.paths, Some(&fx.repo))
            .unwrap()
            .is_empty()
    );
    fx.close_store();
}

#[test]
fn build_failure_is_unknown_and_keeps_scene() {
    let fx = Fx::new();
    fx.sb
        .set_install_script("echo boom-on-stdout\necho boom-on-stderr >&2\nexit 3\n");
    let tree = fx.tree("a");
    let ctx = fx.ctx();
    let (_, id) = key_of(&ctx, &tree).unwrap();
    let e = build(&ctx, &tree, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e), Some("install_failed"));
    let d = e.detail.clone().unwrap();
    let a = d["attempt"].as_u64().unwrap();
    let staging = PathBuf::from(d["staging"].as_str().unwrap());
    assert_eq!(staging, fx.store().deps_staging_dir(&fx.repo, a));
    assert_eq!(
        d["command"],
        serde_json::json!(["bun", "install", "--frozen-lockfile"])
    );
    assert_eq!(d["exit_code"], 3);
    let tail = d["log_tail"].as_str().unwrap();
    assert!(
        tail.contains("boom-on-stdout") && tail.contains("boom-on-stderr"),
        "{tail}"
    );
    assert!(staging.join("tree").is_dir());
    assert!(staging.join("install.log").is_file());
    assert_eq!(find_set(fx.store(), &fx.repo, &id).unwrap(), None);
    let listed = attempts(fx.store(), &fx.paths, Some(&fx.repo)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].id, listed[0].state, listed[0].kind),
        (a, BuildState::Failed, BuildKind::Build)
    );
    assert_eq!(
        listed[0].error.as_ref().map(|e| e.code),
        Some(Code::Unknown)
    );

    // D8: no replay over a kept scene.
    let e2 = build(&ctx, &tree, None).unwrap_err();
    assert_eq!(reason(&e2), Some("previous_attempt"), "{e2:?}");
    assert_eq!(e2.detail.as_ref().unwrap()["attempt"].as_u64(), Some(a));
    assert_eq!(fx.sb.install_count(), 1);

    // Once the scene is removed, install runs again.
    fs::remove_dir_all(&staging).unwrap();
    fx.sb.set_install_script(DEFAULT_INSTALL);
    let set = build(&ctx, &tree, None).unwrap();
    assert_eq!(set.id, id);
    assert_eq!(fx.sb.install_count(), 2);
}

#[test]
fn build_timeout_kills_install_group() {
    let mut fx = Fx::new();
    fx.env.timeout = Duration::from_secs(1);
    fx.sb
        .set_install_script("sleep 60 &\necho $! > \"$STATE/grandchild\"\nsleep 60\n");
    let tree = fx.tree("a");
    let ctx = fx.ctx();
    let start = Instant::now();
    let e = build(&ctx, &tree, None).unwrap_err();
    let took = start.elapsed();
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_secs(8),
        "{took:?}"
    );
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(reason(&e), Some("timeout"), "{e:?}");
    let install: i32 = fx.sb.install_file("pid", 1).trim().parse().unwrap();
    let grandchild: i32 = fs::read_to_string(fx.sb.state.join("grandchild"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while (pid_alive(install) || pid_alive(grandchild)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_alive(install), "install still alive");
    assert!(!pid_alive(grandchild), "grandchild still alive");
    let staging = PathBuf::from(e.detail.as_ref().unwrap()["staging"].as_str().unwrap());
    assert!(staging.join("tree").is_dir());
    let listed = attempts(fx.store(), &fx.paths, Some(&fx.repo)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, BuildState::Failed);
    assert_eq!(listed[0].pid, Some(install));
}

#[test]
fn seal_refuses_when_staging_does_not_match_key() {
    let fx = Fx::new();
    let cases = [
        (
            "modify",
            "mkdir -p node_modules/x\necho x > node_modules/x/a\necho changed >> bun.lock\n"
                .to_string(),
            "inputs_modified",
        ),
        ("none", "true\n".to_string(), "no_deps_dirs"),
        (
            "abs",
            install_with("ln -s /etc/hosts node_modules/abs\n"),
            "symlink_escapes",
        ),
        (
            "fifo",
            install_with("mkfifo node_modules/f\n"),
            "special_file",
        ),
    ];
    let ctx = fx.ctx();
    let mut stagings = Vec::new();
    for (tag, script, want) in &cases {
        fx.sb.set_install_script(script);
        let tree = fx.tree_tagged(tag, tag);
        let (_, id) = key_of(&ctx, &tree).unwrap();
        let e = build(&ctx, &tree, None).unwrap_err();
        assert_eq!(e.code, Code::Unknown, "{tag}: {e:?}");
        assert_eq!(reason(&e), Some(*want), "{tag}: {e:?}");
        assert!(!fx.store().deps_dir(&fx.repo, &id).exists(), "{tag}");
        assert_eq!(find_set(fx.store(), &fx.repo, &id).unwrap(), None);
        let staging = PathBuf::from(e.detail.as_ref().unwrap()["staging"].as_str().unwrap());
        stagings.push(staging);
    }
    for s in &stagings {
        assert!(s.is_dir(), "{} kept", s.display());
    }
    let listed = attempts(fx.store(), &fx.paths, Some(&fx.repo)).unwrap();
    assert_eq!(listed.len(), 4);
    assert!(listed.iter().all(|a| a.state == BuildState::Failed));
    assert!(
        store_attempts(&fx)
            .iter()
            .all(|a| a.state == AttemptState::Failed)
    );
    assert_eq!(fx.sb.install_count(), 4);
}

fn kill_worker_and_install(fx: &mut Fx, worker: &mut std::process::Child) -> i32 {
    // SAFETY: the worker leads its own group.
    unsafe { libc::killpg(worker.id() as i32, libc::SIGKILL) };
    worker.wait().unwrap();
    fx.reopen_store();
    let running: Vec<_> = store_attempts(fx)
        .into_iter()
        .filter(|a| a.state == AttemptState::Running)
        .collect();
    assert_eq!(running.len(), 1);
    let pid = running[0].pid.expect("install pid recorded");
    // SAFETY: install leads its own group (setpgid).
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while group_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!group_alive(pid));
    pid
}

fn lock_is_free(p: &Path) -> bool {
    let f = fs::File::open(p).unwrap();
    // SAFETY: flock on our own descriptor.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[test]
fn killed_builder_leaves_undiscoverable_staging_and_free_lock() {
    let mut fx = Fx::new();
    let tree_a = fx.tree_tagged("a", "a");
    let (_, id_a) = key_of(&fx.ctx(), &tree_a).unwrap();

    fx.sb.set_install_script(SLEEPY_INSTALL);
    fx.close_store();
    let mut w = spawn_build_worker(&fx, &tree_a);
    wait_for_file(&fx.sb.state.join("ready.1"), Duration::from_secs(60));
    kill_worker_and_install(&mut fx, &mut w);

    assert!(lock_is_free(&fx.lock_path(&id_a)));
    assert_eq!(find_set(fx.store(), &fx.repo, &id_a).unwrap(), None);
    assert!(!fx.store().deps_dir(&fx.repo, &id_a).exists());
    let listed = attempts(fx.store(), &fx.paths, Some(&fx.repo)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, BuildState::Interrupted);
    let dead = listed[0].clone();
    assert!(dead.staging.is_dir());

    fx.sb.set_install_script(DEFAULT_INSTALL);
    let set = build(&fx.ctx(), &tree_a, None).unwrap();
    assert_eq!(set.id, id_a);
    assert_eq!(fx.sb.install_count(), 2);
    let closed = store_attempts(&fx)
        .into_iter()
        .find(|a| a.id == dead.id)
        .unwrap();
    assert_eq!(closed.state, AttemptState::Failed);
    assert_eq!(reason(closed.error.as_ref().unwrap()), Some("interrupted"));
    let listed = attempts(fx.store(), &fx.paths, Some(&fx.repo)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].id, listed[0].state),
        (dead.id, BuildState::Failed)
    );
    assert!(dead.staging.is_dir());

    // Tree B: killed twice in a row.
    let tree_b = fx.tree_tagged("b", "b");
    fx.sb.set_install_script(SLEEPY_INSTALL);
    for n in [3, 4] {
        fx.close_store();
        let mut w = spawn_build_worker(&fx, &tree_b);
        wait_for_file(
            &fx.sb.state.join(format!("ready.{n}")),
            Duration::from_secs(60),
        );
        kill_worker_and_install(&mut fx, &mut w);
    }
    let count = fx.sb.install_count();
    assert_eq!(count, 4);
    let e = build(&fx.ctx(), &tree_b, None).unwrap_err();
    assert_eq!(reason(&e), Some("previous_attempt"), "{e:?}");
    assert_eq!(fx.sb.install_count(), count);
    let newest = PathBuf::from(e.detail.as_ref().unwrap()["staging"].as_str().unwrap());
    fs::remove_dir_all(&newest).unwrap();
    fx.sb.set_install_script(DEFAULT_INSTALL);
    build(&fx.ctx(), &tree_b, None).unwrap();
    assert_eq!(fx.sb.install_count(), count + 1);
}

#[test]
fn failed_or_lockless_builds_never_publish() {
    let fx = Fx::new();
    let ctx = fx.ctx();
    let sealed_tree = fx.tree_tagged("sealed", "sealed");
    let sealed = build(&ctx, &sealed_tree, None).unwrap();
    let sealed_dir = fx.store().deps_dir(&fx.repo, &sealed.id);
    let before = snapshot(&sealed_dir);

    // Garbage staging with no record.
    let junk = fx.repo_dir().join(".staging/999");
    fs::create_dir_all(junk.join("node_modules/x")).unwrap();
    fs::write(junk.join("node_modules/x/i.js"), b"1").unwrap();
    assert!(
        attempts(fx.store(), &fx.paths, Some(&fx.repo))
            .unwrap()
            .is_empty()
    );

    // install exits 2.
    fx.sb.set_install_script(&install_with("exit 2\n"));
    let t2 = fx.tree_tagged("two", "two");
    let (_, id2) = key_of(&ctx, &t2).unwrap();
    let e = build(&ctx, &t2, None).unwrap_err();
    assert_eq!(reason(&e), Some("install_failed"));
    assert_eq!(find_set(fx.store(), &fx.repo, &id2).unwrap(), None);
    assert!(!fx.store().deps_dir(&fx.repo, &id2).exists());

    // The lock file is a directory.
    fx.sb.set_install_script(DEFAULT_INSTALL);
    let t3 = fx.tree_tagged("three", "three");
    let (_, id3) = key_of(&ctx, &t3).unwrap();
    fs::create_dir_all(fx.lock_path(&id3)).unwrap();
    let n_att = store_attempts(&fx).len();
    let stagings = fx.staging_dirs();
    let installs = fx.sb.install_count();
    let e = build(&ctx, &t3, None).unwrap_err();
    assert_eq!(e.code, Code::IsDirectory, "{e:?}");
    assert_eq!(store_attempts(&fx).len(), n_att);
    assert_eq!(fx.staging_dirs(), stagings);
    assert_eq!(fx.sb.install_count(), installs);
    assert_eq!(find_set(fx.store(), &fx.repo, &id3).unwrap(), None);

    assert_eq!(snapshot(&sealed_dir), before);
    assert_eq!(
        find_set(fx.store(), &fx.repo, &sealed.id).unwrap(),
        Some(sealed)
    );
}

/// `build` refuses before any attempt; returns the error.
fn refused_before_attempt(fx: &Fx, tree: &Path) -> strata_types::StrataError {
    let e = build(&fx.ctx(), tree, None).unwrap_err();
    assert!(store_attempts(fx).is_empty(), "{e:?}");
    assert!(fx.staging_dirs().is_empty(), "{e:?}");
    assert_eq!(fx.sb.install_count(), 0);
    e
}

#[test]
fn build_lock_is_rooted_and_regular_file_only() {
    let fx = Fx::new();
    let tree = fx.tree("a");
    let (_, id) = key_of(&fx.ctx(), &tree).unwrap();
    let elsewhere = fx.sb.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let repo_dir = fx.repo_dir();
    let locks = repo_dir.join(".locks");
    let lock = fx.lock_path(&id);

    // <repo> itself is a symlink to another directory.
    std::os::unix::fs::symlink(&elsewhere, &repo_dir).unwrap();
    let e = refused_before_attempt(&fx, &tree);
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(reason(&e), Some("symlink"), "{e:?}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    fs::remove_file(&repo_dir).unwrap();

    // `.locks` is a symlink to another directory.
    fs::create_dir_all(&repo_dir).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &locks).unwrap();
    let e = refused_before_attempt(&fx, &tree);
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(detail_str(&e, "path"), Some(locks.to_str().unwrap()));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    fs::remove_file(&locks).unwrap();

    // `.locks` is a regular file.
    fs::write(&locks, b"").unwrap();
    let e = refused_before_attempt(&fx, &tree);
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    fs::remove_file(&locks).unwrap();

    // The lock is a dangling symlink out of the sandbox.
    fs::create_dir_all(&locks).unwrap();
    let outside = std::env::temp_dir().join(format!("strata-deps-never-{}", std::process::id()));
    std::os::unix::fs::symlink(&outside, &lock).unwrap();
    let e = refused_before_attempt(&fx, &tree);
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert_eq!(reason(&e), Some("symlink"), "{e:?}");
    assert!(
        fs::symlink_metadata(&outside).is_err(),
        "target never created"
    );
    fs::remove_file(&lock).unwrap();

    // The lock is a FIFO with no writer: returns within a second.
    let st = std::process::Command::new("mkfifo")
        .arg(&lock)
        .status()
        .unwrap();
    assert!(st.success());
    let (tx, rx) = std::sync::mpsc::channel();
    let (root, main, home, bin, tree2) = (
        fx.paths.root.clone(),
        fx.main.clone(),
        fx.sb.home.clone(),
        fx.sb.bin.clone(),
        tree.clone(),
    );
    // The store is shared: run the call on another thread with a deadline.
    let store = fx.store_arc();
    std::thread::spawn(move || {
        let git = strata_gitio::Git::open_with_env(
            &main,
            &[
                ("HOME", Some(home.as_os_str())),
                ("XDG_CONFIG_HOME", Some(home.join(".config").as_os_str())),
                ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
            ],
        )
        .unwrap();
        let cfg = bun_config(&main);
        let env = InstallEnv {
            path: format!("{}:/usr/bin:/bin", bin.display()),
            timeout: Duration::from_secs(60),
        };
        let paths = Paths { root };
        let repo = repo_name("tool");
        let lock = RecLock::default();
        let ctx = Ctx {
            store: &store,
            paths: &paths,
            repo: &repo,
            cfg: &cfg,
            git: &git,
            env: &env,
            locks: &lock,
        };
        let _ = tx.send(build(&ctx, &tree2, None));
    });
    let r = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("build on a FIFO lock returns within 1 s");
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::NotFile, "{e:?}");
    assert_eq!(reason(&e), Some("fifo"), "{e:?}");
    assert!(store_attempts(&fx).is_empty());
    assert!(fx.staging_dirs().is_empty());
    fs::remove_file(&lock).unwrap();
    fs::remove_dir(&locks).unwrap();

    // Normal: the first build creates `.locks/` and the lock; later builds reuse it.
    let ctx = fx.ctx();
    build(&ctx, &tree, None).unwrap();
    let m = fs::symlink_metadata(&lock).unwrap();
    assert!(m.file_type().is_file());
    assert!(fs::symlink_metadata(&locks).unwrap().is_dir());
    build(&ctx, &tree, None).unwrap();
    assert_eq!(fs::symlink_metadata(&lock).unwrap().ino(), m.ino());
}

fn spawn_holder(
    fx: &Fx,
    lock: &Path,
    hold_ms: u64,
    tag: &str,
) -> (std::process::Child, PathBuf, PathBuf) {
    let ready = fx.sb.root.join(format!("holder-ready-{tag}"));
    let released = fx.sb.root.join(format!("holder-released-{tag}"));
    let ms = hold_ms.to_string();
    let w = spawn_worker(
        "worker",
        "hold",
        &[
            ("STRATA_LOCK", lock.as_os_str()),
            ("STRATA_READY", ready.as_os_str()),
            ("STRATA_RELEASED", released.as_os_str()),
            ("STRATA_HOLD_MS", OsStr::new(&ms)),
        ],
    );
    wait_for_file(&ready, Duration::from_secs(30));
    (w, ready, released)
}

#[test]
fn build_lock_wait_is_bounded() {
    let mut fx = Fx::new();
    fx.env.timeout = Duration::from_secs(2);
    let tree = fx.tree("a");
    let (_, id) = key_of(&fx.ctx(), &tree).unwrap();
    let lock = fx.lock_path(&id);

    let (mut w, _, _) = spawn_holder(&fx, &lock, 10_000, "1");
    let start = Instant::now();
    let e = build(&fx.ctx(), &tree, None).unwrap_err();
    let took = start.elapsed();
    let _ = w.kill();
    let _ = w.wait();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(3),
        "{took:?}"
    );
    assert_eq!(reason(&e), Some("build_lock_busy"));
    assert_eq!(detail_str(&e, "lock"), Some(lock.to_str().unwrap()));
    assert!(e.detail.as_ref().unwrap()["waited"].as_u64().unwrap() >= 2000);
    assert!(store_attempts(&fx).is_empty());
    assert!(fx.staging_dirs().is_empty());
    assert_eq!(fx.sb.install_count(), 0);

    fx.env.timeout = Duration::from_secs(60);
    let (mut w, _, released) = spawn_holder(&fx, &lock, 500, "2");
    let set = build(&fx.ctx(), &tree, None).unwrap();
    w.wait().unwrap();
    let rel: f64 = fs::read_to_string(&released)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let started: f64 = fx.sb.install_file("start", 1).trim().parse().unwrap();
    assert!(
        started > rel && started - rel < 1.0,
        "install started {:.3}s after release",
        started - rel
    );
    assert_eq!(set.id, id);
    let atts = store_attempts(&fx);
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].state, AttemptState::Sealed);
}

#[test]
fn orphan_sealed_dir_is_unknown_not_overwritten() {
    let fx = Fx::new();
    let tree = fx.tree("a");
    let ctx = fx.ctx();
    let (_, id) = key_of(&ctx, &tree).unwrap();
    let dir = fx.store().deps_dir(&fx.repo, &id);
    fs::create_dir_all(dir.join("node_modules")).unwrap();
    fs::write(dir.join("node_modules/q"), b"orphan").unwrap();
    let before = snapshot(&dir);
    let e = build(&ctx, &tree, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e), Some("orphan"));
    assert_eq!(detail_str(&e, "path"), Some(dir.to_str().unwrap()));
    assert_eq!(fx.lock.enters(), 1);
    assert_eq!(snapshot(&dir), before);
    assert_eq!(fx.sb.install_count(), 0);
    assert_eq!(find_set(fx.store(), &fx.repo, &id).unwrap(), None);
}

#[test]
fn clone_into_copies_set_dirs_privately() {
    let mut fx = Fx::new();
    fx.cfg.deps_dirs = rps(&["node_modules", "public/pdfium"]);
    fx.sb.set_install_script(&install_with(
        "mkdir -p public/pdfium\necho z > public/pdfium/z.wasm\n",
    ));
    let tree = fx.tree("a");
    let set = build(&fx.ctx(), &tree, None).unwrap();
    let src = fx.store().deps_dir(&fx.repo, &set.id);
    let sealed_before = snapshot(&src);

    let dst = fx.sb.root.join("dst");
    fs::create_dir_all(&dst).unwrap();
    clone_into(fx.store(), &set, &dst).unwrap();
    for d in ["node_modules", "public/pdfium"] {
        assert_eq!(snapshot(&dst.join(d)), snapshot(&src.join(d)));
        assert_eq!(mode_of(&dst.join(d)), 0o755);
    }
    let a = dst.join("node_modules/x/index.js");
    assert_ne!(
        fs::metadata(&a).unwrap().ino(),
        fs::metadata(src.join("node_modules/x/index.js"))
            .unwrap()
            .ino()
    );
    let dst2 = fx.sb.root.join("dst2");
    fs::create_dir_all(&dst2).unwrap();
    clone_into(fx.store(), &set, &dst2).unwrap();
    let other_before = snapshot(&dst2);
    fs::write(dst.join("node_modules/new.js"), b"new").unwrap();
    fs::remove_file(&a).unwrap();
    assert_eq!(snapshot(&src), sealed_before);
    assert_eq!(snapshot(&dst2), other_before);

    // dst/node_modules exists.
    let dst3 = fx.sb.root.join("dst3");
    fs::create_dir_all(dst3.join("node_modules")).unwrap();
    let e = clone_into(fx.store(), &set, &dst3).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(snapshot(&dst3).len(), 1);

    // dst/public is a symlink out.
    let dst4 = fx.sb.root.join("dst4");
    let outside = fx.sb.root.join("outside");
    fs::create_dir_all(&dst4).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, dst4.join("public")).unwrap();
    let e = clone_into(fx.store(), &set, &dst4).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert!(snapshot(&outside).is_empty());
    assert!(!dst4.join("node_modules").exists());

    // Record present, sealed dir gone.
    force_remove(&src);
    let dst5 = fx.sb.root.join("dst5");
    fs::create_dir_all(&dst5).unwrap();
    let e = clone_into(fx.store(), &set, &dst5).unwrap_err();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");
    assert!(snapshot(&dst5).is_empty());
}

#[test]
fn seal_runs_under_repo_write_lock() {
    let fx = Fx::new();
    let tree = fx.tree_tagged("a", "a");
    let (_, id) = key_of(&fx.ctx(), &tree).unwrap();
    let store = fx.store_arc();
    let repo = fx.repo.clone();
    let set_dir = fx.store().deps_dir(&fx.repo, &id);
    let staging_root = fx.repo_dir().join(".staging");
    let entered: std::sync::Arc<std::sync::Mutex<Option<SystemTime>>> = Default::default();
    {
        let (store, repo, id, set_dir, staging_root, entered) = (
            store.clone(),
            repo.clone(),
            id.clone(),
            set_dir.clone(),
            staging_root.clone(),
            entered.clone(),
        );
        fx.lock.on_enter(move || {
            *entered.lock().unwrap() = Some(SystemTime::now());
            let a = store.deps_attempts(Some(&repo)).unwrap().last().unwrap().id;
            let seal = staging_root.join(a.to_string()).join("seal");
            assert!(seal.is_dir());
            assert_eq!(mode_of(&seal), 0o755);
            assert!(!set_dir.exists());
            assert!(store.deps_set(&repo, &id).unwrap().is_none());
        });
    }
    {
        let (store, repo, id, set_dir) = (store.clone(), repo.clone(), id.clone(), set_dir.clone());
        fx.lock.on_exit(move || {
            assert!(set_dir.is_dir());
            assert_eq!(mode_of(&set_dir), 0o555);
            assert_eq!(mode_of(&set_dir.join("node_modules")), 0o755);
            assert!(store.deps_set(&repo, &id).unwrap().is_some());
            let atts = store.deps_attempts(Some(&repo)).unwrap();
            assert_eq!(atts.last().unwrap().state, AttemptState::Sealed);
        });
    }
    fx.lock.set_busy(3);
    let set = build(&fx.ctx(), &tree, None).unwrap();
    assert_eq!(set.id, id);
    assert_eq!(fx.lock.enters(), 1);
    assert_eq!(fx.lock.busies.load(Ordering::SeqCst), 3);
    let end: f64 = fx.sb.install_file("end", 1).trim().parse().unwrap();
    let ent = entered
        .lock()
        .unwrap()
        .unwrap()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert!(
        end < ent,
        "install ended {end} before the lock was entered {ent}"
    );
    *fx.lock.on_enter.lock().unwrap() = None;
    *fx.lock.on_exit.lock().unwrap() = None;

    // Busy past any bounded wait: the build still seals.
    let tree_b = fx.tree_tagged("b", "b");
    fx.lock.set_busy(150);
    let set_b = build(&fx.ctx(), &tree_b, None).unwrap();
    assert_eq!(fx.lock.enters(), 2);
    assert_eq!(
        fx.store()
            .deps_set(&fx.repo, &set_b.id)
            .unwrap()
            .unwrap()
            .set,
        set_b
    );
    assert_eq!(
        store_attempts(&fx).last().unwrap().state,
        AttemptState::Sealed
    );

    // gc's premise: a 0555 sealed root cannot move to another parent
    // until it is made removable.
    let other = fx.sb.root.join("gc-target");
    fs::create_dir_all(&other).unwrap();
    let e = fs::rename(&set_dir, other.join("x")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EACCES));
    let h = fs::File::open(&set_dir).unwrap();
    strata_apfs::make_removable(&h).unwrap();
    fs::rename(&set_dir, other.join("x")).unwrap();
}

#[test]
fn build_preflight_and_staging_refusals() {
    // No `bun` on PATH.
    let mut fx = Fx::new();
    let tree = fx.tree("a");
    fx.env.path = "/usr/bin:/bin".into();
    let e = build(&fx.ctx(), &tree, None).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    assert!(!fx.repo_dir().join(".locks").exists());
    assert!(store_attempts(&fx).is_empty());
    assert!(fx.staging_dirs().is_empty());
    fx.env = fx.sb.install_env();

    // The workspace's bun.lock changes after the attempt begins.
    test_seams::reset();
    let ws = fx.sb.root.join("ws");
    fx.sb.archive_into(&fx.main, "HEAD", &ws);
    let ws_lock = ws.join("bun.lock");
    test_seams::after_attempt_begins(Box::new(move || fs::write(&ws_lock, b"lock v2\n").unwrap()));
    let trash = fx.trash_count();
    let e = build(&fx.ctx(), &tree, Some(&ws)).unwrap_err();
    test_seams::reset();
    assert_eq!(e.code, Code::Drift, "{e:?}");
    assert_eq!(reason(&e), Some("inputs_changed"));
    let (expected, found) = (
        detail_str(&e, "expected").unwrap(),
        detail_str(&e, "found").unwrap(),
    );
    assert_ne!(expected, found);
    assert!(fx.staging_dirs().is_empty());
    assert_eq!(fx.trash_count(), trash + 1);
    let atts = store_attempts(&fx);
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].state, AttemptState::Refused);
    assert_eq!(fx.sb.install_count(), 0);

    // A tracked intermediate symlink in the base tree.
    let outside = fx.sb.root.join("outside-sub");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep"), b"keep").unwrap();
    let before = snapshot(&outside);
    let tree2 = fx.tree("b");
    std::os::unix::fs::symlink(&outside, tree2.join("sub")).unwrap();
    fx.cfg.install_inputs = rps(&["package.json", "bun.lock", "patches", "sub/package.json"]);
    let ws2 = fx.sb.root.join("ws2");
    fx.sb.archive_into(&fx.main, "HEAD", &ws2);
    fs::create_dir_all(ws2.join("sub")).unwrap();
    fs::write(ws2.join("sub/package.json"), b"{\"name\":\"sub\"}\n").unwrap();
    let e = build(&fx.ctx(), &tree2, Some(&ws2)).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(snapshot(&outside), before);
    assert!(fx.staging_dirs().is_empty());
    let atts = store_attempts(&fx);
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[1].state, AttemptState::Refused);
    assert_eq!(fx.sb.install_count(), 0);
}

/// `paths.root` on another volume (a disk image): `CrossVolume`, nothing
/// written. Needs `hdiutil`; run with `--ignored`.
#[test]
#[ignore]
fn build_preflight_cross_volume() {
    let sb = Sandbox::new();
    let main = sb.init_project("tool");
    let img = sb.root.join("vol.dmg");
    let mnt = sb.root.join("mnt");
    fs::create_dir_all(&mnt).unwrap();
    let st = std::process::Command::new("hdiutil")
        .args([
            "create",
            "-quiet",
            "-size",
            "20m",
            "-fs",
            "APFS",
            "-volname",
            "strata-deps-xvol",
        ])
        .arg(&img)
        .status()
        .unwrap();
    assert!(st.success());
    let st = std::process::Command::new("hdiutil")
        .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
        .arg(&mnt)
        .arg(&img)
        .status()
        .unwrap();
    assert!(st.success());
    struct Detach(PathBuf);
    impl Drop for Detach {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.0)
                .status();
        }
    }
    let _d = Detach(mnt.clone());
    let root = fs::canonicalize(&mnt).unwrap().join("strata");
    let store = Store::open(&root, &[]).unwrap();
    let git = sb.open(&main);
    let cfg = sb.config(&main);
    let env = sb.install_env();
    let paths = Paths { root: root.clone() };
    let repo = repo_name("tool");
    let lock = RecLock::default();
    let ctx = Ctx {
        store: &store,
        paths: &paths,
        repo: &repo,
        cfg: &cfg,
        git: &git,
        env: &env,
        locks: &lock,
    };
    let tree = sb.root.join("tree");
    sb.archive_into(&main, "HEAD", &tree);
    let e = build(&ctx, &tree, None).unwrap_err();
    assert_eq!(e.code, Code::CrossVolume, "{e:?}");
    assert!(!root.join("deps/tool").exists());
    assert!(store.deps_attempts(None).unwrap().is_empty());
    assert_eq!(sb.install_count(), 0);
    store.close().unwrap();
}

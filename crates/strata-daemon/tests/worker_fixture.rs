//! Slice 1 §9. Workers must refuse unsafe paths before taking any lock or binding.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use strata_daemon::fixture::{FakeLaunchd, LaunchdCall, WorkerFixture};
use strata_daemon::plist::Launchd;
use strata_daemon::startup;
use support::Dfx;

const TEST_BIN: &str = env!("CARGO_BIN_EXE_git-stratad-test");

fn fixture(fx: &Dfx) -> WorkerFixture {
    WorkerFixture::new(&fx.sb.root, &fx.sb.home, &fx.config_dir).unwrap()
}

fn prepare(fx: &Dfx, opts: startup::Options) -> strata_daemon::Result<startup::Core> {
    let (confirm, _, _) = fx.confirmer(vec![]);
    startup::prepare(opts, confirm, &|_| {
        panic!("escaped fixture reached Git::open")
    })
}

#[test]
fn in_process_refuses_paths_before_lock_or_bind() {
    let fx = Dfx::new();
    let other = tempfile::tempdir().unwrap();
    let outside = other.path().canonicalize().unwrap();
    assert!(WorkerFixture::new(&fx.sb.root, &outside, &fx.config_dir).is_err());
    assert!(WorkerFixture::new(&fx.sb.root, &fx.sb.home, &outside).is_err());
    // A caller cannot declare a common ancestor of the fixture and real paths.
    assert!(WorkerFixture::new(Path::new("/"), &fx.sb.home, &fx.config_dir).is_err());

    let lock = strata_daemon::lock_path(&fx.config_dir);
    let assert_refusal = |opts, field| {
        let err = prepare(&fx, opts).err().expect("unsafe worker started");
        let detail = err.detail.unwrap();
        assert_eq!(detail["reason"], "test_fixture", "{detail}");
        assert_eq!(detail["field"], field, "{detail}");
        assert!(!lock.exists(), "daemon lock already created");
        assert!(!fx.socket().exists(), "socket already bound");
    };
    let mut opts = fx.options();
    opts.fixture = None;
    assert_refusal(opts, "fixture");
    let mut opts = fx.options();
    opts.config_dir = outside.clone();
    assert_refusal(opts, "config_dir");
    let mut opts = fx.options();
    opts.config_file = outside.join("missing.toml");
    assert_refusal(opts, "config_file");
    let mut opts = fx.options();
    opts.socket = outside.join("daemon.sock");
    assert_refusal(opts, "socket");
    let mut opts = fx.options();
    opts.socket = fx.sb.path("../escape.sock");
    assert_refusal(opts, "socket");

    let dest = outside.join("absent");
    let config_file = fx.options().config_file;
    fs::remove_file(&config_file).unwrap();
    symlink(&dest, &config_file).unwrap();
    assert_refusal(fx.options(), "config_file");
    fs::remove_file(&config_file).unwrap();
    let alias = fx.sb.path("other-root");
    symlink(&outside, &alias).unwrap();
    let mut config = fx.config.clone();
    config.paths.root = alias.join("missing");
    fx.write_config_text(&toml::to_string(&config).unwrap());
    assert_refusal(fx.options(), "data_root");
    fx.write_config();

    symlink(&dest, &lock).unwrap();
    assert_refusal(fx.options(), "daemon_lock");
    fs::remove_file(&lock).unwrap();

    let store = fx.config.paths.store();
    fs::create_dir_all(&store).unwrap();
    symlink(&dest, store.join("LOCK")).unwrap();
    assert_refusal(fx.options(), "store_lock");
    fs::remove_file(store.join("LOCK")).unwrap();
    fs::remove_dir(&store).unwrap();
    symlink(&outside, &store).unwrap();
    assert_refusal(fx.options(), "store_lock");
    assert!(fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn cross_volume_data_root_is_rejected_before_taking_the_instance_lock() {
    let mut fx = Dfx::new();
    // The sealed system volume differs from the fixture's data volume.
    fx.config.paths.root = Path::new("/usr/bin").join("strata-test-data");
    fx.write_config();

    let error = startup::take_instance(&fx.options())
        .err()
        .expect("a cross-volume fixture acquired the instance lock");
    assert_eq!(error.code, strata_types::Code::CrossVolume);
    assert!(!strata_daemon::lock_path(&fx.config_dir).exists());
}

#[test]
fn in_process_default_data_root_uses_the_fixture_home() {
    let fx = Dfx::new();
    let mut config = toml::Value::try_from(&fx.config).unwrap();
    config.as_table_mut().unwrap().remove("paths");
    fx.write_config_text(&toml::to_string(&config).unwrap());
    let core = fx.prepare().unwrap();
    assert_eq!(core.config.paths.root, fx.sb.home.join(".strata"));
    assert!(core.config.paths.store().join("LOCK").is_file());
    core.close().unwrap();
}

#[test]
fn process_refuses_outside_data_before_instance_lock_and_early_listener() {
    let mut fx = Dfx::new();
    // Exercise containment on the same volume even when TMPDIR is on another.
    let other = tempfile::tempdir_in(fx.sb.root.parent().unwrap()).unwrap();
    fx.config.paths.root = other
        .path()
        .canonicalize()
        .unwrap()
        .join("missing/store-root");
    fx.write_config();
    let socket = fx.socket();
    fs::write(&socket, b"stale socket sentinel").unwrap();
    let mut child = OwnedWorker(
        fixture(&fx)
            .command(Path::new(TEST_BIN))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert!(!child.wait().success());
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    let reply: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(reply["error"]["detail"]["field"], "data_root", "{reply}");
    assert_eq!(
        reply["error"]["detail"]["reason"], "test_fixture",
        "{reply}"
    );
    assert!(
        !strata_daemon::lock_path(&fx.config_dir).exists(),
        "took daemon.lock before refusing"
    );
    assert_eq!(fs::read(&socket).unwrap(), b"stale socket sentinel");
    assert!(fs::read_dir(other.path()).unwrap().next().is_none());
}

#[test]
fn missing_process_fixture_is_refused() {
    let fx = Dfx::new();
    let out = Command::new(TEST_BIN)
        .env_clear()
        .env("HOME", &fx.sb.home)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!strata_daemon::lock_path(&fx.config_dir).exists());
    assert!(!strata_daemon::config_dir_for_home(&fx.sb.home).exists());
}

struct OwnedWorker(Child);
impl OwnedWorker {
    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "worker did not exit");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for OwnedWorker {
    fn drop(&mut self) {
        // Clean up our still-running child if an assertion unwinds.
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn fixture_process_serves_and_shuts_down_under_its_own_home() {
    let fx = Dfx::new();
    let mut child = OwnedWorker(
        fixture(&fx)
            .command(Path::new(TEST_BIN))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "worker exited during startup"
        );
        if let Ok(mut stream) = UnixStream::connect(fx.socket()) {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            writeln!(
                stream,
                r#"{{"id":1,"method":"daemon_status","params":{{}}}}"#
            )
            .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["ok"] == true {
                assert_eq!(reply["data"]["pid"], child.0.id());
                break;
            }
            assert_eq!(reply["error"]["detail"]["reason"], "starting", "{reply}");
        }
        assert!(Instant::now() < deadline, "worker did not serve");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(fx.config.paths.store().join("LOCK").is_file());
    // SAFETY: the child has not been reaped and no other code can reap it.
    assert_eq!(unsafe { libc::kill(child.0.id() as i32, libc::SIGTERM) }, 0);
    assert!(child.wait().success());
    assert!(!fx.socket().exists());
}

#[test]
fn fake_launchd_records_all_six_operations_and_rejects_live_targets() {
    let fx = Dfx::new();
    let fixture = fixture(&fx);
    let label = "test.strata.worker";
    let domain = "fixture/worker";
    assert!(FakeLaunchd::new(&fixture, "gui/501", label).is_err());
    assert!(FakeLaunchd::new(&fixture, domain, strata_daemon::AGENT_LABEL).is_err());
    let mut fake = FakeLaunchd::new(&fixture, domain, label).unwrap();
    fake.print_reply = "fixture job".into();
    let plist = fx.sb.path("test.strata.worker.plist");
    fake.bootstrap(&plist).unwrap();
    fake.bootout(&plist).unwrap();
    fake.enable().unwrap();
    fake.disable().unwrap();
    assert_eq!(fake.print().unwrap(), "fixture job");
    fake.kickstart().unwrap();
    assert!(
        fake.bootstrap(&fx.sb.path("com.gwd.git-stratad.plist"))
            .is_err()
    );
    assert!(
        fake.bootout(
            &tempfile::tempdir()
                .unwrap()
                .path()
                .join("test.strata.worker.plist")
        )
        .is_err()
    );
    assert_eq!(
        fake.calls(),
        [
            LaunchdCall::Bootstrap(plist.clone()),
            LaunchdCall::Bootout(plist),
            LaunchdCall::Enable,
            LaunchdCall::Disable,
            LaunchdCall::Print,
            LaunchdCall::Kickstart
        ]
    );
}

#[test]
fn release_guard_rejects_real_renamed_test_and_seams_daemons() {
    let dir = tempfile::tempdir().unwrap();
    let bins = dir.path();
    let guard = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/desktop/scripts/check-release-workers.sh");
    fs::write(bins.join("git-strata"), b"GIT-STRATA:PRODUCTION:V1").unwrap();
    for binary in [TEST_BIN, env!("CARGO_BIN_EXE_git-stratad")] {
        // A symlink lets the guard inspect the original bytes without copying a binary.
        let candidate = bins.join("git-stratad");
        symlink(binary, &candidate).unwrap();
        let out = Command::new("bash").arg(&guard).arg(bins).output().unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("test-seams binary"),
            "{out:?}"
        );
        fs::remove_file(&candidate).unwrap();
    }
}

#[test]
fn fixture_preflight_rejects_symlinked_startup_paths_before_locking() {
    let fx = Dfx::new();
    let sibling = tempfile::tempdir_in(fx.sb.root.parent().unwrap()).unwrap();
    let export = sibling.path().join(format!("{}-export", i32::MAX));
    fs::create_dir(&export).unwrap();
    let sentinel = export.join("sentinel");
    fs::write(&sentinel, b"preserve").unwrap();
    for tail in [
        "daemon/sync",
        "daemon/main-follow",
        "store/tmp",
        "store/store.db",
        "deps",
        "bases",
        "sessions",
        "search",
    ] {
        let path = fx.config.paths.root.join(tail);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(sibling.path(), &path).unwrap();
        let result = startup::take_instance(&fx.options());
        let err = result
            .err()
            .unwrap_or_else(|| panic!("preflight accepted {tail}"));
        assert_eq!(err.detail.unwrap()["reason"], "test_fixture", "{tail}");
        assert!(!strata_daemon::lock_path(&fx.config_dir).exists());
        assert!(!fx.socket().exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
        fs::remove_file(path).unwrap();
    }
}

/// CR r2 #4: containment below the checked parents. A linked blob shard, a
/// linked scene directory and a linked scene record are each refused before
/// the instance lock, and the sibling targets stay untouched.
#[test]
fn fixture_preflight_rejects_links_below_blob_and_push_dirs() {
    let fx = Dfx::new();
    let sibling = tempfile::tempdir_in(fx.sb.root.parent().unwrap()).unwrap();
    let sentinel = sibling.path().join("sentinel");
    fs::write(&sentinel, b"preserve").unwrap();
    let record = sibling.path().join("push.json");
    fs::write(&record, b"{}").unwrap();
    let root = &fx.config.paths.root;
    for (link, target) in [
        ("store/blobs/ab", sibling.path().to_path_buf()),
        ("daemon/push/1-1", sibling.path().to_path_buf()),
        ("daemon/push/1-2/push.json", record.clone()),
    ] {
        let path = root.join(link);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&target, &path).unwrap();
        let err = startup::take_instance(&fx.options())
            .err()
            .unwrap_or_else(|| panic!("preflight accepted {link}"));
        assert_eq!(err.detail.unwrap()["reason"], "test_fixture", "{link}");
        assert!(!strata_daemon::lock_path(&fx.config_dir).exists());
        assert!(!fx.socket().exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
        assert_eq!(fs::read(&record).unwrap(), b"{}");
        assert_eq!(fs::read_dir(sibling.path()).unwrap().count(), 2, "{link}");
        fs::remove_file(&path).unwrap();
    }
}

/// CR r2 #5: dependency children (install, `--version`) get the fixture's
/// HOME and file-only Git policy after the daemon's GIT_* sweep.
#[test]
fn dependency_children_get_the_fixture_environment() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let child = &core.env.child_env;
    let home = fx.sb.home.clone().into_os_string();
    assert!(child.contains(&("HOME".into(), home)), "{child:?}");
    assert!(
        child.contains(&("GIT_CONFIG_NOSYSTEM".into(), "1".into())),
        "{child:?}"
    );
    assert!(
        child.contains(&("GIT_ALLOW_PROTOCOL".into(), "file".into())),
        "{child:?}"
    );
    core.close().unwrap();
}

#[test]
fn unrelated_outside_root_is_refused_without_a_volume_diagnostic() {
    let mut fx = Dfx::new();
    fx.config.paths.root = Path::new("/usr/bin/unregistered-strata-root").into();
    fx.write_config();
    let err = startup::take_instance(&fx.options()).err().unwrap();
    assert_eq!(err.code, strata_types::Code::InvalidInput);
    assert_eq!(err.detail.unwrap()["field"], "data_root");
    assert!(!strata_daemon::lock_path(&fx.config_dir).exists());
}

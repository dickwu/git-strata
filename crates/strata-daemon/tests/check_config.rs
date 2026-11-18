//! Print-only flags and root-bound store state, isolated from the live install.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use strata_daemon::config_dir_for_home;
use strata_store::{Seam, Store, sql};
use strata_types::{Code, STORE_SCHEMA, serde_rfc3339};
use support::Dfx;

fn daemon_print(home: &Path, args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_git-stratad"))
        .env_clear()
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn check_config_validates_without_creating_a_store_or_starting() {
    let fx = Dfx::new();
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/config.toml");
    let out = daemon_print(&fx.sb.home, &[Path::new("--check-config"), &template]);
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["code"], "InvalidInput");
    assert_eq!(reply["error"]["detail"]["reason"], "missing");

    let out = daemon_print(
        &fx.sb.home,
        &[
            Path::new("--check-config"),
            &fx.config_dir.join("config.toml"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!({"ok": true, "error": null})
    );
    assert!(!fx.config.paths.root.exists());
    assert!(!config_dir_for_home(&fx.sb.home).exists());
    assert!(!fx.config_dir.join("pins.json").exists());
}

#[test]
fn check_config_rejects_a_missing_data_root_below_a_symlink() {
    let mut fx = Dfx::new();
    let alias = fx.sb.path("alias");
    std::os::unix::fs::symlink(&fx.sb.home, &alias).unwrap();
    fx.config.paths.root = alias.join("strata-data");
    fx.write_config();
    let out = daemon_print(
        &fx.sb.home,
        &[
            Path::new("--check-config"),
            &fx.config_dir.join("config.toml"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["error"]["detail"]["reason"], "not_realpath");
    assert!(!fx.config.paths.root.exists());
}

#[test]
fn check_config_rejects_a_missing_data_root_below_a_dangling_symlink() {
    let mut fx = Dfx::new();
    let alias = fx.sb.path("dangling");
    std::os::unix::fs::symlink(fx.sb.path("no-such-target"), &alias).unwrap();
    fx.config.paths.root = alias.join("missing/strata-data");
    fx.write_config();

    let out = daemon_print(
        &fx.sb.home,
        &[
            Path::new("--check-config"),
            &fx.config_dir.join("config.toml"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["error"]["detail"]["reason"], "not_realpath");
    assert_eq!(reply["error"]["detail"]["link"], json!(alias));
    assert!(!fx.sb.path("no-such-target").exists());
}

#[test]
fn check_config_rejects_a_data_root_below_a_file() {
    let mut fx = Dfx::new();
    let file = fx.sb.path("file");
    fs::write(&file, b"fixture").unwrap();
    fx.config.paths.root = file.join("strata-data");
    fx.write_config();

    let out = daemon_print(
        &fx.sb.home,
        &[
            Path::new("--check-config"),
            &fx.config_dir.join("config.toml"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["error"]["code"], "NotDirectory");
    assert_eq!(reply["error"]["detail"]["path"], json!(file));
}

#[test]
fn check_config_rejects_root_and_checkout_on_different_volumes() {
    let mut fx = Dfx::new();
    // The sealed system volume differs from the fixture's data volume.
    fx.config.paths.root = Path::new("/usr/bin").join("strata-test-data");
    fx.write_config();

    let out = daemon_print(
        &fx.sb.home,
        &[
            Path::new("--check-config"),
            &fx.config_dir.join("config.toml"),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["error"]["code"], "CrossVolume");
    assert!(!fx.config.paths.root.exists());
}

#[test]
fn check_config_argument_error_still_prints_json() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon_print(home.path(), &[Path::new("--check-config")]);
    assert_eq!(out.status.code(), Some(1));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["detail"]["reason"], "args");
}

#[test]
fn print_plist_uses_conditional_keep_alive_with_a_cleared_environment() {
    let home = tempfile::tempdir().unwrap();
    let bin = Path::new("/Applications/GIT STRATA.app/Contents/MacOS/git-stratad");
    let out = daemon_print(home.path(), &[Path::new("--print-plist"), bin]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(
        "<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>"
    ));
    assert!(text.contains(&format!("<string>{}</string>", bin.display())));
    assert!(!config_dir_for_home(home.path()).exists());
}

fn read_state(fx: &Dfx) -> Value {
    serde_json::from_slice(&fs::read(fx.config_dir.join("store_state.json")).unwrap()).unwrap()
}

fn assert_open_state(fx: &Dfx, migrated_from: Option<u32>) {
    let state = read_state(fx);
    assert_eq!(state["root"], json!(fx.config.paths.root));
    assert_eq!(state["user_version"], STORE_SCHEMA);
    assert_eq!(state["migrated_from"], json!(migrated_from));
    assert!(state.get("migrating_to").is_none());
    serde_rfc3339::parse(state["at"].as_str().unwrap()).unwrap();
}

#[test]
fn store_state_writes_a_new_file_without_truncating_an_existing_link() {
    let fx = Dfx::new();
    let linked = fx.sb.path("unrelated.txt");
    fs::write(&linked, b"untouched").unwrap();
    fs::hard_link(&linked, fx.config_dir.join("store_state.json")).unwrap();

    let core = fx.prepare().unwrap();
    assert_open_state(&fx, None);
    assert_eq!(fs::read(linked).unwrap(), b"untouched");
    core.close().unwrap();
}

#[test]
fn store_state_is_unchanged_after_cross_volume_rejection() {
    let mut fx = Dfx::new();
    fx.prepare().unwrap().close().unwrap();
    let path = fx.config_dir.join("store_state.json");
    let before = fs::read(&path).unwrap();
    fx.config.paths.root = Path::new("/usr/bin").join("strata-test-data");
    fx.write_config();

    let error = fx.prepare().err().expect("store volumes differ");
    assert_eq!(error.code, Code::CrossVolume);
    assert_eq!(fs::read(path).unwrap(), before);
}

fn make_v6_store(fx: &Dfx) {
    // As in strata-store/tests/migrate_v7.rs, remove the v7 addition to
    // build a v6 fixture that the real store migrator will accept.
    let db = fx.config.paths.root.join("store/store.db");
    let conn = sql::open(&db, sql::Mode::Shared).unwrap();
    conn.execute_batch("DROP TABLE session_marks; PRAGMA user_version = 6;")
        .unwrap();
    conn.close().unwrap();
}

#[test]
fn store_state_tracks_open_and_migration_without_reusing_old_backups() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    assert_open_state(&fx, None);
    core.close().unwrap();
    make_v6_store(&fx);

    Store::arm_next_open(Seam::MigrateFailsBeforeCommit);
    let e = fx
        .prepare()
        .err()
        .expect("migration seam must fail startup");
    assert_eq!(e.detail.unwrap()["seam"], "migrate_fails_before_commit");
    assert_eq!(
        read_state(&fx),
        json!({ "root": fx.config.paths.root, "migrating_to": STORE_SCHEMA })
    );

    let core = fx.prepare().unwrap();
    assert_open_state(&fx, Some(6));
    core.close().unwrap();

    let core = fx.prepare().unwrap();
    assert_open_state(&fx, None);
    core.close().unwrap();
}

#[test]
fn store_state_retains_the_marker_when_migration_commit_is_uncertain() {
    let fx = Dfx::new();
    fx.prepare().unwrap().close().unwrap();
    make_v6_store(&fx);

    Store::arm_next_open(Seam::MigrateCommitSyncFails);
    let e = fx.prepare().err().expect("migration COMMIT must fail");
    assert_eq!(e.code, Code::Unknown);
    let detail = e.detail.unwrap();
    assert_eq!(detail["reason"], "migration_commit");
    assert_eq!(detail["outcome"], "uncertain");
    assert_eq!(
        read_state(&fx),
        json!({ "root": fx.config.paths.root, "migrating_to": STORE_SCHEMA })
    );
}

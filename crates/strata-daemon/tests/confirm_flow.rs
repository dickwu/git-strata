//! daemon.md §8 Task 4: confirm order, pinned flags, PATH and the plist.

mod support;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use strata_confirm::seams::Verdict;
use strata_daemon::confirm_flow::{self, recheck, require_validated};
use strata_daemon::plist::{self, Launchd};
use strata_daemon::{DEFAULT_PATH, DaemonWarning, PinnedFlags};
use strata_types::{Action, Code};
use support::*;

fn pins_bytes(fx: &Dfx) -> Vec<u8> {
    fs::read(strata_daemon::pins_path(&fx.config_dir)).unwrap()
}

fn set_flags(fx: &mut Dfx, deploy: bool, confirm: bool, push: bool) {
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.deploy_on_push = deploy;
    rc.publish.confirm = confirm;
    rc.publish.push = push;
    fx.write_config();
}

fn pending(core: &strata_daemon::startup::Core) -> bool {
    core.warnings.snapshot().contains(&DaemonWarning::ConfigRelaxPending { repo: repo_name("tool") })
}

#[test]
fn config_relax_not_applied_on_restart() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, true, true, true);
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    // First start: the loaded config becomes the pins, not a relaxation.
    assert!(!pending(&core));
    assert_eq!(core.pinned(&repo_name("tool")).unwrap(), PinnedFlags { deploy_on_push: true, confirm: true, push: true, ..Default::default() });
    core.close().unwrap();
    let before = pins_bytes(&fx);

    // While stopped, deploy_on_push true → false (V5 keeps confirm as is).
    set_flags(&mut fx, false, true, true);
    for _ in 0..2 {
        let core = fx.prepare_with(fx.confirmer(vec![Verdict::pass()]).0).unwrap();
        let f = core.pinned(&repo_name("tool")).unwrap();
        assert!(f.deploy_on_push, "relaxation not applied");
        assert!(confirm_flow::needs_confirm(&f), "a push still needs confirming");
        assert!(core.config.repos[&repo_name("tool")].needs_human_confirm());
        assert!(pending(&core));
        core.close().unwrap();
        assert_eq!(pins_bytes(&fx), before);
    }
    assert!(log.shown().is_empty());
    assert_eq!(log.probes(), 0);
}

#[test]
fn config_relax_does_not_prompt_on_file_change() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, false, false, true);
    let (c, log, _) = fx.confirmer(vec![]);
    fx.prepare_with(c).unwrap().close().unwrap();

    // Tightening: written back at once, no credential.
    set_flags(&mut fx, false, true, true);
    let (c, log2, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    assert!(core.pinned(&repo_name("tool")).unwrap().confirm);
    assert!(!pending(&core));
    core.close().unwrap();
    let text = String::from_utf8(pins_bytes(&fx)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["repos"]["tool"]["confirm"], serde_json::json!(true));

    // Push true → false tightens too.
    set_flags(&mut fx, false, true, false);
    let (c, log3, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    assert!(!core.pinned(&repo_name("tool")).unwrap().push);
    core.close().unwrap();

    // Relaxing every field: only pending, never a prompt.
    set_flags(&mut fx, false, false, true);
    let (c, log4, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    assert!(pending(&core));
    assert_eq!(core.pinned(&repo_name("tool")).unwrap(), PinnedFlags { deploy_on_push: false, confirm: true, push: false, ..Default::default() });
    core.close().unwrap();
    for l in [log, log2, log3, log4] {
        assert!(l.shown().is_empty());
        assert_eq!(l.probes(), 0);
    }
}

#[test]
fn config_relax_writes_pin_after_validate() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, true, true, true);
    fx.prepare().unwrap().close().unwrap();
    set_flags(&mut fx, false, true, true);
    let pinned_before = pins_bytes(&fx);

    // require fails (the human cancelled): no write.
    let (c, log, _) = fx.confirmer(vec![Verdict::la(-2)]);
    let core = fx.prepare_with(c).unwrap();
    let e = core.config_relax(&repo_name("tool")).unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
    assert_eq!(log.shown().len(), 1);
    assert_eq!(pins_bytes(&fx), pinned_before);
    assert!(pending(&core));
    core.close().unwrap();

    // validate fails (the token expired between require and validate): no write.
    let (c, log, clock) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let e = core
        .config_relax_with(&repo_name("tool"), &|| clock.advance(strata_confirm::TOKEN_TTL + Duration::from_secs(1)))
        .unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
    assert_eq!(log.shown().len(), 1);
    assert_eq!(pins_bytes(&fx), pinned_before);
    assert!(pending(&core));
    core.close().unwrap();

    // Passed: require, then validate redeems, then pins.json is written.
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let seen_between = RefCell::new(None);
    let shown_between = RefCell::new(0);
    core.config_relax_with(&repo_name("tool"), &|| {
        *seen_between.borrow_mut() = Some(pins_bytes(&fx));
        *shown_between.borrow_mut() = log.shown().len();
    })
    .unwrap();
    assert_eq!(*shown_between.borrow(), 1, "require ran first");
    assert_eq!(seen_between.borrow().as_deref(), Some(pinned_before.as_slice()), "not written before validate");
    assert_ne!(pins_bytes(&fx), pinned_before);
    assert!(!core.pinned(&repo_name("tool")).unwrap().deploy_on_push);
    assert!(!pending(&core));
    assert!(!log.shown()[0].is_empty());
    // Nothing left to relax: InvalidInput not_relaxed, no prompt.
    let e = core.config_relax(&repo_name("tool")).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("not_relaxed"));
    assert_eq!(log.shown().len(), 1);
    // Unknown repo: NoSuchRepo, no prompt.
    assert_eq!(core.config_relax(&repo_name("nope")).unwrap_err().code, Code::NoSuchRepo);
    assert_eq!(log.shown().len(), 1);
    core.close().unwrap();

    // The relaxation survives a restart and is no longer pending.
    let core = fx.prepare().unwrap();
    assert!(!core.pinned(&repo_name("tool")).unwrap().deploy_on_push);
    assert!(!pending(&core));
    core.close().unwrap();

    // confirm itself writes no file into the config dir.
    let names: Vec<String> = fs::read_dir(&fx.config_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    let mut names = names;
    names.sort();
    assert_eq!(names, vec!["config.toml", "daemon.lock", "pins.json"]);
}

#[test]
fn prompt_holds_no_queue_or_repo_lock() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, true, true, true);
    fx.prepare().unwrap().close().unwrap();
    set_flags(&mut fx, false, true, true);
    let (c, log, _) = fx.confirmer(vec![Verdict::Hang]);
    let core = fx.prepare_with(c).unwrap();
    let d = strata_daemon::startup::Daemon::start(&core).unwrap();
    let sid = d.sessions.new(None, &[repo_name("tool")]).unwrap().manifest.id;
    std::thread::scope(|s| {
        let h = s.spawn(|| core.config_relax(&repo_name("tool")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while log.shown().is_empty() {
            assert!(Instant::now() < deadline, "prompt never shown");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The prompt is up: both locks are free (a held one would be Busy
        // after the 300 ms test bound).
        let t = Instant::now();
        core.locks.with_sessions(std::slice::from_ref(&sid), || core.locks.with_repo(&repo_name("tool"), || Ok(()))).unwrap();
        assert!(t.elapsed() < Duration::from_millis(250));
        assert!(core.locks.holds().is_empty());
        let e = h.join().unwrap().unwrap_err();
        assert_eq!(e.code, Code::NotConfirmed);
    });
    drop(d);
    core.close().unwrap();
}

#[test]
fn action_mismatch_is_not_confirmed() {
    let fx = Dfx::new();
    let (c, _, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass()]);
    let a = Action::PushDeploy { repo: repo_name("tool"), branch: "main".into(), url: "git@github.com:acme/tool.git".into() };
    let token = require_validated(&c, a.clone()).unwrap();
    recheck(&token, &a).unwrap();
    let other = Action::PushDeploy { repo: repo_name("tool"), branch: "release".into(), url: "git@github.com:acme/tool.git".into() };
    let e = recheck(&token, &other).unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed);
    // Redeemed once: validating the same token again is refused.
    assert_eq!(c.validate(&token, &a).unwrap_err().code, Code::NotConfirmed);
}

#[test]
fn daemon_never_returns_not_tty() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let src = fs::read_to_string(&p).unwrap();
        for bad in ["NotTty", "require_tty", "isatty"] {
            assert!(!src.contains(bad), "{bad} in {}", p.display());
        }
    }
}

#[test]
fn install_env_path_is_d1() {
    assert_eq!(DEFAULT_PATH, "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    // The production layout (under a temp HOME) installs with D1.
    let fx = Dfx::new();
    let home = fx.sb.path("prodhome");
    let mut opts = strata_daemon::startup::Options::for_home(&home);
    let d1 = format!("{}/.bun/bin:{DEFAULT_PATH}", home.display());
    assert_eq!(opts.install_path, d1);
    assert_eq!(opts.config_dir, home.join("Library/Application Support/git-strata"));
    fs::create_dir_all(&opts.config_dir).unwrap();
    fs::copy(fx.config_dir.join("config.toml"), &opts.config_file).unwrap();
    opts.socket = fx.socket();
    let env = fx.sb.git_env();
    let core = strata_daemon::startup::prepare(opts, fx.confirmer(vec![]).0, &move |p: &Path| strata_gitio::Git::open_with_env(p, &env)).unwrap();
    assert_eq!(core.env.path, d1);
    core.close().unwrap();
}

#[derive(Default)]
struct FakeLaunchd {
    calls: std::sync::Mutex<Vec<(String, PathBuf)>>,
}

impl Launchd for FakeLaunchd {
    fn bootstrap(&self, p: &Path) -> strata_daemon::Result<()> {
        self.calls.lock().unwrap().push(("bootstrap".into(), p.to_path_buf()));
        Ok(())
    }
    fn bootout(&self, p: &Path) -> strata_daemon::Result<()> {
        self.calls.lock().unwrap().push(("bootout".into(), p.to_path_buf()));
        Ok(())
    }
}

#[test]
fn plist_has_label_keepalive_aqua_and_d1_path() {
    let home = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(home.path()).unwrap();
    let bin = Path::new("/opt/strata/bin/git-stratad");
    let fake = FakeLaunchd::default();
    let p = plist::install_agent_with(bin, &home, &fake).unwrap();
    assert_eq!(p, home.join("Library/LaunchAgents/com.gwd.git-stratad.plist"));
    assert_eq!(*fake.calls.lock().unwrap(), vec![("bootstrap".to_string(), p.clone())]);

    // plutil reads it as a property list (lint + convert to JSON on stdout).
    let out = std::process::Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-"]).arg(&p).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["Label"], "com.gwd.git-stratad");
    assert_eq!(v["KeepAlive"], true);
    assert_eq!(v["RunAtLoad"], true);
    assert_eq!(v["LimitLoadToSessionType"], "Aqua");
    assert_eq!(v["ProcessType"], "Interactive");
    // Review P1-5: launchd waits for in-flight writes before SIGKILL.
    assert_eq!(v["ExitTimeOut"], plist::EXIT_TIMEOUT_SECS);
    // Incident 2026-10-27: launchd's default soft limit of 256 descriptors ran out.
    assert_eq!(v["SoftResourceLimits"]["NumberOfFiles"], strata_daemon::startup::FD_LIMIT);
    assert!(v.get("HardResourceLimits").is_none(), "the hard limit is left alone");
    assert_eq!(v["ProgramArguments"], serde_json::json!([bin]));
    assert_eq!(v["EnvironmentVariables"]["PATH"], format!("{}/.bun/bin:{DEFAULT_PATH}", home.display()));
    let err = v["StandardErrorPath"].as_str().unwrap();
    assert_eq!(Path::new(err), home.join("Library/Application Support/git-strata/daemon.err.log"));
    assert!(!err.contains('~'));

    plist::uninstall_agent_with(&home, &fake).unwrap();
    assert!(!p.exists());
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
    // A relative bin is refused before anything is written or loaded.
    let other = tempfile::tempdir().unwrap();
    let e = plist::install_agent_with(Path::new("git-stratad"), other.path(), &fake).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert!(!plist::agent_plist_path(other.path()).exists());
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
}

/// review P0-1: no pins.json is not "adopt the config". Every repo is
/// pinned gated (`confirm = true`) and a configured `confirm = false` waits
/// for `config relax` (Touch ID).
#[test]
fn missing_pins_file_means_needs_confirmation() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, false, false, true);
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_unseeded(c).unwrap();
    let f = core.pinned(&repo_name("tool")).unwrap();
    assert_eq!(f, PinnedFlags { deploy_on_push: false, confirm: true, push: true, ..Default::default() });
    assert!(pending(&core), "the looser config waits for config relax");
    assert!(log.shown().is_empty(), "startup never prompts");
    // Only a confirmed relax adopts it.
    core.config_relax(&repo_name("tool")).unwrap();
    assert!(!core.pinned(&repo_name("tool")).unwrap().confirm);
    assert_eq!(log.shown().len(), 1);
    core.close().unwrap();
}

/// review P0-1: deleting pins.json after a gated pin, loosening the config
/// and restarting keeps the gate.
#[test]
fn deleted_pins_file_does_not_loosen() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, true, true, true);
    fx.prepare().unwrap().close().unwrap();
    assert!(fx.prepare().unwrap().pinned(&repo_name("tool")).unwrap().confirm);
    fs::remove_file(strata_daemon::pins_path(&fx.config_dir)).unwrap();
    set_flags(&mut fx, false, false, true);
    let core = fx.prepare_unseeded(fx.confirmer(vec![]).0).unwrap();
    assert!(confirm_flow::needs_confirm(&core.pinned(&repo_name("tool")).unwrap()));
    assert!(core.config.repos[&repo_name("tool")].needs_human_confirm());
    assert!(pending(&core));
    core.close().unwrap();
}

/// review P0-1: a repo renamed in config.toml has no pin under its new name;
/// it starts gated, not as the (looser) config says.
#[test]
fn renamed_repo_does_not_reset_the_pin() {
    let mut fx = Dfx::new();
    set_flags(&mut fx, true, true, true);
    fx.prepare().unwrap().close().unwrap();
    let mut rc = fx.config.repos.remove(&repo_name("tool")).unwrap();
    let moved = rc.path.with_file_name("tool2");
    fs::rename(&rc.path, &moved).unwrap();
    rc.path = moved;
    rc.publish.confirm = false;
    rc.deploy_on_push = false;
    fx.config.repos.insert(repo_name("tool2"), rc);
    fx.write_config();
    let core = fx.prepare().unwrap();
    let f = core.pinned(&repo_name("tool2")).unwrap();
    assert!(f.confirm, "renamed repo is gated");
    assert!(core.config.repos[&repo_name("tool2")].needs_human_confirm());
    assert!(core.warnings.snapshot().contains(&DaemonWarning::ConfigRelaxPending { repo: repo_name("tool2") }));
    core.close().unwrap();
}

// ---- rm-followups §1.8 (B2): rm.regenerable is pinned like confirm ----

fn set_regenerable(fx: &mut Dfx, list: &[&str]) {
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.rm.regenerable = list.iter().map(|s| s.to_string()).collect();
    fx.write_config();
}

fn v(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn pinned_regen(core: &strata_daemon::startup::Core) -> Vec<String> {
    core.pinned(&repo_name("tool")).unwrap().regenerable
}

#[test]
fn regenerable_narrower_is_syntactic() {
    use confirm_flow::regenerable_narrower as n;
    let pin = v(&["gen/**", "!gen/keep.json", "a.json"]);
    assert!(n(&pin, &pin));
    assert!(n(&pin, &v(&[])), "emptied: matches nothing");
    assert!(n(&pin, &v(&["!other.json"])), "no positives left: matches nothing");
    assert!(n(&pin, &v(&["!gen/keep.json"])));
    assert!(n(&pin, &v(&["gen/**", "!gen/keep.json"])));
    assert!(n(&pin, &v(&["gen/**", "!gen/keep.json", "a.json", "!gen/x.json"])), "appended negation");
    assert!(!n(&pin, &v(&["gen/**", "a.json"])), "negation removed");
    assert!(!n(&pin, &v(&["gen/**", "!gen/kept.json", "a.json"])), "negation edited");
    assert!(!n(&pin, &v(&["gen/*", "!gen/keep.json", "a.json"])), "positive edited");
    assert!(!n(&pin, &v(&["gen/**", "!gen/keep.json", "a.json", "b.json"])), "positive added");
    assert!(!n(&pin, &v(&["a.json", "gen/**", "!gen/keep.json"])), "reordered");
    assert!(!n(&pin, &v(&["gen/**", "!gen/x.json", "!gen/keep.json", "a.json"])), "negation inserted mid-list");
    assert!(!n(&v(&[]), &v(&["a.json"])));
    assert!(n(&v(&[]), &v(&["!a.json"])));
}

#[test]
fn pins_json_without_regenerable_reads_empty_and_list_waits_for_relax() {
    let mut fx = Dfx::new();
    // Seeded pins.json in the old three-field format.
    fx.prepare().unwrap().close().unwrap();
    let old = pins_bytes(&fx);
    assert!(!String::from_utf8_lossy(&old).contains("regenerable"));
    set_regenerable(&mut fx, &["gen/**"]);
    let core = fx.prepare().unwrap();
    assert!(pinned_regen(&core).is_empty());
    assert!(core.config.repos[&repo_name("tool")].rm.regenerable.is_empty(), "effective list is the pin");
    assert!(pending(&core), "widening shows as pending relax");
    core.close().unwrap();
    assert_eq!(pins_bytes(&fx), old, "an empty list is not written");
}

#[test]
fn regenerable_unpinned_repo_is_empty_until_relax() {
    let mut fx = Dfx::new();
    set_regenerable(&mut fx, &["gen/**"]);
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_unseeded(c).unwrap();
    assert!(pinned_regen(&core).is_empty());
    assert!(pending(&core));
    core.config_relax(&repo_name("tool")).unwrap();
    assert_eq!(pinned_regen(&core), v(&["gen/**"]));
    assert_eq!(log.shown().len(), 1);
    core.close().unwrap();
    let text = String::from_utf8(pins_bytes(&fx)).unwrap();
    assert!(text.contains("\"regenerable\""), "{text}");
}

#[test]
fn regenerable_widening_and_edits_need_relax_narrowing_applies() {
    let mut fx = Dfx::new();
    set_regenerable(&mut fx, &["gen/**", "!gen/keep.json"]);
    let core = fx.prepare_unseeded(fx.confirmer(vec![Verdict::pass()]).0).unwrap();
    core.config_relax(&repo_name("tool")).unwrap();
    core.close().unwrap();

    // Widening (a positive added), an edit, and removing a negation: all pending.
    for (list, what) in [
        (&["gen/**", "!gen/keep.json", "more.json"][..], "added"),
        (&["gen/*.json", "!gen/keep.json"][..], "edited"),
        (&["gen/**"][..], "negation removed"),
    ] {
        set_regenerable(&mut fx, list);
        let core = fx.prepare().unwrap();
        assert_eq!(pinned_regen(&core), v(&["gen/**", "!gen/keep.json"]), "{what}");
        assert_eq!(core.config.repos[&repo_name("tool")].rm.regenerable, v(&["gen/**", "!gen/keep.json"]), "{what}");
        assert!(pending(&core), "{what}");
        core.close().unwrap();
    }

    // Appending a negation is narrower: applied at start, no prompt.
    set_regenerable(&mut fx, &["gen/**", "!gen/keep.json", "!gen/also.json"]);
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    assert_eq!(pinned_regen(&core), v(&["gen/**", "!gen/keep.json", "!gen/also.json"]));
    assert!(!pending(&core));
    assert!(log.shown().is_empty());
    core.close().unwrap();

    // Deleting every positive is narrower too.
    set_regenerable(&mut fx, &["!gen/keep.json", "!gen/also.json"]);
    let core = fx.prepare().unwrap();
    assert_eq!(pinned_regen(&core), v(&["!gen/keep.json", "!gen/also.json"]));
    assert!(!pending(&core));
    core.close().unwrap();
}

#[test]
fn relax_reloads_regenerable_without_restart() {
    let mut fx = Dfx::new();
    fx.prepare().unwrap().close().unwrap();
    set_regenerable(&mut fx, &["gen/**"]);
    let (c, _log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = strata_daemon::startup::Daemon::start(&core).unwrap();
    assert!(d.sessions.regenerable(&repo_name("tool")).is_empty());
    d.config_relax(&repo_name("tool")).unwrap();
    assert_eq!(d.sessions.regenerable(&repo_name("tool")), v(&["gen/**"]));
    assert!(!pending(&core));
    drop(d);
    core.close().unwrap();
}

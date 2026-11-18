//! Lifecycle tests on a fake service manager and clock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use super::launchctl::{
    BootoutTarget, LAUNCHCTL, Outcome, Verb, argv, dispatch_fence, printed_exit_timeout,
    printed_pid, read_stdout,
};
use super::*;
use crate::update::UpdateFuture;

#[derive(Default)]
struct Fake {
    loaded: bool,
    disabled: bool,
    /// Override the successful print output for fixture-specific cases.
    loaded_dump: Option<String>,
    fail: Option<&'static str>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl Fake {
    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, verb: Verb, label: &str, plist: Option<&Path>) -> Result<(), GuiError> {
        self.calls.lock().unwrap().push(argv(verb, label, plist));
        let step = verb.as_str();
        match self.fail {
            Some(s) if s == step => Err(GuiError::launchctl_failed(
                step,
                Some(5),
                "Bootstrap failed: 5: Input/output error\n",
            )),
            _ => Ok(()),
        }
    }
}

impl ServiceManager for Fake {
    fn print(&self, label: &str) -> Result<Option<String>, GuiError> {
        self.record(Verb::Print, label, None)?;
        Ok(self
            .loaded_dump
            .clone()
            .or_else(|| self.loaded.then(|| job_dump(None, Some(42)))))
    }
    fn kickstart(&self, label: &str) -> Result<(), GuiError> {
        self.record(Verb::Kickstart, label, None)
    }
    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        self.record(Verb::Bootstrap, AGENT_LABEL, Some(plist))
    }
    fn bootout(&self, label: &str) -> Result<(), GuiError> {
        self.record(Verb::Bootout, label, None)
    }
    fn disable(&self, label: &str) -> Result<(), GuiError> {
        self.record(Verb::Disable, label, None)
    }
    fn enable(&self, label: &str) -> Result<(), GuiError> {
        self.record(Verb::Enable, label, None)
    }
    fn print_disabled(&self, label: &str) -> Result<bool, GuiError> {
        self.record(Verb::PrintDisabled, label, None)?;
        Ok(self.disabled)
    }
}

const FAST: WaitPolicy = WaitPolicy {
    interval: Duration::from_millis(5),
    timeout: Duration::from_millis(120),
};

fn home(with_plist: bool) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    if with_plist {
        let p = plist_for_home(d.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "<plist/>").unwrap();
    }
    d
}

/// A `launchctl print` service dump shaped like macOS 15's.
fn job_dump(path: Option<&str>, pid: Option<u32>) -> String {
    let uid = unsafe { libc::getuid() };
    let path = path.map(|p| format!("\tpath = {p}\n")).unwrap_or_default();
    let process = match pid {
        Some(pid) => format!("\tpid = {pid}\n\tstate = running\n"),
        None => "\tstate = not running\n".to_owned(),
    };
    format!(
        "gui/{uid}/{AGENT_LABEL} = {{\n\tactive count = 1\n{path}\tstdout path = /tmp/o.log\n\tenvironment = {{\n\t\tpath = /nested\n\t}}\n\texit timeout = 900\n{process}}}\n"
    )
}

/// HOME and the login home are the same directory.
fn starter(home: &Path, fake: Arc<Fake>) -> Starter {
    Starter::new(
        Some(home.to_path_buf()),
        Some(home.to_path_buf()),
        fake,
        FAST,
    )
}

/// The plist path the app uses: under the canonical home (D38).
fn canon_plist(home: &Path) -> PathBuf {
    plist_for_home(&std::fs::canonicalize(home).unwrap())
}

fn down() -> Result<Value, GuiError> {
    Err(GuiError::from(strata_client::daemon_down(Path::new(
        "/x/daemon.sock",
    ))))
}

fn waiting(errno: i64) -> Result<Value, GuiError> {
    Err(GuiError::new(
        strata_types::Code::Busy,
        "git-stratad is waiting for its store: open failed",
        json!({ "reason": "store_unavailable", "state": "store_unavailable",
                "last_error": { "code": "Io", "message": "Operation not permitted", "detail": { "errno": errno } } }),
    ))
}

/// A probe that plays `script` then repeats its last entry.
fn scripted(
    script: Vec<Result<Value, GuiError>>,
) -> impl FnMut() -> std::future::Ready<Result<Value, GuiError>> {
    let mut i = 0;
    move || {
        let r = script[i.min(script.len() - 1)].clone();
        i += 1;
        std::future::ready(r)
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn collect() -> (Arc<Mutex<Vec<Progress>>>, impl FnMut(Progress) + Send) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    (seen, move |p| s.lock().unwrap().push(p))
}

#[test]
fn loaded_agent_is_kickstarted_then_waited_for() {
    let h = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let (seen, mut on) = collect();
    let r = rt()
        .block_on(
            starter(h.path(), fake.clone())
                .start(scripted(vec![down(), down(), Ok(json!({}))]), &mut on),
        )
        .unwrap();
    assert_eq!(r.action, Action::Kickstart);
    assert_eq!(
        fake.calls(),
        [
            argv(Verb::Print, AGENT_LABEL, None),
            argv(Verb::Kickstart, AGENT_LABEL, None),
        ]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Progress::Launching {
                action: Action::Kickstart
            },
            Progress::Waiting,
            Progress::Ready
        ]
    );
}

#[test]
fn unloaded_agent_with_plist_is_bootstrapped_from_that_plist() {
    let h = home(true);
    let fake = Arc::new(Fake::default());
    let (_, mut on) = collect();
    let r = rt()
        .block_on(starter(h.path(), fake.clone()).start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap();
    assert_eq!(r.action, Action::Bootstrap);
    assert_eq!(
        fake.calls(),
        [
            argv(Verb::Print, AGENT_LABEL, None),
            argv(Verb::Bootstrap, AGENT_LABEL, Some(&canon_plist(h.path()))),
        ]
    );
}

#[test]
fn no_plist_is_setup_needed_and_runs_nothing_after_print() {
    let h = home(false);
    let fake = Arc::new(Fake::default());
    let (seen, mut on) = collect();
    let e = rt()
        .block_on(starter(h.path(), fake.clone()).start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("plist_missing"));
    assert_eq!(
        e.detail["plist"],
        canon_plist(h.path()).to_string_lossy().as_ref()
    );
    assert_eq!(fake.calls(), [argv(Verb::Print, AGENT_LABEL, None)]);
    assert!(seen.lock().unwrap().is_empty());
    assert!(!plist_for_home(h.path()).exists(), "S0 writes no plist");
    let a = rt().block_on(starter(h.path(), fake).agent()).unwrap();
    assert_eq!(a.state, AgentState::NotInstalled);
    assert_eq!(a.command, "git-strata daemon start");
}

#[test]
fn agent_state_reports_each_variant() {
    let with = home(true);
    let without = home(false);
    let loaded = Fake {
        loaded: true,
        ..Fake::default()
    };
    assert_eq!(
        agent_state(&loaded, &plist_for_home(without.path())).unwrap(),
        AgentState::Loaded
    );
    let idle = Fake::default();
    assert_eq!(
        agent_state(&idle, &plist_for_home(with.path())).unwrap(),
        AgentState::Installed
    );
    assert_eq!(
        agent_state(&idle, &plist_for_home(without.path())).unwrap(),
        AgentState::NotInstalled
    );
    assert_eq!(
        serde_json::to_value(AgentState::NotInstalled).unwrap(),
        json!("not_installed")
    );
}

#[test]
fn launchctl_failure_carries_its_stderr() {
    let h = home(true);
    let fake = Arc::new(Fake {
        fail: Some("bootstrap"),
        ..Fake::default()
    });
    let (_, mut on) = collect();
    let e = rt()
        .block_on(starter(h.path(), fake).start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("launchctl_failed"));
    assert_eq!(e.detail["step"], "bootstrap");
    assert_eq!(e.detail["status"], 5);
    assert_eq!(
        e.detail["stderr"],
        "Bootstrap failed: 5: Input/output error"
    );
}

#[test]
fn tcc_wait_is_reported_as_the_prompt() {
    let h = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let (seen, mut on) = collect();
    let e = rt()
        .block_on(starter(h.path(), fake).start(scripted(vec![down(), waiting(1)]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("tcc_pending"));
    assert!(
        seen.lock()
            .unwrap()
            .contains(&Progress::WaitingStore { tcc: true })
    );
}

#[test]
fn a_store_error_that_is_not_tcc_is_its_own_reason() {
    let h = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let (_, mut on) = collect();
    let e = rt()
        .block_on(starter(h.path(), fake).start(scripted(vec![waiting(2)]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("store_unavailable"));
    assert_eq!(e.detail["store_error"], "Operation not permitted");
}

#[test]
fn a_daemon_that_never_answers_times_out() {
    let h = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let (seen, mut on) = collect();
    let e = rt()
        .block_on(starter(h.path(), fake).start(scripted(vec![down()]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("start_timeout"));
    assert!(!seen.lock().unwrap().contains(&Progress::Ready));
}

#[test]
fn tcc_then_ready_succeeds() {
    let h = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let (seen, mut on) = collect();
    rt().block_on(starter(h.path(), fake).start(
        scripted(vec![waiting(4), waiting(4), Ok(json!({}))]),
        &mut on,
    ))
    .unwrap();
    let s = seen.lock().unwrap();
    assert_eq!(
        s.iter()
            .filter(|p| **p == Progress::WaitingStore { tcc: true })
            .count(),
        1,
        "sent once"
    );
    assert_eq!(s.last(), Some(&Progress::Ready));
}

#[test]
fn one_start_at_a_time() {
    let h = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let s = Arc::new(Starter::new(
        Some(h.path().to_path_buf()),
        Some(h.path().to_path_buf()),
        fake,
        WaitPolicy {
            interval: Duration::from_millis(20),
            timeout: Duration::from_millis(300),
        },
    ));
    let rt = rt();
    let first = {
        let s = s.clone();
        rt.spawn(async move {
            let mut on = |_p: Progress| {};
            s.start(scripted(vec![down()]), &mut on).await
        })
    };
    std::thread::sleep(Duration::from_millis(60));
    let mut on = |_p: Progress| {};
    let e = rt
        .block_on(s.start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("start_in_flight"));
    assert_eq!(
        rt.block_on(first).unwrap().unwrap_err().reason(),
        Some("start_timeout")
    );
    // The slot is free again.
    let mut on = |_p: Progress| {};
    rt.block_on(s.start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap();
}

#[test]
fn no_home_is_no_home() {
    let s = Starter::new(None, None, Arc::new(Fake::default()), FAST);
    let mut on = |_p: Progress| {};
    assert_eq!(
        rt().block_on(s.start(scripted(vec![Ok(json!({}))]), &mut on))
            .unwrap_err()
            .reason(),
        Some("no_home")
    );
}

// ---- D38 (cli.md): login home only, own agent only ----

#[test]
fn a_foreign_home_is_refused_before_any_launchctl() {
    let h = home(true);
    let login = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        ..Fake::default()
    });
    let s = Starter::new(
        Some(h.path().to_path_buf()),
        Some(login.path().to_path_buf()),
        fake.clone(),
        FAST,
    );
    let (seen, mut on) = collect();
    let e = rt()
        .block_on(s.start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(e.code, "InvalidInput");
    assert_eq!(e.reason(), Some("foreign_home"));
    assert_eq!(e.detail["home"], h.path().to_string_lossy().as_ref());
    assert_eq!(
        e.detail["login_home"],
        login.path().to_string_lossy().as_ref()
    );
    assert!(seen.lock().unwrap().is_empty());
    let a = rt().block_on(s.agent()).unwrap();
    assert_eq!(a.state, AgentState::Foreign);
    assert_eq!(a.refusal.unwrap().reason(), Some("foreign_home"));
    assert!(
        fake.calls().is_empty(),
        "no launchctl at all: {:?}",
        fake.calls()
    );
}

#[test]
fn an_unresolvable_login_home_is_refused() {
    let h = home(true);
    let fake = Arc::new(Fake::default());
    let s = Starter::new(Some(h.path().to_path_buf()), None, fake.clone(), FAST);
    let mut on = |_p: Progress| {};
    let e = rt()
        .block_on(s.start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(e.reason(), Some("foreign_home"));
    assert!(e.detail["login_home"].is_null());
    let missing = h.path().join("gone");
    let e = scope::lifecycle_scope(&missing, Some(&missing)).unwrap_err();
    assert_eq!(
        e.reason(),
        Some("foreign_home"),
        "a HOME that does not exist"
    );
    assert!(fake.calls().is_empty());
}

#[test]
fn a_symlinked_alias_of_the_login_home_is_the_login_home() {
    let login = home(true);
    let links = tempfile::tempdir().unwrap();
    let alias = links.path().join("alias");
    std::os::unix::fs::symlink(login.path(), &alias).unwrap();
    let fake = Arc::new(Fake::default());
    let s = Starter::new(
        Some(alias),
        Some(login.path().to_path_buf()),
        fake.clone(),
        FAST,
    );
    let mut on = |_p: Progress| {};
    let r = rt()
        .block_on(s.start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap();
    assert_eq!(r.action, Action::Bootstrap);
    assert_eq!(
        fake.calls()[1],
        argv(
            Verb::Bootstrap,
            AGENT_LABEL,
            Some(&canon_plist(login.path()))
        ),
        "bootstraps the canonical plist"
    );
}

#[test]
fn an_agent_loaded_from_another_plist_is_foreign_and_not_kickstarted() {
    let h = home(true);
    let other = home(true);
    let loaded = canon_plist(other.path());
    let expected = canon_plist(h.path());
    let dump = job_dump(loaded.to_str(), Some(42));
    for path_key in ["\tpath = ", "\tpath  =  ", "\tpath\t=\t"] {
        let fake = Arc::new(Fake {
            loaded_dump: Some(dump.replacen("\tpath = ", path_key, 1)),
            ..Fake::default()
        });
        let (seen, mut on) = collect();
        let e = rt()
            .block_on(starter(h.path(), fake.clone()).start(scripted(vec![Ok(json!({}))]), &mut on))
            .unwrap_err();
        assert_eq!(e.code, "InvalidInput");
        assert_eq!(e.reason(), Some("foreign_agent"));
        assert_eq!(e.detail["loaded_path"], loaded.to_str().unwrap());
        assert_eq!(e.detail["expected"], expected.to_str().unwrap());
        assert_eq!(fake.calls(), [argv(Verb::Print, AGENT_LABEL, None)]);
        assert!(seen.lock().unwrap().is_empty());
        let a = rt().block_on(starter(h.path(), fake).agent()).unwrap();
        assert_eq!(a.state, AgentState::Foreign);
        assert_eq!(a.refusal.unwrap().reason(), Some("foreign_agent"));
    }
}

#[test]
fn our_own_plist_under_an_alias_or_without_a_path_line_is_ours() {
    let h = home(true);
    let links = tempfile::tempdir().unwrap();
    let alias = links.path().join("alias");
    std::os::unix::fs::symlink(h.path(), &alias).unwrap();
    for from in [
        Some(plist_for_home(&alias).to_string_lossy().into_owned()),
        None,
    ] {
        let fake = Arc::new(Fake {
            loaded_dump: Some(job_dump(from.as_deref(), Some(42))),
            ..Fake::default()
        });
        let mut on = |_p: Progress| {};
        let r = rt()
            .block_on(starter(h.path(), fake).start(scripted(vec![Ok(json!({}))]), &mut on))
            .unwrap();
        assert_eq!(r.action, Action::Kickstart);
    }
}

#[test]
fn loaded_path_reads_only_the_top_level_path() {
    assert_eq!(
        scope::loaded_path(&job_dump(
            Some("/Users/a/Library/LaunchAgents/x.plist"),
            None
        ))
        .as_deref(),
        Some("/Users/a/Library/LaunchAgents/x.plist")
    );
    assert_eq!(
        scope::loaded_path(&job_dump(None, None)),
        None,
        "nested and stdout paths do not count"
    );
    assert_eq!(scope::loaded_path(""), None);
    // A nested dict before the top-level line, and a depth-0 `path =`.
    let t =
        "path = /depth0\nsvc = {\n\tsub = {\n\t\tpath = /nested\n\t}\n\tpath =\n\tpath = /top\n}\n";
    assert_eq!(scope::loaded_path(t).as_deref(), Some("/top"));
    assert_eq!(scope::loaded_path("svc = {\n\tpath => /alien\n}\n"), None);
}

#[test]
fn anchored_keeps_a_deleted_tail_under_a_canonical_ancestor() {
    let d = tempfile::tempdir().unwrap();
    let canon = std::fs::canonicalize(d.path()).unwrap();
    assert_eq!(
        scope::anchored(&d.path().join("gone/x.plist")),
        canon.join("gone/x.plist")
    );
}

#[test]
fn the_login_home_resolves_from_the_password_database() {
    let l = scope::login_home().expect("the test user has a passwd entry");
    assert!(l.is_absolute());
}

#[test]
fn only_the_main_window() {
    assert!(main_only("main").is_ok());
    assert_eq!(
        main_only("tray").unwrap_err().reason(),
        Some("not_main_window")
    );
}

#[test]
fn argv_is_fixed_per_verb() {
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let job = format!("{domain}/{AGENT_LABEL}");
    let plist = "/Users/Test Home/Library/LaunchAgents/com.gwd.git-stratad.plist";
    let expected: [&[&str]; 7] = [
        &["print", &job],
        &["print-disabled", &domain],
        &["kickstart", &job],
        &["bootstrap", &domain, plist],
        &["bootout", &job],
        &["disable", &job],
        &["enable", &job],
    ];
    for (verb, expected) in Verb::ALL.into_iter().zip(expected) {
        assert_eq!(argv(verb, AGENT_LABEL, Some(Path::new(plist))), expected);
    }
}

fn fence_call(
    verb: Verb,
    target: Option<BootoutTarget>,
    label: &str,
    home: &Path,
    login: &Path,
    probe: Outcome,
) -> (Result<(), GuiError>, Vec<Vec<String>>) {
    let mut probe = Some(probe);
    let mut calls = Vec::new();
    let result = dispatch_fence(
        verb,
        label,
        target,
        || scope::lifecycle_scope(home, Some(login)),
        |step| {
            calls.push(argv(step, label, None));
            Ok(if step == Verb::Print {
                probe.take().expect("only one preflight print")
            } else {
                outcome(Some(0), "", "")
            })
        },
    );
    (result, calls)
}

#[test]
fn bootout_dispatch_requires_the_verified_pid_or_stopped_job() {
    let h = home(true);
    let plist = plist_for_home(h.path());
    let job = |pid| job_dump(plist.to_str(), pid);
    let dispatch = |target, dump: &str| {
        fence_call(
            Verb::Bootout,
            Some(target),
            AGENT_LABEL,
            h.path(),
            h.path(),
            outcome(Some(0), "", dump),
        )
    };
    let stopped = job(None);
    let scheduled = stopped.replace("\tstate = not running", "\tstate = spawn scheduled");
    for (target, dump) in [
        (BootoutTarget::Sealed(42), job(Some(42))),
        (BootoutTarget::Stopped, stopped),
        (BootoutTarget::Stopped, scheduled),
    ] {
        let (result, calls) = dispatch(target, &dump);
        result.unwrap();
        assert_eq!(
            calls,
            [
                argv(Verb::Print, AGENT_LABEL, None),
                argv(Verb::Bootout, AGENT_LABEL, None)
            ]
        );
    }
    for (target, pid, expected) in [
        (BootoutTarget::Sealed(42), Some(43), Some(42)),
        (BootoutTarget::Sealed(42), None, Some(42)),
        (BootoutTarget::Stopped, Some(43), None),
    ] {
        let (result, calls) = dispatch(target, &job(pid));
        let error = result.unwrap_err();
        assert_eq!(error.reason(), Some("bootout_target_changed"));
        assert_eq!(error.detail["expected_pid"], json!(expected));
        assert_eq!(error.detail["observed_pid"], json!(pid));
        assert_eq!(calls, [argv(Verb::Print, AGENT_LABEL, None)]);
    }
}

#[test]
fn fence_dispatch_handles_explicit_unload_without_bootout() {
    let h = home(false);
    for verb in [Verb::Bootout, Verb::Disable, Verb::Enable] {
        let (result, calls) = fence_call(
            verb,
            Some(BootoutTarget::Stopped),
            AGENT_LABEL,
            h.path(),
            h.path(),
            outcome(Some(113), &missing_job(), ""),
        );
        result.unwrap();
        let mut expected = vec![argv(Verb::Print, AGENT_LABEL, None)];
        if verb != Verb::Bootout {
            expected.push(argv(verb, AGENT_LABEL, None));
        }
        assert_eq!(calls, expected);
    }
}

#[test]
fn fence_dispatch_refuses_foreign_home_label_and_plist() {
    let h = home(true);
    let foreign_home = home(true);
    for verb in [Verb::Bootout, Verb::Disable, Verb::Enable] {
        let stop = Some(BootoutTarget::Stopped);
        let (result, calls) = fence_call(
            verb,
            stop,
            AGENT_LABEL,
            foreign_home.path(),
            h.path(),
            outcome(Some(113), &missing_job(), ""),
        );
        assert_eq!(result.unwrap_err().reason(), Some("foreign_home"));
        assert!(calls.is_empty());

        let (result, calls) = fence_call(
            verb,
            stop,
            "foreign.agent",
            h.path(),
            h.path(),
            outcome(Some(113), &missing_job(), ""),
        );
        assert_eq!(result.unwrap_err().reason(), Some("launchctl_failed"));
        assert!(calls.is_empty());

        let foreign_plist = job_dump(
            Some("/Library/LaunchAgents/com.gwd.git-stratad.plist"),
            None,
        );
        let (result, calls) = fence_call(
            verb,
            stop,
            AGENT_LABEL,
            h.path(),
            h.path(),
            outcome(Some(0), "", &foreign_plist),
        );
        assert_eq!(result.unwrap_err().reason(), Some("foreign_agent"));
        assert_eq!(calls, [argv(Verb::Print, AGENT_LABEL, None)]);
    }
}

#[test]
fn bootout_dispatch_refuses_unverified_or_ambiguous_targets() {
    let h = home(true);
    let (result, calls) = fence_call(
        Verb::Bootout,
        None,
        AGENT_LABEL,
        h.path(),
        h.path(),
        outcome(Some(113), &missing_job(), ""),
    );
    assert_eq!(
        result.unwrap_err().reason(),
        Some("bootout_target_required")
    );
    assert!(calls.is_empty());

    let plist = canon_plist(h.path());
    let stopped_job = job_dump(plist.to_str(), None);
    let unknown = stopped_job.replace("\tstate = not running", "\tstate = unknown");
    for probe in [
        outcome(Some(0), "", &job_dump(None, None)),
        outcome(Some(113), "Could not find domain for user gui", ""),
        outcome(Some(0), "", "truncated dump"),
        outcome(Some(0), "", &unknown),
    ] {
        let (result, calls) = fence_call(
            Verb::Bootout,
            Some(BootoutTarget::Stopped),
            AGENT_LABEL,
            h.path(),
            h.path(),
            probe,
        );
        let error = result.unwrap_err();
        assert_eq!(error.reason(), Some("launchctl_failed"));
        assert_eq!(error.detail["step"], "print");
        assert_eq!(calls, [argv(Verb::Print, AGENT_LABEL, None)]);
    }
}

#[test]
fn bootout_dispatch_observes_a_pid_with_extra_whitespace() {
    let h = home(true);
    let plist = canon_plist(h.path());
    let job = job_dump(plist.to_str(), None).replace(
        "\tstate = not running",
        "\tstate = spawn scheduled\n\tpid  =  42",
    );
    let (result, calls) = fence_call(
        Verb::Bootout,
        Some(BootoutTarget::Stopped),
        AGENT_LABEL,
        h.path(),
        h.path(),
        outcome(Some(0), "", &job),
    );
    let error = result.unwrap_err();
    assert_eq!(error.reason(), Some("bootout_target_changed"));
    assert_eq!(error.detail["observed_pid"], 42);
    assert_eq!(calls, [argv(Verb::Print, AGENT_LABEL, None)]);
}

fn outcome(status: Option<i32>, stderr: &str, out: &str) -> Outcome {
    Outcome {
        status,
        success: status == Some(0),
        stderr: stderr.into(),
        out: out.into(),
    }
}

fn missing_job() -> String {
    let uid = unsafe { libc::getuid() };
    format!("Could not find service \"{AGENT_LABEL}\" in domain for user gui: {uid}\n")
}

#[test]
fn print_requires_the_exact_label_and_domain_to_confirm_unload() {
    let uid = unsafe { libc::getuid() };
    let missing = missing_job();
    assert_eq!(
        outcome(Some(113), &format!("Bad request.\n{missing}"), "").printed_job(AGENT_LABEL),
        Ok(None)
    );
    assert_eq!(
        outcome(Some(113), "Bad request.\n", &missing).printed_job(AGENT_LABEL),
        Ok(None)
    );
    let job = job_dump(None, None);
    assert_eq!(
        outcome(Some(0), "", &job).printed_job(AGENT_LABEL),
        Ok(Some(job))
    );

    for (status, text) in [
        (Some(5), missing.clone()),
        (Some(113), String::new()),
        (
            Some(113),
            format!("Could not find domain for user gui: {uid}"),
        ),
        (Some(113), missing.replace(AGENT_LABEL, "foreign.agent")),
        (Some(113), missing.replace("user gui:", "uid:")),
        (Some(113), format!("{}0\n", missing.trim_end())),
        (
            Some(113),
            missing.replace(&format!("gui: {uid}"), &format!("gui: {}", uid + 1)),
        ),
    ] {
        let error = outcome(status, &text, "")
            .printed_job(AGENT_LABEL)
            .unwrap_err();
        assert_eq!(error.reason(), Some("launchctl_failed"));
        assert_eq!(error.detail["step"], "print");
        assert_eq!(error.detail["status"], json!(status));
    }
}

#[test]
fn print_does_not_count_a_truncated_diagnostic_as_missing() {
    let missing = missing_job();
    let line = missing.trim_end();
    let padding = "x".repeat(8 * 1024 - line.len() - 1);
    let stderr = format!("{padding}\n{line}0\n");
    // Non-probe stdout and stderr use the same bounded pipe reader.
    let capture =
        String::from_utf8(read_stdout(Verb::Bootout, stderr.as_bytes()).unwrap()).unwrap();
    assert!(
        outcome(Some(113), &capture, "")
            .printed_job(AGENT_LABEL)
            .is_err()
    );
}

#[test]
fn print_disabled_reads_only_the_exact_label_in_a_complete_table() {
    let table = |entries: &str| format!("disabled services = {{\n{entries}\n}}\n");
    for (value, expected) in [
        ("disabled", true),
        ("true", true),
        ("enabled", false),
        ("false", false),
    ] {
        let out = table(&format!(
            "\t\"{AGENT_LABEL}.other\" => disabled\n\t\"{AGENT_LABEL}\" => {value}"
        ));
        assert_eq!(
            outcome(Some(0), "", &out).disabled(AGENT_LABEL),
            Ok(expected)
        );
    }
    let out = table(&format!("\t\"{AGENT_LABEL}.other\" => disabled"));
    assert_eq!(outcome(Some(0), "", &out).disabled(AGENT_LABEL), Ok(false));
    let empty = "disabled services = (no disabled services)\n";
    assert_eq!(outcome(Some(0), "", empty).disabled(AGENT_LABEL), Ok(false));
    assert!(
        outcome(Some(1), "probe failure", empty)
            .disabled(AGENT_LABEL)
            .is_err()
    );

    for out in [
        String::new(),
        "disabled services = {\n".into(),
        table(&format!("\t\"{AGENT_LABEL}\" => unknown")),
        table(&format!(
            "\t\"{AGENT_LABEL}\" => enabled\n\t\"{AGENT_LABEL}\" => disabled"
        )),
    ] {
        let error = outcome(Some(0), "", &out)
            .disabled(AGENT_LABEL)
            .unwrap_err();
        assert_eq!(error.reason(), Some("launchctl_failed"));
        assert_eq!(error.detail["step"], "print-disabled");
    }
    let error = outcome(Some(1), "probe failure", &table(""))
        .disabled(AGENT_LABEL)
        .unwrap_err();
    assert_eq!(error.detail["status"], 1);
}

#[test]
fn large_job_dumps_keep_the_pid_and_closing_brace() {
    let job = job_dump(None, Some(42)).replace(
        "\t\tpath = /nested\n",
        &"\t\tEXTRA => a-long-value\n".repeat(600),
    );
    assert!(job.find("\tpid = 42").unwrap() > 8 * 1024);
    let stdout = String::from_utf8(read_stdout(Verb::Print, job.as_bytes()).unwrap()).unwrap();
    let printed = outcome(Some(0), "", &stdout)
        .printed_job(AGENT_LABEL)
        .unwrap()
        .unwrap();
    assert_eq!(printed_pid(&printed), Some(Some(42)));
    assert_eq!(
        printed_exit_timeout(&printed),
        Some(Duration::from_secs(900))
    );

    let error = outcome(Some(5), "", &stdout)
        .printed_job(AGENT_LABEL)
        .unwrap_err();
    assert!(error.detail["stderr"].as_str().unwrap().len() <= 8 * 1024);
}

#[test]
fn a_large_disabled_table_can_find_the_label_after_eight_kib() {
    let entries: String = (0..400)
        .map(|index| format!("\t\"com.example.long.{index}\" => enabled\n"))
        .collect();
    assert!(entries.len() > 8 * 1024);
    let table = format!("disabled services = {{\n{entries}\t\"{AGENT_LABEL}\" => disabled\n}}\n");
    let stdout =
        String::from_utf8(read_stdout(Verb::PrintDisabled, table.as_bytes()).unwrap()).unwrap();
    assert_eq!(
        outcome(Some(0), "", &stdout).disabled(AGENT_LABEL),
        Ok(true)
    );
}

#[test]
fn a_failed_print_never_falls_back_to_bootstrap() {
    let h = home(true);
    let fake = Arc::new(Fake {
        fail: Some("print"),
        ..Fake::default()
    });
    let (seen, mut on) = collect();
    let error = rt()
        .block_on(starter(h.path(), fake.clone()).start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(error.reason(), Some("launchctl_failed"));
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(fake.calls(), [argv(Verb::Print, AGENT_LABEL, None)]);
}

struct TestClock {
    start: Instant,
    elapsed: Mutex<Duration>,
}

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + *self.elapsed.lock().unwrap()
    }

    fn monotonic(&self) -> Instant {
        self.start + *self.elapsed.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) -> UpdateFuture<'_, ()> {
        Box::pin(async move { *self.elapsed.lock().unwrap() += duration })
    }
}

fn resolve_bootout(
    child: Option<Result<(), GuiError>>,
    exit_timeout: u64,
    probes: Vec<Result<Option<String>, GuiError>>,
) -> (BootoutOutcome, Vec<u64>) {
    let clock = TestClock {
        start: Instant::now(),
        elapsed: Mutex::default(),
    };
    let mut observed_at = Vec::new();
    let mut probes = probes.into_iter();
    let verdict = rt().block_on(wait_for_bootout(
        || {
            observed_at.push(clock.monotonic().duration_since(clock.start).as_secs());
            std::future::ready(probes.next().expect("an unexpected print probe"))
        },
        Path::new(BOOTOUT_PLIST),
        &clock,
        Duration::from_secs(exit_timeout),
        child.as_ref(),
    ));
    assert_eq!(probes.next(), None, "every expected probe was used");
    (verdict, observed_at)
}

const BOOTOUT_PLIST: &str = "/fake-home/Library/LaunchAgents/com.gwd.git-stratad.plist";

fn loaded_job(pid: Option<u32>) -> Result<Option<String>, GuiError> {
    Ok(Some(job_dump(Some(BOOTOUT_PLIST), pid)))
}

#[test]
fn different_pids_remain_visible_after_braces_in_environment_values() {
    let job = |pid| {
        job_dump(Some(BOOTOUT_PLIST), Some(pid)).replace(
            "\t\tpath = /nested\n",
            "\t\tFRAGMENT => prefix {\n\t\tBRACE => {\n\t\tpid => 99\n",
        )
    };
    let before = job(42);
    let after = job(43);
    assert_eq!(printed_pid(&before), Some(Some(42)));
    assert_eq!(printed_pid(&after), Some(Some(43)));

    let child = Err(GuiError::launchctl_failed(
        "bootout",
        Some(5),
        "bootout failed",
    ));
    let probes = vec![Ok(Some(before.clone())), Ok(Some(before)), Ok(Some(after))];
    assert_eq!(
        resolve_bootout(Some(child), 5, probes),
        (BootoutOutcome::Uncertain, vec![0, 5, 10])
    );
}

#[test]
fn an_incomplete_job_dump_cannot_prove_a_stopped_job() {
    let stopped = job_dump(None, None);
    for malformed in [
        stopped.trim_end_matches("}\n").to_owned(),
        stopped.replacen("\t}\n", "", 1),
        format!("{stopped}}}\n"),
        stopped.replace("\tstate = not running", "\tstate = running"),
        stopped.replace("\tstate = not running\n", ""),
        stopped.replace("\tstate = not running", "\tpid => 42"),
    ] {
        assert_eq!(printed_pid(&malformed), None, "{malformed}");
    }
}

#[test]
fn bootout_is_done_when_a_later_print_confirms_unload() {
    let failed_probe = GuiError::launchctl_failed("print", Some(5), "probe failure");
    for child in [
        Ok(()),
        Err(GuiError::launchctl_failed("bootout", None, "timed out")),
    ] {
        assert_eq!(
            resolve_bootout(Some(child), 7, vec![Err(failed_probe.clone()), Ok(None)]),
            (BootoutOutcome::Unloaded, vec![0, 5]),
        );
    }
}

#[test]
fn bootout_definite_failure_requires_nonzero_exit_and_two_stable_reads_after_timeout() {
    let failed = GuiError::launchctl_failed("bootout", Some(5), "bootout failed");
    for pid in [Some(42), None] {
        assert_eq!(
            resolve_bootout(Some(Err(failed.clone())), 7, vec![loaded_job(pid); 4]),
            (BootoutOutcome::DefiniteFailure, vec![0, 5, 7, 12]),
        );
    }
}

#[test]
fn bootout_keeps_uncertain_outcomes_distinct_from_definite_failure() {
    for child in [
        Some(Ok(())),
        Some(Err(GuiError::launchctl_failed(
            "bootout",
            None,
            "timed out",
        ))),
        None,
    ] {
        assert_eq!(
            resolve_bootout(child, 5, vec![loaded_job(Some(42)); 2]),
            (BootoutOutcome::Uncertain, vec![0, 5]),
        );
    }
    let failed = GuiError::launchctl_failed("bootout", Some(5), "bootout failed");
    for later in [
        loaded_job(Some(43)),
        Err(GuiError::launchctl_failed(
            "print",
            Some(5),
            "probe failure",
        )),
        Ok(Some("truncated dump".to_string())),
        Ok(Some(
            job_dump(Some(BOOTOUT_PLIST), Some(42)).replace("\tstate = running", "\tpid = 43"),
        )),
    ] {
        let probes = vec![loaded_job(Some(42)), loaded_job(Some(42)), later];
        assert_eq!(
            resolve_bootout(Some(Err(failed.clone())), 5, probes),
            (BootoutOutcome::Uncertain, vec![0, 5, 10]),
        );
    }
}

#[test]
fn bootout_polling_cannot_confirm_failure_from_an_unknown_state() {
    let stopped = job_dump(Some(BOOTOUT_PLIST), None);
    let unknown = Ok(Some(
        stopped.replace("\tstate = not running", "\tstate = unknown"),
    ));
    let failed = Some(Err(GuiError::launchctl_failed(
        "bootout",
        Some(5),
        "bootout failed",
    )));

    assert_eq!(
        resolve_bootout(
            failed.clone(),
            5,
            vec![
                Ok(Some(stopped.clone())),
                Ok(Some(stopped)),
                unknown.clone()
            ],
        ),
        (BootoutOutcome::Uncertain, vec![0, 5, 10])
    );
    assert_eq!(
        resolve_bootout(failed, 5, vec![unknown; 2]),
        (BootoutOutcome::Uncertain, vec![0, 5])
    );
}

#[test]
fn bootout_polling_requires_owned_plist_in_first_post_timeout_probe() {
    let failed = GuiError::launchctl_failed("bootout", Some(5), "bootout failed");
    for path in [Some("/foreign/agent.plist"), None] {
        let probes = vec![loaded_job(None), Ok(Some(job_dump(path, None)))];
        assert_eq!(
            resolve_bootout(Some(Err(failed.clone())), 5, probes),
            (BootoutOutcome::Uncertain, vec![0, 5]),
            "service dump path: {path:?}"
        );
    }
}

#[test]
fn bootout_polling_requires_owned_plist_in_second_post_timeout_probe() {
    let failed = GuiError::launchctl_failed("bootout", Some(5), "bootout failed");
    for path in [Some("/foreign/agent.plist"), None] {
        let probes = vec![
            loaded_job(None),
            loaded_job(None),
            Ok(Some(job_dump(path, None))),
        ];
        assert_eq!(
            resolve_bootout(Some(Err(failed.clone())), 5, probes),
            (BootoutOutcome::Uncertain, vec![0, 5, 10]),
            "service dump path: {path:?}"
        );
    }
}

/// The code of a Rust file without comments and without its test module.
fn code(src: &str) -> String {
    let body = src.split("#[cfg(test)]").next().unwrap();
    body.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn literals(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut esc = false;
    for c in code.chars() {
        match (&mut cur, c) {
            (None, '"') => cur = Some(String::new()),
            (Some(_), _) if esc => esc = false,
            (Some(_), '\\') => esc = true,
            (Some(s), '"') => {
                out.push(std::mem::take(s));
                cur = None;
            }
            (Some(s), ch) => s.push(ch),
            _ => {}
        }
    }
    out
}

/// §6.1: the service uses fixed launchctl verbs and cannot force a restart.
#[test]
fn service_uses_only_fixed_launchctl_verbs() {
    let files = [
        ("mod.rs", include_str!("mod.rs")),
        ("launchctl.rs", include_str!("launchctl.rs")),
        ("scope.rs", include_str!("scope.rs")),
        (
            "commands/service.rs",
            include_str!("../commands/service.rs"),
        ),
    ];
    let forbidden = [
        "stop", "kill", "remove", "unload", "-k", "signal", "sigterm",
    ];
    for (name, src) in files {
        let c = code(src);
        for lit in literals(&c) {
            for token in lit.split_whitespace() {
                assert!(
                    !forbidden.iter().any(|arg| token.eq_ignore_ascii_case(arg)),
                    "{name}: forbidden argv token {token:?}"
                );
            }
        }
        for f in ["fn stop", "fn kill", "fn remove", "fn unregister", ".kill("] {
            assert!(!c.contains(f), "{name} has {f}");
        }
    }
    let lc = code(include_str!("launchctl.rs"));
    assert!(lc.contains("Command::new(LAUNCHCTL)"));
    assert!(lc.contains("pub const LAUNCHCTL: &str = \"/bin/launchctl\";"));
    assert_eq!(LAUNCHCTL, "/bin/launchctl");
    assert!(lc.contains(".env_clear()"));
    assert!(!lc.contains(".arg("));
    assert_eq!(lc.matches(".args(").count(), 1);
    assert!(lc.contains(".args(argv(verb, label, plist))"));
    let verbs: Vec<_> = lc
        .lines()
        .filter(|line| line.trim_start().starts_with("Verb::"))
        .flat_map(literals)
        .collect();
    assert_eq!(
        verbs,
        [
            "print",
            "print-disabled",
            "kickstart",
            "bootstrap",
            "bootout",
            "disable",
            "enable"
        ]
    );
}

const UPDATER_COMMAND_BUILDER: &str = r"
    fn command(operation: Operation<'_>, login_home: &Path) -> Command {
        let (program, args) = operation.argv();
        let mut command = Command::new(program);
        command.args(args).env_clear().envs(environment(login_home));
        command
    }
";

fn without_whitespace(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

fn uses_fixed_updater_command(source: &str) -> bool {
    source.contains(&without_whitespace(UPDATER_COMMAND_BUILDER))
        && ["Command::new(", ".args(", ".env_clear(", ".envs("]
            .into_iter()
            .all(|call| source.matches(call).count() == 1)
        && [".arg(", ".env(", ".env_remove(", ".pre_exec("]
            .into_iter()
            .all(|call| !source.contains(call))
}

#[test]
fn updater_spawn_guard_checks_the_builder_and_refuses_later_changes() {
    let allowed = without_whitespace(UPDATER_COMMAND_BUILDER);
    assert!(uses_fixed_updater_command(&allowed));
    for (checked, replacement) in [
        ("Command::new(program)", "Command::new(\"/usr/bin/false\")"),
        (".args(args)", ".args(other_args)"),
        (".env_clear()", ""),
        ("environment(login_home)", "environment(other_home)"),
    ] {
        assert!(!uses_fixed_updater_command(
            &allowed.replace(checked, replacement)
        ));
    }
    assert!(!uses_fixed_updater_command(&format!(
        "{allowed}command.arg(extra);"
    )));
    assert!(!uses_fixed_updater_command(&format!(
        "{allowed}command.env(key,value);"
    )));
}

/// Process constructors are confined to the launchctl and updater runners.
#[test]
fn nothing_else_spawns_a_process() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let launchctl = src.join("service/launchctl.rs");
    let update_exec = src.join("update/exec.rs");
    let mut files = Vec::new();
    walk(&src, &mut files);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let c = without_whitespace(&code(&text));
        let n = c.matches("Command::new(").count();
        if f == launchctl {
            assert_eq!(n, 1);
        } else if f == update_exec {
            // T00 has only the operation list and environment. Once the
            // runner lands, its sole constructor must apply both to the
            // actual command, without subsequent argument or env changes.
            let has_runner = c.contains("Command") || c.contains("fncommand(");
            assert_eq!(n, usize::from(has_runner));
            if has_runner {
                assert!(
                    uses_fixed_updater_command(&c),
                    "update/exec.rs must use {UPDATER_COMMAND_BUILDER}"
                );
            }
        } else {
            assert_eq!(n, 0, "{} spawns a process", f.display());
            assert!(
                !c.contains("process::Command"),
                "{} uses process::Command",
                f.display()
            );
        }
    }
}

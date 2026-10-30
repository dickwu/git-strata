//! S0 tests. A fake ServiceManager only: nothing here runs launchctl.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::launchctl::{LAUNCHCTL, Verb, argv};
use super::*;

#[derive(Default)]
struct Fake {
    loaded: bool,
    /// The top-level `path = ` launchd reports when loaded.
    loaded_from: Option<String>,
    fail: Option<&'static str>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn result(&self, step: &str) -> Result<(), GuiError> {
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
        self.calls.lock().unwrap().push(format!("print {label}"));
        self.result("print")?;
        Ok(self.loaded.then(|| dump(self.loaded_from.as_deref())))
    }
    fn kickstart(&self, label: &str) -> Result<(), GuiError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("kickstart {label}"));
        self.result("kickstart")
    }
    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("bootstrap {}", plist.display()));
        self.result("bootstrap")
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
fn dump(path: Option<&str>) -> String {
    let p = path.map(|p| format!("\tpath = {p}\n")).unwrap_or_default();
    format!(
        "gui/501/{AGENT_LABEL} = {{\n\tactive count = 1\n{p}\tstdout path = /tmp/o.log\n\tenvironment = {{\n\t\tpath = /nested\n\t}}\n\tstate = running\n}}\n"
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
        vec![
            format!("print {AGENT_LABEL}"),
            format!("kickstart {AGENT_LABEL}")
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
        vec![
            format!("print {AGENT_LABEL}"),
            format!("bootstrap {}", canon_plist(h.path()).display())
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
    assert_eq!(fake.calls(), vec![format!("print {AGENT_LABEL}")]);
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
        format!("bootstrap {}", canon_plist(login.path()).display()),
        "bootstraps the canonical plist"
    );
}

#[test]
fn an_agent_loaded_from_another_plist_is_foreign_and_not_kickstarted() {
    let h = home(true);
    let other = home(true);
    let fake = Arc::new(Fake {
        loaded: true,
        loaded_from: Some(canon_plist(other.path()).to_string_lossy().into_owned()),
        ..Fake::default()
    });
    let (seen, mut on) = collect();
    let e = rt()
        .block_on(starter(h.path(), fake.clone()).start(scripted(vec![Ok(json!({}))]), &mut on))
        .unwrap_err();
    assert_eq!(e.code, "InvalidInput");
    assert_eq!(e.reason(), Some("foreign_agent"));
    assert_eq!(
        e.detail["loaded_path"],
        canon_plist(other.path()).to_string_lossy().as_ref()
    );
    assert_eq!(
        e.detail["expected"],
        canon_plist(h.path()).to_string_lossy().as_ref()
    );
    assert_eq!(fake.calls(), vec![format!("print {AGENT_LABEL}")]);
    assert!(seen.lock().unwrap().is_empty());
    let a = rt().block_on(starter(h.path(), fake).agent()).unwrap();
    assert_eq!(a.state, AgentState::Foreign);
    assert_eq!(a.refusal.unwrap().reason(), Some("foreign_agent"));
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
            loaded: true,
            loaded_from: from,
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
        scope::loaded_path(&dump(Some("/Users/a/Library/LaunchAgents/x.plist"))).as_deref(),
        Some("/Users/a/Library/LaunchAgents/x.plist")
    );
    assert_eq!(
        scope::loaded_path(&dump(None)),
        None,
        "nested and stdout paths do not count"
    );
    assert_eq!(scope::loaded_path(""), None);
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
    assert_eq!(
        argv(Verb::Print, AGENT_LABEL, None),
        vec!["print".to_string(), format!("gui/{uid}/{AGENT_LABEL}")]
    );
    assert_eq!(
        argv(Verb::Kickstart, AGENT_LABEL, None),
        vec!["kickstart".to_string(), format!("gui/{uid}/{AGENT_LABEL}")],
        "no -k: a running daemon is never restarted"
    );
    let plist = PathBuf::from("/Users/x/Library/LaunchAgents/com.gwd.git-stratad.plist");
    assert_eq!(
        argv(Verb::Bootstrap, AGENT_LABEL, Some(&plist)),
        vec![
            "bootstrap".to_string(),
            format!("gui/{uid}"),
            plist.to_string_lossy().into_owned()
        ]
    );
    assert_eq!(
        Verb::ALL.map(Verb::as_str),
        ["print", "kickstart", "bootstrap"]
    );
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

/// §13 Q3, start only: no stop / bootout / kill / remove / unload /
/// disable / `-k` anywhere launchctl could be handed one, and the only
/// process the GUI spawns is /bin/launchctl.
#[test]
fn service_never_stops_and_spawns_only_launchctl() {
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
        "stop", "bootout", "kill", "remove", "unload", "disable", "-k", "signal", "sigterm",
    ];
    for (name, src) in files {
        let c = code(src);
        for lit in literals(&c) {
            let l = lit.to_lowercase();
            for f in forbidden {
                // "-k" only as a whole argument.
                let hit = if f == "-k" {
                    l.split_whitespace().any(|w| w == "-k")
                } else {
                    l.contains(f)
                };
                assert!(!hit, "{name}: string literal {lit:?} contains {f:?}");
            }
        }
        for f in [
            "fn stop",
            "fn bootout",
            "fn kill",
            "fn remove",
            "fn unregister",
            ".kill(",
        ] {
            assert!(!c.contains(f), "{name} has {f}");
        }
    }
    let lc = code(include_str!("launchctl.rs"));
    assert_eq!(lc.matches("Command::new(").count(), 1);
    assert!(lc.contains("Command::new(LAUNCHCTL)"));
    assert!(lc.contains("pub const LAUNCHCTL: &str = \"/bin/launchctl\";"));
    assert_eq!(LAUNCHCTL, "/bin/launchctl");
    assert!(lc.contains(".env_clear()"));
    assert!(!lc.contains(".arg("), "argv comes only from argv()");
    // Every verb literal in launchctl.rs is one of the three.
    let verbs: Vec<String> = literals(&lc)
        .into_iter()
        .filter(|l| l.chars().all(|c| c.is_ascii_lowercase()) && !l.is_empty())
        .collect();
    for v in &verbs {
        assert!(
            ["print", "kickstart", "bootstrap"].contains(&v.as_str()),
            "unexpected literal {v:?}"
        );
    }
}

/// No other source file in the crate spawns a process.
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
    let mut files = Vec::new();
    walk(&src, &mut files);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let c = code(&text);
        let n = c.matches("Command::new(").count();
        if f.ends_with("service/launchctl.rs") {
            assert_eq!(n, 1);
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

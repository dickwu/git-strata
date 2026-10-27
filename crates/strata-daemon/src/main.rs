//! git-stratad: the per-user daemon. Started by launchd (`RunAtLoad`,
//! `KeepAlive.SuccessfulExit = false`); never exits on idle.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use strata_daemon::startup::{self, Daemon, InstanceLock, Options};
use strata_daemon::waiting::{self, Policy, WaitState, Waiting};
use strata_types::StrataError;

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

fn fail(e: &StrataError) -> ExitCode {
    // One JSON line on stdout, the message on stderr.
    let line = serde_json::json!({ "ok": false, "error": e });
    println!("{line}");
    eprintln!("git-stratad: {}", e.message);
    ExitCode::FAILURE
}

fn run() -> Result<(), StrataError> {
    startup::refuse_test_seams()?;
    let home = std::env::var_os("HOME").ok_or_else(|| {
        StrataError::new(strata_types::Code::InvalidInput, "HOME is not set")
    })?;
    let opts = Options::for_home(std::path::Path::new(&home));
    // SAFETY: the handler only stores to an atomic. Installed before the
    // store wait so `daemon stop` (SIGTERM) ends a wait at once.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    // 2. Single instance, held across the store wait below.
    let mut instance = Some(InstanceLock::take(&strata_daemon::lock_path(&opts.config_dir))?);
    let open_git = |p: &std::path::Path| strata_gitio::Git::open(p);
    // §5.1a: an I/O error in steps 3–7 (EINTR / EPERM while macOS TCC
    // decides on a removable volume, …) is waited out in-process with
    // backoff instead of exiting into a launchd KeepAlive respawn loop.
    // Meanwhile the socket answers Busy / store_unavailable.
    let state = WaitState::default();
    let mut waiting: Option<Waiting> = None;
    let alert = opts.alert.clone();
    let got = waiting::wait_until(
        || startup::prepare_locked(opts.clone(), strata_confirm::Confirmer::new(), &open_git, &mut instance),
        Policy::PRODUCTION,
        &|| TERM.load(Ordering::SeqCst),
        &|line| alert(line),
        &mut |e, n, wait| {
            state.failed(e, n, wait);
            if waiting.is_none() {
                match startup::bind(&opts.socket).map_err(|e| e.message).and_then(|l| {
                    Waiting::start(l, state.clone()).map_err(|e| e.to_string())
                }) {
                    Ok(w) => waiting = Some(w),
                    Err(m) => alert(&format!("cannot answer on the socket while waiting: {m}")),
                }
            }
        },
    );
    let core = match got {
        Ok(Some(core)) => core,
        Ok(None) => {
            // SIGTERM while waiting: nothing was opened.
            if let Some(w) = waiting {
                w.abandon(&opts.socket);
            }
            return Ok(());
        }
        Err(e) => {
            if let Some(w) = waiting {
                w.abandon(&opts.socket);
            }
            return Err(e);
        }
    };
    state.starting();
    let result = (|| {
        let daemon = Daemon::start(&core)?;
        // session.md D53: back-to-back `new`/`add` reuse a fetch younger
        // than this (publish fetches for itself). Tests keep the default 0.
        daemon.sessions.set_fetch_window(strata_session::NEW_FETCH_WINDOW);
        // The waiting socket, when there was one, becomes the real one.
        let listener = match waiting.take().and_then(Waiting::finish) {
            Some(l) => l,
            None => startup::bind(&core.opts.socket)?,
        };
        strata_daemon::handlers::serve(&daemon, &listener, &TERM)
    })();
    drop(waiting);
    let closed = core.close();
    result.and(closed)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

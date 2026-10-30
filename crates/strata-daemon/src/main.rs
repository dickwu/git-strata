//! git-stratad: the per-user daemon. Started by launchd (`RunAtLoad`,
//! `KeepAlive.SuccessfulExit = false`); never exits on idle. With
//! `--version` or `--print-plist [BIN]` it only prints (plist::Invocation).

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use strata_daemon::plist::{self, Invocation};
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

fn fail_quiet(e: &StrataError) -> ExitCode {
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
    // Incident 2026-10-27: launchd's default soft descriptor limit (256)
    // ran out with 25 live workspaces (each watcher holds ~8). Raise it
    // before anything is opened.
    match startup::raise_fd_limit() {
        Ok((before, after)) if after > before => (opts.alert)(&format!("raised the open-file limit from {before} to {after}")),
        Ok(_) => {}
        Err(e) => (opts.alert)(&format!("cannot raise the open-file limit: {e}")),
    }
    // 2. Single instance, held across the store wait below.
    let mut instance = Some(InstanceLock::take(&strata_daemon::lock_path(&opts.config_dir))?);
    let open_git = |p: &std::path::Path| strata_gitio::Git::open(p);
    // §5.1a: an I/O error in steps 3–7 (EINTR / EPERM while macOS TCC
    // decides on a removable volume, …) is waited out in-process with
    // backoff instead of exiting into a launchd KeepAlive respawn loop.
    // Meanwhile the socket answers Busy / store_unavailable.
    let state = WaitState::default();
    let alert = opts.alert.clone();
    // Review P1-6: answer `Busy starting` before the first attempt, which
    // may itself stall in a syscall while TCC decides.
    let mut waiting: Option<Waiting> = match waiting::answer_early(&opts.socket, &state) {
        Ok(w) => Some(w),
        Err(m) => {
            alert(&format!("cannot answer on the socket while starting: {m}"));
            None
        }
    };
    let got = waiting::wait_until(
        || startup::prepare_locked(opts.clone(), strata_confirm::Confirmer::new(), &open_git, &mut instance),
        Policy::PRODUCTION,
        &|| TERM.load(Ordering::SeqCst),
        &|line| alert(line),
        &mut |e, n, wait| {
            state.failed(e, n, wait);
            if waiting.is_none() {
                match waiting::answer_early(&opts.socket, &state) {
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

/// `--version` / `--print-plist` (plist::Invocation): print and exit,
/// before the seams check, the lock or anything under HOME.
fn print(inv: Invocation) -> Result<(), StrataError> {
    match inv {
        Invocation::Serve => unreachable!("serve is not a print"),
        Invocation::Help => print!("{}", plist::USAGE),
        Invocation::Version => println!("git-stratad {}", env!("CARGO_PKG_VERSION")),
        Invocation::PrintPlist(bin) => {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .filter(|h| h.is_absolute())
                .ok_or_else(|| StrataError::new(strata_types::Code::InvalidInput, "HOME is not an absolute path"))?;
            let bin = match bin {
                Some(b) => b,
                None => std::env::current_exe().map_err(|e| {
                    StrataError::new(strata_types::Code::Unknown, format!("cannot resolve this executable: {e}"))
                })?,
            };
            print!("{}", plist::plist_text(&bin, &home));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match plist::parse_invocation(&args) {
        Ok(Invocation::Serve) => {}
        // Print invocations report errors on stderr only, so a failed
        // `--print-plist > file` never leaves JSON in the plist.
        Ok(inv) => {
            return match print(inv) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail_quiet(&e),
            };
        }
        Err(e) => return fail_quiet(&e),
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

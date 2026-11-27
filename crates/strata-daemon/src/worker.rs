//! Process entry shared by the launchd daemon and the isolated test worker.

use crate::Result;
use crate::startup::{self, Daemon, Options};
use crate::waiting::{self, Policy, WaitState, Waiting};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use strata_confirm::Confirmer;
use strata_gitio::Git;

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

extern "C" fn on_hup(_: libc::c_int) {
    crate::reload::HUP.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn serve(
    opts: Options,
    confirmer: impl Fn() -> Confirmer,
    open_git: impl Fn(&Path) -> Result<Git> + Clone + Send + Sync + 'static,
) -> Result<()> {
    // SAFETY: the handler only stores to an atomic. Installed before the
    // store wait so `daemon stop` (SIGTERM) ends a wait at once.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_hup as *const () as libc::sighandler_t);
    }
    // Incident 2026-10-27: launchd's default soft descriptor limit (256)
    // ran out with 25 live workspaces (each watcher holds ~8). Raise it
    // before anything is opened.
    match startup::raise_fd_limit() {
        Ok((before, after)) if after > before => (opts.alert)(&format!(
            "raised the open-file limit from {before} to {after}"
        )),
        Ok(_) => {}
        Err(e) => (opts.alert)(&format!("cannot raise the open-file limit: {e}")),
    }
    // 2. Single instance, held across the store wait below.
    let mut instance = Some(startup::take_instance(&opts)?);
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
        || startup::prepare_locked(opts.clone(), confirmer(), &open_git, &mut instance),
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
        daemon
            .sessions
            .set_fetch_window(strata_session::NEW_FETCH_WINDOW);
        // The waiting socket, when there was one, becomes the real one.
        let listener = match waiting.take().and_then(Waiting::finish) {
            Some(l) => l,
            None => startup::bind(&core.opts.socket)?,
        };
        crate::handlers::serve(&daemon, &listener, &TERM)
    })();
    drop(waiting);
    let closed = core.close();
    result.and(closed)
}

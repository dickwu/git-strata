//! git-stratad: the per-user daemon. Started by launchd (`RunAtLoad`,
//! `KeepAlive.SuccessfulExit = false`); never exits on idle.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use strata_daemon::startup::{self, Daemon, Options};
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
    let core = startup::prepare(opts, strata_confirm::Confirmer::new(), &|p| strata_gitio::Git::open(p))?;
    let result = (|| {
        let daemon = Daemon::start(&core)?;
        let listener = startup::bind(&core.opts.socket)?;
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
        }
        daemon.serve(&listener, &TERM, &|req| daemon.handle(req))
    })();
    let closed = core.close();
    result.and(closed)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

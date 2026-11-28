//! git-stratad: the per-user daemon. Started by launchd (`RunAtLoad`,
//! `KeepAlive.SuccessfulExit = false`); never exits on idle. With
//! `--version`, `--print-plist [BIN]` or `--check-config PATH` it
//! only prints (plist::Invocation).

strata_types::mark_release_binary!();

use std::process::ExitCode;

use strata_daemon::plist::{self, Invocation};
use strata_daemon::startup::{self, Options};
use strata_types::StrataError;

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
    let home = std::env::var_os("HOME")
        .ok_or_else(|| StrataError::new(strata_types::Code::InvalidInput, "HOME is not set"))?;
    let opts = Options::for_home(std::path::Path::new(&home));
    strata_daemon::worker::serve(opts, strata_confirm::Confirmer::new, |p| {
        strata_gitio::Git::open(p)
    })
}

/// Print and exit before the seams check, the lock or server startup.
fn print(inv: Invocation) -> Result<(), StrataError> {
    match inv {
        Invocation::Serve => unreachable!("serve is not a print"),
        Invocation::Help => print!("{}", plist::USAGE),
        Invocation::Version => println!("git-stratad {}", env!("CARGO_PKG_VERSION")),
        Invocation::PrintPlist(bin) => {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .filter(|h| h.is_absolute())
                .ok_or_else(|| {
                    StrataError::new(
                        strata_types::Code::InvalidInput,
                        "HOME is not an absolute path",
                    )
                })?;
            let bin = match bin {
                Some(b) => b,
                None => std::env::current_exe().map_err(|e| {
                    StrataError::new(
                        strata_types::Code::Unknown,
                        format!("cannot resolve this executable: {e}"),
                    )
                })?,
            };
            print!("{}", plist::plist_text(&bin, &home));
        }
        Invocation::CheckConfig(path) => {
            plist::check_config(&path)?;
            println!("{}", serde_json::json!({ "ok": true, "error": null }));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // --check-config has a JSON result. For --print-plist, keep stdout
    // empty on every failure so a redirected file never contains JSON.
    let print_failed: fn(&StrataError) -> ExitCode =
        if args.first().is_some_and(|arg| arg == "--check-config") {
            fail
        } else {
            fail_quiet
        };
    match plist::parse_invocation(&args) {
        Ok(Invocation::Serve) => {}
        Ok(inv) => {
            return match print(inv) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => print_failed(&e),
            };
        }
        Err(e) => return print_failed(&e),
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

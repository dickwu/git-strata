//! Process fixture for cargo integration tests. Both arguments and HOME are mandatory.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use strata_daemon::Result;
use strata_daemon::fixture::WorkerFixture;
use strata_daemon::seams::{Confirmer, FakeClock, ScriptedAuth};
use strata_types::{Code, StrataError};

strata_types::mark_release_binary!();

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [root, config_dir] = args.as_slice() else {
        return Err(StrataError::new(
            Code::InvalidInput,
            "expected fixture root and config directory",
        ));
    };
    let home = PathBuf::from(
        std::env::var_os("HOME")
            .ok_or_else(|| StrataError::new(Code::InvalidInput, "expected fixture HOME"))?,
    );
    let fixture = WorkerFixture::new(Path::new(root), &home, Path::new(config_dir))?;
    strata_daemon::worker::serve(
        fixture.options(),
        || {
            Confirmer::with_seams(
                ScriptedAuth::new(false, Ok(()), vec![]),
                FakeClock::new(),
                Duration::from_secs(2),
                Duration::from_millis(200),
            )
        },
        move |path| fixture.open_git(path),
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("git-stratad-test: {}", e.message);
            println!("{}", serde_json::json!({"ok": false, "error": e}));
            ExitCode::FAILURE
        }
    }
}

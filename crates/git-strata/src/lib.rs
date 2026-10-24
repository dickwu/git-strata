//! git-strata, the command-line client (cli.md). Every call prints exactly
//! one compact JSON envelope on stdout (§4) and exits 0 or 1; human text
//! goes to stderr only. The binary never runs `git` itself and never
//! calls strata-confirm (§1): confirmations happen in the daemon.

pub mod args;
pub mod confirm;
pub mod ctx;
pub mod env;
pub mod envelope;
pub mod ro;
pub mod rpc;
pub mod show;
pub mod tty;

use std::ffi::OsString;

use clap::Parser;
use clap::error::ErrorKind;
use serde_json::{Value, json};
use strata_types::{Code, StrataError};

use crate::args::{Cli, Cmd, ConfigCmd, DaemonCmd, DepsCmd};
use crate::ctx::Ctx;
use crate::envelope::{Envelope, emit_stdout};

pub type Result<T> = std::result::Result<T, StrataError>;

/// Parse `std::env::args_os()`, run, print the envelope, return the exit code.
pub fn run() -> i32 {
    run_args(std::env::args_os().collect())
}

pub fn run_args(argv: Vec<OsString>) -> i32 {
    let cli = match Cli::try_parse_from(&argv) {
        Ok(c) => c,
        Err(e) => match e.kind() {
            // D19: help and version are clap text, exit 0, no envelope.
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                let _ = e.print();
                return 0;
            }
            _ => return emit_stdout(&Envelope::err(parse_error(&argv, &e))),
        },
    };
    emit_stdout(&dispatch(cli.cmd))
}

/// A clap rejection as `InvalidInput` (§3.1): an unknown or missing
/// subcommand carries the sorted command list of its level.
fn parse_error(argv: &[OsString], e: &clap::Error) -> StrataError {
    let words: Vec<String> = argv.iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let first = words.iter().find(|w| !w.starts_with('-'));
    let group = first.and_then(|g| args::group_commands(g).map(|c| (g.clone(), c)));
    let msg = e.to_string().lines().next().unwrap_or("invalid arguments").trim_start_matches("error: ").to_string();
    match e.kind() {
        ErrorKind::InvalidSubcommand
        | ErrorKind::MissingSubcommand
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let (level, commands) = match group {
                Some((g, c)) => (Some(g), c),
                None => (None, args::top_commands()),
            };
            let mut d = json!({ "reason": "unknown_command", "commands": commands });
            if let Some(g) = level {
                d["group"] = json!(g);
            }
            StrataError::new(Code::InvalidInput, msg).with_detail(d)
        }
        _ => StrataError::new(Code::InvalidInput, msg).with_detail(json!({ "reason": "args" })),
    }
}

fn dispatch(cmd: Cmd) -> Envelope {
    match cmd {
        Cmd::Daemon(DaemonCmd::Status) => Envelope::from_result(daemon_status_offline()),
        cmd => match run_cmd(cmd) {
            Ok(env) => env,
            Err(e) => Envelope::err(e),
        },
    }
}

fn run_cmd(cmd: Cmd) -> Result<Envelope> {
    use crate::confirm as c;
    // Argument checks that need no environment come first.
    match &cmd {
        Cmd::Resolve { abandon, accept, .. } if *abandon == *accept => {
            return c::resolve_choice_error(*abandon as usize + *accept as usize);
        }
        Cmd::Chain { accept_origin, restore, .. } if accept_origin.is_some() == *restore => {
            return c::resolve_choice_error(accept_origin.is_some() as usize + *restore as usize);
        }
        _ => {}
    }
    let ctx = Ctx::from_env()?;
    match cmd {
        Cmd::Deps(DepsCmd::Approve { id, sessions }) => c::deps_approve(&ctx, &id, &sessions),
        Cmd::Push { repo } => c::push(&ctx, &repo),
        Cmd::Publish { from, assign } => c::publish(&ctx, &from, assign.as_deref()),
        Cmd::Assign { file, session } => c::assign(&ctx, &file, &session),
        Cmd::Rm { session, discard } => c::rm(&ctx, &session, discard),
        Cmd::Resolve { session, repo, abandon, accept } => c::resolve(&ctx, &session, &repo, abandon, accept),
        Cmd::Chain { repo, accept_origin, restore } => c::chain(&ctx, &repo, accept_origin, restore),
        Cmd::Config(ConfigCmd::Relax { repo }) => c::config_relax(&ctx, &repo),
        other => Err(StrataError::new(Code::Unsupported, format!("{other:?} is not implemented yet"))),
    }
}

fn daemon_status_offline() -> Result<Value> {
    let paths = env::Paths::from_env()?;
    Ok(json!({
        "running": false,
        "socket": paths.socket.to_string_lossy(),
        "holders": [],
        "unknown_publishes": [],
        "alerts": [],
    }))
}

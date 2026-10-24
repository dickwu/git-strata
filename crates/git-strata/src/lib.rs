//! git-strata, the command-line client (cli.md). Every call prints exactly
//! one compact JSON envelope on stdout (§4) and exits 0 or 1; human text
//! goes to stderr only. The binary never runs `git` itself and never
//! calls strata-confirm (§1): confirmations happen in the daemon.

pub mod args;
pub mod cmds;
pub mod confirm;
pub mod ctx;
pub mod env;
pub mod envelope;
pub mod launch;
pub mod local;
pub mod ro;
pub mod rpc;
pub mod show;
pub mod tty;

use std::ffi::OsString;

use clap::Parser;
use clap::error::ErrorKind;
use serde_json::json;
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
    let env = dispatch(cli.cmd);
    let code = emit_stdout(&env);
    // D20: after a successful JSON, the pending-proposal hint for the
    // session the command resolved; its failures change nothing.
    if code == 0
        && let Some(s) = HINT.lock().ok().and_then(|mut h| h.take())
        && let Ok(config) = env::Paths::from_env().and_then(|p| p.config())
    {
        ro::pending_hint(&config, &s);
    }
    code
}

static HINT: std::sync::Mutex<Option<strata_types::SessionId>> = std::sync::Mutex::new(None);

/// The session a command resolved (for the D20 hint after the envelope).
pub fn hint(s: strata_types::SessionId) {
    if let Ok(mut h) = HINT.lock() {
        *h = Some(s);
    }
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
        Cmd::Daemon(DaemonCmd::Status) => cmds::daemon_status().unwrap_or_else(Envelope::err),
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
        Cmd::New { ticket_flag, ticket, repos } => cmds::new(&ctx, ticket_flag, ticket, &repos),
        Cmd::Status { sel } => cmds::status(&ctx, &sel),
        Cmd::Checkpoint { tool_call, message, session, paths } => cmds::checkpoint(&ctx, tool_call, message, session, &paths),
        Cmd::Restore { checkpoint, paths, session } => cmds::restore(&ctx, &checkpoint, &paths, session),
        Cmd::Grep { pattern, sel } => cmds::grep(&ctx, &pattern, &sel),
        Cmd::Files { glob, sel } => cmds::files(&ctx, glob.as_deref(), &sel),
        Cmd::Diff { base, session, between, repo } => cmds::diff(&ctx, base, session, between, repo),
        Cmd::Deps(DepsCmd::Propose { session, repo }) => cmds::deps_propose(&ctx, session, repo),
        Cmd::Deps(DepsCmd::List { repo }) => cmds::deps_list(&ctx, repo),
        Cmd::Gc => cmds::gc(&ctx),
        Cmd::Daemon(DaemonCmd::Start) => launch::start(&ctx.paths.home),
        Cmd::Daemon(DaemonCmd::Stop) => launch::stop(),
        Cmd::Daemon(DaemonCmd::Status) => cmds::daemon_status(),
    }
}

//! The one command tree (cli.md §3, §3.1). `git-strata` and `git strata`
//! parse the same tree (D1). No `--yes`, `--force` or `--human` anywhere
//! (§1); there is no flag that skips a TTY check or a confirmation.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "git-strata",
    version,
    about = "Per-session workspaces over shared base snapshots; every call prints one JSON envelope on stdout",
    subcommand_required = true,
    arg_required_else_help = false,
    disable_help_subcommand = true,
    after_long_help = crate::help::TOP
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// `--session` / `--all` (both together is `both_selectors`).
#[derive(Debug, Args, Clone, Default)]
pub struct Select {
    /// Session id or ticket (an ambiguous ticket is AmbiguousMatch)
    #[arg(long)]
    pub session: Option<String>,
    /// Every live session
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Create a session with one workspace per repo
    #[command(long_about = crate::help::NEW, after_long_help = crate::help::NEW_AFTER)]
    New {
        /// Ticket name (also accepted positionally)
        #[arg(long = "ticket", value_name = "T")]
        ticket_flag: Option<String>,
        /// Ticket name
        #[arg(value_name = "TICKET")]
        ticket: Option<String>,
        /// Repos to track, comma separated (required; every configured repo still gets a workspace)
        #[arg(long, value_delimiter = ',', value_name = "A,B")]
        repos: Vec<String>,
    },
    /// Session status (cwd's session, --session, or --all)
    #[command(after_long_help = crate::help::STATUS_AFTER)]
    Status {
        #[command(flatten)]
        sel: Select,
    },
    /// Record a checkpoint (exactly one of --tool-call / -m)
    #[command(after_long_help = crate::help::CHECKPOINT_AFTER)]
    Checkpoint {
        /// Tool call id (hook mode; reads the hook JSON on a non-TTY stdin).
        /// Without a value, the id is the hook JSON's `tool_use_id`
        #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
        tool_call: Option<String>,
        /// Manual checkpoint message
        #[arg(short = 'm', long = "message", value_name = "MSG")]
        message: Option<String>,
        /// Session id or ticket (default: cwd's session)
        #[arg(long)]
        session: Option<String>,
        /// A changed path (repeatable)
        #[arg(long = "path", value_name = "P")]
        paths: Vec<String>,
    },
    /// Restore a checkpoint (whole workspace, or only the given paths)
    #[command(after_long_help = crate::help::RESTORE_AFTER)]
    Restore {
        /// Checkpoint id
        checkpoint: String,
        /// Paths to restore
        paths: Vec<String>,
        /// Session id or ticket (default: cwd's session)
        #[arg(long)]
        session: Option<String>,
    },
    /// Search session workspaces (cwd's session, --session, or --all)
    #[command(after_long_help = crate::help::GREP_AFTER)]
    Grep {
        /// Regular expression
        pattern: String,
        #[command(flatten)]
        sel: Select,
    },
    /// List workspace files matching a glob (default: every file)
    #[command(after_long_help = crate::help::FILES_AFTER)]
    Files {
        /// Glob over repo paths
        glob: Option<String>,
        #[command(flatten)]
        sel: Select,
    },
    /// Show changes: against the base (default), or between two sessions
    #[command(after_long_help = crate::help::DIFF_AFTER)]
    Diff {
        /// Against the pinned base (default)
        #[arg(long)]
        base: bool,
        /// Session id or ticket (default: cwd's session)
        #[arg(long)]
        session: Option<String>,
        /// Two sessions to compare (needs --repo)
        #[arg(long, num_args = 2, value_names = ["A", "B"])]
        between: Option<Vec<String>>,
        /// Repo to compare (default: cwd's repo, or the session's only repo)
        #[arg(long)]
        repo: Option<String>,
    },
    /// Dependency proposals
    #[command(subcommand, subcommand_required = true, after_long_help = crate::help::DEPS_AFTER)]
    Deps(DepsCmd),
    /// Publish session changes to the publish branch
    #[command(long_about = crate::help::PUBLISH, after_long_help = crate::help::PUBLISH_AFTER)]
    Publish {
        /// SESSION:PATH to include (repeatable; without it, interactive on a TTY)
        #[arg(long = "from", value_name = "S:PATH")]
        from: Vec<String>,
        /// Same as `git-strata assign --file FILE --session SESSION`
        #[arg(long, num_args = 2, value_names = ["FILE", "SESSION"])]
        assign: Option<Vec<String>>,
    },
    /// Push a repo's publish branch (asks for confirmation when configured)
    #[command(long_about = crate::help::PUSH, after_long_help = crate::help::PUSH_AFTER)]
    Push {
        /// Repo name
        repo: String,
    },
    /// Assign a conflicted file to a session
    #[command(after_long_help = crate::help::ASSIGN_AFTER)]
    Assign {
        /// Repo path of the file
        #[arg(long, value_name = "PATH")]
        file: String,
        /// Session id or ticket
        #[arg(long)]
        session: String,
    },
    /// Remove a session
    #[command(after_long_help = crate::help::RM_AFTER)]
    Rm {
        /// Session id or ticket
        #[arg(long)]
        session: String,
        /// Discard unpublished changes (needs a TTY and confirmation)
        #[arg(long)]
        discard: bool,
    },
    /// Collect garbage (old bases, removed sessions)
    #[command(after_long_help = crate::help::GC_AFTER)]
    Gc,
    /// Daemon control
    #[command(subcommand, subcommand_required = true, after_long_help = crate::help::DAEMON_AFTER)]
    Daemon(DaemonCmd),
    /// Resolve an interrupted workspace move (needs a TTY)
    #[command(after_long_help = crate::help::RESOLVE_AFTER)]
    Resolve {
        /// Session id or ticket
        #[arg(long)]
        session: String,
        /// Repo name
        #[arg(long)]
        repo: String,
        /// Abandon the move
        #[arg(long)]
        abandon: bool,
        /// Accept the move
        #[arg(long)]
        accept: bool,
    },
    /// Resolve a diverged publish chain (needs a TTY)
    #[command(after_long_help = crate::help::CHAIN_AFTER)]
    Chain {
        /// Repo name
        #[arg(long)]
        repo: String,
        /// Accept origin and replay or drop the local chain
        #[arg(long, value_enum, value_name = "replay|drop")]
        accept_origin: Option<AcceptOrigin>,
        /// Restore the local chain onto origin (pushes)
        #[arg(long)]
        restore: bool,
    },
    /// Configuration
    #[command(subcommand, subcommand_required = true, after_long_help = crate::help::CONFIG_AFTER)]
    Config(ConfigCmd),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AcceptOrigin {
    Replay,
    Drop,
}

#[derive(Debug, Subcommand)]
pub enum DepsCmd {
    /// Propose new dependencies from a session's workspace
    #[command(after_long_help = crate::help::DEPS_AFTER)]
    Propose {
        /// Session id or ticket (default: cwd's session)
        #[arg(long)]
        session: Option<String>,
        /// Repo name (default: cwd's repo)
        #[arg(long)]
        repo: Option<String>,
        /// Also move this session (only the proposer) onto the proposed
        /// set now, without confirmation; other sessions stay pinned
        #[arg(long)]
        switch: bool,
    },
    /// Approve a proposal (needs a TTY and confirmation)
    #[command(after_long_help = crate::help::DEPS_AFTER)]
    Approve {
        /// Proposal id
        id: String,
        /// Only these sessions, comma separated
        #[arg(long, value_delimiter = ',', value_name = "S,S")]
        sessions: Vec<String>,
    },
    /// List open proposals
    #[command(after_long_help = crate::help::DEPS_AFTER)]
    List {
        /// Only this repo
        #[arg(long)]
        repo: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCmd {
    /// launchctl bootstrap the per-user agent
    #[command(after_long_help = crate::help::DAEMON_AFTER)]
    Start,
    /// launchctl bootout the per-user agent
    #[command(after_long_help = crate::help::DAEMON_AFTER)]
    Stop,
    /// Whether the daemon runs, its locks and alerts
    #[command(after_long_help = crate::help::DAEMON_AFTER)]
    Status,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Accept a relaxed safety setting of a repo (needs a TTY and confirmation)
    #[command(after_long_help = crate::help::CONFIG_AFTER)]
    Relax {
        /// Repo name
        repo: String,
    },
}

/// The top-level command names, sorted.
pub fn top_commands() -> Vec<String> {
    use clap::CommandFactory;
    let mut v: Vec<String> = Cli::command().get_subcommands().map(|c| c.get_name().to_string()).collect();
    v.sort();
    v
}

/// The commands of a group (`deps`, `daemon`, `config`), sorted.
pub fn group_commands(group: &str) -> Option<Vec<String>> {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let g = cmd.get_subcommands().find(|c| c.get_name() == group)?;
    if !g.has_subcommands() {
        return None;
    }
    let mut v: Vec<String> = g.get_subcommands().map(|c| c.get_name().to_string()).collect();
    v.sort();
    Some(v)
}

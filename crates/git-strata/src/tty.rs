//! `require_tty` (cli.md §3) and the pinned safety flags it is gated on
//! (§4 invariant 1, D21). Nothing here reads an environment variable or a
//! flag; nothing here connects to the daemon.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::json;
use strata_daemon::PinnedFlags;
use strata_types::{Code, Config, RepoName, StrataError};

use crate::Result;

/// Whether stdin (fd 0) is a terminal.
pub fn stdin_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(0) == 1 }
}

/// Whether stderr (fd 2) is a terminal: human notices that duplicate
/// envelope data are printed only then.
pub fn stderr_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(2) == 1 }
}

/// Ok only when stdin is a terminal; otherwise `NotTty`. Runs before any
/// connection to the daemon.
pub fn require_tty() -> Result<()> {
    if stdin_is_tty() {
        Ok(())
    } else {
        Err(StrataError::new(Code::NotTty, "this command asks a human to confirm; run it in a terminal")
            .with_detail(json!({ "reason": "not_tty" })))
    }
}

/// The same, naming the command in the detail.
pub fn require_tty_for(command: &str) -> Result<()> {
    require_tty().map_err(|mut e| {
        e.detail = Some(json!({ "reason": "not_tty", "command": command }));
        e
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinsFile {
    repos: BTreeMap<RepoName, PinnedFlags>,
}

/// What pins.json says.
pub enum Pins {
    /// No file: the configuration's own values apply.
    Missing,
    Read(BTreeMap<RepoName, PinnedFlags>),
    /// Unreadable or unparsable: treated as "confirm and push" (D21).
    Unreadable,
}

impl Pins {
    pub fn load(path: &Path) -> Pins {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<PinsFile>(&bytes) {
                Ok(f) => Pins::Read(f.repos),
                Err(_) => Pins::Unreadable,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Pins::Missing,
            Err(_) => Pins::Unreadable,
        }
    }

    fn pinned(&self, repo: &RepoName) -> Option<Option<PinnedFlags>> {
        match self {
            Pins::Missing => Some(None),
            Pins::Read(m) => Some(m.get(repo).copied()),
            Pins::Unreadable => None,
        }
    }

    /// Whether a push of `repo` asks a human. Either the pin or the file
    /// asking is enough (the stricter of the two; the daemon tightens at
    /// once and relaxes only through `config relax`). An unreadable pins
    /// file, or an unknown repo / unreadable config, asks.
    pub fn needs_confirm(&self, config: Option<&Config>, repo: &RepoName) -> bool {
        let file = config.and_then(|c| c.repos.get(repo)).map(|rc| rc.needs_human_confirm());
        match (self.pinned(repo), file) {
            (None, _) => true,
            (_, None) => true,
            (Some(Some(p)), Some(f)) => p.confirm || f,
            (Some(None), Some(f)) => f,
        }
    }

    /// Whether a publish of `repo` pushes (pin or file; unknown pushes).
    pub fn pushes(&self, config: Option<&Config>, repo: &RepoName) -> bool {
        let file = config.and_then(|c| c.repos.get(repo)).map(|rc| rc.publish.push);
        match (self.pinned(repo), file) {
            (None, _) => true,
            (_, None) => true,
            (Some(Some(p)), Some(f)) => p.push || f,
            (Some(None), Some(f)) => f,
        }
    }
}

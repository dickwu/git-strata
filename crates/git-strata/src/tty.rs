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
    /// No file: every repo counts as unpinned (needs confirmation).
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
            Pins::Read(m) => Some(m.get(repo).cloned()),
            Pins::Unreadable => None,
        }
    }

    /// Whether a push of `repo` asks a human. Either the pin or the file
    /// asking is enough (the stricter of the two; the daemon tightens at
    /// once and relaxes only through `config relax`). An unreadable or
    /// missing pins file, a repo missing from it (new or renamed), or an
    /// unknown repo / unreadable config, asks: the daemon pins such a repo
    /// with `confirm = true` until `config relax`.
    pub fn needs_confirm(&self, config: Option<&Config>, repo: &RepoName) -> bool {
        let file = config.and_then(|c| c.repos.get(repo)).map(|rc| rc.needs_human_confirm());
        match (self.pinned(repo), file) {
            (Some(Some(p)), Some(f)) => p.confirm || f,
            _ => true,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(confirm: bool) -> Config {
        Config::parse(&format!(
            "[paths]\nroot = \"/tmp/r\"\n\n[repo.app]\npath = \"/tmp/app\"\ndefault_branch = \"main\"\ndeps_dirs = []\ndeploy_on_push = false\n\n[repo.app.publish]\nbranch = \"main\"\npush = true\nconfirm = {confirm}\n"
        ))
        .unwrap()
    }

    /// review P0-1: no pins file, or a repo missing from it, asks.
    #[test]
    fn missing_pin_needs_confirmation() {
        let app = RepoName::try_from("app".to_string()).unwrap();
        let c = config(false);
        assert!(Pins::Missing.needs_confirm(Some(&c), &app));
        assert!(Pins::Read(BTreeMap::new()).needs_confirm(Some(&c), &app));
        assert!(Pins::Unreadable.needs_confirm(Some(&c), &app));
        let open = PinnedFlags { deploy_on_push: false, confirm: false, push: true, ..Default::default() };
        let pinned = Pins::Read([(app.clone(), open)].into_iter().collect());
        assert!(!pinned.needs_confirm(Some(&c), &app));
        assert!(pinned.needs_confirm(Some(&config(true)), &app));
    }
}

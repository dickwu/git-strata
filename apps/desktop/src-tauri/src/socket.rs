//! Where git-stratad listens. The desktop may not depend on strata-daemon
//! (gui.md §2.3), so this repeats `strata_daemon::config_dir_for_home` +
//! `socket_path`: `$HOME/Library/Application Support/git-strata/daemon.sock`,
//! the same path the cli derives from HOME. No other variable moves it.

use std::path::{Path, PathBuf};

pub fn socket_for_home(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("git-strata")
        .join("daemon.sock")
}

/// The socket for this process's HOME; `None` when HOME is unset or empty.
pub fn default_socket() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(socket_for_home(Path::new(&home)))
}

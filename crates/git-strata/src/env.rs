//! Where things are: `$HOME/Library/Application Support/git-strata`
//! holds config.toml, pins.json and daemon.sock (daemon.md §6). No
//! variable other than HOME moves them.

use std::path::PathBuf;

use serde_json::json;
use strata_types::{Code, Config, StrataError};

use crate::Result;

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub socket: PathBuf,
    pub pins: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Paths> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| StrataError::new(Code::InvalidInput, "HOME is not set").with_detail(json!({ "reason": "no_home" })))?;
        let config_dir = strata_daemon::config_dir_for_home(&home);
        Ok(Paths {
            config_file: config_dir.join("config.toml"),
            socket: strata_daemon::socket_path(&config_dir),
            pins: strata_daemon::pins_path(&config_dir),
            home,
            config_dir,
        })
    }

    /// Parse config.toml (the same parser the daemon uses).
    pub fn config(&self) -> Result<Config> {
        let text = match std::fs::read_to_string(&self.config_file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StrataError::new(Code::NoSuchPath, format!("no config at {}", self.config_file.display()))
                    .with_detail(json!({ "reason": "no_config", "path": self.config_file.to_string_lossy() })));
            }
            Err(e) => {
                return Err(StrataError::new(Code::InvalidInput, format!("cannot read {}: {e}", self.config_file.display()))
                    .with_detail(json!({ "reason": "config_unreadable", "path": self.config_file.to_string_lossy() })));
            }
        };
        Config::parse(&text)
    }
}

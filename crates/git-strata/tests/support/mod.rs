//! Shared helpers for the git-strata tests: run the real binary with a
//! throw-away HOME under /tmp (the socket path must fit sockaddr_un).
#![allow(dead_code)]

pub mod json;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_git-strata");

/// A HOME with a .gitconfig and the git-strata config dir.
pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Home {
        let dir = tempfile::Builder::new().prefix("gh-").tempdir_in("/tmp").expect("home");
        fs::write(
            dir.path().join(".gitconfig"),
            "[user]\n\tname = Strata Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join(".config")).unwrap();
        fs::create_dir_all(Self::cfg_of(dir.path())).unwrap();
        Home { dir }
    }

    fn cfg_of(home: &Path) -> PathBuf {
        home.join("Library/Application Support/git-strata")
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn config_dir(&self) -> PathBuf {
        Self::cfg_of(self.path())
    }

    pub fn socket(&self) -> PathBuf {
        self.config_dir().join("daemon.sock")
    }

    /// The binary with this HOME, a clean git environment, stdin /dev/null.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        for (k, _) in std::env::vars_os() {
            let up = k.to_string_lossy().to_ascii_uppercase();
            if up.starts_with("GIT_") || up.starts_with("STRATA") {
                c.env_remove(&k);
            }
        }
        c.args(args)
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(self.path())
            .stdin(Stdio::null());
        c
    }

    pub fn run(&self, args: &[&str]) -> Run {
        Run::from(self.cmd(args).output().expect("spawn git-strata"))
    }
}

impl Default for Home {
    fn default() -> Self {
        Self::new()
    }
}

/// One finished run: exit code, stdout, stderr, the envelope (serde view).
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

impl Run {
    /// The envelope, checked to be one compact line ending in `\n`.
    pub fn env(&self) -> serde_json::Value {
        assert!(self.stdout.ends_with('\n'), "no trailing newline: {self:?}");
        assert_eq!(self.stdout.matches('\n').count(), 1, "not one line: {self:?}");
        serde_json::from_str(self.stdout.trim_end_matches('\n')).unwrap_or_else(|e| panic!("not JSON ({e}): {self:?}"))
    }

    pub fn ok(&self) -> serde_json::Value {
        let v = self.env();
        assert_eq!(v["ok"], true, "expected success: {self:?}");
        assert_eq!(self.code, 0, "{self:?}");
        v["data"].clone()
    }

    pub fn err_code(&self) -> String {
        let v = self.env();
        assert_eq!(v["ok"], false, "expected failure: {self:?}");
        assert_eq!(self.code, 1, "{self:?}");
        v["error"]["code"].as_str().unwrap_or_default().to_string()
    }

    pub fn reason(&self) -> String {
        self.env()["error"]["detail"]["reason"].as_str().unwrap_or_default().to_string()
    }
}

/// Every name of types' `Code`, read from the enum.
pub fn code_names() -> Vec<String> {
    strata_types::Code::ALL.iter().map(|c| c.as_str().to_string()).collect()
}

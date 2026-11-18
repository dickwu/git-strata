//! Process arguments for the desktop updater (version-upgrade design §6.1).
//!
//! The runner clears
//! the inherited environment with `env_clear()` and sets only the variables
//! from `environment`, using the verified login home.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Output};

use serde_json::json;
use strata_types::Code;

use super::cli_link::{AdminCliLinkInstaller, AdminCliLinkOperation};
use super::setup::SetupExecutor;
use crate::error::GuiError;

const BUNDLED_DAEMON: &str = "/Applications/GIT STRATA.app/Contents/MacOS/git-stratad";
const PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

/// Source and destination pairs for the administrator-approved CLI links.
pub const ADMIN_CLI_LINKS: [(&str, &str); 2] = [
    (
        "/Applications/GIT STRATA.app/Contents/MacOS/git-strata",
        "/usr/local/bin/git-strata",
    ),
    (BUNDLED_DAEMON, "/usr/local/bin/git-stratad"),
];

pub enum Operation<'a> {
    ZipBackup { bundle: &'a Path, zip: &'a Path },
    UnzipBackup { zip: &'a Path, staging: &'a Path },
    VerifyCodesign { bundle: &'a Path },
    LintPlist { plist: &'a Path },
    DaemonVersion,
    PrintDaemonPlist { program: &'a Path },
    CheckDaemonConfig { config: &'a Path },
    UpdateSkills,
    AdminCliLink(AdminCliLinkOperation),
}

impl<'a> Operation<'a> {
    /// Program and argv passed directly to the process runner. Path operands
    /// come from absolute paths checked by the caller.
    pub fn argv(self) -> (&'static str, Vec<OsString>) {
        let flag = OsStr::new::<str>;
        let (program, args) = match self {
            Self::ZipBackup { bundle, zip } => (
                "/usr/bin/ditto",
                vec![
                    flag("-c"),
                    flag("-k"),
                    flag("--keepParent"),
                    bundle.as_os_str(),
                    zip.as_os_str(),
                ],
            ),
            Self::UnzipBackup { zip, staging } => (
                "/usr/bin/ditto",
                vec![flag("-x"), flag("-k"), zip.as_os_str(), staging.as_os_str()],
            ),
            Self::VerifyCodesign { bundle } => (
                "/usr/bin/codesign",
                vec![
                    flag("--verify"),
                    flag("--deep"),
                    flag("--strict"),
                    bundle.as_os_str(),
                ],
            ),
            Self::LintPlist { plist } => {
                ("/usr/bin/plutil", vec![flag("-lint"), plist.as_os_str()])
            }
            Self::DaemonVersion => (BUNDLED_DAEMON, vec![flag("--version")]),
            Self::PrintDaemonPlist { program } => (
                BUNDLED_DAEMON,
                vec![flag("--print-plist"), program.as_os_str()],
            ),
            Self::CheckDaemonConfig { config } => (
                BUNDLED_DAEMON,
                vec![flag("--check-config"), config.as_os_str()],
            ),
            Self::UpdateSkills => (
                "/opt/homebrew/bin/npx",
                vec![
                    flag("skills"),
                    flag("update"),
                    flag("git-strata"),
                    flag("-g"),
                ],
            ),
            Self::AdminCliLink(operation) => {
                let (program, args) = operation.argv();
                return (program, Vec::from(args));
            }
        };
        (program, args.into_iter().map(OsStr::to_os_string).collect())
    }
}

pub fn environment(login_home: &Path) -> [(&'static str, &OsStr); 2] {
    [("HOME", login_home.as_os_str()), ("PATH", OsStr::new(PATH))]
}

fn command(operation: Operation<'_>, login_home: &Path) -> Command {
    let (program, args) = operation.argv();
    let mut command = Command::new(program);
    command.args(args).env_clear().envs(environment(login_home));
    command
}

fn run(home: &Path, operation: Operation<'_>) -> Result<Output, GuiError> {
    let login = crate::service::scope::login_home();
    let home = crate::service::scope::lifecycle_scope(home, login.as_deref())?;
    command(operation, &home)
        .current_dir("/")
        .output()
        .map_err(|error| {
            GuiError::new(
                Code::Unknown,
                format!("Could not run the updater operation: {error}"),
                json!({"reason": "update_exec_spawn", "errno": error.raw_os_error()}),
            )
        })
}

pub(super) fn checked(home: &Path, operation: Operation<'_>) -> Result<Vec<u8>, GuiError> {
    let codesign = matches!(operation, Operation::VerifyCodesign { .. });
    let admin = matches!(operation, Operation::AdminCliLink(_));
    let output = run(home, operation)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (code, reason) = if admin && stderr.contains("(-128)") {
        (Code::NotConfirmed, "cli_link_cancelled")
    } else if codesign
        && output.status.code() == Some(1)
        && [
            "code object is not signed at all",
            "a sealed resource is missing or invalid",
            "invalid signature",
            "code or signature have been modified",
        ]
        .iter()
        .any(|text| stderr.contains(text))
    {
        (Code::CheckFailed, "bad_codesign")
    } else {
        (Code::Unknown, "update_exec_failed")
    };
    Err(GuiError::new(
        code,
        stderr.trim().chars().take(2048).collect::<String>(),
        json!({"reason": reason, "status": output.status.code()}),
    ))
}

pub(super) struct FixedRunner;

impl SetupExecutor for FixedRunner {
    fn run(&self, home: &Path, operation: Operation<'_>) -> Result<Vec<u8>, GuiError> {
        match operation {
            Operation::CheckDaemonConfig { .. } => {
                // The validator returns a structured JSON error on exit 1.
                let output = run(home, operation)?;
                Ok(output.stdout)
            }
            _ => checked(home, operation),
        }
    }
}

impl AdminCliLinkInstaller for FixedRunner {
    fn run(&self, home: &Path, operation: AdminCliLinkOperation) -> Result<Vec<u8>, GuiError> {
        checked(home, Operation::AdminCliLink(operation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_programs_and_argv_are_fixed() {
        let bundle = Path::new("/tmp/Test App.app");
        let zip = Path::new("/tmp/previous.zip");
        let staging = Path::new("/tmp/restore");
        let plist = Path::new("/tmp/daemon.plist");
        let program = Path::new("/tmp/legacy/bin/git-stratad");
        let config = Path::new("/tmp/config.toml");
        let daemon = "/Applications/GIT STRATA.app/Contents/MacOS/git-stratad";
        let operations = [
            Operation::ZipBackup { bundle, zip },
            Operation::UnzipBackup { zip, staging },
            Operation::VerifyCodesign { bundle },
            Operation::LintPlist { plist },
            Operation::DaemonVersion,
            Operation::PrintDaemonPlist { program },
            Operation::CheckDaemonConfig { config },
            Operation::UpdateSkills,
            Operation::AdminCliLink(AdminCliLinkOperation::fixture()),
        ];
        for operation in operations {
            let (expected_program, expected_args): (&str, &[&str]) = match &operation {
                Operation::ZipBackup { .. } => (
                    "/usr/bin/ditto",
                    &[
                        "-c",
                        "-k",
                        "--keepParent",
                        "/tmp/Test App.app",
                        "/tmp/previous.zip",
                    ],
                ),
                Operation::UnzipBackup { .. } => (
                    "/usr/bin/ditto",
                    &["-x", "-k", "/tmp/previous.zip", "/tmp/restore"],
                ),
                Operation::VerifyCodesign { .. } => (
                    "/usr/bin/codesign",
                    &["--verify", "--deep", "--strict", "/tmp/Test App.app"],
                ),
                Operation::LintPlist { .. } => ("/usr/bin/plutil", &["-lint", "/tmp/daemon.plist"]),
                Operation::DaemonVersion => (daemon, &["--version"]),
                Operation::PrintDaemonPlist { .. } => {
                    (daemon, &["--print-plist", "/tmp/legacy/bin/git-stratad"])
                }
                Operation::CheckDaemonConfig { .. } => {
                    (daemon, &["--check-config", "/tmp/config.toml"])
                }
                Operation::UpdateSkills => (
                    "/opt/homebrew/bin/npx",
                    &["skills", "update", "git-strata", "-g"],
                ),
                Operation::AdminCliLink(operation) => {
                    let (_, expected) = operation.argv();
                    let cmd = command(
                        Operation::AdminCliLink(AdminCliLinkOperation::fixture()),
                        Path::new("/Users/test"),
                    );
                    assert_eq!(cmd.get_program(), OsStr::new("/usr/bin/osascript"));
                    assert_eq!(
                        cmd.get_args().collect::<Vec<_>>(),
                        expected.iter().map(OsString::as_os_str).collect::<Vec<_>>()
                    );
                    continue;
                }
            };
            let home = Path::new("/Users/test");
            let actual = command(operation, home);
            assert_eq!(actual.get_program(), OsStr::new(expected_program));
            assert!(Path::new(actual.get_program()).is_absolute());
            assert_eq!(
                actual.get_args().collect::<Vec<_>>(),
                expected_args.iter().map(OsStr::new).collect::<Vec<_>>()
            );
            let actual_env: std::collections::BTreeMap<_, _> = actual.get_envs().collect();
            assert_eq!(
                actual_env,
                environment(home)
                    .into_iter()
                    .map(|(name, value)| (OsStr::new(name), Some(value)))
                    .collect()
            );
        }
    }

    #[test]
    fn updater_environment_and_admin_links_are_fixed() {
        assert_eq!(
            environment(Path::new("/Users/test")),
            [
                ("HOME", OsStr::new("/Users/test")),
                (
                    "PATH",
                    OsStr::new("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"),
                ),
            ]
        );
        assert_eq!(
            ADMIN_CLI_LINKS,
            [
                (
                    "/Applications/GIT STRATA.app/Contents/MacOS/git-strata",
                    "/usr/local/bin/git-strata",
                ),
                (
                    "/Applications/GIT STRATA.app/Contents/MacOS/git-stratad",
                    "/usr/local/bin/git-stratad",
                ),
            ]
        );
    }
}

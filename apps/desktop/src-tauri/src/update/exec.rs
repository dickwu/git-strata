//! Process arguments for the desktop updater (version-upgrade design §6.1).
//!
//! This scaffold describes commands without running them. The runner clears
//! the inherited environment with `env_clear()` and sets only the variables
//! from `environment`, using the verified login home.

use std::ffi::OsStr;
use std::path::Path;

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
}

impl<'a> Operation<'a> {
    /// Program and argv passed directly to the process runner. Path operands
    /// come from absolute paths checked by the caller.
    pub fn argv(self) -> (&'static str, Vec<&'a OsStr>) {
        let flag = OsStr::new::<str>;
        match self {
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
        }
    }
}

pub fn environment(login_home: &Path) -> [(&'static str, &OsStr); 2] {
    [("HOME", login_home.as_os_str()), ("PATH", OsStr::new(PATH))]
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
            };
            let (program, args) = operation.argv();
            assert_eq!(program, expected_program);
            assert!(Path::new(program).is_absolute());
            assert_eq!(
                args,
                expected_args.iter().map(OsStr::new).collect::<Vec<_>>()
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

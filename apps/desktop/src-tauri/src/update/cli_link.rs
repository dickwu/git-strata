//! Install the two bundle links and label CLI entries without executing them.

use std::ffi::{CString, OsString};
use std::fmt::Write;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::json;
use strata_types::Code;

use crate::error::GuiError;

use super::{exec::ADMIN_CLI_LINKS, setup::SetupPaths};

const BINARIES: [&str; 2] = ["git-strata", "git-stratad"];
static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The shared updater runner executes the fixed operation with a cleared
/// environment and verified login HOME. Integration binds it as managed state.
pub struct AdminCliLinks(pub Arc<dyn AdminCliLinkInstaller>);

pub trait AdminCliLinkInstaller: Send + Sync {
    fn run(&self, login_home: &Path, operation: AdminCliLinkOperation)
    -> Result<Vec<u8>, GuiError>;
}

pub struct AdminCliLinkOperation(());

impl AdminCliLinkOperation {
    #[cfg(test)]
    pub(super) fn fixture() -> Self {
        Self(())
    }

    pub fn argv(&self) -> (&'static str, [OsString; 3]) {
        // Paths come solely from the updater allowlist and are plain literals
        // in the privileged program. The AppleScript quotes the program as one
        // shell argument and never receives paths from the webview.
        let mut program = String::from("my $homebrew = '/opt/homebrew';\nmy @links = (\n");
        for (source, path) in ADMIN_CLI_LINKS {
            writeln!(program, "    ['{source}', '{path}'],").expect("writing to String");
        }
        program.push_str(");\n");
        program.push_str(ADMIN_INSTALLER);
        (
            "/usr/bin/osascript",
            ["-e".into(), ADMIN_PROMPT.into(), program.into()],
        )
    }
}

const ADMIN_PROMPT: &str = r#"on run argv
    set installer to "/usr/bin/perl -T -e " & quoted form of (item 1 of argv)
    return do shell script installer with administrator privileges
end run
"#;

// Runs only after macOS authorizes the fixed AppleScript. A directory writable
// by the login user is handled by Rust without elevation. Checking each parent
// before its child keeps privileged mutations in root-controlled directories.
const ADMIN_INSTALLER: &str = r#"
use strict;
use warnings;
use Cwd qw(abs_path);
use Errno qw(EEXIST ENOENT);
use Fcntl qw(S_ISDIR S_ISLNK);
use File::Basename qw(basename dirname);
use File::Temp qw(tempdir);

$ENV{PATH} = '/usr/bin:/bin';
$ENV{LC_ALL} = 'C';
delete @ENV{qw(IFS CDPATH ENV BASH_ENV)};
umask 0022;
my $bin = dirname($links[0][1]);
my $prefix = dirname($bin);
my @brew = lstat($homebrew);
die "Homebrew's prefix appeared; retry installation.\n" if @brew;
die "Could not inspect $homebrew: $!\n" unless $! == ENOENT;

sub acl_allows_changes {
    for my $ace (@_) {
        my ($action, $rights) = $ace =~ /^\s*\d+:\s+.+\s+(allow|deny)\s+([a-z_,]+)\s*$/;
        return 1 unless defined $action;
        next if $action eq 'deny';
        for my $right (split /,/, $rights) {
            # Write and unrecognized rights require the login-user path.
            return 1 unless $right =~ /\A(?:read|list|search|execute|readattr|readextattr|readsecurity|inherited|file_inherit|directory_inherit|limit_inherit|only_inherit)\z/;
        }
    }
    return 0;
}

for my $dir (dirname($prefix), $prefix, $bin) {
    my @entry = lstat($dir);
    if (!@entry && $! == ENOENT && $dir ne dirname($prefix)) {
        mkdir($dir, 0755) or $! == EEXIST or die "Could not create $dir: $!\n";
        @entry = lstat($dir);
    }
    die "$dir is not a directory.\n" unless @entry && S_ISDIR($entry[2]);
    open(my $listing, '-|', '/bin/ls', '-lde', $dir) or die "Could not inspect $dir: $!\n";
    my $header = <$listing>;
    my @acl = <$listing>;
    close($listing) && defined $header or die "Could not inspect $dir.\n";
    if ($entry[4] != $> || ($entry[2] & 0022) || acl_allows_changes(@acl)) {
        print "login-user\n";
        exit 0;
    }
}

sub normalized {
    my @parts;
    for my $part (split m{/+}, $_[0]) {
        next if $part eq '' || $part eq '.';
        if ($part eq '..') { pop @parts; }
        else { push @parts, $part; }
    }
    return '/' . join('/', @parts);
}

sub inspect {
    my ($entry, $path, $source) = @_;
    my @info = lstat($entry);
    if (!@info) {
        return [] if $! == ENOENT;
        die "Could not inspect $path: $!\n";
    }
    die "$path is already occupied.\n" unless S_ISLNK($info[2]);
    my $target = readlink($entry);
    die "Could not read $path: $!\n" unless defined $target;
    my $absolute = $target =~ m{^/} ? $target : "$bin/$target";
    my $normalized = normalized($absolute);
    for my $brew ($prefix, $homebrew) {
        die "$path is a Homebrew formula link.\n"
            if $normalized =~ m{^\Q$brew\E/(?:Cellar|opt)/[^/]+(?:/|$)};
    }
    if (stat($absolute)) {
        my $real = abs_path($absolute);
        my $bundle = abs_path($source);
        return undef if defined($real) && defined($bundle) && $real eq $bundle;
        die "$path points to another installation.\n";
    }
    die "Could not inspect the target of $path: $!\n" unless $! == ENOENT;
    return [$info[0], $info[1], $target];
}

my @changes;
for my $link (@links) {
    my ($source, $path) = @$link;
    die "Missing bundled command $source.\n" unless -f $source;
    my $entry = inspect($path, $path, $source);
    push @changes, [$source, $path, $entry] if defined $entry;
}

for my $change (@changes) {
    my ($source, $path, $expected) = @$change;
    if (!@$expected) {
        symlink($source, $path) or die "Could not create $path: $!\n";
        next;
    }
    my $holding = tempdir('.git-strata-link-XXXXXX', DIR => $bin, CLEANUP => 0);
    my $claimed = "$holding/" . basename($path);
    if (!rename($path, $claimed)) {
        my $error = $!;
        rmdir($holding);
        die "Could not claim $path: $error\n";
    }
    my $installed = eval {
        my $actual = inspect($claimed, $path, $source);
        die "$path changed during installation.\n" unless defined($actual) && @$actual == 3
            && $actual->[0] == $expected->[0] && $actual->[1] == $expected->[1]
            && $actual->[2] eq $expected->[2];
        symlink($source, $path) or die "Could not create $path: $!\n";
        1;
    };
    my $error = $@;
    if (!$installed) {
        # The checked bin directory and fixed basename determine the exact
        # destination. Hard-link the entry itself without replacing anything.
        system('/bin/ln', '-hP', $claimed, $bin) == 0
            or die "${error}The original entry is preserved at $claimed.\n";
    }
    unlink($claimed) or die "Could not clear $claimed: $!\n";
    rmdir($holding) or die "Could not clear $holding: $!\n";
    die $error unless $installed;
}
print "installed\n";
"#;

enum AdminResult {
    Installed,
    LoginUser,
}

impl AdminResult {
    fn from_output(output: &[u8]) -> Result<Self, GuiError> {
        match String::from_utf8_lossy(output).trim() {
            "installed" => Ok(Self::Installed),
            "login-user" => Ok(Self::LoginUser),
            _ => Err(GuiError::internal(
                "Unexpected administrator link installer reply.",
            )),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CliLabel {
    pub path: PathBuf,
    pub label: String,
}

pub struct LinkPaths {
    pub bundle: PathBuf,
    pub homebrew: PathBuf,
    pub local: PathBuf,
}

impl LinkPaths {
    pub fn installed(bundle: PathBuf) -> Self {
        Self {
            bundle,
            homebrew: "/opt/homebrew".into(),
            local: "/usr/local".into(),
        }
    }

    fn source(&self, name: &str) -> PathBuf {
        self.bundle.join("Contents/MacOS").join(name)
    }

    /// §2.1 displays only the CLI, in default PATH order.
    pub fn labels(&self) -> Result<Vec<CliLabel>, GuiError> {
        let name = BINARIES[0];
        let mut labels = Vec::new();
        for prefix in [&self.homebrew, &self.local] {
            let path = prefix.join("bin").join(name);
            let label = match self.inspect(&path, &self.source(name))? {
                Entry::Absent => continue,
                Entry::App => "this app".into(),
                Entry::Formula(Some(version)) => format!("Homebrew formula {version}"),
                Entry::Formula(None) => "Homebrew formula".into(),
                Entry::Dangling { .. } | Entry::Other => "other".into(),
            };
            labels.push(CliLabel { path, label });
        }
        Ok(labels)
    }

    #[cfg(test)]
    pub(in crate::update) fn install_without_elevation(&self) -> Result<(), GuiError> {
        self.install(|| panic!("fake installation should use the user-writable prefix"))
    }

    fn install(
        &self,
        administrator: impl FnOnce() -> Result<AdminResult, GuiError>,
    ) -> Result<(), GuiError> {
        // A missing Homebrew prefix, as opposed to a broken or inaccessible
        // one, selects the administrator path.
        let (prefix, admin) = match fs::symlink_metadata(&self.homebrew) {
            Ok(_) => (&self.homebrew, false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (&self.local, true),
            Err(e) => return Err(io_error(&self.homebrew, e)),
        };
        let bin = prefix.join("bin");
        if !admin {
            require_kind(&bin, |meta| meta.is_dir())?;
        }
        let changes = self.plan(&bin)?;
        if changes.is_empty() {
            return Ok(());
        }
        if admin {
            if matches!(administrator()?, AdminResult::LoginUser) {
                require_directory(&self.local)?;
                match fs::create_dir(&bin) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(io_error(&bin, e)),
                }
                require_directory(&bin)?;
                self.write_links(self.plan(&bin)?)?;
            }
        } else {
            self.write_links(changes)?;
        }
        for name in BINARIES {
            let path = bin.join(name);
            if self.inspect(&path, &self.source(name))? != Entry::App {
                return Err(GuiError::new(
                    Code::Unknown,
                    format!(
                        "The bundle link at {} could not be verified.",
                        path.display()
                    ),
                    json!({ "reason": "cli_link_unverified", "path": path }),
                ));
            }
        }
        Ok(())
    }

    fn plan(&self, bin: &Path) -> Result<Vec<LinkChange>, GuiError> {
        let mut changes = Vec::new();
        for name in BINARIES {
            let source = self.source(name);
            require_kind(&source, |meta| meta.is_file())?;
            let path = bin.join(name);
            match self.inspect(&path, &source)? {
                Entry::App => {}
                Entry::Formula(_) => return Err(occupied(&path, "Homebrew formula")),
                Entry::Other => return Err(occupied(&path, "another installation")),
                expected => changes.push(LinkChange {
                    source,
                    path,
                    expected,
                }),
            }
        }
        Ok(changes)
    }

    fn write_links(&self, changes: Vec<LinkChange>) -> Result<(), GuiError> {
        for LinkChange {
            source,
            path,
            expected,
        } in changes
        {
            if matches!(expected, Entry::Dangling { .. }) {
                self.replace_dangling(&path, &source, &expected)?;
            } else {
                // The kernel refuses any entry created since preflight.
                symlink(source, &path).map_err(|e| io_error(&path, e))?;
            }
        }
        Ok(())
    }

    fn replace_dangling(
        &self,
        path: &Path,
        source: &Path,
        expected: &Entry,
    ) -> Result<(), GuiError> {
        let dir = path
            .parent()
            .ok_or_else(|| GuiError::internal("command link has no parent"))?;
        let holding = claim_directory(dir).map_err(|e| io_error(dir, e))?;
        let claimed = holding.join("entry");
        let result = self.replace_claimed(path, source, expected, &claimed);
        // A failed restoration may leave a foreign entry here for recovery.
        let _ = fs::remove_dir(&holding);
        result
    }

    fn replace_claimed(
        &self,
        path: &Path,
        source: &Path,
        expected: &Entry,
        claimed: &Path,
    ) -> Result<(), GuiError> {
        fs::rename(path, claimed).map_err(|e| io_error(path, e))?;
        let result = self.inspect_at(claimed, path, source).and_then(|actual| {
            if &actual != expected {
                return Err(io_error(
                    path,
                    io::Error::new(io::ErrorKind::AlreadyExists, "the destination changed"),
                ));
            }
            symlink(source, path).map_err(|e| io_error(path, e))
        });
        match result {
            Ok(()) => fs::remove_file(claimed).map_err(|e| io_error(claimed, e)),
            Err(error) => {
                restore_claim(claimed, path)?;
                Err(error)
            }
        }
    }

    fn inspect(&self, path: &Path, source: &Path) -> Result<Entry, GuiError> {
        self.inspect_at(path, path, source)
    }

    fn inspect_at(&self, entry: &Path, path: &Path, source: &Path) -> Result<Entry, GuiError> {
        let link = match fs::symlink_metadata(entry) {
            Ok(link) if link.is_symlink() => link,
            Ok(_) => return Ok(Entry::Other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Entry::Absent),
            Err(e) => return Err(io_error(path, e)),
        };
        let target = fs::read_link(entry).map_err(|e| io_error(path, e))?;
        // A relative link is still interpreted at its original location.
        let absolute = path.parent().unwrap_or(Path::new("/")).join(&target);
        let normalized = normalize(&absolute);
        // Follow symlinks for existing targets so an opt/ alias points to a
        // versioned keg. Also recognize raw formula paths for missing kegs.
        match fs::canonicalize(&absolute) {
            Ok(real) => {
                if let Some(entry) = self.formula(&real).or_else(|| self.formula(&normalized)) {
                    return Ok(entry);
                }
                Ok(if fs::canonicalize(source).ok().as_ref() == Some(&real) {
                    Entry::App
                } else {
                    Entry::Other
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(self.formula(&normalized).unwrap_or(Entry::Dangling {
                    target,
                    device: link.dev(),
                    inode: link.ino(),
                }))
            }
            Err(e) => Err(io_error(path, e)),
        }
    }

    fn formula(&self, target: &Path) -> Option<Entry> {
        for prefix in [&self.homebrew, &self.local] {
            let Ok(relative) = target.strip_prefix(prefix) else {
                continue;
            };
            let mut parts = relative.iter();
            match (parts.next(), parts.next()) {
                (Some(layout), Some(_)) if layout == "Cellar" => {
                    let version = parts.next().and_then(|part| part.to_str());
                    return Some(Entry::Formula(version.map(str::to_owned)));
                }
                (Some(layout), Some(_)) if layout == "opt" => return Some(Entry::Formula(None)),
                _ => {}
            }
        }
        None
    }
}

struct LinkChange {
    source: PathBuf,
    path: PathBuf,
    expected: Entry,
}

fn claim_directory(parent: &Path) -> io::Result<PathBuf> {
    let template = parent.join(".git-strata-link-XXXXXX");
    let mut template = c_path(&template)?.into_bytes_with_nul();
    // SAFETY: the mutable NUL-terminated template has six trailing Xs and
    // mkdtemp keeps the result in this buffer, creating a private directory.
    let created = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    if created.is_null() {
        return Err(io::Error::last_os_error());
    }
    template.pop();
    Ok(std::ffi::OsString::from_vec(template).into())
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

fn restore_claim(claimed: &Path, destination: &Path) -> Result<(), GuiError> {
    let from = c_path(claimed).map_err(|e| io_error(claimed, e))?;
    let to = c_path(destination).map_err(|e| io_error(destination, e))?;
    // SAFETY: the paths are NUL-terminated and valid for the call. EXCL
    // preserves any destination that appeared while the entry was held.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        return Ok(());
    }
    Err(GuiError::new(
        Code::Unknown,
        format!(
            "Could not restore {}. The original entry is preserved at {}: {}",
            destination.display(),
            claimed.display(),
            io::Error::last_os_error(),
        ),
        json!({ "reason": "cli_link_restore", "path": destination, "preserved_at": claimed }),
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Absent,
    App,
    Formula(Option<String>),
    Dangling {
        target: PathBuf,
        device: u64,
        inode: u64,
    },
    Other,
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn require_kind(path: &Path, accept: impl FnOnce(fs::Metadata) -> bool) -> Result<(), GuiError> {
    let metadata = fs::metadata(path).map_err(|e| io_error(path, e))?;
    if accept(metadata) {
        Ok(())
    } else {
        Err(io_error(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "unexpected file type"),
        ))
    }
}

fn require_directory(path: &Path) -> Result<(), GuiError> {
    if fs::symlink_metadata(path)
        .map_err(|e| io_error(path, e))?
        .is_dir()
    {
        return Ok(());
    }
    Err(io_error(
        path,
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a directory without symlinks",
        ),
    ))
}

fn io_error(path: &Path, e: io::Error) -> GuiError {
    GuiError::new(
        Code::InvalidInput,
        format!("{}: {e}", path.display()),
        json!({ "reason": "cli_link_file", "path": path }),
    )
}

fn occupied(path: &Path, owner: &str) -> GuiError {
    GuiError::new(
        Code::Exists,
        format!(
            "{} belongs to {owner}. Both command links are left as they are.",
            path.display()
        ),
        json!({ "reason": "cli_link_occupied", "path": path }),
    )
}

pub(crate) async fn install(window: &tauri::WebviewWindow) -> Result<(), GuiError> {
    use tauri::Manager;

    let _install = INSTALL.try_lock().map_err(|_| {
        GuiError::new(
            Code::Busy,
            "Link installation is already in progress.",
            json!({ "reason": "cli_link_in_progress" }),
        )
    })?;
    let paths = SetupPaths::installed()?;
    let admin = window
        .try_state::<AdminCliLinks>()
        .map(|state| state.inner().0.clone());
    tokio::task::spawn_blocking(move || {
        LinkPaths::installed(paths.bundle).install(|| {
            let output = admin
                .ok_or_else(|| {
                    GuiError::new(
                        Code::Unsupported,
                        "This build cannot request administrator approval for the command links.",
                        json!({ "reason": "cli_link_admin_unavailable" }),
                    )
                })?
                .run(&paths.home, AdminCliLinkOperation(()))?;
            AdminResult::from_output(&output)
        })
    })
    .await
    .map_err(|e| GuiError::internal(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> LinkPaths {
        let paths = LinkPaths {
            bundle: root.join("GIT STRATA.app"),
            homebrew: root.join("opt/homebrew"),
            local: root.join("usr/local"),
        };
        fs::create_dir_all(paths.homebrew.join("bin")).unwrap();
        for name in BINARIES {
            let source = paths.source(name);
            fs::create_dir_all(source.parent().unwrap()).unwrap();
            fs::write(source, "fake command").unwrap();
        }
        paths
    }

    #[test]
    fn creates_absent_links_and_replaces_dangling_links_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let bin = paths.homebrew.join("bin");
        symlink("../missing", bin.join("git-strata")).unwrap();
        for _ in 0..2 {
            paths.install(|| panic!("unexpected admin prompt")).unwrap();
            for name in BINARIES {
                assert_eq!(fs::read_link(bin.join(name)).unwrap(), paths.source(name));
            }
        }
        assert_eq!(
            paths.labels().unwrap(),
            [CliLabel {
                path: bin.join("git-strata"),
                label: "this app".into()
            }]
        );
    }

    #[test]
    fn labels_relative_formula_link_and_preserves_it_even_when_dangling() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let bin = paths.homebrew.join("bin");
        let keg = paths.homebrew.join("Cellar/git-strata/0.2.3/bin");
        fs::create_dir_all(&keg).unwrap();
        fs::write(keg.join("git-strata"), "formula command").unwrap();
        let link = bin.join("git-strata");
        let target = Path::new("../Cellar/git-strata/0.2.3/bin/git-strata");
        symlink(target, &link).unwrap();
        for missing in [false, true] {
            if missing {
                fs::remove_file(keg.join("git-strata")).unwrap();
            }
            assert_eq!(paths.labels().unwrap()[0].label, "Homebrew formula 0.2.3");
            assert_eq!(
                paths
                    .install(|| panic!("unexpected admin prompt"))
                    .unwrap_err()
                    .reason(),
                Some("cli_link_occupied")
            );
            assert_eq!(fs::read_link(&link).unwrap(), target);
            assert!(!bin.join("git-stratad").exists());
        }
    }

    #[test]
    fn protects_dangling_versioned_formula_links_under_both_prefixes() {
        for apple_silicon in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let paths = fixture(temp.path());
            let prefix = if apple_silicon {
                &paths.homebrew
            } else {
                fs::remove_dir_all(&paths.homebrew).unwrap();
                &paths.local
            };
            let bin = prefix.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let link = bin.join("git-strata");
            for (target, label) in [
                (
                    "../Cellar/git-strata@0.2/0.2.4/bin/git-strata",
                    "Homebrew formula 0.2.4",
                ),
                ("../opt/git-strata@0.2/bin/git-strata", "Homebrew formula"),
            ] {
                symlink(target, &link).unwrap();
                assert_eq!(paths.labels().unwrap()[0].label, label);
                assert_eq!(
                    paths
                        .install(|| panic!("an occupied path does not need an admin prompt"))
                        .unwrap_err()
                        .reason(),
                    Some("cli_link_occupied")
                );
                assert_eq!(fs::read_link(&link).unwrap(), Path::new(target));
                assert!(fs::symlink_metadata(bin.join("git-stratad")).is_err());
                fs::remove_file(&link).unwrap();
            }
        }
    }

    #[test]
    fn refuses_an_existing_regular_file_or_foreign_live_link() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let bin = paths.homebrew.join("bin");
        let foreign = bin.join("git-strata");
        fs::write(&foreign, "another CLI").unwrap();
        let daemon = bin.join("git-stratad");
        symlink(&foreign, &daemon).unwrap();
        assert_eq!(paths.labels().unwrap()[0].label, "other");
        assert_eq!(
            paths
                .install(|| panic!("unexpected admin prompt"))
                .unwrap_err()
                .reason(),
            Some("cli_link_occupied")
        );
        fs::rename(&foreign, bin.join("alternate")).unwrap();
        symlink("alternate", &foreign).unwrap();
        assert_eq!(paths.labels().unwrap()[0].label, "other");
        assert_eq!(
            paths
                .install(|| panic!("unexpected admin prompt"))
                .unwrap_err()
                .reason(),
            Some("cli_link_occupied")
        );
        assert_eq!(fs::read_link(&daemon).unwrap(), foreign);
    }

    #[test]
    fn preserves_file_or_formula_link_created_after_dangling_link_preflight() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let bin = paths.homebrew.join("bin");
        let destination = bin.join("git-strata");
        let competitor = bin.join("competitor");
        for formula in [false, true] {
            symlink("../missing", &destination).unwrap();
            let changes = paths.plan(&bin).unwrap();
            if formula {
                symlink("../Cellar/git-strata/0.3.0/bin/git-strata", &competitor).unwrap();
            } else {
                fs::write(&competitor, "another CLI").unwrap();
            }
            let inode = fs::symlink_metadata(&competitor).unwrap().ino();
            fs::rename(&competitor, &destination).unwrap();
            assert!(paths.write_links(changes).is_err());
            assert_eq!(fs::symlink_metadata(&destination).unwrap().ino(), inode);
            if formula {
                assert_eq!(
                    fs::read_link(&destination).unwrap(),
                    Path::new("../Cellar/git-strata/0.3.0/bin/git-strata"),
                );
            } else {
                assert_eq!(fs::read_to_string(&destination).unwrap(), "another CLI");
            }
            assert_eq!(fs::read_dir(&bin).unwrap().count(), 1);
            fs::remove_file(&destination).unwrap();
        }
    }

    #[test]
    fn claim_restoration_keeps_both_entries_when_the_destination_is_reoccupied() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("git-strata");
        let holding = claim_directory(temp.path()).unwrap();
        let claimed = holding.join("entry");
        fs::write(&path, "new entry").unwrap();
        symlink("family/old-entry", &claimed).unwrap();
        let error = restore_claim(&claimed, &path).unwrap_err();
        assert_eq!(error.reason(), Some("cli_link_restore"));
        assert_eq!(error.detail["preserved_at"], json!(&claimed));
        assert_eq!(
            fs::read_link(claimed).unwrap(),
            Path::new("family/old-entry")
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "new entry");
    }

    #[test]
    fn occupied_second_destination_leaves_the_first_absent() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        let bin = paths.homebrew.join("bin");
        fs::write(bin.join("git-stratad"), "another daemon").unwrap();
        let e = paths
            .install(|| panic!("unexpected admin prompt"))
            .unwrap_err();
        assert_eq!(e.reason(), Some("cli_link_occupied"));
        assert!(fs::symlink_metadata(bin.join("git-strata")).is_err());
        assert_eq!(
            fs::read_to_string(bin.join("git-stratad")).unwrap(),
            "another daemon"
        );
    }

    #[test]
    fn absent_homebrew_prefix_uses_the_admin_runner_and_verifies_its_links() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        fs::remove_dir(paths.homebrew.join("bin")).unwrap();
        fs::remove_dir(&paths.homebrew).unwrap();
        let err = paths.install(|| Ok(AdminResult::Installed)).unwrap_err();
        assert_eq!(err.reason(), Some("cli_link_unverified"));
        fs::create_dir_all(&paths.local).unwrap();
        paths
            .install(|| AdminResult::from_output(b"login-user\n"))
            .unwrap();
        paths.install(|| panic!("already linked")).unwrap();
        assert_eq!(paths.labels().unwrap()[0].label, "this app");
    }

    #[test]
    fn login_user_fallback_checks_both_destinations_again_after_authorization() {
        let temp = tempfile::tempdir().unwrap();
        let paths = fixture(temp.path());
        fs::remove_dir_all(&paths.homebrew).unwrap();
        let bin = paths.local.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let second = bin.join("git-stratad");
        let error = paths
            .install(|| {
                fs::write(&second, "arrived during authorization").unwrap();
                Ok(AdminResult::LoginUser)
            })
            .unwrap_err();
        assert_eq!(error.reason(), Some("cli_link_occupied"));
        assert!(fs::symlink_metadata(bin.join("git-strata")).is_err());
        assert_eq!(
            fs::read_to_string(second).unwrap(),
            "arrived during authorization"
        );
    }

    #[test]
    fn admin_operation_has_a_fixed_prompt_and_link_pairs() {
        let (program, [flag, prompt, script]) = AdminCliLinkOperation(()).argv();
        assert_eq!(program, "/usr/bin/osascript");
        assert_eq!(flag, "-e");
        assert!(
            prompt
                .to_str()
                .unwrap()
                .contains("with administrator privileges")
        );
        let script = script.to_str().unwrap();
        for (source, dest) in ADMIN_CLI_LINKS {
            assert!(script.contains(&format!("['{source}', '{dest}']")));
        }
        assert!(AdminResult::from_output(b"").is_err());
    }
}

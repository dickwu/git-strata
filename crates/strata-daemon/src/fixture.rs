//! Startup boundary shared by in-process tests and the git-stratad-test process.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;
use strata_types::{Code, Config, RepoPath, StrataError};

use crate::Result;
use crate::startup::Options;

mod launchd;
pub use launchd::{FakeLaunchd, LaunchdCall};

/// A short, harness-owned directory directly beneath the system /tmp.
/// The independent root is never inferred from HOME, config.toml or the socket.
pub const PREFIX: &str = "gsfx-";

#[derive(Clone)]
pub struct WorkerFixture {
    root: PathBuf,
    home: PathBuf,
    config_dir: PathBuf,
}

fn refusal(field: &str, path: &Path) -> StrataError {
    StrataError::new(
        Code::InvalidInput,
        format!("unsafe test fixture {field}: {}", path.display()),
    )
    .with_detail(json!({"reason": "test_fixture", "field": field, "path": path}))
}

impl WorkerFixture {
    /// The caller owns the TempDir for the entire lifetime of its workers.
    pub fn new(root: &Path, home: &Path, config_dir: &Path) -> Result<Self> {
        let fixture = Self {
            root: root.to_owned(),
            home: home.to_owned(),
            config_dir: config_dir.to_owned(),
        };
        fixture.check_root()?;
        fixture.check_path("HOME", home)?;
        fixture.check_path("config_dir", config_dir)?;
        Ok(fixture)
    }

    fn check_root(&self) -> Result<()> {
        let short_tmp = fs::canonicalize("/tmp").map_err(|_| refusal("root", &self.root))?;
        if self.root.parent() != Some(short_tmp.as_path())
            || !self
                .root
                .file_name()
                .is_some_and(|n| n.as_encoded_bytes().starts_with(PREFIX.as_bytes()))
        {
            return Err(refusal("root", &self.root));
        }
        let meta = fs::symlink_metadata(&self.root).map_err(|_| refusal("root", &self.root))?;
        // SAFETY: geteuid has no preconditions.
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(refusal("root", &self.root));
        }
        Ok(())
    }

    /// Check even a missing leaf or a dangling link without following a link
    /// into another tree. Only normal components may follow the fixture root.
    fn check_path(&self, field: &str, path: &Path) -> Result<()> {
        let tail = path
            .strip_prefix(&self.root)
            .map_err(|_| refusal(field, path))?;
        if tail.as_os_str().is_empty() {
            return Ok(());
        }
        let rel = tail
            .to_str()
            .and_then(|s| RepoPath::try_from(s.to_owned()).ok())
            .ok_or_else(|| refusal(field, path))?;
        // attrs_of resolves parents with openat(O_NOFOLLOW) from the fixture fd
        // and inspects the final component with lstat semantics.
        match strata_apfs::attrs_of(&self.root, &rel) {
            Ok(a) if !matches!(a.kind, strata_apfs::FileKind::Symlink { .. }) => Ok(()),
            Err(e) if e.code == Code::NoSuchPath => Ok(()),
            _ => Err(refusal(field, path)),
        }
    }

    pub fn open_git(&self, repo: &Path) -> Result<strata_gitio::Git> {
        use std::ffi::OsStr;
        strata_gitio::Git::open_with_env(
            repo,
            &[
                ("HOME", Some(self.home.as_os_str())),
                (
                    "XDG_CONFIG_HOME",
                    Some(self.home.join(".config").as_os_str()),
                ),
                ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
                ("GIT_ALLOW_PROTOCOL", Some(OsStr::new("file"))),
            ],
        )
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// Set on dependency children (install, `--version`) after the daemon's
    /// sanitization, which strips every GIT_* and keeps the runner's HOME
    /// (CR r2 #5).
    pub fn child_env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        vec![
            ("HOME".into(), self.home.clone().into_os_string()),
            (
                "XDG_CONFIG_HOME".into(),
                self.home.join(".config").into_os_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_ALLOW_PROTOCOL".into(), "file".into()),
        ]
    }

    pub fn options(&self) -> Options {
        let mut opts = Options::for_home(&self.home);
        opts.config_dir = self.config_dir.clone();
        opts.config_file = self.config_dir.join("config.toml");
        opts.socket = crate::socket_path(&self.config_dir);
        opts.fixture = Some(self.clone());
        opts
    }

    /// Set the child's HOME without changing the test runner's environment.
    pub fn command(&self, binary: &Path) -> Command {
        let mut cmd = Command::new(binary);
        cmd.arg(&self.root)
            .arg(&self.config_dir)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .current_dir(&self.home)
            .stdin(Stdio::null());
        cmd
    }

    pub(crate) fn check_layout(opts: &Options) -> Result<&Self> {
        let fixture = opts
            .fixture
            .as_ref()
            .ok_or_else(|| refusal("fixture", &opts.config_dir))?;
        fixture.check_root()?;
        fixture.check_path("HOME", &fixture.home)?;
        fixture.check_path("git_config", &fixture.home.join(".gitconfig"))?;
        fixture.check_path("git_config", &fixture.home.join(".config/git/config"))?;
        if opts.config_dir != fixture.config_dir {
            return Err(refusal("config_dir", &opts.config_dir));
        }
        for (field, path) in [
            ("config_dir", &opts.config_dir),
            ("config_file", &opts.config_file),
            ("socket", &opts.socket),
            ("daemon_lock", &crate::lock_path(&opts.config_dir)),
            ("pins", &crate::pins_path(&opts.config_dir)),
        ] {
            fixture.check_path(field, path)?;
        }
        for name in [
            "audit.jsonl",
            "audit.jsonl.1",
            crate::audit::SESSIONS_AUDIT_FILE,
            "sessions-audit.jsonl.1",
            "store_state.json",
            "last_shutdown.json",
        ] {
            fixture.check_path("config_state", &opts.config_dir.join(name))?;
        }
        fixture.check_path(
            "pins_tmp",
            &opts
                .config_dir
                .join(format!(".pins.json.{}.tmp", std::process::id())),
        )?;
        Ok(fixture)
    }

    // Git worktrees legitimately use "../.." in commondir. Check every
    // component before reducing it, so a link cannot redirect a parent step.
    fn git_pointer(&self, parent: &Path, value: &str) -> Result<PathBuf> {
        let path = parent.join(value);
        let tail = path
            .strip_prefix(&self.root)
            .map_err(|_| refusal("git_dir", &path))?;
        let mut current = self.root.clone();
        for part in tail.components() {
            match part {
                std::path::Component::Normal(name) => {
                    current.push(name);
                    self.check_path("git_dir", &current)?;
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir if current != self.root => {
                    current.pop();
                }
                _ => return Err(refusal("git_dir", &path)),
            }
        }
        Ok(current)
    }

    fn check_git_dir(&self, checkout: &Path) -> Result<()> {
        let marker = checkout.join(".git");
        let git_dir = if marker.is_file() {
            let text = fs::read_to_string(&marker).map_err(|_| refusal("git_dir", &marker))?;
            let value = text
                .trim_end()
                .strip_prefix("gitdir: ")
                .ok_or_else(|| refusal("git_dir", &marker))?;
            self.git_pointer(checkout, value)?
        } else if marker.is_dir() {
            marker
        } else {
            checkout.to_owned()
        };
        self.check_path("git_dir", &git_dir)?;
        let pointer = git_dir.join("commondir");
        self.check_path("git_dir", &pointer)?;
        let common = if pointer.is_file() {
            let text = fs::read_to_string(&pointer).map_err(|_| refusal("git_dir", &pointer))?;
            self.git_pointer(&git_dir, text.trim_end())?
        } else {
            git_dir
        };
        self.check_path("ref_sweep", &common.join("refs/strata"))
    }

    pub(crate) fn parse_config(&self, text: &str) -> Result<Config> {
        let config = Config::parse_for_fixture(text, &self.home)?;
        let root = &config.paths.root;
        if !root.starts_with(&self.root) {
            // The cross-volume regression uses this one known name on the sealed
            // system volume. Compare its known parent only; arbitrary outside
            // paths are rejected lexically without probing them or their ancestors.
            if root == Path::new("/usr/bin/strata-test-data") {
                for rc in config.repos.values() {
                    self.check_path("checkout", &rc.path)?;
                    if matches!(
                        strata_apfs::same_volume(Path::new("/usr/bin"), &rc.path),
                        Ok(false)
                    ) {
                        return Err(StrataError::new(
                            Code::CrossVolume,
                            "fixture checkout and data root are on different volumes",
                        )
                        .with_detail(json!({"root":root,"checkout":rc.path})));
                    }
                }
            }
            return Err(refusal("data_root", root));
        }
        self.check_path("data_root", root)?;
        self.check_path("store_lock", &config.paths.store().join("LOCK"))?;
        for rel in [
            "store/tmp",
            "store/trash",
            "store/backups",
            "store/blobs",
            "store/store.db",
            "store/store.db-wal",
            "store/store.db-shm",
            "store/store.db-tshm",
            "store/store.db-journal",
            "store/clean-shutdown",
            "bases",
            "deps",
            "sessions/.repos",
            "search",
            "daemon/sync",
            "daemon/main-follow",
            "daemon/push",
            "daemon/du/cache.json",
            "daemon/du/sessions.jsonl",
        ] {
            self.check_path("startup_path", &root.join(rel))?;
        }
        self.check_path(
            "du_tmp",
            &root.join(format!("daemon/du/.cache.json.{}.tmp", std::process::id())),
        )?;
        // CR r2 #4: one level further. Store init accepts an existing blob
        // shard and startup reads every scene's push.json by pathname, so a
        // link at either depth could carry I/O out of the fixture. The parents
        // were checked above, so listing them follows no link.
        for (field, dir, record) in [
            ("blob_shard", root.join("store/blobs"), None),
            (
                "push_scene",
                root.join(crate::resolve::PUSH_SCENES),
                Some(crate::resolve::SCENE_FILE),
            ),
        ] {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                self.check_path(field, &path)?;
                if let Some(record) = record {
                    self.check_path(field, &path.join(record))?;
                }
            }
        }
        for (name, repo) in &config.repos {
            if !repo.path.starts_with(&self.root) {
                return Err(refusal("checkout", &repo.path));
            }
            // Preserve the ordinary alias diagnostic only for a fixture-owned target.
            if let Err(error) = self.check_path("checkout", &repo.path) {
                let parent = repo.path.parent().ok_or_else(|| error.clone())?;
                self.check_path("checkout", parent)?;
                let target = fs::read_link(&repo.path).map_err(|_| error.clone())?;
                let target = parent.join(target);
                self.check_path("checkout", &target)?;
                return Err(StrataError::new(
                    Code::InvalidInput,
                    format!("repo {name} is not its realpath"),
                )
                .with_detail(
                    json!({"reason":"not_realpath","repo":name,"path":repo.path,"realpath":target}),
                ));
            }
            crate::startup::check_realpath(name, &repo.path)?;
            self.check_path("git_dir", &repo.path.join(".git"))?;
            self.check_git_dir(&repo.path)?;
            for dir in [
                config.paths.deps().join(name.as_str()).join(".inputs"),
                config
                    .paths
                    .bases()
                    .join(name.as_str())
                    .join(strata_session::BASE_STAGING_DIR),
                root.join("search").join(name.as_str()),
            ] {
                self.check_path("startup_path", &dir)?;
            }
        }
        Ok(config)
    }
}

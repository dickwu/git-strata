//! `attempts`: adoptions and cold installs that are running, interrupted or
//! failed (§3, §4.2).

use std::os::fd::AsRawFd;

use strata_store::{AttemptKind, AttemptState, Store};
use strata_types::{Code, Paths, RepoName};

use crate::layout::rp;
use crate::{BuildAttempt, BuildKind, BuildState, Result};

/// Whether someone holds the build lock `<root>/deps/<repo>/.locks/<id>.lock`.
/// A missing or unopenable lock file counts as free.
fn lock_held(paths: &Paths, repo: &RepoName, set: &strata_types::DepsSetId) -> Result<bool> {
    let rel = rp(format!("{}/.locks/{}.lock", repo.as_str(), set.as_str()));
    let file = match strata_apfs::open_rooted_file(&paths.deps(), &rel) {
        Ok(f) => f,
        Err(e)
            if matches!(
                e.code,
                Code::NoSuchPath | Code::NotDirectory | Code::NotFile | Code::IsDirectory
            ) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    loop {
        // SAFETY: flock on a descriptor we own; released when `file` closes.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(false);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) => return Ok(true),
            _ => {
                return Err(strata_apfs::classify_io_error(
                    &err,
                    strata_apfs::IoSide::Read,
                    &paths.deps().join(rel.as_str()),
                ));
            }
        }
    }
}

/// Running, interrupted and failed attempts of `repo` (every repo when
/// `None`), in attempt order. Sealed and refused attempts, and attempts whose
/// staging directory is gone, are not reported.
pub fn attempts(
    store: &Store,
    paths: &Paths,
    repo: Option<&RepoName>,
) -> Result<Vec<BuildAttempt>> {
    let mut out = Vec::new();
    for a in store.deps_attempts(repo)? {
        if matches!(a.state, AttemptState::Running | AttemptState::Failed)
            && std::fs::symlink_metadata(&a.staging).is_err()
        {
            continue;
        }
        let state = match a.state {
            AttemptState::Sealed | AttemptState::Refused => continue,
            AttemptState::Failed => BuildState::Failed,
            AttemptState::Running => {
                if lock_held(paths, &a.repo, &a.set)? {
                    BuildState::Running
                } else {
                    BuildState::Interrupted
                }
            }
        };
        let kind = match a.kind {
            AttemptKind::Adopt => BuildKind::Adopt,
            AttemptKind::Build => BuildKind::Build,
        };
        out.push(BuildAttempt {
            id: a.id,
            repo: a.repo,
            set: a.set,
            kind,
            state,
            staging: a.staging,
            pid: a.pid,
            started: a.started,
            error: a.error,
        });
    }
    Ok(out)
}

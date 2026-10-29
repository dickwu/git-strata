//! rm-followups R1: the orphan sweep (session.md §5.6a). An orphan is a
//! top-level `sessions/<sid>` or `sessions/.repos/<sid>` directory whose
//! session is Removed or has no row; it goes to the store's trash under
//! the open-files guard. Rows in Creating, Active, Removing or Unknown are
//! never touched.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_apfs::{IoSide, classify_io_error};
use strata_store::Lifecycle;
use strata_types::{Code, SessionId, StrataError};

use crate::recover::Leftover;
use crate::rm_note::removed_leftovers;
use crate::sys::lstat;
use crate::{Result, Sessions};

/// One orphan moved to the trash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRetired {
    pub session: SessionId,
    pub path: PathBuf,
    pub trash: PathBuf,
}

/// What one sweep did; a failure is a per-item warning, never an error of
/// the enclosing operation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sweep {
    pub retired: Vec<OrphanRetired>,
    pub warnings: Vec<StrataError>,
}

/// A real directory (not a symlink) and its (dev, ino).
pub(crate) type Ident = (u64, u64);

/// A real directory (not a symlink) and its identity.
pub(crate) fn dir_identity(p: &Path) -> Result<Option<Ident>> {
    Ok(match lstat(p).map_err(|e| classify_io_error(&e, IoSide::Read, p))? {
        Some(m) if m.is_dir() && !m.file_type().is_symlink() => Some((m.dev(), m.ino())),
        _ => None,
    })
}

impl Sessions<'_> {
    /// The current orphans (no writes, no lock): `gc --dry-run`'s list.
    pub fn orphans(&self) -> Result<Vec<(SessionId, PathBuf)>> {
        Ok(removed_leftovers(self.store, &self.config.paths.root)?
            .into_iter()
            .filter_map(|l| match l {
                Leftover::RemovedSession { session, path } => Some((session, path)),
                _ => None,
            })
            .collect())
    }

    /// Retire every orphan, or only `only`'s two directories (the end of
    /// a successful `remove`). Takes `enroll` per item only; the caller
    /// holds no repo lock.
    pub fn sweep_orphans(&self, only: Option<&SessionId>) -> Sweep {
        let mut out = Sweep::default();
        let items = match only {
            Some(id) => {
                let root = self.config.paths.root.join("sessions");
                vec![(id.clone(), root.join(id.as_str())), (id.clone(), root.join(".repos").join(id.as_str()))]
            }
            None => match self.orphans() {
                Ok(v) => v,
                Err(e) => {
                    out.warnings.push(e);
                    return out;
                }
            },
        };
        for (session, path) in items {
            match self.sweep_one(&session, &path) {
                Ok(Some(trash)) => out.retired.push(OrphanRetired { session, path, trash }),
                Ok(None) => {}
                Err(e) => out.warnings.push(e),
            }
        }
        out
    }

    /// The per-item procedure: identity and open files without a lock,
    /// then under `enroll` the row and the identity again, then retire.
    fn sweep_one(&self, id: &SessionId, path: &Path) -> Result<Option<PathBuf>> {
        // 1.
        let Some(ident) = dir_identity(path)? else { return Ok(None) };
        let open = strata_deps::open_files(path)?;
        if !open.is_empty() {
            let mut pids: Vec<i32> = open.iter().map(|o| o.pid).collect();
            pids.sort_unstable();
            pids.dedup();
            return Err(StrataError::new(
                Code::OpenFiles,
                format!(
                    "orphan {} of session {id} kept: processes {} hold files in it",
                    path.display(),
                    pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
                ),
            )
            .with_detail(json!({ "reason": "orphan_open_files", "session": id.as_str(), "path": path, "open": open })));
        }
        self.seams.hit("orphan.enroll", None)?;
        // 2.
        let _enroll = self.enroll.lock().unwrap_or_else(|p| p.into_inner());
        // 3.
        let gone = match self.store.session(id)? {
            None => true,
            Some(r) => r.lifecycle == Lifecycle::Removed,
        };
        if !gone || dir_identity(path)? != Some(ident) {
            return Ok(None);
        }
        // 4.
        self.seams.hit("orphan.retire", None)?;
        match self.store.retire_dir(path) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.code == Code::NoSuchPath => Ok(None),
            Err(e) => Err(e),
        }
    }
}

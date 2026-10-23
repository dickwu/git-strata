//! §5.12 `project`: materialize an overlay's entries onto a clone of its base
//! inside the caller's staging tree. Every write is rooted at dst.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use serde_json::json;
use strata_apfs::{FileKind, FileMeta, RemoveKind, RenameMode};
use strata_store::Store;
use strata_types::{Code, EntryKind, OverlayEntry, RepoPath, StrataError};

use crate::Result;
use crate::overlay::{MODE_EXEC, MODE_LINK, Overlay, ancestors, entry_blob, entry_from, rp};

/// Prefix of project's private temporary names directly under dst.
const TEMP_PREFIX: &str = ".strata-project-";

fn depth(p: &str) -> usize {
    p.matches('/').count()
}

fn corrupt(dst: &Path, p: &str, why: &str) -> StrataError {
    let shown = dst.join(p);
    StrataError::new(
        Code::Corrupt,
        format!(
            "{}: {why}; dst is not a whole clone of the base",
            shown.display()
        ),
    )
    .with_detail(json!({ "path": shown.to_string_lossy(), "reason": why }))
}

/// 把 overlay 的条目落到 dst 上（§5.12）。
pub fn project(store: &Store, overlay: &Overlay, dst: &Path) -> Result<()> {
    // 1. Shape, existence, and dst under <store root>/sessions/.
    overlay.validate()?;
    let real = std::fs::canonicalize(dst)
        .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, dst))?;
    let sessions = std::fs::canonicalize(store.root())
        .unwrap_or_else(|_| store.root().to_path_buf())
        .join("sessions");
    if !real.starts_with(&sessions) || real == sessions {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!("{} is not under {}", dst.display(), sessions.display()),
        )
        .with_detail(json!({ "reason": "dst_outside_sessions", "path": dst.to_string_lossy() })));
    }
    let dst = real.as_path();

    // 2. Removals, deepest first: Deleted paths and rename sources.
    let mut gone: Vec<&RepoPath> = overlay
        .entries
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::Deleted => Some(&e.path),
            _ => entry_from(e),
        })
        .collect();
    gone.sort_by(|a, b| depth(b.as_str()).cmp(&depth(a.as_str())).then(a.cmp(b)));
    for p in &gone {
        match strata_apfs::remove_rooted(dst, p, RemoveKind::File) {
            Ok(()) => {}
            Err(e) if e.code == Code::NoSuchPath => {
                return Err(corrupt(dst, p.as_str(), "base file to remove is missing"));
            }
            Err(e) if e.code == Code::IsDirectory => {
                return Err(corrupt(
                    dst,
                    p.as_str(),
                    "base file to remove is a directory",
                ));
            }
            Err(e) => return Err(e),
        }
    }

    // 3. Directories emptied by step 2 with no live entry under them.
    let live: Vec<&OverlayEntry> = overlay
        .entries
        .iter()
        .filter(|e| !matches!(e.kind, EntryKind::Deleted))
        .collect();
    let needed: BTreeSet<&str> = live
        .iter()
        .flat_map(|e| ancestors(e.path.as_str()))
        .collect();
    let mut dirs: Vec<&str> = gone
        .iter()
        .flat_map(|p| ancestors(p.as_str()))
        .filter(|d| !needed.contains(d))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    dirs.sort_by(|a, b| depth(b).cmp(&depth(a)).then(a.cmp(b)));
    for d in dirs {
        match strata_apfs::remove_rooted(dst, &rp(d), RemoveKind::EmptyDir) {
            Ok(()) => {}
            // Still holds base files the overlay keeps: not emptied.
            Err(e) if e.code == Code::NotEmpty => {}
            Err(e) => return Err(e),
        }
    }

    // 4. Writes, shallowest first, each through a private temp name.
    let mut writes = live;
    writes.sort_by(|a, b| {
        depth(a.path.as_str())
            .cmp(&depth(b.path.as_str()))
            .then(a.path.cmp(&b.path))
    });
    let mut seq: u64 = 0;
    for e in writes {
        if let Some((parent, _)) = e.path.as_str().rsplit_once('/') {
            make_parent(dst, parent)?;
        }
        let tmp = stage(store, dst, e, &mut seq)?;
        match strata_apfs::rename_rooted(dst, &tmp, &e.path, RenameMode::Excl) {
            Ok(()) => {}
            Err(err) if err.code == Code::Exists => {
                strata_apfs::rename_rooted(dst, &tmp, &e.path, RenameMode::Swap)?;
                // tmp now holds what was at the path: the base's copy.
                let a = strata_apfs::attrs_of(dst, &tmp)?;
                if a.kind == FileKind::Dir {
                    return Err(corrupt(
                        dst,
                        e.path.as_str(),
                        "a directory is where a file goes",
                    ));
                }
                strata_apfs::remove_rooted(dst, &tmp, RemoveKind::File)?;
            }
            Err(err) => return Err(err),
        }
    }
    // 5. No F_FULLFSYNC: the caller syncs before its swap.
    Ok(())
}

/// Create the missing parents of an entry. A file in the way must have been
/// removed in step 2 (else `Corrupt`); a symlink in the way stays `NotDirectory`.
fn make_parent(dst: &Path, parent: &str) -> Result<()> {
    match strata_apfs::mkdir_rooted(dst, &rp(parent), 0o755, true) {
        Ok(()) => Ok(()),
        Err(err) if matches!(err.code, Code::NotDirectory | Code::Exists) => {
            for a in ancestors(parent).chain(std::iter::once(parent)) {
                match strata_apfs::attrs_of(dst, &rp(a)) {
                    Ok(at) if at.kind == FileKind::File => {
                        return Err(corrupt(dst, a, "a base file is where a directory goes"));
                    }
                    Ok(at) if at.kind == FileKind::Dir => continue,
                    _ => break,
                }
            }
            Err(err)
        }
        Err(err) => Err(err),
    }
}

/// Put the entry's content at a fresh `.strata-project-<n>` directly under dst.
fn stage(store: &Store, dst: &Path, e: &OverlayEntry, seq: &mut u64) -> Result<RepoPath> {
    let blob = entry_blob(e).expect("live entry has a blob");
    let target = if e.mode == MODE_LINK {
        Some(store.read_blob(blob, 1024)?)
    } else {
        None
    };
    loop {
        let tmp = rp(&format!("{TEMP_PREFIX}{seq}"));
        *seq += 1;
        let r = match &target {
            Some(t) => strata_apfs::symlink_rooted(dst, &tmp, OsStr::from_bytes(t)),
            None => store.get_blob_rooted(blob, dst, &tmp),
        };
        match r {
            Ok(()) => {}
            Err(err) if err.code == Code::Exists => continue,
            Err(err) => return Err(err),
        }
        if e.mode == MODE_EXEC {
            let f = strata_apfs::open_rooted_file(dst, &tmp)?;
            strata_apfs::set_file_meta(
                &f,
                &FileMeta {
                    clear_flags: false,
                    mode: Some(0o755),
                    mtime: None,
                },
            )?;
        }
        return Ok(tmp);
    }
}

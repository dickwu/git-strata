//! publish's install-input snapshot (deps-drift design r8 §2.2 step 4):
//! the staged install inputs copied into the deps area,
//! `<root>/deps/<repo>/.inputs/<row>/tree`, so a cold install outside the
//! publish locks reads exactly what the publish staged and nothing an
//! agent can reach. `.inputs` is not a set name: deps gc never sweeps it.

use std::path::{Path, PathBuf};

use serde_json::json;
use strata_apfs::{CloneKind, RenameMode};
use strata_types::{Paths, RepoName, RepoPath};

use crate::inputs::{Member, input_members};
use crate::layout::{rp, unknown};
use crate::seal::mkdir_parent;
use crate::{Ctx, Result, inputs_hash};

/// `<root>/deps/<repo>/.inputs/<row>`.
pub fn snapshot_dir(paths: &Paths, repo: &RepoName, row: u64) -> PathBuf {
    paths.deps().join(repo.as_str()).join(".inputs").join(row.to_string())
}

/// The snapshot's input tree (`build`'s `inputs_from`).
pub fn snapshot_tree(paths: &Paths, repo: &RepoName, row: u64) -> PathBuf {
    snapshot_dir(paths, repo, row).join("tree")
}

/// Copy the install-input members of `from` (a staged worktree) into a new
/// snapshot for publish row `row` and check that it hashes to `expect`.
/// Files are cloned into a flat slot first and renamed into place, both
/// ends rooted and never following a symlink (like `build::prepare`). Any
/// failure removes the snapshot before returning. Returns its directory.
pub fn snapshot_inputs(ctx: &Ctx, from: &Path, row: u64, expect: &str) -> Result<PathBuf> {
    let deps = ctx.paths.deps();
    let rel = rp(format!("{}/.inputs/{row}", ctx.repo.as_str()));
    strata_apfs::mkdir_rooted(&deps, &rp(format!("{}/.inputs", ctx.repo.as_str())), 0o755, true)?;
    strata_apfs::mkdir_rooted(&deps, &rel, 0o755, false)?;
    let snap = deps.join(rel.as_str());
    let r = (|| -> Result<()> {
        strata_apfs::mkdir_rooted(&snap, &rp(".slot"), 0o755, false)?;
        strata_apfs::mkdir_rooted(&snap, &rp("tree"), 0o755, false)?;
        for (k, (m, kind)) in input_members(ctx.cfg, ctx.git, from)?.into_iter().enumerate() {
            let dst = rp(format!("tree/{}", m.as_str()));
            mkdir_parent(&snap, &dst)?;
            match kind {
                Member::File => {
                    let slot = rp(format!(".slot/{k}"));
                    strata_apfs::clone_from_rooted(from, &m, &snap.join(slot.as_str()), CloneKind::File)?;
                    crate::seams::after_snapshot_clone(&snap, &dst);
                    strata_apfs::rename_rooted(&snap, &slot, &dst, RenameMode::Excl)?;
                }
                Member::Link(target) => strata_apfs::symlink_rooted(&snap, &dst, target.as_os_str())?,
            }
        }
        strata_apfs::remove_rooted(&snap, &rp(".slot"), strata_apfs::RemoveKind::EmptyDir)?;
        let got = inputs_hash(ctx.cfg, ctx.git, &snap.join("tree"))?;
        let got = crate::seams::snapshot_hash(got);
        if got != expect {
            return Err(unknown(
                "inputs_snapshot",
                "the install-input snapshot does not hash like the staged worktree",
                json!({ "expected": expect, "got": got, "snapshot": snap.to_string_lossy() }),
            ));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(snap),
        Err(mut e) => {
            if !remove_snapshot(ctx.paths, ctx.repo, row)
                && let Some(d) = e.detail.get_or_insert_with(|| json!({})).as_object_mut()
            {
                d.insert("snapshot_left".into(), json!(snap.to_string_lossy()));
            }
            Err(e)
        }
    }
}

/// Remove publish row `row`'s snapshot. `true` when nothing is left (an
/// absent snapshot included).
pub fn remove_snapshot(paths: &Paths, repo: &RepoName, row: u64) -> bool {
    let root = paths.deps().join(repo.as_str()).join(".inputs");
    if std::fs::symlink_metadata(&root).is_err() {
        return true;
    }
    remove_rooted_tree(&root, &rp(row.to_string()))
}

fn remove_rooted_tree(root: &Path, rel: &RepoPath) -> bool {
    match strata_apfs::remove_tree_rooted(root, rel) {
        Ok(r) => r.stuck_total == 0,
        Err(e) => e.code == strata_types::Code::NoSuchPath,
    }
}

/// Daemon startup (before serving): remove every `deps/*/.inputs/*` a
/// crashed publish left. Returns the snapshots that could not be removed.
pub fn clean_snapshots(paths: &Paths) -> Vec<PathBuf> {
    let mut left = Vec::new();
    let Ok(repos) = std::fs::read_dir(paths.deps()) else {
        return left;
    };
    for r in repos.flatten() {
        let root = r.path().join(".inputs");
        match std::fs::symlink_metadata(&root) {
            Ok(m) if m.is_dir() => {}
            _ => continue,
        }
        let Ok(rows) = std::fs::read_dir(&root) else { continue };
        for row in rows.flatten() {
            let name = row.file_name().to_string_lossy().into_owned();
            let Ok(rel) = RepoPath::try_from(name) else { continue };
            if !remove_rooted_tree(&root, &rel) {
                left.push(root.join(rel.as_str()));
            }
        }
    }
    left
}

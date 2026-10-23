//! §5.9 `build_manifest`: the identity of every file of an exported tree.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rayon::prelude::*;
use strata_apfs::{Attrs, FileKind, WalkOptions, open_rooted_file, walk_with};
use strata_types::{BaseManifest, Code, ManifestEntry, RepoConfig, RepoPath, StrataError, TreeOid};

use crate::Result;
use crate::context::{Excluded, Fold};
use crate::hash::{git_mode, hash_bytes, hash_file, is_special};

/// Walks `dir` (an exported tree) and returns its manifest. Never writes the Store.
pub fn build_manifest(dir: &Path, tree: &TreeOid, config: &RepoConfig) -> Result<BaseManifest> {
    let x = Excluded::new(config, Fold::of(dir));
    let keep = move |p: &Path, _: &Attrs| !x.covers(p.to_str().unwrap_or_default());
    let mut found: Vec<(RepoPath, Attrs)> = Vec::new();
    for item in walk_with(dir, &WalkOptions::default(), &keep) {
        let (p, a) = item?;
        if is_special(&a.kind) {
            return Err(StrataError::new(
                Code::Unsupported,
                format!(
                    "{}: a FIFO, socket or device file in a base tree",
                    p.as_str()
                ),
            )
            .with_detail(serde_json::json!({ "path": p.as_str() })));
        }
        if !matches!(a.kind, FileKind::Dir) {
            found.push((p, a));
        }
    }
    let entries = found
        .into_par_iter()
        .map(|(p, a)| {
            let mode = git_mode(&a).expect("file or symlink");
            let (blob, size) = match &a.kind {
                FileKind::Symlink { target } => {
                    let b = target.as_os_str().as_bytes();
                    (hash_bytes(b), b.len() as u64)
                }
                _ => hash_file(&open_rooted_file(dir, &p)?, &dir.join(p.as_str()))?,
            };
            Ok((p, ManifestEntry { blob, mode, size }))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(BaseManifest {
        tree: tree.clone(),
        entries,
    })
}

//! Print-only configuration preflight. All filesystem checks are read-only.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde_json::json;
use strata_apfs::{IoSide, classify_io_error, retry_eintr, same_volume, volume_caps};
use strata_types::{Code, Config, StrataError};

use crate::Result;
use crate::startup::{check_realpath, check_realpath_root};

pub fn check_config(path: &Path) -> Result<()> {
    let read_error = |e, path: &Path| classify_io_error(&e, IoSide::Read, path);
    let text = retry_eintr(|| fs::read_to_string(path)).map_err(|e| read_error(e, path))?;
    let config = Config::parse(&text)?;
    config.validate()?;

    let root = &config.paths.root;
    let ancestor = check_root_ancestor(root)?;
    let caps = volume_caps(ancestor)?;
    if caps.fs_type != "apfs" || caps.clone == Some(false) {
        return Err(StrataError::new(
            Code::Unsupported,
            format!(
                "store root {} needs a volume that supports APFS clones",
                root.display()
            ),
        )
        .with_detail(
            json!({ "reason": "volume_capabilities", "root": root, "fs_type": caps.fs_type }),
        ));
    }

    for (name, repo) in &config.repos {
        let checkout = &repo.path;
        check_realpath(name, checkout)?;
        check_directory(checkout)?;
        if !retry_eintr(|| same_volume(ancestor, checkout)).map_err(|e| read_error(e, checkout))? {
            return Err(StrataError::new(
                Code::CrossVolume,
                format!(
                    "store root {} is not on the same volume as checkout {}",
                    root.display(),
                    checkout.display()
                ),
            )
            .with_detail(json!({ "root": root, "checkout": checkout })));
        }
    }
    Ok(())
}

fn check_root_ancestor(root: &Path) -> Result<&Path> {
    let mut ancestor = root;
    loop {
        // The final component may be a dangling link. Following it would
        // skip over it and mistake the parent for a usable data root.
        let err = match retry_eintr(|| fs::symlink_metadata(ancestor)) {
            Ok(meta) if meta.is_symlink() => {
                return Err(StrataError::new(
                    Code::InvalidInput,
                    format!(
                        "paths.root {} passes through the symbolic link {}",
                        root.display(),
                        ancestor.display(),
                    ),
                )
                .with_detail(json!({ "reason": "not_realpath", "path": root, "link": ancestor })));
            }
            Ok(meta) => {
                check_realpath_root(ancestor)?;
                require_directory(ancestor, &meta)?;
                return Ok(ancestor);
            }
            Err(err) => err,
        };
        match ancestor.parent() {
            Some(parent)
                if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                ancestor = parent;
            }
            _ => return Err(classify_io_error(&err, IoSide::Read, ancestor)),
        }
    }
}

fn check_directory(path: &Path) -> Result<()> {
    let meta = retry_eintr(|| fs::metadata(path))
        .map_err(|e| classify_io_error(&e, IoSide::Read, path))?;
    require_directory(path, &meta)
}

fn require_directory(path: &Path, meta: &fs::Metadata) -> Result<()> {
    if !meta.is_dir() {
        return Err(StrataError::new(
            Code::NotDirectory,
            format!("{} is not a directory", path.display()),
        )
        .with_detail(json!({ "path": path })));
    }
    Ok(())
}

//! `stage(staging, paths)` (§5.9): the reverse of `export_tree`, through
//! `update-index` so clean filters and eol normalisation apply and nothing
//! outside the batch is ever added.

use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

use serde_json::json;
use strata_types::{Code, RepoPath, TreeOid};

use crate::command::{Opts, failed};
use crate::err::{as_unknown, errd, path_str};
use crate::{Git, Result, fsx};

impl Git {
    /// Stage exactly `paths` of `staging` into its private index and
    /// write the tree (§5.9).
    pub fn stage(&self, staging: &Path, paths: &[RepoPath]) -> Result<TreeOid> {
        // Step 1: refusals.
        let gd = self.staging_gitdir(staging)?;
        let batch: BTreeSet<&str> = paths.iter().map(|p| p.as_str()).collect();
        let (mut writes, mut deletes) = (Vec::new(), Vec::new());
        for p in batch {
            let full = staging.join(p);
            match fsx::lstat(&full)? {
                None => deletes.push(p),
                Some(m) => {
                    let ft = m.file_type();
                    if ft.is_dir() {
                        deletes.push(p);
                    } else if ft.is_file() || ft.is_symlink() {
                        writes.push(p);
                    } else {
                        let kind = if ft.is_fifo() {
                            "fifo"
                        } else if ft.is_socket() {
                            "socket"
                        } else {
                            "device"
                        };
                        return Err(errd(
                            Code::NotFile,
                            format!("{} is a {kind}, not a file", full.display()),
                            json!({ "path": path_str(&full) }),
                        ));
                    }
                }
            }
        }
        let idx = gd.join("index");
        let nul_list = |v: &[&str]| -> Vec<u8> {
            let mut b = Vec::new();
            for p in v {
                b.extend_from_slice(Path::new(p).as_os_str().as_bytes());
                b.push(0);
            }
            b
        };
        let run = |args: &[&str], input: Vec<u8>, what: &str| -> Result<Vec<u8>> {
            let mut cmd = self.tree_command(&gd, staging, Some(&idx));
            cmd.args(args);
            let out = self.run(cmd, Opts::default().stdin(input)).map_err(|e| as_unknown(e, what))?;
            if !out.ok() {
                return Err(failed(what, &out));
            }
            Ok(out.stdout)
        };
        // Step 2: deletions; step 3: writes (clean filters apply).
        if !deletes.is_empty() {
            run(&["update-index", "--force-remove", "-z", "--stdin"], nul_list(&deletes), "update-index --force-remove")?;
        }
        if !writes.is_empty() {
            run(&["update-index", "--add", "--replace", "-z", "--stdin"], nul_list(&writes), "update-index --add")?;
        }
        // Step 4: the tree.
        let out = run(&["write-tree"], Vec::new(), "write-tree")?;
        let s = String::from_utf8_lossy(&out).trim().to_string();
        TreeOid::try_from(s.clone())
            .map_err(|_| errd(Code::Unknown, format!("write-tree printed {s:?}"), json!({ "reason": "bad-oid" })))
    }
}

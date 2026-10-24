//! Additions for publish (publish.md §2 item 3, §4.6): one atomic ref
//! transaction over several `refs/strata/` refs, single-path tree reads,
//! and blob bytes by object id. Read-only except `ref_transaction`.

use std::collections::BTreeMap;

use serde_json::json;
use strata_types::{Code, CommitOid, RepoPath, TreeOid};

use crate::command::{Opts, failed};
use crate::err::{errd, lossy};
use crate::refs::{ZERO_OID, check_strata_ref};
use crate::tree::{Size, parse_ls_tree};
use crate::{Git, Result};

/// One ref of a [`Git::ref_transaction`]. `new = None` only verifies that
/// the ref is still at `old` (keeps it at its value inside the transaction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefUpdate {
    pub name: String,
    pub new: Option<CommitOid>,
    /// The value the ref must have now; `None` = absent.
    pub old: Option<CommitOid>,
}

impl RefUpdate {
    /// Compare-and-swap `name` from `old` to `new`.
    pub fn set(name: impl Into<String>, new: CommitOid, old: Option<CommitOid>) -> RefUpdate {
        RefUpdate { name: name.into(), new: Some(new), old }
    }
    /// Require `name` to be `old` without changing it.
    pub fn verify(name: impl Into<String>, old: Option<CommitOid>) -> RefUpdate {
        RefUpdate { name: name.into(), new: None, old }
    }
}

/// One entry of a tree at a path (`ls-tree -l`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeItem {
    /// git mode (`0o100644`, `0o100755`, `0o120000`, `0o040000`, `0o160000`).
    pub mode: u32,
    /// `blob`, `tree` or `commit`.
    pub kind: String,
    /// The object id (40 hex).
    pub oid: String,
    /// Blob size in bytes; 0 for non-blobs.
    pub size: u64,
}

/// A blob read out of a tree: its bytes and git mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeBlob {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl Git {
    /// Apply every update in one `update-ref --stdin` transaction: all or
    /// none. Every name must be under `refs/strata/`. A ref not at its
    /// `old` → `HeadMoved` with `{ref, expected, actual}`; nothing moved.
    pub fn ref_transaction(&self, updates: &[RefUpdate]) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut script = String::from("start\n");
        for u in updates {
            check_strata_ref(&u.name)?;
            let old = u.old.as_ref().map(|o| o.as_str()).unwrap_or(ZERO_OID);
            match &u.new {
                Some(n) => script.push_str(&format!("update {} {} {}\n", u.name, n.as_str(), old)),
                None => script.push_str(&format!("verify {} {}\n", u.name, old)),
            }
        }
        script.push_str("prepare\ncommit\n");
        let mut c = self.repo_command();
        c.args(["update-ref", "-m", "strata", "--stdin"]);
        let out = self.run(c, Opts::c().stdin(script.into_bytes()))?;
        if out.ok() {
            return Ok(());
        }
        for u in updates {
            let actual = self.read_ref(&u.name)?;
            if actual != u.old {
                return Err(errd(
                    Code::HeadMoved,
                    format!("{} moved", u.name),
                    json!({
                        "ref": u.name,
                        "expected": u.old.as_ref().map(|o| o.as_str()),
                        "actual": actual.as_ref().map(|o| o.as_str()),
                    }),
                ));
            }
        }
        Err(failed("update-ref --stdin", &out))
    }

    /// Every entry of `tree` at or below each of `paths`
    /// (`ls-tree -r -t -z -l --full-tree <tree> -- <paths>`), keyed by path.
    /// Directories appear as `tree` items. Read-only.
    pub fn tree_items(&self, tree: &TreeOid, paths: &[RepoPath]) -> Result<BTreeMap<String, TreeItem>> {
        let mut map = BTreeMap::new();
        if paths.is_empty() {
            return Ok(map);
        }
        let mut c = self.repo_command();
        c.env("GIT_LITERAL_PATHSPECS", "1");
        c.args(["ls-tree", "-r", "-t", "-z", "-l", "--full-tree", tree.as_str(), "--"]);
        for p in paths {
            c.arg(p.as_str());
        }
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(failed("ls-tree", &out));
        }
        for r in parse_ls_tree(&out.stdout)? {
            let size = match r.size {
                Size::Bytes(n) => n,
                _ => 0,
            };
            map.insert(lossy(&r.path), TreeItem { mode: r.mode, kind: r.kind, oid: r.oid, size });
        }
        Ok(map)
    }

    /// The bytes and mode of the file or symlink at `path` in `tree`;
    /// `None` when the tree has no entry there; `IsDirectory` when the
    /// path is a directory of the tree.
    pub fn blob_bytes(&self, tree: &TreeOid, path: &RepoPath) -> Result<Option<TreeBlob>> {
        let items = self.tree_items(tree, std::slice::from_ref(path))?;
        match items.get(path.as_str()) {
            None => Ok(None),
            Some(i) if i.kind == "blob" => Ok(Some(TreeBlob { bytes: self.object_bytes(&i.oid)?, mode: i.mode })),
            Some(_) => Err(errd(
                Code::IsDirectory,
                format!("{} is a directory of tree {}", path.as_str(), tree.as_str()),
                json!({ "path": path.as_str(), "tree": tree.as_str() }),
            )),
        }
    }

    /// The bytes of blob `oid` (`cat-file blob`).
    pub fn object_bytes(&self, oid: &str) -> Result<Vec<u8>> {
        if oid.len() != 40 || !oid.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(errd(Code::InvalidInput, format!("bad object id {oid:?}"), json!({ "oid": oid })));
        }
        let mut c = self.repo_command();
        c.args(["cat-file", "blob", oid]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(failed("cat-file", &out));
        }
        Ok(out.stdout)
    }
}

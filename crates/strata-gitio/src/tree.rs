//! Base trees: resolve, validate once per tree, path table (§5.4).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::json;
use strata_types::{Code, CommitOid, RepoPath, StrataError, TreeOid};

use crate::command::{Opts, failed};
use crate::config;
use crate::err::{errd, lossy};
use crate::fsx;
use crate::{Git, Result};

/// APFS's limit on one name (types.md §6.3).
pub(crate) const NAME_MAX: usize = 255;
/// Path tables kept (least recently used evicted).
pub(crate) const TABLES_KEPT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    Bytes(u64),
    Bad,
    Dash,
}

/// One structurally valid `ls-tree -r -z -l` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawEntry {
    pub mode: u32,
    pub kind: String,
    pub oid: String,
    pub size: Size,
    pub path: Vec<u8>,
}

/// One validated tree entry.
#[derive(Clone, Debug)]
pub(crate) struct TreeEntry {
    pub mode: u32,
    pub path: RepoPath,
}

/// A validated tree's path table.
#[derive(Debug)]
pub(crate) struct TreeTable {
    pub entries: Vec<TreeEntry>,
    pub icase: bool,
    files: HashSet<String>,
    dirs: HashSet<String>,
}

fn fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

impl TreeTable {
    fn key(&self, p: &str) -> String {
        if self.icase { fold(p) } else { p.to_string() }
    }
    /// A file or symlink entry (what the index holds after checkout).
    pub(crate) fn has_file(&self, p: &RepoPath) -> bool {
        self.files.contains(&self.key(p.as_str()))
    }
    /// An entry, or a directory above one.
    pub(crate) fn contains(&self, p: &RepoPath) -> bool {
        let k = self.key(p.as_str());
        self.files.contains(&k) || self.dirs.contains(&k)
    }
}

fn garbled(why: &str) -> StrataError {
    errd(Code::Unknown, format!("ls-tree output is malformed: {why}"), json!({ "reason": "ls-tree-parse" }))
}

/// Parse `ls-tree -r -z -l` output exactly; anything off → Unknown.
pub(crate) fn parse_ls_tree(out: &[u8]) -> Result<Vec<RawEntry>> {
    let mut v = Vec::new();
    if out.is_empty() {
        return Ok(v);
    }
    if out.last() != Some(&0) {
        return Err(garbled("missing trailing NUL"));
    }
    for rec in out[..out.len() - 1].split(|&b| b == 0) {
        let tab = rec.iter().position(|&b| b == b'\t').ok_or_else(|| garbled("record without a tab"))?;
        let (head, path) = (&rec[..tab], &rec[tab + 1..]);
        if path.is_empty() {
            return Err(garbled("empty path"));
        }
        let head = std::str::from_utf8(head).map_err(|_| garbled("header is not ASCII"))?;
        let fields: Vec<&str> = head.split(' ').filter(|f| !f.is_empty()).collect();
        if fields.len() != 4 {
            return Err(garbled("wrong field count"));
        }
        let m = fields[0];
        if m.len() != 6 || !m.bytes().all(|c| (b'0'..=b'7').contains(&c)) {
            return Err(garbled("bad mode"));
        }
        let mode = u32::from_str_radix(m, 8).map_err(|_| garbled("bad mode"))?;
        let kind = fields[1];
        if !matches!(kind, "blob" | "tree" | "commit" | "tag") {
            return Err(garbled("bad object type"));
        }
        let oid = fields[2];
        if oid.len() != 40 || !oid.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
            return Err(garbled("bad object id"));
        }
        let size = match fields[3] {
            "BAD" => Size::Bad,
            "-" => Size::Dash,
            s if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) => {
                Size::Bytes(s.parse().map_err(|_| garbled("size out of range"))?)
            }
            _ => return Err(garbled("bad size")),
        };
        v.push(RawEntry { mode, kind: kind.to_string(), oid: oid.to_string(), size, path: path.to_vec() });
    }
    Ok(v)
}

fn unsupported(path: &str, why: &str) -> StrataError {
    errd(Code::Unsupported, format!("{path}: {why}"), json!({ "path": path }))
}

fn collision(a: &str, b: &str) -> StrataError {
    errd(
        Code::Unsupported,
        format!("{a} and {b} collide on checkout"),
        json!({ "reason": "collision", "paths": [a, b], "path": a }),
    )
}

/// The semantic checks of §5.4 step 3 over parsed records.
pub(crate) fn build_table(raw: Vec<RawEntry>, icase: bool) -> Result<TreeTable> {
    let mut entries = Vec::with_capacity(raw.len());
    for r in raw {
        let shown = lossy(&r.path);
        match (r.mode, r.kind.as_str()) {
            (0o100644 | 0o100755 | 0o120000, "blob") => {}
            (0o160000, "commit") => return Err(unsupported(&shown, "gitlink (submodule) entries are not supported")),
            _ => return Err(unsupported(&shown, &format!("mode {:06o} {} is not supported", r.mode, r.kind))),
        }
        match r.size {
            Size::Bytes(_) => {}
            Size::Bad => {
                return Err(errd(
                    Code::NoSuchCommit,
                    format!("blob {} for {shown} is missing", r.oid),
                    json!({ "path": shown, "blob": r.oid }),
                ));
            }
            Size::Dash => return Err(garbled("blob without a size")),
        }
        let s = String::from_utf8(r.path).map_err(|_| {
            errd(Code::NotText, format!("tree path {shown} is not UTF-8"), json!({ "path": shown }))
        })?;
        let path = RepoPath::try_from(s.clone())?;
        for comp in s.split('/') {
            if comp.len() > NAME_MAX {
                return Err(errd(
                    Code::InvalidInput,
                    format!("a component of {s} is longer than {NAME_MAX} bytes"),
                    json!({ "path": s, "reason": "name-too-long" }),
                ));
            }
            if comp.eq_ignore_ascii_case(".git") {
                return Err(unsupported(&s, "a .git component"));
            }
        }
        entries.push(TreeEntry { mode: r.mode, path });
    }
    // Collisions: NFC-equal names (RepoPath is NFC), ASCII case when
    // icase, and a name that is both an entry and a directory.
    let key = |p: &str| if icase { fold(p) } else { p.to_string() };
    let mut files: HashMap<String, String> = HashMap::new();
    for e in &entries {
        let k = key(e.path.as_str());
        if let Some(prev) = files.insert(k, e.path.as_str().to_string()) {
            return Err(collision(&prev, e.path.as_str()));
        }
    }
    let mut dirs: HashMap<String, String> = HashMap::new();
    for e in &entries {
        let p = e.path.as_str();
        let mut end = 0;
        while let Some(i) = p[end..].find('/') {
            let d = &p[..end + i];
            let k = key(d);
            if let Some(f) = files.get(&k) {
                return Err(collision(f, p));
            }
            dirs.entry(k).or_insert_with(|| d.to_string());
            end += i + 1;
        }
    }
    Ok(TreeTable { entries, icase, files: files.into_keys().collect(), dirs: dirs.into_keys().collect() })
}

/// Errors that depend only on the tree's content, so they stay true.
fn permanent(e: &StrataError) -> bool {
    matches!(e.code, Code::Unsupported | Code::NotText | Code::InvalidInput)
}

impl Git {
    /// The root tree of `commit`, validated as a base (§5.4).
    pub fn tree_of(&self, commit: &CommitOid) -> Result<TreeOid> {
        let no_commit = || {
            errd(
                Code::NoSuchCommit,
                format!("commit {} is not in the repository", commit.as_str()),
                json!({ "commit": commit.as_str() }),
            )
        };
        let c = self.resolve_commit(commit.as_str())?.ok_or_else(no_commit)?;
        let mut cmd = self.repo_command();
        cmd.args(["rev-parse", "--verify", "--quiet", "--end-of-options"]).arg(format!("{}^{{tree}}", c.as_str()));
        let out = self.run(cmd, Opts::c())?;
        let tree = match out.status {
            Some(0) => {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                TreeOid::try_from(s).map_err(|_| failed("rev-parse ^{tree}", &out))?
            }
            Some(1) | Some(128) => return Err(no_commit()),
            _ => return Err(failed("rev-parse ^{tree}", &out)),
        };
        self.validated(&tree)?;
        Ok(tree)
    }

    /// Whether `path` is an entry of `tree` or a directory above one
    /// (ASCII case-insensitively when the repository ignores case).
    pub fn tree_contains(&self, tree: &TreeOid, path: &RepoPath) -> Result<bool> {
        Ok(self.tree_table(tree)?.contains(path))
    }

    /// The verdict for `tree`, from the cache or by validating it.
    pub(crate) fn validated(&self, tree: &TreeOid) -> Result<()> {
        let icase = self.main_ignorecase()?;
        if let Some((ic, v)) = self.inner.cache.lock().unwrap().tree_verdicts.get(tree.as_str())
            && *ic == icase
        {
            return v.clone();
        }
        self.tree_table_with(tree, icase).map(|_| ())
    }

    /// The path table of a valid `tree` (re-running ls-tree if evicted).
    pub(crate) fn tree_table(&self, tree: &TreeOid) -> Result<Arc<TreeTable>> {
        let icase = self.main_ignorecase()?;
        {
            let mut cache = self.inner.cache.lock().unwrap();
            if let Some((ic, Err(e))) = cache.tree_verdicts.get(tree.as_str())
                && *ic == icase
            {
                return Err(e.clone());
            }
            cache.tick += 1;
            let tick = cache.tick;
            if let Some((t, table)) = cache.tree_tables.get_mut(tree.as_str())
                && table.icase == icase
            {
                *t = tick;
                return Ok(table.clone());
            }
        }
        self.tree_table_with(tree, icase)
    }

    fn tree_table_with(&self, tree: &TreeOid, icase: bool) -> Result<Arc<TreeTable>> {
        let res = self.validate_tree(tree, icase);
        let mut cache = self.inner.cache.lock().unwrap();
        match &res {
            Ok(table) => {
                cache.tree_verdicts.insert(tree.as_str().to_string(), (icase, Ok(())));
                cache.tick += 1;
                let tick = cache.tick;
                cache.tree_tables.insert(tree.as_str().to_string(), (tick, table.clone()));
                while cache.tree_tables.len() > TABLES_KEPT {
                    let oldest = cache.tree_tables.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone());
                    match oldest {
                        Some(k) => {
                            cache.tree_tables.remove(&k);
                        }
                        None => break,
                    }
                }
            }
            Err(e) if permanent(e) => {
                cache.tree_verdicts.insert(tree.as_str().to_string(), (icase, Err(e.clone())));
            }
            Err(_) => {}
        }
        res
    }

    fn validate_tree(&self, tree: &TreeOid, icase: bool) -> Result<Arc<TreeTable>> {
        let mut c = self.repo_command();
        c.args(["ls-tree", "-r", "-z", "-l", "--full-tree", tree.as_str()]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            let stderr = out.stderr_str();
            if stderr.contains("Could not read") || stderr.contains("not a tree object") {
                return Err(errd(
                    Code::NoSuchCommit,
                    format!("tree {} is incomplete in the repository", tree.as_str()),
                    json!({ "tree": tree.as_str(), "stderr": stderr }),
                ));
            }
            return Err(failed("ls-tree", &out));
        }
        let raw = parse_ls_tree(&out.stdout)?;
        Ok(Arc::new(build_table(raw, icase)?))
    }

    /// `core.ignorecase` of the main checkout's `<common>/config`.
    pub(crate) fn main_ignorecase(&self) -> Result<bool> {
        let common = &self.inner.common_dir;
        let Some((bytes, _, _)) = fsx::read_human(&common.join("config"))? else {
            return Ok(false);
        };
        Ok(config::config_bool(&bytes, "core.ignorecase")?.unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn rec(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn code(r: Result<Vec<RawEntry>>) -> Code {
        r.unwrap_err().code
    }

    #[test]
    fn ls_tree_output_parse_is_exact() {
        let good = rec(&format!("100644 blob {OID}      12\ta b.txt"));
        let v = parse_ls_tree(&good).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].size, Size::Bytes(12));
        assert_eq!(v[0].path, b"a b.txt");
        assert_eq!(parse_ls_tree(b"").unwrap().len(), 0);
        let two = [rec(&format!("100644 blob {OID} 1\ta")), rec(&format!("160000 commit {OID}       -\tsub"))].concat();
        assert_eq!(parse_ls_tree(&two).unwrap().len(), 2);
        let bad = rec(&format!("100644 blob {OID}     BAD\tx"));
        assert_eq!(parse_ls_tree(&bad).unwrap()[0].size, Size::Bad);

        // Truncations and missing NUL.
        assert_eq!(code(parse_ls_tree(&good[..good.len() - 1])), Code::Unknown);
        assert_eq!(code(parse_ls_tree(&good[..20])), Code::Unknown);
        let cut = [good.clone(), b"100644 blob".to_vec()].concat();
        assert_eq!(code(parse_ls_tree(&cut)), Code::Unknown);
        // Field counts, OID lengths, modes, sizes.
        for s in [
            format!("100644 {OID} 12\tx"),
            format!("100644 blob {OID} 12 extra\tx"),
            format!("100644 blob {} 12\tx", &OID[..39]),
            format!("100644 blob {OID}0 12\tx"),
            format!("100644 blob {} 12\tx", OID.to_uppercase()),
            format!("10064 blob {OID} 12\tx"),
            format!("10064x blob {OID} 12\tx"),
            format!("100648 blob {OID} 12\tx"),
            format!("100644 blub {OID} 12\tx"),
            format!("100644 blob {OID} 1x\tx"),
            format!("100644 blob {OID} -1\tx"),
            format!("100644 blob {OID} 99999999999999999999999\tx"),
            format!("100644 blob {OID} 12x"),
            format!("100644 blob {OID} 12\t"),
        ] {
            assert_eq!(code(parse_ls_tree(&rec(&s))), Code::Unknown, "{s:?}");
        }
        // A well-formed but unknown mode is refused by validation.
        let odd = parse_ls_tree(&rec(&format!("100664 blob {OID} 1\tx"))).unwrap();
        assert_eq!(build_table(odd, false).unwrap_err().code, Code::Unsupported);
        let dash = parse_ls_tree(&rec(&format!("100644 blob {OID} -\tx"))).unwrap();
        assert_eq!(build_table(dash, false).unwrap_err().code, Code::Unknown);
    }

    #[test]
    fn table_answers_entries_and_parents() {
        let raw = parse_ls_tree(&[rec(&format!("100644 blob {OID} 1\tDir/a.txt")), rec(&format!("120000 blob {OID} 1\tl"))].concat()).unwrap();
        let t = build_table(raw.clone(), false).unwrap();
        let p = |s: &str| RepoPath::try_from(s.to_string()).unwrap();
        assert!(t.contains(&p("Dir")) && t.contains(&p("Dir/a.txt")) && t.contains(&p("l")));
        assert!(!t.contains(&p("dir")) && !t.has_file(&p("Dir")));
        let t = build_table(raw, true).unwrap();
        assert!(t.contains(&p("dir")) && t.has_file(&p("DIR/A.TXT")));
    }
}

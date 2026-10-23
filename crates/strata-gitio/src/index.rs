//! The git index read in-process (§5.7): versions 2–4, split index,
//! repository ownership, `core.ignorecase`, and the immutable `IndexInfo`.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::{Code, RepoPath, StrataError};

use crate::err::{errd, path_str, unknown_reason};
use crate::fsx::{self, FileKey};
use crate::{Git, Result, config};

/// A snapshot of one work tree's index: never changes after it is returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexInfo {
    /// Every path in the index, spelled as git recorded it.
    pub tracked: BTreeSet<RepoPath>,
    /// `core.ignorecase` as git reads it for this work tree.
    pub ignorecase: bool,
    /// ASCII-lowercased `tracked`, filled only when `ignorecase`.
    folded: BTreeSet<String>,
}

impl IndexInfo {
    pub(crate) fn new(tracked: BTreeSet<RepoPath>, ignorecase: bool) -> IndexInfo {
        let folded = if ignorecase {
            tracked.iter().map(|p| p.as_str().to_ascii_lowercase()).collect()
        } else {
            BTreeSet::new()
        };
        IndexInfo { tracked, ignorecase, folded }
    }

    /// Whether `path` is in the index (ASCII case-insensitively when
    /// `ignorecase`, as git does; Unicode case is not folded).
    pub fn is_tracked(&self, path: &RepoPath) -> bool {
        if self.ignorecase {
            self.folded.contains(&path.as_str().to_ascii_lowercase())
        } else {
            self.tracked.contains(path)
        }
    }

    /// Whether any tracked path lies strictly under directory `dir`.
    pub fn tracked_under(&self, dir: &RepoPath) -> bool {
        if self.ignorecase {
            let prefix = format!("{}/", dir.as_str().to_ascii_lowercase());
            return self.folded.range(prefix.clone()..).next().is_some_and(|s| s.starts_with(&prefix));
        }
        let d = dir.as_str();
        for p in self.tracked.range(dir.clone()..) {
            let s = p.as_str();
            if !s.starts_with(d) {
                return false;
            }
            // Between `d` and `d/` sort only names continuing with bytes
            // below '/' ('-', '.', ...); skip them.
            if s.as_bytes().get(d.len()) == Some(&b'/') {
                return true;
            }
            if s.as_bytes().get(d.len()).is_some_and(|&c| c > b'/') {
                return false;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Metadata files

/// The path in a gitfile/`commondir` body: only git's line endings are
/// stripped; spaces stay. Empty → `Unknown`.
pub(crate) fn metadata_path(bytes: &[u8], what: &str) -> Result<PathBuf> {
    let b = fsx::strip_eol(bytes);
    if b.is_empty() || b.contains(&0) {
        return Err(unknown_reason(format!("{what} holds no path"), "bad-metadata"));
    }
    Ok(PathBuf::from(OsStr::from_bytes(b)))
}

// ---------------------------------------------------------------------------
// Index parsing

const HASH: usize = 20;
const MIN_ENTRY: usize = 64;

fn trunc(why: &str) -> StrataError {
    unknown_reason(format!("index is truncated: {why}"), "truncated-index")
}

fn bad(why: &str) -> StrataError {
    unknown_reason(format!("not a usable index: {why}"), "bad-index")
}

fn be32(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| trunc("u32"))
}

fn be16(b: &[u8], at: usize) -> Result<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| trunc("u16"))
}

/// git's offset varint (`decode_varint`).
fn varint(b: &[u8], at: &mut usize) -> Result<u64> {
    let mut c = *b.get(*at).ok_or_else(|| trunc("varint"))?;
    *at += 1;
    let mut val = (c & 127) as u64;
    while c & 128 != 0 {
        val = val.checked_add(1).ok_or_else(|| bad("varint overflow"))?;
        if val >> 57 != 0 {
            return Err(bad("varint overflow"));
        }
        c = *b.get(*at).ok_or_else(|| trunc("varint"))?;
        *at += 1;
        val = (val << 7) + (c & 127) as u64;
    }
    Ok(val)
}

/// The split-index link extension.
#[derive(Debug, Default)]
pub(crate) struct Link {
    pub base: [u8; HASH],
    pub delete: Option<Ewah>,
    pub replace: Option<Ewah>,
}

/// A raw EWAH bitmap, decoded against a bound later.
#[derive(Debug)]
pub(crate) struct Ewah {
    words: Vec<u64>,
}

impl Ewah {
    fn read(b: &[u8], at: &mut usize) -> Result<Ewah> {
        let _bits = be32(b, *at)?;
        let n = be32(b, *at + 4)? as usize;
        *at += 8;
        let need = n.checked_mul(8).ok_or_else(|| trunc("ewah"))?;
        let words = b.get(*at..*at + need).ok_or_else(|| trunc("ewah words"))?;
        let words = words.chunks(8).map(|c| u64::from_be_bytes(c.try_into().unwrap())).collect();
        *at += need;
        let _rlw = be32(b, *at)?;
        *at += 4;
        Ok(Ewah { words })
    }

    /// Set bit positions, all below `limit`; a run or literal setting a bit
    /// at or beyond `limit`, or a marker claiming more literal words than
    /// the bitmap holds, is refused.
    pub(crate) fn positions(&self, limit: usize) -> Result<Vec<usize>> {
        let mut out = Vec::new();
        let mut pos: u64 = 0;
        let mut i = 0;
        let limit64 = limit as u64;
        while i < self.words.len() {
            let rlw = self.words[i];
            i += 1;
            let run_bit = rlw & 1 == 1;
            let run_len = (rlw >> 1) & 0xFFFF_FFFF;
            let lits = (rlw >> 33) as usize;
            let run_end = pos + run_len * 64;
            if run_bit && run_len > 0 {
                if run_end > limit64 {
                    return Err(bad("split-index bitmap run beyond the shared index"));
                }
                out.extend(pos as usize..run_end as usize);
            }
            pos = run_end;
            if lits > self.words.len() - i {
                return Err(bad("split-index bitmap claims more literal words than it holds"));
            }
            for w in &self.words[i..i + lits] {
                for bit in 0..64 {
                    if w >> bit & 1 == 1 {
                        let p = pos + bit;
                        if p >= limit64 {
                            return Err(bad("split-index bitmap bit beyond the shared index"));
                        }
                        out.push(p as usize);
                    }
                }
                pos += 64;
            }
            i += lits;
        }
        Ok(out)
    }
}

/// One parsed index file.
#[derive(Debug)]
pub(crate) struct Parsed {
    /// Entry names in file order (raw bytes; may be empty only in a split
    /// index's replacement entries).
    pub names: Vec<Vec<u8>>,
    pub link: Option<Link>,
    pub checksum: [u8; HASH],
}

/// Parse an index file (§5.7 step 5). Errors: `Unknown` (truncated, bad
/// signature/version/layout), `Unsupported` (sparse index).
pub(crate) fn parse_index(b: &[u8]) -> Result<Parsed> {
    if b.len() < 12 {
        return Err(trunc("header"));
    }
    if &b[..4] != b"DIRC" {
        return Err(bad("signature"));
    }
    let version = be32(b, 4)?;
    if !(2..=4).contains(&version) {
        return Err(bad(&format!("version {version}")));
    }
    let count = be32(b, 8)? as usize;
    if b.len() < 12 + HASH {
        return Err(trunc("no checksum"));
    }
    let body_end = b.len() - HASH;
    // Never trust the count for allocation.
    let mut names: Vec<Vec<u8>> = Vec::with_capacity(count.min((body_end - 12) / MIN_ENTRY));
    let mut at = 12usize;
    let mut prev: Vec<u8> = Vec::new();
    for _ in 0..count {
        let start = at;
        if at + 62 > body_end {
            return Err(trunc("entry header"));
        }
        let mode = be32(b, start + 24)?;
        let flags = be16(b, start + 60)?;
        let mut name_at = start + 62;
        if flags & 0x4000 != 0 {
            if version < 3 {
                return Err(bad("extended flag in a version 2 index"));
            }
            name_at += 2;
        }
        if mode & 0o170000 == 0o040000 {
            return Err(errd(
                Code::Unsupported,
                "sparse index (directory entries) is not supported",
                json!({ "reason": "sparse-index" }),
            ));
        }
        let name;
        if version == 4 {
            let mut p = name_at;
            let strip = varint(&b[..body_end], &mut p)? as usize;
            if strip > prev.len() {
                return Err(bad("v4 prefix strips more than the previous name"));
            }
            let rest = &b[p..body_end];
            let nul = rest.iter().position(|&c| c == 0).ok_or_else(|| trunc("v4 name"))?;
            let mut n = prev[..prev.len() - strip].to_vec();
            n.extend_from_slice(&rest[..nul]);
            at = p + nul + 1;
            name = n;
        } else {
            let mut len = (flags & 0x0FFF) as usize;
            let rest = &b[name_at.min(body_end)..body_end];
            if len == 0x0FFF {
                len = rest.iter().position(|&c| c == 0).ok_or_else(|| trunc("long name"))?;
            } else if rest.len() <= len {
                return Err(trunc("name"));
            }
            let size = ((name_at - start) + len + 8) & !7;
            if start + size > body_end {
                return Err(trunc("entry padding"));
            }
            name = rest[..len].to_vec();
            at = start + size;
        }
        prev = name.clone();
        names.push(name);
    }
    // Extensions.
    let mut link = None;
    while at < body_end {
        if at + 8 > body_end {
            return Err(trunc("extension header"));
        }
        let sig = &b[at..at + 4];
        let size = be32(b, at + 4)? as usize;
        let data_at = at + 8;
        if size > body_end - data_at {
            return Err(trunc("extension"));
        }
        let data = &b[data_at..data_at + size];
        match sig {
            b"sdir" => {
                return Err(errd(
                    Code::Unsupported,
                    "sparse index is not supported",
                    json!({ "reason": "sparse-index" }),
                ));
            }
            b"link" => {
                if data.len() < HASH {
                    return Err(trunc("link extension"));
                }
                let mut l = Link { base: data[..HASH].try_into().unwrap(), ..Default::default() };
                let mut p = HASH;
                if p < data.len() {
                    l.delete = Some(Ewah::read(data, &mut p)?);
                    l.replace = Some(Ewah::read(data, &mut p)?);
                }
                if p != data.len() {
                    return Err(bad("trailing bytes in link extension"));
                }
                link = Some(l);
            }
            s if s[0].is_ascii_uppercase() => {}
            _ => return Err(bad(&format!("required extension {:?}", String::from_utf8_lossy(sig)))),
        }
        at = data_at + size;
    }
    Ok(Parsed { names, link, checksum: b[body_end..].try_into().unwrap() })
}

/// Merge a split index with its shared half (`merge_base_index`).
pub(crate) fn merge_split(main: Parsed, shared: Option<Parsed>) -> Result<Vec<Vec<u8>>> {
    let Some(link) = main.link else {
        if main.names.iter().any(|n| n.is_empty()) {
            return Err(bad("empty entry name"));
        }
        return Ok(main.names);
    };
    let mut base: Vec<Option<Vec<u8>>> = match shared {
        None => {
            if link.base != [0u8; HASH] {
                return Err(bad("split index without its shared index"));
            }
            Vec::new()
        }
        Some(s) => {
            if s.checksum != link.base {
                return Err(bad("shared index does not match the split index"));
            }
            if s.link.is_some() {
                return Err(bad("shared index is itself split"));
            }
            s.names.into_iter().map(Some).collect()
        }
    };
    let n = base.len();
    let deletes = match &link.delete {
        Some(e) => e.positions(n)?,
        None => Vec::new(),
    };
    let replaces = match &link.replace {
        Some(e) => e.positions(n)?,
        None => Vec::new(),
    };
    if replaces.len() > main.names.len() {
        return Err(bad("more replacements than split-index entries"));
    }
    let mut own = main.names.into_iter();
    for _ in &replaces {
        let e = own.next().unwrap();
        if !e.is_empty() {
            return Err(bad("replacement entry carries a name"));
        }
    }
    for p in deletes {
        base[p] = None;
    }
    let mut out: Vec<Vec<u8>> = base.into_iter().flatten().collect();
    for e in own {
        if e.is_empty() {
            return Err(bad("empty entry name"));
        }
        out.push(e);
    }
    Ok(out)
}

/// Entry names → tracked set: `NotText` for non-UTF-8, NFC via `RepoPath`.
pub(crate) fn to_tracked(names: Vec<Vec<u8>>) -> Result<BTreeSet<RepoPath>> {
    let mut out = BTreeSet::new();
    for n in names {
        let s = String::from_utf8(n).map_err(|e| {
            errd(
                Code::NotText,
                "index holds a path that is not UTF-8",
                json!({ "path": String::from_utf8_lossy(e.as_bytes()) }),
            )
        })?;
        let p = RepoPath::try_from(s.clone()).map_err(|e| {
            errd(Code::Unknown, format!("index path {s:?}: {}", e.message), json!({ "reason": "bad-path", "path": s }))
        })?;
        out.insert(p);
    }
    Ok(out)
}

/// Parse index bytes plus (when split) the shared half, read by `shared`.
pub(crate) fn parse_full(
    b: &[u8],
    shared: impl FnOnce(&str) -> Result<Option<Vec<u8>>>,
) -> Result<BTreeSet<RepoPath>> {
    let main = parse_index(b)?;
    let shared_parsed = match &main.link {
        Some(l) if l.base != [0u8; HASH] => {
            let hex: String = l.base.iter().map(|c| format!("{c:02x}")).collect();
            match shared(&format!("sharedindex.{hex}"))? {
                None => return Err(bad("split index without its shared index")),
                Some(sb) => Some(parse_index(&sb)?),
            }
        }
        _ => None,
    };
    to_tracked(merge_split(main, shared_parsed)?)
}

// ---------------------------------------------------------------------------
// read_index

/// Where a work tree's git files live.
pub(crate) struct Located {
    /// The work tree root used for rooted reads of work-tree files.
    pub wt_root: PathBuf,
    /// Root for rooted reads of git-dir files.
    pub root: PathBuf,
    /// The git dir relative to `root` ("" when the root is the git dir).
    pub rel: String,
    pub common: PathBuf,
    pub gitdir: PathBuf,
    /// The main repository's (human) files: read the human way.
    pub human: bool,
}

impl Located {
    pub(crate) fn file(&self, name: &str) -> String {
        if self.rel.is_empty() { name.to_string() } else { format!("{}/{name}", self.rel) }
    }
}

fn unknown_from(e: StrataError, what: &Path) -> StrataError {
    errd(
        Code::Unknown,
        format!("{}: {}", what.display(), e.message),
        json!({ "path": path_str(what), "code": e.code.as_str(), "detail": e.detail }),
    )
}

impl Git {
    /// The tracked set and `core.ignorecase` of `worktree` (§5.7).
    pub fn read_index(&self, worktree: &Path) -> Result<IndexInfo> {
        let loc = self.locate(worktree)?;
        self.read_index_at(&loc, worktree)
    }

    pub(crate) fn read_index_at(&self, loc: &Located, worktree: &Path) -> Result<IndexInfo> {
        let mut keys: Vec<Option<FileKey>> = Vec::new();
        let ignorecase = self.ignorecase_of(loc, &mut keys)?;
        let index_rel = loc.file("index");
        let index_at = loc.root.join(&index_rel);
        let (bytes, key) = match fsx::read_rooted(&loc.root, &index_rel).map_err(|e| unknown_from(e, &index_at))? {
            Some(x) => x,
            None => {
                return Err(errd(
                    Code::Unknown,
                    format!("{} has no index", worktree.display()),
                    json!({ "reason": "no-index", "path": path_str(&index_at) }),
                ));
            }
        };
        keys.push(Some(key));
        if let Some((k, info)) = self.inner.cache.lock().unwrap().index.get(&index_at)
            && *k == keys
        {
            return Ok(info.clone());
        }
        let tracked = parse_full(&bytes, |name| {
            let rel = loc.file(name);
            Ok(fsx::read_rooted(&loc.root, &rel).map_err(|e| unknown_from(e, &loc.root.join(&rel)))?.map(|x| x.0))
        })?;
        let info = IndexInfo::new(tracked, ignorecase);
        self.inner.cache.lock().unwrap().index.insert(index_at, (keys, info.clone()));
        Ok(info)
    }

    pub(crate) fn locate(&self, worktree: &Path) -> Result<Located> {
        let root: &Path = if worktree.as_os_str().as_bytes() == self.inner.root_arg.as_os_str().as_bytes() {
            &self.inner.root_real
        } else {
            worktree
        };
        let dotgit = root.join(".git");
        let gitdir = match strata_apfs::open_rooted_file(root, &fsx::rp(".git")?) {
            Ok(f) => {
                use std::io::Read;
                let mut buf = Vec::new();
                (&f).take(fsx::GITFILE_MAX as u64 + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| errd(Code::Unknown, format!("read {}: {e}", dotgit.display()), json!({"path": path_str(&dotgit)})))?;
                fsx::lexical_normalize(&fsx::parse_gitfile(&buf, root)?)
            }
            Err(e) if e.code == Code::IsDirectory => dotgit.clone(),
            Err(e) if e.code == Code::NoSuchPath => return Err(e),
            Err(e) if e.code == Code::InvalidInput && reason_of(&e) == Some("root_not_canonical") => return Err(e),
            Err(e) => return Err(unknown_from(e, &dotgit)),
        };
        if !gitdir.is_absolute() {
            return Err(unknown_reason(format!("git dir {} is not absolute", gitdir.display()), "bad-gitfile"));
        }
        // commondir, rooted at the git dir.
        let common = match fsx::read_rooted_limited(&gitdir, "commondir", Some(fsx::GITFILE_MAX as u64))
            .map_err(|e| unknown_from(e, &gitdir.join("commondir")))?
        {
            None => gitdir.clone(),
            Some((b, _)) => {
                let p = metadata_path(&b, "commondir")?;
                fsx::lexical_normalize(&if p.is_absolute() { p } else { gitdir.join(p) })
            }
        };
        let human = match std::fs::canonicalize(&common) {
            Ok(r) => r == self.inner.common_dir,
            Err(_) => false,
        };
        if !human && !self.is_owned_shadow(&common)? {
            return Err(errd(
                Code::InvalidInput,
                format!("{} belongs to another repository", worktree.display()),
                json!({ "reason": "foreign-worktree", "path": path_str(worktree), "common_dir": path_str(&common) }),
            ));
        }
        let wt_root = root.to_path_buf();
        let (root, rel) = match gitdir.strip_prefix(&common) {
            Ok(r) => (common.clone(), r.to_str().ok_or_else(|| errd(Code::NotText, "git dir is not UTF-8", json!({"path": path_str(&gitdir)})))?.to_string()),
            Err(_) => (gitdir.clone(), String::new()),
        };
        Ok(Located { wt_root, root, rel, common, gitdir, human })
    }

    /// Whether `common` is one of this repository's shadow repositories:
    /// `strata.shadow = true` and its first alternate is our object store.
    /// Positive answers are cached by path.
    pub(crate) fn is_owned_shadow(&self, common: &Path) -> Result<bool> {
        if self.inner.cache.lock().unwrap().shadows.contains(common) {
            return Ok(true);
        }
        let cfg = match fsx::read_rooted(common, "config") {
            Ok(Some((b, _))) => b,
            Ok(None) => return Ok(false),
            Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory) => return Ok(false),
            Err(e) => return Err(unknown_from(e, &common.join("config"))),
        };
        if config::config_bool(&cfg, "strata.shadow").ok().flatten() != Some(true) {
            return Ok(false);
        }
        let alt = match fsx::read_rooted(common, "objects/info/alternates") {
            Ok(Some((b, _))) => b,
            Ok(None) => return Ok(false),
            Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory) => return Ok(false),
            Err(e) => return Err(unknown_from(e, &common.join("objects/info/alternates"))),
        };
        let first = alt.split(|&c| c == b'\n').next().unwrap_or(b"");
        let first = fsx::strip_eol(first);
        if first.is_empty() {
            return Ok(false);
        }
        let ours = std::fs::canonicalize(self.inner.common_dir.join("objects"));
        let theirs = std::fs::canonicalize(Path::new(OsStr::from_bytes(first)));
        let owned = matches!((ours, theirs), (Ok(a), Ok(b)) if a == b);
        if owned {
            self.inner.cache.lock().unwrap().shadows.insert(common.to_path_buf());
        }
        Ok(owned)
    }

    /// `core.ignorecase` for a located work tree (§5.7 step 6).
    fn ignorecase_of(&self, loc: &Located, keys: &mut Vec<Option<FileKey>>) -> Result<bool> {
        let common_cfg = if loc.human {
            fsx::read_human(&loc.common.join("config"))?.map(|(b, k, _)| (b, k))
        } else {
            fsx::read_rooted(&loc.common, "config").map_err(|e| unknown_from(e, &loc.common.join("config")))?
        };
        keys.push(common_cfg.as_ref().map(|x| x.1));
        let entries = match &common_cfg {
            Some((b, _)) => config::parse(b)?,
            None => Vec::new(),
        };
        let mut ic = config::get_bool(&entries, "core.ignorecase")?.unwrap_or(false);
        if config::get_bool(&entries, "extensions.worktreeconfig")? == Some(true) {
            let wt = if loc.human {
                fsx::read_human(&loc.gitdir.join("config.worktree"))?.map(|(b, k, _)| (b, k))
            } else {
                let rel = loc.file("config.worktree");
                fsx::read_rooted(&loc.root, &rel).map_err(|e| unknown_from(e, &loc.root.join(&rel)))?
            };
            keys.push(wt.as_ref().map(|x| x.1));
            if let Some((b, _)) = wt
                && let Some(v) = config::config_bool(&b, "core.ignorecase")?
            {
                ic = v;
            }
        }
        Ok(ic)
    }
}

pub(crate) fn reason_of(e: &StrataError) -> Option<&str> {
    e.detail.as_ref()?.get("reason")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RepoPath {
        RepoPath::try_from(s.to_string()).unwrap()
    }

    /// One on-disk entry. `prev` is the previous name (v4 compression).
    fn entry(version: u32, name: &[u8], prev: &[u8], mode: u32) -> Vec<u8> {
        let mut e = vec![0u8; 24];
        e.extend_from_slice(&mode.to_be_bytes());
        e.extend_from_slice(&[0u8; 12]); // uid gid size
        e.extend_from_slice(&[0xab; 20]); // oid
        let len = name.len().min(0xFFF) as u16;
        e.extend_from_slice(&len.to_be_bytes());
        if version == 4 {
            let common = prev.iter().zip(name).take_while(|(a, b)| a == b).count();
            let strip = prev.len() - common;
            e.extend(encode_varint(strip as u64));
            e.extend_from_slice(&name[common..]);
            e.push(0);
        } else {
            e.extend_from_slice(name);
            let size = (62 + name.len() + 8) & !7;
            e.resize(size, 0);
        }
        e
    }

    fn encode_varint(mut v: u64) -> Vec<u8> {
        let mut buf = vec![(v & 127) as u8];
        while v >> 7 != 0 {
            v = (v >> 7) - 1;
            buf.push(128 | (v & 127) as u8);
        }
        buf.reverse();
        buf
    }

    fn build(version: u32, names: &[&[u8]], exts: &[(&[u8; 4], Vec<u8>)], hash: [u8; 20]) -> Vec<u8> {
        build_count(version, names, names.len() as u32, exts, hash)
    }

    fn build_count(version: u32, names: &[&[u8]], count: u32, exts: &[(&[u8; 4], Vec<u8>)], hash: [u8; 20]) -> Vec<u8> {
        let mut b = b"DIRC".to_vec();
        b.extend_from_slice(&version.to_be_bytes());
        b.extend_from_slice(&count.to_be_bytes());
        let mut prev: &[u8] = b"";
        for n in names {
            b.extend(entry(version, n, prev, 0o100644));
            prev = n;
        }
        for (sig, data) in exts {
            b.extend_from_slice(&sig[..]);
            b.extend_from_slice(&(data.len() as u32).to_be_bytes());
            b.extend_from_slice(data);
        }
        b.extend_from_slice(&hash);
        b
    }

    fn tracked(b: &[u8]) -> Result<Vec<String>> {
        let set = parse_full(b, |_| Ok(None))?;
        Ok(set.into_iter().map(|p| p.as_str().to_string()).collect())
    }

    fn reason(e: &StrataError) -> String {
        reason_of(e).unwrap_or("").to_string()
    }

    #[test]
    fn metadata_paths_strip_only_git_line_endings() {
        assert_eq!(metadata_path(b"../..", "x").unwrap(), PathBuf::from("../.."));
        assert_eq!(metadata_path(b" ../.. \r\n", "x").unwrap(), PathBuf::from(" ../.. "));
        assert_eq!(metadata_path(b" a b \n", "x").unwrap(), PathBuf::from(" a b "));
        assert_eq!(metadata_path(b"\r\n", "x").unwrap_err().code, Code::Unknown);
        assert_eq!(metadata_path(b"", "x").unwrap_err().code, Code::Unknown);
        let p = fsx::parse_gitfile(b"gitdir:  /x/y \r\n", Path::new("/t")).unwrap();
        assert_eq!(p, PathBuf::from(" /x/y ").as_path().to_path_buf().pipe(|p| Path::new("/t").join(p)));
        assert_eq!(fsx::parse_gitfile(b"gitdir: /x/y", Path::new("/t")).unwrap(), PathBuf::from("/x/y"));
        assert!(fsx::parse_gitfile(b"gitdir: \n", Path::new("/t")).is_err());
    }

    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    impl<T> Pipe for T {}

    #[test]
    fn reads_a_version_2_index() {
        let b = build(2, &[b"a.txt", b"dir/b.rs", b"z"], &[], [0; 20]);
        assert_eq!(tracked(&b).unwrap(), ["a.txt", "dir/b.rs", "z"]);
        let info = IndexInfo::new(parse_full(&b, |_| Ok(None)).unwrap(), false);
        assert!(info.is_tracked(&rp("dir/b.rs")));
        assert!(!info.is_tracked(&rp("untracked.txt")));
        assert!(!info.is_tracked(&rp("A.txt")));
    }

    #[test]
    fn tracked_lookup_folds_ascii_case_when_ignorecase() {
        let set: BTreeSet<RepoPath> = ["assets/logo.png", "ébauche.png"].iter().map(|s| rp(s)).collect();
        let info = IndexInfo::new(set.clone(), true);
        assert!(info.is_tracked(&rp("Assets/Logo.PNG")));
        assert!(info.tracked.contains(&rp("assets/logo.png")));
        assert!(!info.tracked.contains(&rp("Assets/Logo.PNG")));
        assert!(!info.is_tracked(&rp("Ébauche.png")));
        assert!(info.is_tracked(&rp("ébauche.png")));
        let strict = IndexInfo::new(set, false);
        assert!(!strict.is_tracked(&rp("Assets/Logo.PNG")));
        assert!(RepoPath::try_from("/assets/logo.png".to_string()).is_err());
    }

    #[test]
    fn tracked_under_stops_at_separator() {
        let set: BTreeSet<RepoPath> =
            ["keep/a.txt", "keep-x/b", "keep.d", "qlogs_like/x", "zz/Deep/f"].iter().map(|s| rp(s)).collect();
        let info = IndexInfo::new(set.clone(), false);
        assert!(info.tracked_under(&rp("keep")));
        assert!(!info.tracked_under(&rp("Keep")));
        assert!(!info.tracked_under(&rp("qlogs")));
        assert!(info.tracked_under(&rp("zz/Deep")));
        assert!(!info.tracked_under(&rp("keep/a.txt")));
        let folded = IndexInfo::new(set, true);
        assert!(folded.tracked_under(&rp("KEEP")));
        assert!(folded.tracked_under(&rp("zz/deep")));
        assert!(!folded.tracked_under(&rp("qlogs")));
    }

    #[test]
    fn handles_a_path_at_and_beyond_the_length_field_limit() {
        for len in [0xFFE, 0xFFF, 0x1000, 0x1100] {
            let name: Vec<u8> = (0..len).map(|i| if i % 100 == 99 { b'/' } else { b'a' }).collect();
            let b = build(2, &[&name, b"zz"], &[], [0; 20]);
            let got = tracked(&b).unwrap();
            assert_eq!(got.len(), 2, "len {len:#x}");
            assert_eq!(got[0].len(), len);
            assert_eq!(got[1], "zz");
        }
    }

    #[test]
    fn rejects_input_that_is_not_an_index() {
        assert_eq!(parse_index(b"").unwrap_err().code, Code::Unknown);
        let mut b = build(2, &[b"a"], &[], [0; 20]);
        b[0] = b'X';
        assert_eq!(parse_index(&b).unwrap_err().code, Code::Unknown);
        for v in [1u32, 5] {
            let mut b = build(2, &[b"a"], &[], [0; 20]);
            b[4..8].copy_from_slice(&v.to_be_bytes());
            assert_eq!(parse_index(&b).unwrap_err().code, Code::Unknown);
        }
    }

    #[test]
    fn refuses_a_truncated_index_instead_of_inventing_entries() {
        let b = build(2, &[b"first-name.txt", b"second-name.txt"], &[], [0; 20]);
        let first_name = 12 + 62 + 3;
        let second_name = 12 + 80 + 62 + 3;
        for cut in [6, first_name, second_name] {
            // Truncate, then keep a 20-byte "checksum" so only the cut matters.
            let t = &b[..cut];
            assert_eq!(parse_index(t).unwrap_err().code, Code::Unknown, "cut {cut}");
        }
    }

    #[test]
    fn tolerates_the_trailing_data_a_real_index_carries() {
        let b = build(2, &[b"a", b"b"], &[(b"TREE", vec![1, 2, 3]), (b"UNTR", vec![0; 9])], [7; 20]);
        assert_eq!(tracked(&b).unwrap(), ["a", "b"]);
        let b = build(3, &[b"a"], &[(b"REUC", vec![])], [7; 20]);
        assert_eq!(tracked(&b).unwrap(), ["a"]);
    }

    #[test]
    fn a_count_larger_than_the_data_is_refused() {
        let b = build_count(2, &[b"a", b"b"], 3, &[], [0; 20]);
        let e = parse_index(&b).unwrap_err();
        assert_eq!(e.code, Code::Unknown);
    }

    #[test]
    fn a_header_claiming_four_billion_entries_does_not_reserve_for_them() {
        let b = build_count(2, &[b"a"], u32::MAX, &[], [0; 20]);
        assert_eq!(parse_index(&b).unwrap_err().code, Code::Unknown);
    }

    #[test]
    fn reads_a_version_4_index_with_prefix_compressed_paths() {
        let names: [&[u8]; 5] = [b"src/a.rs", b"src/ab.rs", b"src/b/c.rs", b"t", b"tests/x"];
        let b = build(4, &names, &[(b"TREE", vec![])], [1; 20]);
        assert_eq!(tracked(&b).unwrap(), ["src/a.rs", "src/ab.rs", "src/b/c.rs", "t", "tests/x"]);
    }

    /// EWAH with only literal words for these bit positions.
    fn ewah(bits: &[usize]) -> Vec<u8> {
        let nwords = bits.iter().max().map(|m| m / 64 + 1).unwrap_or(0);
        let mut lits = vec![0u64; nwords];
        for &b in bits {
            lits[b / 64] |= 1 << (b % 64);
        }
        let mut words = vec![(nwords as u64) << 33];
        words.extend(lits);
        ewah_raw(&words, nwords * 64)
    }

    fn ewah_raw(words: &[u64], bits: usize) -> Vec<u8> {
        let mut out = (bits as u32).to_be_bytes().to_vec();
        out.extend((words.len() as u32).to_be_bytes());
        for w in words {
            out.extend(w.to_be_bytes());
        }
        out.extend(0u32.to_be_bytes());
        out
    }

    fn link(base: [u8; 20], del: Option<Vec<u8>>, rep: Option<Vec<u8>>) -> Vec<u8> {
        let mut d = base.to_vec();
        if let (Some(a), Some(b)) = (del, rep) {
            d.extend(a);
            d.extend(b);
        }
        d
    }

    fn split(main: &[u8], shared: &[u8]) -> Result<Vec<String>> {
        let s = parse_full(main, |name| {
            assert!(name.starts_with("sharedindex."));
            Ok(Some(shared.to_vec()))
        })?;
        Ok(s.into_iter().map(|p| p.as_str().to_string()).collect())
    }

    #[test]
    fn reads_a_split_index_through_its_shared_half() {
        let shared = build(2, &[b"a", b"b", b"c", b"d"], &[], [9; 20]);
        // delete b (1), replace c (2), add e.
        let main = build(2, &[b"", b"e"], &[(b"link", link([9; 20], Some(ewah(&[1])), Some(ewah(&[2]))))], [3; 20]);
        assert_eq!(split(&main, &shared).unwrap(), ["a", "c", "d", "e"]);
        // No bitmaps: nothing deleted or replaced.
        let main = build(2, &[b"e"], &[(b"link", link([9; 20], None, None))], [3; 20]);
        assert_eq!(split(&main, &shared).unwrap(), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn a_split_index_without_a_matching_shared_half_is_refused() {
        let shared = build(2, &[b"a", b"b"], &[], [9; 20]);
        let main = build(2, &[b"e"], &[(b"link", link([8; 20], None, None))], [3; 20]);
        // Missing.
        assert_eq!(tracked(&main).unwrap_err().code, Code::Unknown);
        // Mismatched checksum.
        assert_eq!(split(&main, &shared).unwrap_err().code, Code::Unknown);
        // Bitmap past the shared entries.
        let main = build(2, &[b"e"], &[(b"link", link([9; 20], Some(ewah(&[2])), Some(ewah(&[]))))], [3; 20]);
        assert_eq!(split(&main, &shared).unwrap_err().code, Code::Unknown);
        // More replacements than entries.
        let main = build(2, &[b""], &[(b"link", link([9; 20], Some(ewah(&[])), Some(ewah(&[0, 1]))))], [3; 20]);
        assert_eq!(split(&main, &shared).unwrap_err().code, Code::Unknown);
        // All-zero base: no shared half needed.
        let main = build(2, &[b"e", b"f"], &[(b"link", link([0; 20], None, None))], [3; 20]);
        assert_eq!(tracked(&main).unwrap(), ["e", "f"]);
    }

    #[test]
    fn ewah_runs_and_literals_are_bounded_by_the_shared_index() {
        // Run of 64 ones then one literal word.
        let e = Ewah { words: vec![1 | (1 << 1) | (1u64 << 33), 0b101] };
        assert_eq!(e.positions(200).unwrap().len(), 66);
        assert_eq!(e.positions(66).unwrap_err().code, Code::Unknown);
        assert_eq!(e.positions(63).unwrap_err().code, Code::Unknown);
        // A huge run of ones is refused without materialising it.
        let e = Ewah { words: vec![1 | (0xFFFF_FFFFu64 << 1)] };
        assert_eq!(e.positions(10).unwrap_err().code, Code::Unknown);
        // A huge run of zeros is fine.
        let e = Ewah { words: vec![0xFFFF_FFFFu64 << 1] };
        assert!(e.positions(10).unwrap().is_empty());
        // Marker claims literals the bitmap does not hold.
        let e = Ewah { words: vec![3u64 << 33, 1] };
        assert_eq!(e.positions(1000).unwrap_err().code, Code::Unknown);
        // Through the parser too.
        let shared = build(2, &[b"a", b"b"], &[], [9; 20]);
        let bad = ewah_raw(&[5u64 << 33, 1], 64);
        let main = build(2, &[b"e"], &[(b"link", link([9; 20], Some(bad), Some(ewah(&[]))))], [3; 20]);
        assert_eq!(split(&main, &shared).unwrap_err().code, Code::Unknown);
    }

    #[test]
    fn sparse_entries_and_sdir_are_unsupported() {
        let b = build(2, &[b"a"], &[(b"sdir", vec![])], [0; 20]);
        assert_eq!(parse_index(&b).unwrap_err().code, Code::Unsupported);
        let mut b = b"DIRC".to_vec();
        b.extend(2u32.to_be_bytes());
        b.extend(1u32.to_be_bytes());
        b.extend(entry(2, b"dir/", b"", 0o040000));
        b.extend([0u8; 20]);
        assert_eq!(parse_index(&b).unwrap_err().code, Code::Unsupported);
    }

    #[test]
    fn non_utf8_names_are_not_text() {
        let b = build(2, &[b"\xff.txt"], &[], [0; 20]);
        assert_eq!(tracked(&b).unwrap_err().code, Code::NotText);
        let b = build(2, &[b"a/../b"], &[], [0; 20]);
        assert_eq!(reason(&tracked(&b).unwrap_err()), "bad-path");
    }

    #[test]
    fn reads_ignorecase_from_a_repository_config() {
        let ic = |s: &str| config::config_bool(s.as_bytes(), "core.ignorecase").unwrap().unwrap_or(false);
        assert!(ic("[core]\n\tignorecase = true\n"));
        assert!(ic("[CORE]\n\tIgnoreCase = True\n"));
        assert!(ic("[core]\n\tignoreCase = yes\n"));
        assert!(!ic("[other]\n\tignorecase = true\n"));
        assert!(!ic("[core \"x\"]\n\tignorecase = true\n"));
        assert!(!ic("[core]\n\tbare = true\n"));
        assert!(!ic(""));
    }
}

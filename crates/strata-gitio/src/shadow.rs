//! Per-session shadow repositories (§5.6): `ensure_shadow`, `shadow_at`,
//! `shadow_drift`, and gitio's own config writer (§9 "values written the way
//! git writes them").

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_apfs::{RemoveKind, RenameMode};
use strata_types::{Code, StrataError};

use crate::command::{Opts, QUERY_TIMEOUT, failed, flag};
use crate::err::{as_unknown, errd, path_str};
use crate::{Git, Result, config, fsx};

/// One session's shadow repository on one repo (§4.2). Only
/// `Git::ensure_shadow` and `Git::shadow_at` make one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shadow {
    /// realpath of the shadow repository.
    dir: PathBuf,
}

impl Shadow {
    /// The shadow repository's directory (a realpath).
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A mirror that differs from the main checkout today, or a pre-push guard
/// that may be off (§5.6). Reported only; gitio never repairs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorDrift {
    /// `info/exclude` bytes differ, or only one side has it.
    Exclude,
    /// `info/attributes`, likewise.
    Attributes,
    /// A copied key (§4.2) whose values differ, or an `include.*`,
    /// `includeIf.*` or `extensions.*` key in the shadow config. Named as
    /// `git config --list` names it.
    Config { key: String },
    /// The pre-push guard may be off: git's effective hooks dir in the
    /// workspace is not `<dir>/hooks` (`hooks_path`), or `hooks/pre-push` is
    /// no longer gitio's (`hook_replaced`).
    PrePushGuardDisabled { hooks_path: Option<String>, hook_replaced: bool },
}

/// The pre-push guard (design §6.8).
pub(crate) const PRE_PUSH: &[u8] = b"#!/bin/sh\n\
# git-strata pre-push guard: pushes from a strata workspace go through `git-strata publish`.\n\
if [ \"${STRATA_PUBLISH-}\" = \"1\" ]; then\n\
\texit 0\n\
fi\n\
echo 'git-strata: push refused in a strata workspace; publish with git-strata publish' >&2\n\
exit 1\n";

/// Exact core keys copied from the main checkout (lower-cased, §4.2).
const COPIED_CORE: &[&str] = &[
    "core.ignorecase",
    "core.precomposeunicode",
    "core.autocrlf",
    "core.eol",
    "core.safecrlf",
    "core.symlinks",
    "core.filemode",
    "core.checkroundtripencoding",
    "core.excludesfile",
    "core.attributesfile",
];
/// Whole sections copied.
const COPIED_SECTIONS: &[&str] = &["filter", "diff", "merge"];

/// Whether `key` (as `git config --list` names it) is on the copy list.
pub(crate) fn is_copied(key: &[u8]) -> bool {
    let k = String::from_utf8_lossy(key);
    if COPIED_CORE.contains(&k.as_ref()) {
        return true;
    }
    match k.split_once('.') {
        Some((sec, rest)) => COPIED_SECTIONS.contains(&sec) && !rest.is_empty(),
        None => false,
    }
}

/// Keys the shadow config never gets from `ensure_shadow`.
fn is_foreign(key: &[u8]) -> bool {
    key.starts_with(b"include.") || key.starts_with(b"includeif.") || key.starts_with(b"extensions.")
}

/// One `--null --list` record.
pub(crate) type ListEntry = (Vec<u8>, Option<Vec<u8>>);

/// Parse `git config --null --list`: `key\nvalue\0`, or `key\0` for a key
/// without `=`.
pub(crate) fn parse_null_list(out: &[u8]) -> Vec<ListEntry> {
    let mut v = Vec::new();
    for rec in out.split(|&c| c == 0) {
        if rec.is_empty() {
            continue;
        }
        match rec.iter().position(|&c| c == b'\n') {
            Some(i) => v.push((rec[..i].to_vec(), Some(rec[i + 1..].to_vec()))),
            None => v.push((rec.to_vec(), None)),
        }
    }
    v
}

/// A value written the way git 2.54's `git config` writes it.
pub(crate) fn quote_value(v: &[u8]) -> Vec<u8> {
    let quote = v.first() == Some(&b' ')
        || v.last() == Some(&b' ')
        || v.iter().any(|&c| c == b';' || c == b'#' || c == b'\r');
    let mut out = Vec::with_capacity(v.len() + 2);
    if quote {
        out.push(b'"');
    }
    for &c in v {
        match c {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'"' | b'\\' => {
                out.push(b'\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    if quote {
        out.push(b'"');
    }
    out
}

/// Split `section.sub.sec.name` into (section, subsection, name).
fn split_key(key: &[u8]) -> Option<(&[u8], Option<&[u8]>, &[u8])> {
    let first = key.iter().position(|&c| c == b'.')?;
    let last = key.iter().rposition(|&c| c == b'.')?;
    let section = &key[..first];
    let name = &key[last + 1..];
    let sub = if last > first { Some(&key[first + 1..last]) } else { None };
    if section.is_empty() || name.is_empty() {
        return None;
    }
    Some((section, sub, name))
}

/// Render config entries; a new section header whenever (section, sub)
/// changes, as `git config --file` does when appending.
pub(crate) fn render_config(entries: &[ListEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cur: Option<(Vec<u8>, Option<Vec<u8>>)> = None;
    for (key, val) in entries {
        let Some((sec, sub, name)) = split_key(key) else { continue };
        let this = (sec.to_vec(), sub.map(<[u8]>::to_vec));
        if cur.as_ref() != Some(&this) {
            out.push(b'[');
            out.extend_from_slice(sec);
            if let Some(s) = sub {
                out.extend_from_slice(b" \"");
                for &c in s {
                    if c == b'"' || c == b'\\' {
                        out.push(b'\\');
                    }
                    out.push(c);
                }
                out.push(b'"');
            }
            out.extend_from_slice(b"]\n");
            cur = Some(this);
        }
        out.push(b'\t');
        out.extend_from_slice(name);
        if let Some(v) = val {
            out.extend_from_slice(b" = ");
            out.extend_from_slice(&quote_value(v));
        }
        out.push(b'\n');
    }
    out
}

fn fsync_file(f: &File, shown: &Path) -> Result<()> {
    if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
        let e = std::io::Error::last_os_error();
        return Err(strata_apfs::classify_io_error(&e, strata_apfs::IoSide::AfterWrite, shown));
    }
    Ok(())
}

/// Ownership check result.
enum Owned {
    Yes,
    No(&'static str),
}

/// One side of a mirror comparison.
#[derive(PartialEq, Eq)]
enum Side {
    Absent,
    Bytes(Vec<u8>),
    /// Not a regular file, or unreadable: always "different".
    Bad,
}

impl Git {
    /// Whether the existing directory `real` (a realpath) is a complete
    /// shadow repository of this repo. Read errors → `Unknown`.
    fn shadow_owned(&self, real: &Path) -> Result<Owned> {
        let unknown = |e: StrataError, rel: &str| as_unknown(e, &format!("read {}", real.join(rel).display()));
        let cfg = match fsx::read_rooted(real, "config") {
            Ok(Some((b, _))) => b,
            Ok(None) => return Ok(Owned::No("no-config")),
            Err(e) => return Err(unknown(e, "config")),
        };
        if config::config_bool(&cfg, "strata.shadow")? != Some(true) {
            return Ok(Owned::No("no-marker"));
        }
        let alt = match fsx::read_rooted(real, "objects/info/alternates") {
            Ok(Some((b, _))) => b,
            Ok(None) => return Ok(Owned::No("no-alternates")),
            Err(e) => return Err(unknown(e, "objects/info/alternates")),
        };
        let first = fsx::strip_eol(alt.split(|&c| c == b'\n').next().unwrap_or(b""));
        if first.is_empty() {
            return Ok(Owned::No("foreign-alternates"));
        }
        let ours = std::fs::canonicalize(self.inner.common_dir.join("objects"));
        let theirs = std::fs::canonicalize(Path::new(OsStr::from_bytes(first)));
        match (ours, theirs) {
            (Ok(a), Ok(b)) if a == b => Ok(Owned::Yes),
            _ => Ok(Owned::No("foreign-alternates")),
        }
    }

    /// `shadow` still is one of ours (§5.6 "同 shadow_at 的核对").
    pub(crate) fn require_owned(&self, shadow: &Shadow) -> Result<()> {
        let not_ours = |reason: &str| {
            errd(
                Code::InvalidInput,
                format!("{} is not a shadow repository of this repo", shadow.dir.display()),
                json!({ "path": path_str(&shadow.dir), "reason": reason }),
            )
        };
        match fsx::lstat(&shadow.dir)? {
            Some(m) if m.is_dir() => {}
            _ => return Err(not_ours("not-a-directory")),
        }
        match self.shadow_owned(&shadow.dir)? {
            Owned::Yes => Ok(()),
            Owned::No(r) => Err(not_ours(r)),
        }
    }

    /// Build a session's shadow repository at `dir` (§5.6). Returns the
    /// existing one untouched when `dir` already is a complete shadow of
    /// this repo.
    pub fn ensure_shadow(&self, dir: &Path) -> Result<Shadow> {
        // Step 1: refusals, before any write (G9).
        fsx::require_absolute(dir, "shadow dir")?;
        if fsx::lexical_normalize(dir).as_os_str() != dir.as_os_str() {
            return Err(errd(
                Code::InvalidInput,
                format!("shadow dir is not a canonical path: {}", dir.display()),
                json!({ "path": path_str(dir), "reason": "not-canonical" }),
            ));
        }
        if self.inside_main(dir) {
            return Err(errd(
                Code::InvalidInput,
                format!("shadow dir {} is inside the main checkout", dir.display()),
                json!({ "path": path_str(dir), "reason": "inside-main" }),
            ));
        }
        let parent = dir.parent().unwrap_or(Path::new("/"));
        match fsx::lstat(parent)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("parent of shadow dir does not exist: {}", parent.display()),
                    json!({ "path": path_str(parent) }),
                ));
            }
            Some(m) if !std::fs::metadata(parent).map(|m| m.is_dir()).unwrap_or(m.is_dir()) => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("parent of shadow dir is not a directory: {}", parent.display()),
                    json!({ "path": path_str(parent) }),
                ));
            }
            Some(_) => {}
        }
        let exists = |why: &str| {
            errd(
                Code::Exists,
                format!("{} exists and is not a complete shadow repository of this repo", dir.display()),
                json!({ "path": path_str(dir), "reason": why }),
            )
        };
        if let Some(m) = fsx::lstat(dir)? {
            if !m.is_dir() {
                return Err(exists("not-a-directory"));
            }
            let real = std::fs::canonicalize(dir).map_err(|e| fsx::io_err(&e, "realpath", dir))?;
            return match self.shadow_owned(&real) {
                Ok(Owned::Yes) => Ok(Shadow { dir: real }),
                Ok(Owned::No(r)) => Err(exists(r)),
                Err(_) => Err(exists("unreadable")),
            };
        }
        let common = &self.inner.common_dir;
        let exclude = fsx::read_human_typed(&common.join("info/exclude"))?.map(|x| x.0);
        let attributes = fsx::read_human_typed(&common.join("info/attributes"))?.map(|x| x.0);
        let main_list = self.local_config_list(None)?;

        // Step 2 onwards: any failure is Unknown, `dir` kept (G4).
        self.build_shadow(dir, exclude, attributes, &main_list).map_err(|e| as_unknown(e, "building the shadow repository"))
    }

    fn build_shadow(
        &self,
        dir: &Path,
        exclude: Option<Vec<u8>>,
        attributes: Option<Vec<u8>>,
        main_list: &[ListEntry],
    ) -> Result<Shadow> {
        let parent = dir.parent().unwrap_or(Path::new("/"));
        let mut cmd = self.command();
        cmd.current_dir(parent).args([
            "-c",
            "init.defaultRefFormat=files",
            "init",
            "--quiet",
            "--bare",
            "--template=",
            "--object-format=sha1",
        ]);
        cmd.arg(dir);
        let out = self.run(cmd, Opts::default().timeout(QUERY_TIMEOUT))?;
        if !out.ok() {
            return Err(failed("init", &out));
        }
        let real = std::fs::canonicalize(dir).map_err(|e| fsx::io_err(&e, "realpath", dir))?;

        // Step 3: alternates.
        let objects = std::fs::canonicalize(self.inner.common_dir.join("objects"))
            .map_err(|e| fsx::io_err(&e, "realpath", &self.inner.common_dir.join("objects")))?;
        mkdir_ok_if_exists(&real, "objects/info", 0o755)?;
        let mut alt = objects.as_os_str().as_bytes().to_vec();
        alt.push(b'\n');
        let f = fsx::write_new_rooted(&real, "objects/info/alternates", &alt, 0o644)?;
        drop(f);

        // Step 4: mirrors, taken this once.
        if exclude.is_some() || attributes.is_some() {
            mkdir_ok_if_exists(&real, "info", 0o755)?;
        }
        if let Some(b) = &exclude {
            fsx::write_new_rooted(&real, "info/exclude", b, 0o644)?;
        }
        if let Some(b) = &attributes {
            fsx::write_new_rooted(&real, "info/attributes", b, 0o644)?;
        }

        // Step 5: the pre-push guard.
        mkdir_ok_if_exists(&real, "hooks", 0o755)?;
        let f = fsx::write_new_rooted(&real, "hooks/pre-push", PRE_PUSH, 0o755)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|e| fsx::io_err(&e, "chmod", &real.join("hooks/pre-push")))?;
        drop(f);

        // Step 6: durability of everything so far, then one full fsync.
        self.fsync_tree(&real, "")?;
        let root = strata_apfs::open_root(&real)?;
        strata_apfs::full_fsync(&root)?;
        self.inner.seams.full_fsync(&real);

        // Step 7: config, last; the marker makes the shadow complete.
        let mut entries: Vec<ListEntry> = vec![
            (b"core.repositoryformatversion".to_vec(), Some(b"0".to_vec())),
            (b"core.bare".to_vec(), Some(b"true".to_vec())),
        ];
        entries.extend(main_list.iter().filter(|(k, _)| is_copied(k)).cloned());
        let mut hooks = real.as_os_str().as_bytes().to_vec();
        hooks.extend_from_slice(b"/hooks");
        entries.push((b"core.hooksPath".to_vec(), Some(hooks)));
        entries.push((b"strata.shadow".to_vec(), Some(b"true".to_vec())));
        let body = render_config(&entries);
        let tmp = format!("config.strata-{}", fsx::nonce());
        let mut f = fsx::write_new_rooted(&real, &tmp, &body, 0o644)?;
        f.flush().map_err(|e| fsx::io_err(&e, "write", &real.join(&tmp)))?;
        fsync_file(&f, &real.join(&tmp))?;
        self.inner.seams.fsync(&real.join(&tmp));
        drop(f);
        // apfs has no overwriting rename: swap, then drop init's config.
        strata_apfs::rename_rooted(&real, &fsx::rp(&tmp)?, &fsx::rp("config")?, RenameMode::Swap)?;
        strata_apfs::remove_rooted(&real, &fsx::rp(&tmp)?, RemoveKind::File)?;
        let root = strata_apfs::open_root(&real)?;
        fsync_file(&root, &real)?;
        strata_apfs::full_fsync(&root)?;
        self.inner.seams.full_fsync(&real);
        Ok(Shadow { dir: real })
    }

    /// fsync every file and directory under `root/rel` (children first).
    fn fsync_tree(&self, root: &Path, rel: &str) -> Result<()> {
        let here = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
        let rd = std::fs::read_dir(&here).map_err(|e| fsx::io_err(&e, "read_dir", &here))?;
        let mut names: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let child = if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
            let ft = e.file_type().map_err(|er| fsx::io_err(&er, "stat", &root.join(&child)))?;
            if ft.is_dir() {
                self.fsync_tree(root, &child)?;
            } else if ft.is_file() {
                let f = strata_apfs::open_rooted(root, &fsx::rp(&child)?)?;
                fsync_file(&f, &root.join(&child))?;
                self.inner.seams.fsync(&root.join(&child));
            }
        }
        let f = if rel.is_empty() { strata_apfs::open_root(root)? } else { strata_apfs::open_rooted(root, &fsx::rp(rel)?)? };
        fsync_file(&f, &here)?;
        self.inner.seams.fsync(&here);
        Ok(())
    }

    /// `git config --local --null --list` of the main checkout (`None`) or
    /// a shadow repository.
    fn local_config_list(&self, shadow: Option<&Path>) -> Result<Vec<ListEntry>> {
        let cmd = match shadow {
            None => {
                let mut c = self.repo_command();
                c.args(["config", "--local", "--null", "--list"]);
                c
            }
            Some(dir) => {
                let mut c = self.command();
                c.current_dir(dir).arg(flag("--git-dir=", dir)).args(["config", "--local", "--null", "--list"]);
                c
            }
        };
        let out = self.run(cmd, Opts::default().timeout(QUERY_TIMEOUT))?;
        if !out.ok() {
            return Err(failed("config --list", &out));
        }
        Ok(parse_null_list(&out.stdout))
    }

    /// Recognise the shadow repository at `dir`, read-only (§5.6).
    pub fn shadow_at(&self, dir: &Path) -> Result<Shadow> {
        fsx::require_absolute(dir, "shadow dir")?;
        let m = match fsx::lstat(dir)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("shadow repository does not exist: {}", dir.display()),
                    json!({ "path": path_str(dir) }),
                ));
            }
            Some(m) => m,
        };
        let not_ours = |reason: &str| {
            errd(
                Code::InvalidInput,
                format!("{} is not a shadow repository of this repo", dir.display()),
                json!({ "path": path_str(dir), "reason": reason }),
            )
        };
        if !m.is_dir() {
            return Err(not_ours("not-a-directory"));
        }
        let real = std::fs::canonicalize(dir).map_err(|e| fsx::io_err(&e, "realpath", dir))?;
        match self.shadow_owned(&real)? {
            Owned::Yes => Ok(Shadow { dir: real }),
            Owned::No(r) => Err(not_ours(r)),
        }
    }

    /// Differences between the shadow's mirrors and the main checkout
    /// today, and whether the pre-push guard may be off (§5.6). Read-only.
    pub fn shadow_drift(&self, shadow: &Shadow, workspace: &Path) -> Result<Vec<MirrorDrift>> {
        self.require_owned(shadow)?;
        fsx::require_absolute(workspace, "workspace")?;
        let mut drift = Vec::new();

        // 1. Mirrors, byte for byte.
        for (rel, item) in [("info/exclude", MirrorDrift::Exclude), ("info/attributes", MirrorDrift::Attributes)] {
            let main = fsx::read_human(&self.inner.common_dir.join(rel))?.map(|x| x.0);
            let main = match main {
                Some(b) => Side::Bytes(b),
                None => Side::Absent,
            };
            let ours = match fsx::read_rooted(&shadow.dir, rel) {
                Ok(Some((b, _))) => Side::Bytes(b),
                Ok(None) => Side::Absent,
                Err(_) => Side::Bad,
            };
            if ours == Side::Bad || ours != main {
                drift.push(item);
            }
        }

        // Copied keys by value; foreign keys in the shadow list.
        let main_list = self.local_config_list(None)?;
        let shadow_list = self.local_config_list(Some(&shadow.dir))?;
        let group = |l: &[ListEntry]| {
            let mut m: BTreeMap<Vec<u8>, Vec<Option<Vec<u8>>>> = BTreeMap::new();
            for (k, v) in l.iter().filter(|(k, _)| is_copied(k)) {
                m.entry(k.clone()).or_default().push(v.clone());
            }
            m
        };
        let (gm, gs) = (group(&main_list), group(&shadow_list));
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for k in gm.keys().chain(gs.keys()) {
            if gm.get(k) != gs.get(k) {
                keys.insert(String::from_utf8_lossy(k).into_owned());
            }
        }
        for (k, _) in &shadow_list {
            if is_foreign(k) {
                keys.insert(String::from_utf8_lossy(k).into_owned());
            }
        }
        drift.extend(keys.into_iter().map(|key| MirrorDrift::Config { key }));

        // 2. The pre-push guard.
        let dotgit = workspace.join(".git");
        if fsx::lstat(&dotgit)?.is_some() {
            let mut cmd = self.command();
            cmd.current_dir(workspace)
                .arg(flag("--git-dir=", &dotgit))
                .arg(flag("--work-tree=", workspace))
                .args(["rev-parse", "--path-format=absolute", "--git-path", "hooks"]);
            let out = self.run(cmd, Opts::c().timeout(QUERY_TIMEOUT))?;
            if !out.ok() {
                let mut e = failed("rev-parse --git-path hooks", &out);
                if let Some(serde_json::Value::Object(m)) = e.detail.as_mut() {
                    m.insert("reason".into(), json!("hooks-lookup"));
                }
                return Err(e);
            }
            let got = fsx::strip_eol(&out.stdout).to_vec();
            let want = shadow.dir.join("hooks");
            let hooks_path = if got.as_slice() == want.as_os_str().as_bytes() {
                None
            } else {
                Some(String::from_utf8_lossy(&got).into_owned())
            };
            let hook_replaced = !self.hook_intact(&shadow.dir);
            if hooks_path.is_some() || hook_replaced {
                drift.push(MirrorDrift::PrePushGuardDisabled { hooks_path, hook_replaced });
            }
        }
        Ok(drift)
    }

    /// `hooks/pre-push` is still gitio's: same bytes, executable.
    fn hook_intact(&self, dir: &Path) -> bool {
        let Ok(rel) = fsx::rp("hooks/pre-push") else { return false };
        let Ok(f) = strata_apfs::open_rooted_file(dir, &rel) else { return false };
        let Ok(m) = f.metadata() else { return false };
        if m.mode() & 0o111 != 0o111 {
            return false;
        }
        use std::io::Read;
        let mut b = Vec::new();
        if (&f).take(PRE_PUSH.len() as u64 + 1).read_to_end(&mut b).is_err() {
            return false;
        }
        b == PRE_PUSH
    }
}

/// `mkdir_rooted(root, rel)`, an existing directory being fine.
fn mkdir_ok_if_exists(root: &Path, rel: &str, mode: u32) -> Result<()> {
    match strata_apfs::mkdir_rooted(root, &fsx::rp(rel)?, mode, false) {
        Ok(()) => Ok(()),
        Err(e) if e.code == Code::Exists => {
            let a = strata_apfs::attrs_of(root, &fsx::rp(rel)?)?;
            if a.kind == strata_apfs::FileKind::Dir { Ok(()) } else { Err(e) }
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_quoted_like_git() {
        assert_eq!(quote_value(b"true"), b"true");
        assert_eq!(quote_value(b" a"), b"\" a\"");
        assert_eq!(quote_value(b"a "), b"\"a \"");
        assert_eq!(quote_value(b"a#b"), b"\"a#b\"");
        assert_eq!(quote_value(b"a;b"), b"\"a;b\"");
        assert_eq!(quote_value(b"a\"b\\c"), b"a\\\"b\\\\c");
        assert_eq!(quote_value(b"a\nb\tc"), b"a\\nb\\tc");
        assert_eq!(quote_value(b"a\rb"), b"\"a\rb\"");
    }

    #[test]
    fn copy_list() {
        assert!(is_copied(b"core.autocrlf"));
        assert!(is_copied(b"core.checkroundtripencoding"));
        assert!(is_copied(b"filter.lfs.clean"));
        assert!(is_copied(b"diff.x.y.textconv"));
        assert!(!is_copied(b"core.hookspath"));
        assert!(!is_copied(b"core.bare"));
        assert!(!is_copied(b"remote.origin.url"));
        assert!(!is_copied(b"filter"));
        assert!(is_copied(b"merge.conflictstyle"));
    }

    #[test]
    fn renders_sections() {
        let e: Vec<ListEntry> = vec![
            (b"core.bare".to_vec(), Some(b"true".to_vec())),
            (b"filter.a\"b.clean".to_vec(), Some(b"x".to_vec())),
            (b"filter.a\"b.required".to_vec(), None),
            (b"strata.shadow".to_vec(), Some(b"true".to_vec())),
        ];
        assert_eq!(
            String::from_utf8(render_config(&e)).unwrap(),
            "[core]\n\tbare = true\n[filter \"a\\\"b\"]\n\tclean = x\n\trequired\n[strata]\n\tshadow = true\n"
        );
        assert_eq!(parse_null_list(b"a.b\nc\0d.e\0"), vec![(b"a.b".to_vec(), Some(b"c".to_vec())), (b"d.e".to_vec(), None)]);
    }
}

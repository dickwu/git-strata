//! FSEvents flags → `WatchEvent` (§5.7 step 3–4). Pure; unit-tested with
//! synthetic flags.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::WatchEvent;
use crate::casefold::case_fold_eq;

pub(crate) const MUST_SCAN_SUB_DIRS: u32 = 0x1;
pub(crate) const USER_DROPPED: u32 = 0x2;
pub(crate) const KERNEL_DROPPED: u32 = 0x4;
pub(crate) const EVENT_IDS_WRAPPED: u32 = 0x8;
pub(crate) const HISTORY_DONE: u32 = 0x10;
pub(crate) const ROOT_CHANGED: u32 = 0x20;
pub(crate) const MOUNT: u32 = 0x40;
pub(crate) const UNMOUNT: u32 = 0x80;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_CREATED: u32 = 0x100;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_REMOVED: u32 = 0x200;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_INODE_META_MOD: u32 = 0x400;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_RENAMED: u32 = 0x800;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_MODIFIED: u32 = 0x1000;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_XATTR_MOD: u32 = 0x8000;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_IS_FILE: u32 = 0x10000;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_IS_DIR: u32 = 0x20000;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ITEM_CLONED: u32 = 0x400000;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dropped,
    Rescan,
    Ignore,
    Changed,
}

pub(crate) fn classify(flags: u32) -> Kind {
    if flags & MUST_SCAN_SUB_DIRS != 0 && flags & (USER_DROPPED | KERNEL_DROPPED) != 0 {
        Kind::Dropped
    } else if flags & (EVENT_IDS_WRAPPED | ROOT_CHANGED) != 0 {
        Kind::Dropped
    } else if flags & MUST_SCAN_SUB_DIRS != 0 || flags & (MOUNT | UNMOUNT) != 0 {
        Kind::Rescan
    } else if flags & HISTORY_DONE != 0 {
        Kind::Ignore
    } else {
        Kind::Changed
    }
}

/// What the callback needs to translate a batch.
pub(crate) struct Translator {
    /// realpath of the root, no trailing slash.
    pub canonical: Vec<u8>,
    /// The root as the caller passed it, no trailing slash.
    pub root: PathBuf,
    /// Excluded directories, NFC, split into components.
    pub exclude: Vec<Vec<String>>,
    pub case_sensitive: bool,
}

fn trim_slash(b: &[u8]) -> &[u8] {
    let mut b = b;
    while b.len() > 1 && b.ends_with(b"/") {
        b = &b[..b.len() - 1];
    }
    b
}

impl Translator {
    pub(crate) fn new(canonical: &Path, root: &Path, exclude: &[strata_types::RepoPath], case_sensitive: bool) -> Translator {
        Translator {
            canonical: trim_slash(canonical.as_os_str().as_bytes()).to_vec(),
            root: PathBuf::from(OsStr::from_bytes(trim_slash(root.as_os_str().as_bytes()))),
            exclude: exclude.iter().map(|p| p.as_str().nfc().collect::<String>().split('/').map(str::to_string).collect()).collect(),
            case_sensitive,
        }
    }

    /// (path with the caller's root, the part below the root if any).
    fn rebase(&self, raw: &[u8]) -> (PathBuf, Option<Vec<u8>>) {
        let raw = trim_slash(raw);
        let c = &self.canonical[..];
        if raw == c {
            return (self.root.clone(), Some(Vec::new()));
        }
        if raw.len() > c.len() && raw.starts_with(c) && (raw[c.len()] == b'/' || c == b"/") {
            let rest = if c == b"/" { &raw[1..] } else { &raw[c.len() + 1..] };
            return (self.root.join(OsStr::from_bytes(rest)), Some(rest.to_vec()));
        }
        (PathBuf::from(OsStr::from_bytes(raw)), None)
    }

    fn excluded(&self, rel: &[u8]) -> bool {
        if rel.is_empty() || self.exclude.is_empty() {
            return false;
        }
        let rel = String::from_utf8_lossy(rel);
        let comps: Vec<String> = rel.split('/').map(|c| c.nfc().collect()).collect();
        self.exclude.iter().any(|ex| {
            ex.len() <= comps.len()
                && ex.iter().zip(&comps).all(|(a, b)| if self.case_sensitive { a == b } else { case_fold_eq(a, b) })
        })
    }

    /// One callback's events → at most one `Changed` first, then rescans and
    /// drops in event order.
    pub(crate) fn translate<'a>(&self, events: impl IntoIterator<Item = (u32, &'a [u8])>) -> Vec<WatchEvent> {
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        let mut rest = Vec::new();
        for (flags, raw) in events {
            match classify(flags) {
                Kind::Ignore => {}
                Kind::Dropped => rest.push(WatchEvent::Dropped),
                k => {
                    let (path, rel) = self.rebase(raw);
                    if rel.as_deref().is_some_and(|r| self.excluded(r)) {
                        continue;
                    }
                    if k == Kind::Rescan {
                        rest.push(WatchEvent::MustRescan(path));
                    } else if seen.insert(path.clone()) {
                        changed.push(path);
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(rest.len() + 1);
        if !changed.is_empty() {
            out.push(WatchEvent::Changed(changed));
        }
        out.extend(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_types::RepoPath;

    fn tr(exclude: &[&str], cs: bool) -> Translator {
        let ex: Vec<RepoPath> = exclude.iter().map(|s| RepoPath::try_from(s.to_string()).unwrap()).collect();
        Translator::new(Path::new("/private/tmp/x"), Path::new("/tmp/x"), &ex, cs)
    }

    fn one(t: &Translator, flags: u32, p: &str) -> Vec<WatchEvent> {
        t.translate([(flags, p.as_bytes())])
    }

    #[test]
    fn watch_flags_map_to_events() {
        let t = tr(&[], false);
        let root = "/private/tmp/x/";
        for f in [MUST_SCAN_SUB_DIRS | USER_DROPPED, MUST_SCAN_SUB_DIRS | KERNEL_DROPPED, EVENT_IDS_WRAPPED, ROOT_CHANGED] {
            assert_eq!(one(&t, f, root), vec![WatchEvent::Dropped], "{f:#x}");
        }
        assert_eq!(one(&t, MUST_SCAN_SUB_DIRS, "/private/tmp/x/d"), vec![WatchEvent::MustRescan("/tmp/x/d".into())]);
        assert_eq!(one(&t, MOUNT | ITEM_IS_DIR, "/private/tmp/x/m"), vec![WatchEvent::MustRescan("/tmp/x/m".into())]);
        assert_eq!(one(&t, UNMOUNT, "/private/tmp/x/m"), vec![WatchEvent::MustRescan("/tmp/x/m".into())]);
        assert_eq!(one(&t, HISTORY_DONE, ""), vec![]);
        for f in [
            ITEM_CREATED,
            ITEM_REMOVED,
            ITEM_RENAMED,
            ITEM_MODIFIED,
            ITEM_INODE_META_MOD,
            ITEM_XATTR_MOD,
            ITEM_CLONED | ITEM_IS_DIR,
            ITEM_CREATED | ITEM_REMOVED | ITEM_MODIFIED | ITEM_XATTR_MOD | ITEM_IS_FILE,
            ITEM_IS_FILE,
        ] {
            assert_eq!(one(&t, f, "/private/tmp/x/f"), vec![WatchEvent::Changed(vec!["/tmp/x/f".into()])], "{f:#x}");
        }
        // One batch: Changed deduplicated and merged, first; then rescans/drops in order.
        let batch: Vec<(u32, &[u8])> = vec![
            (ITEM_CREATED, b"/private/tmp/x/a"),
            (MUST_SCAN_SUB_DIRS, b"/private/tmp/x/s"),
            (ITEM_MODIFIED, b"/private/tmp/x/b"),
            (MUST_SCAN_SUB_DIRS | USER_DROPPED, b"/private/tmp/x/"),
            (ITEM_REMOVED, b"/private/tmp/x/a"),
            (ITEM_CREATED, b"/private/tmp/x"),
        ];
        assert_eq!(
            t.translate(batch),
            vec![
                WatchEvent::Changed(vec!["/tmp/x/a".into(), "/tmp/x/b".into(), "/tmp/x".into()]),
                WatchEvent::MustRescan("/tmp/x/s".into()),
                WatchEvent::Dropped,
            ]
        );
        // A path outside the canonical prefix is kept as is.
        assert_eq!(one(&t, ITEM_CREATED, "/private/tmp/xy"), vec![WatchEvent::Changed(vec!["/private/tmp/xy".into()])]);
    }

    #[test]
    fn translate_excludes_by_component() {
        let t = tr(&["node_modules", "a/b"], false);
        let c = |p: &str| one(&t, ITEM_CREATED, p);
        assert_eq!(c("/private/tmp/x/node_modules/pkg/x"), vec![]);
        assert_eq!(c("/private/tmp/x/node_modules"), vec![]);
        assert_eq!(c("/private/tmp/x/Node_Modules/y"), vec![], "case-insensitive volume folds");
        assert_eq!(c("/private/tmp/x/a/b/c"), vec![]);
        assert_eq!(c("/private/tmp/x/a/bc").len(), 1);
        assert_eq!(c("/private/tmp/x/node_modules2/y").len(), 1);
        assert_eq!(c("/private/tmp/x/src/node_modules").len(), 1, "anchored at the root");
        assert_eq!(one(&t, MUST_SCAN_SUB_DIRS, "/private/tmp/x/node_modules/p"), vec![]);
        assert_eq!(one(&t, MUST_SCAN_SUB_DIRS | USER_DROPPED, "/private/tmp/x/node_modules/p"), vec![WatchEvent::Dropped]);
        let cs = tr(&["node_modules"], true);
        assert_eq!(one(&cs, ITEM_CREATED, "/private/tmp/x/Node_Modules/y").len(), 1, "case-sensitive: bytes");
        let nfc = tr(&["caf\u{e9}"], true);
        assert_eq!(one(&nfc, ITEM_CREATED, "/private/tmp/x/cafe\u{301}/y"), vec![], "both sides NFC");
    }
}

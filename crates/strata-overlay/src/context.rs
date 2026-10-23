//! §3 `WorkspaceRef` and `Base`, plus the name and fold helpers the scan,
//! manifest and restore share.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use strata_gitio::Git;
use strata_store::Store;
use strata_types::{
    BaseGen, BaseManifest, Code, RepoConfig, RepoName, RepoPath, SessionId, StrataError,
};

use crate::Result;

/// One workspace of one session, as the caller sees it.
#[derive(Clone, Copy)]
pub struct WorkspaceRef<'a> {
    pub session: &'a SessionId,
    pub repo: &'a RepoName,
    /// Absolute path of the workspace (the root every rooted call uses).
    pub root: &'a Path,
    pub config: &'a RepoConfig,
    pub git: &'a Git,
    pub store: &'a Store,
}

/// A base generation with its manifest and its exported directory.
#[derive(Clone, Debug)]
pub struct Base {
    pub r#gen: BaseGen,
    pub manifest: Arc<BaseManifest>,
    pub dir: PathBuf,
}

/// Folds names the way the volume compares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fold {
    pub case_sensitive: bool,
}

impl Fold {
    pub fn key(&self, s: &str) -> String {
        if self.case_sensitive {
            s.to_owned()
        } else {
            strata_apfs::case_fold_key(s)
        }
    }

    /// Case sensitivity of the volume holding `path`; case-insensitive (the
    /// safer reading) when the volume does not answer.
    pub fn of(path: &Path) -> Fold {
        Fold {
            case_sensitive: strata_apfs::volume_caps(path)
                .map(|c| c.case_sensitive)
                .unwrap_or(false),
        }
    }
}

/// X1-X2 of §5.1: `.git` at the root, the deps dirs and the seed caches,
/// matched with the volume's fold (X3, the seed files, lives in the tracker).
#[derive(Clone, Debug)]
pub(crate) struct Excluded {
    fold: Fold,
    keys: Vec<String>,
}

impl Excluded {
    pub fn new(config: &RepoConfig, fold: Fold) -> Excluded {
        let keys = config
            .deps_dirs
            .iter()
            .chain(config.seed_caches.iter())
            .map(|p| fold.key(p.as_str()))
            .collect();
        Excluded { fold, keys }
    }

    /// `path` is the root `.git`, or at or below it or an excluded dir.
    pub fn covers(&self, path: &str) -> bool {
        let first = path.split('/').next().unwrap_or(path);
        if self.fold.key(first) == ".git" {
            return true;
        }
        let k = self.fold.key(path);
        self.keys.iter().any(|x| under(&k, x))
    }
}

/// `k` equals `x` or starts with `x/` (keys of paths, so prefix-closed).
pub(crate) fn under(k: &str, x: &str) -> bool {
    k == x || (k.len() > x.len() && k.starts_with(x) && k.as_bytes()[x.len()] == b'/')
}

/// A directory entry name under `parent` as a `RepoPath`; a name that is not
/// UTF-8 is `NotText` (APFS refuses such names, other volumes may not).
pub(crate) fn child_path(parent: Option<&RepoPath>, name: &OsStr) -> Result<RepoPath> {
    let Some(n) = name.to_str() else {
        let mut e = StrataError::new(
            Code::NotText,
            format!("file name is not UTF-8: {}", name.to_string_lossy()),
        );
        e.detail = Some(
            serde_json::json!({ "name": name.to_string_lossy(), "parent": parent.map(|p| p.as_str()) }),
        );
        return Err(e);
    };
    let s = match parent {
        Some(p) => format!("{}/{n}", p.as_str()),
        None => n.to_owned(),
    };
    RepoPath::try_from(s.clone()).map_err(|_| {
        StrataError::new(Code::InvalidInput, format!("not a repository path: {s}"))
            .with_detail(serde_json::json!({ "path": s }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn non_utf8_names_are_not_text() {
        let e = child_path(None, OsStr::from_bytes(b"a\xffb")).unwrap_err();
        assert_eq!(e.code, Code::NotText);
        let p = RepoPath::try_from("src".to_owned()).unwrap();
        assert_eq!(
            child_path(Some(&p), OsStr::new("x.rs")).unwrap().as_str(),
            "src/x.rs"
        );
    }

    #[test]
    fn excluded_folds_with_the_volume() {
        let cfg: RepoConfig = serde_json::from_value(serde_json::json!({
            "path": "/x", "default_branch": "main",
            "publish": { "branch": "main", "push": false, "confirm": false }
        }))
        .unwrap();
        let ci = Excluded::new(
            &cfg,
            Fold {
                case_sensitive: false,
            },
        );
        assert!(ci.covers("node_modules"));
        assert!(ci.covers("Node_Modules/x"));
        assert!(ci.covers("src-tauri/target/debug"));
        assert!(ci.covers(".GIT/config"));
        assert!(!ci.covers("a/.git/config"));
        assert!(!ci.covers("node_modules_x"));
        assert!(!ci.covers("src-tauri"));
        let cs = Excluded::new(
            &cfg,
            Fold {
                case_sensitive: true,
            },
        );
        assert!(!cs.covers("Node_Modules/x"));
        assert!(cs.covers("node_modules/x"));
    }
}

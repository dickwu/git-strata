//! `open` (§5.2) and the git version check.

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;
use strata_types::Code;

use crate::command::{Opts, base_command, failed, run};
use crate::err::{errd, path_str};
use crate::seams::Seams;
use crate::{FETCH_TIMEOUT, Git, Inner, MIN_GIT_VERSION, Result};

static GIT_VERSION: OnceLock<(u32, u32)> = OnceLock::new();

/// Parse `git version X.Y.Z…` and require at least `MIN_GIT_VERSION`.
pub(crate) fn check_version(text: &str) -> Result<(u32, u32)> {
    let unparsable = || errd(Code::Unsupported, "cannot parse git version", json!({ "version": text.trim() }));
    let rest = text.trim().strip_prefix("git version ").ok_or_else(unparsable)?;
    let ver = rest.split_whitespace().next().ok_or_else(unparsable)?;
    let mut parts = ver.split('.');
    let num = |s: Option<&str>| -> Option<u32> {
        let s = s?;
        let d: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
        d.parse().ok()
    };
    let major = num(parts.next()).ok_or_else(unparsable)?;
    let minor = num(parts.next()).ok_or_else(unparsable)?;
    if (major, minor) < MIN_GIT_VERSION {
        return Err(errd(
            Code::Unsupported,
            format!("git {ver} is older than {}.{}", MIN_GIT_VERSION.0, MIN_GIT_VERSION.1),
            json!({ "version": ver }),
        ));
    }
    Ok((major, minor))
}

fn ensure_version(seams: &Seams, env: &[(OsString, Option<OsString>)]) -> Result<()> {
    if GIT_VERSION.get().is_some() {
        return Ok(());
    }
    let mut cmd = base_command(env);
    cmd.arg("version");
    let out = run(seams, cmd, Opts::c())?;
    if !out.ok() {
        return Err(failed("version", &out));
    }
    let v = check_version(&String::from_utf8_lossy(&out.stdout))?;
    let _ = GIT_VERSION.set(v);
    Ok(())
}

pub(crate) fn open_impl(root: &Path, env: Vec<(OsString, Option<OsString>)>) -> Result<Git> {
    let seams = Arc::new(Seams::default());
    ensure_version(&seams, &env)?;

    let root_real = match std::fs::canonicalize(root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(errd(Code::NoSuchPath, format!("{} does not exist", root.display()), json!({ "path": path_str(root) })));
        }
        Err(e) => {
            return Err(errd(Code::InvalidInput, format!("cannot resolve {}: {e}", root.display()), json!({ "path": path_str(root) })));
        }
    };
    if !root_real.is_dir() {
        return Err(errd(Code::InvalidInput, format!("{} is not a directory", root.display()), json!({ "path": path_str(root) })));
    }

    let mut cmd = base_command(&env);
    cmd.arg("-C").arg(root).args([
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-common-dir",
        "--show-object-format",
    ]);
    let out = run(&seams, cmd, Opts::c())?;
    let not_repo = |why: String| errd(Code::InvalidInput, why, json!({ "path": path_str(root) }));
    if !out.ok() {
        return Err(not_repo(format!(
            "{} is not the work tree of a git repository: {}",
            root.display(),
            out.stderr_str().trim_end()
        )));
    }
    let stdout = String::from_utf8(out.stdout.clone()).map_err(|_| not_repo("non-UTF-8 repository path".into()))?;
    let lines: Vec<&str> = stdout.lines().collect();
    if lines.len() != 3 {
        return Err(not_repo(format!("{} has no work tree", root.display())));
    }
    let toplevel = std::fs::canonicalize(lines[0]).map_err(|e| not_repo(format!("cannot resolve toplevel: {e}")))?;
    if toplevel != root_real {
        return Err(errd(
            Code::InvalidInput,
            format!("{} is not the root of its work tree ({})", root.display(), toplevel.display()),
            json!({ "path": path_str(root), "toplevel": path_str(&toplevel) }),
        ));
    }
    let common = std::fs::canonicalize(lines[1]).map_err(|e| not_repo(format!("cannot resolve common dir: {e}")))?;

    // strata.shadow and extensions.refStorage from the common dir's config.
    let mut cmd = base_command(&env);
    cmd.current_dir(&common).arg("--git-dir=.").args([
        "config",
        "--local",
        "--null",
        "--get-regexp",
        r"^(strata\.shadow|extensions\.refstorage)$",
    ]);
    let out = run(&seams, cmd, Opts::c())?;
    match out.status {
        Some(0) | Some(1) => {}
        _ => return Err(failed("config", &out)),
    }
    for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let (key, val) = match rec.iter().position(|b| *b == b'\n') {
            Some(i) => (&rec[..i], Some(String::from_utf8_lossy(&rec[i + 1..]).into_owned())),
            None => (rec, None),
        };
        let key = String::from_utf8_lossy(key).to_ascii_lowercase();
        if key == "strata.shadow" && crate::config::parse_bool(val.as_deref()) == Some(true) {
            return Err(errd(
                Code::InvalidInput,
                format!("{} is a strata workspace, not a main checkout", root.display()),
                json!({ "reason": "strata-workspace", "path": path_str(root) }),
            ));
        }
        if key == "extensions.refstorage" {
            let v = val.unwrap_or_default();
            if !v.eq_ignore_ascii_case("files") {
                return Err(errd(
                    Code::Unsupported,
                    format!("ref storage {v} is not supported"),
                    json!({ "refStorage": v }),
                ));
            }
        }
    }
    if lines[2] != "sha1" {
        return Err(errd(
            Code::Unsupported,
            format!("object format {} is not supported", lines[2]),
            json!({ "objectFormat": lines[2] }),
        ));
    }

    let main_worktree = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf)
    } else {
        None
    };
    let inner = Inner {
        root_arg: root.to_path_buf(),
        root_real,
        common_dir: common,
        main_worktree,
        env: Mutex::new(env),
        fetch_timeout: Mutex::new(FETCH_TIMEOUT),
        cache: Mutex::new(Default::default()),
        seams,
    };
    Ok(Git { inner: Arc::new(inner) })
}

impl Git {
    /// Open a repository by the root of its main checkout or of a human's
    /// linked worktree (§5.2).
    pub fn open(root: &Path) -> Result<Git> {
        open_impl(root, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_git_is_unsupported() {
        let e = check_version("git version 2.43.5\n").unwrap_err();
        assert_eq!(e.code, Code::Unsupported);
        assert_eq!(check_version("git version 2.44.0\n").unwrap(), (2, 44));
        assert_eq!(check_version("git version 2.54.0 (Apple Git-157)\n").unwrap(), (2, 54));
        assert_eq!(check_version("git version 3.0.0").unwrap(), (3, 0));
        assert_eq!(check_version("git version 1.99.9").unwrap_err().code, Code::Unsupported);
        assert_eq!(check_version("not git").unwrap_err().code, Code::Unsupported);
    }
}

//! Read-only checks in the staging layout (publish.md §5.5, D3).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::json;
use strata_types::{Code, RepoConfig, RepoPath, StrataError};

use crate::Result;
use crate::stage::remove_layout;

/// D5-style cap for the output tail in a `CheckFailed` detail.
pub const CHECK_OUTPUT_TAIL: usize = 8 * 1024;

/// The number of checks that ran (§3). 0 when none are configured (D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChecksRan {
    pub ran: u32,
}

/// Split one configured command line by config.md §2.4 (the splitter
/// `validate_commands` uses, reached through `install_argv`).
fn argv_of(cfg: &RepoConfig, line: &str) -> Result<Vec<String>> {
    let mut one = cfg.clone();
    one.install = Some(line.to_string());
    one.install_argv()?.ok_or_else(|| StrataError::new(Code::InvalidInput, "empty check command"))
}

fn tail(bytes: &[u8]) -> String {
    let mut s = bytes.len().saturating_sub(CHECK_OUTPUT_TAIL);
    // Do not start inside a UTF-8 sequence (at most 3 continuation bytes).
    for _ in 0..3 {
        if s < bytes.len() && s > 0 && (bytes[s] & 0xC0) == 0x80 {
            s += 1;
        }
    }
    String::from_utf8_lossy(&bytes[s..]).into_owned()
}

/// Run one check with `cwd` = the staged repo directory, no shell, stdin
/// closed, stdout and stderr captured together (in order of arrival per
/// stream; the tail is what the detail keeps).
fn run_one(argv: &[String], cwd: &Path, tmp: &Path) -> std::io::Result<(Option<i32>, Option<i32>, Vec<u8>)> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // check-tmpdir: a temp dir of this publish's own, so a tool cache that
    // lands in $TMPDIR (phpstan without tmpDir) never holds paths of another
    // session or of an earlier, removed layout.
    let mut t = tmp.as_os_str().to_os_string();
    t.push("/");
    cmd.env("TMPDIR", &t).env("TMP", tmp).env("TEMP", tmp);
    // The staging `.git` gitfile decides the repository; no inherited
    // GIT_* variable may point a check at the main checkout.
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    let mut child = cmd.spawn()?;
    let mut err = child.stderr.take().unwrap();
    let errs = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.read_to_end(&mut v);
        v
    });
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out)?;
    let status = child.wait()?;
    out.extend(errs.join().unwrap_or_default());
    use std::os::unix::process::ExitStatusExt;
    Ok((status.code(), status.signal(), out))
}

/// Name of the per-publish temp dir directly under the layout root. `+` is
/// not legal in a repo name, so it can never be a sibling repo's directory.
pub const CHECK_TMP_DIR: &str = "+tmp";

/// `<layout root>/+tmp` for `staged` (`<layout root>/<repo>`), created 0700
/// (an existing one is reused). It leaves with the layout root.
pub fn check_tmp_dir(staged: &Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let layout = staged
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "staged path has no parent"))?;
    let tmp = layout.join(CHECK_TMP_DIR);
    match std::fs::DirBuilder::new().mode(0o700).create(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let m = std::fs::symlink_metadata(&tmp)?;
            if !m.is_dir() {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} is not a directory", tmp.display())));
            }
        }
        Err(e) => return Err(std::io::Error::new(e.kind(), format!("cannot create {}: {e}", tmp.display()))),
    }
    Ok(tmp)
}

/// §3 `check`: `validate_commands` first (a mutating command →
/// `CheckMutates`, nothing runs), then every check in order in `staged`
/// (`<layout root>/<repo>`). A failing check removes the layout root before
/// `CheckFailed` is returned; a removal that cannot finish is `Unknown`.
pub fn check(cfg: &RepoConfig, staged: &Path) -> Result<ChecksRan> {
    cfg.validate_commands()?;
    let mut ran = 0u32;
    let mut tmp: Option<std::path::PathBuf> = None;
    for (index, line) in cfg.checks.iter().enumerate() {
        let argv = argv_of(cfg, line)?;
        let result = match &tmp {
            Some(t) => run_one(&argv, staged, t),
            None => check_tmp_dir(staged).and_then(|t| {
                let r = run_one(&argv, staged, &t);
                tmp = Some(t);
                r
            }),
        };
        ran += 1;
        let failed = match result {
            Ok((Some(0), _, _)) => None,
            Ok((code, signal, out)) => Some(json!({
                "command": line, "index": index, "exit_code": code, "signal": signal, "output_tail": tail(&out),
            })),
            Err(e) => Some(json!({ "command": line, "index": index, "exit_code": null, "spawn_error": e.to_string() })),
        };
        if let Some(mut detail) = failed {
            detail["checks_ran"] = json!(ran);
            let named = StrataError::new(Code::CheckFailed, format!("check `{line}` failed; nothing was committed"))
                .with_detail(detail);
            // §6: a cleanup that fails is `Unknown`; the original code and
            // detail stay in `detail.cause`.
            remove_staged_layout(staged).map_err(|mut u| {
                let d = u.detail.get_or_insert_with(|| json!({}));
                d["cause"] = json!({ "code": named.code.as_str(), "message": named.message, "detail": named.detail });
                u
            })?;
            return Err(named);
        }
    }
    Ok(ChecksRan { ran })
}

/// Remove the layout root that holds `staged` (its parent directory).
pub(crate) fn remove_staged_layout(staged: &Path) -> Result<()> {
    let layout = staged.parent().ok_or_else(|| StrataError::new(Code::InvalidInput, "staged path has no parent"))?;
    let (Some(root), Some(name)) = (layout.parent(), layout.file_name()) else {
        return Err(StrataError::new(Code::InvalidInput, "layout root has no parent"));
    };
    let rel = RepoPath::try_from(name.to_string_lossy().into_owned())?;
    remove_layout(root, &rel)
}

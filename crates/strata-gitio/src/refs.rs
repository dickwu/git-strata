//! Refs under `refs/strata/` and ancestry (§5.10).

use serde_json::json;
use strata_types::{Code, CommitOid, StrataError};

use crate::command::{Opts, Out, failed};
use crate::err::errd;
use crate::{Git, Result};

pub(crate) const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// A local approximation of `git check-ref-format` for names gitio reads
/// or writes, restricted to `refs/strata/`.
pub(crate) fn check_strata_ref(name: &str) -> Result<()> {
    let bad = |why: &str| -> StrataError {
        errd(Code::InvalidInput, format!("ref {name:?} refused: {why}"), json!({ "ref": name }))
    };
    let Some(rest) = name.strip_prefix("refs/strata/") else {
        return Err(bad("not under refs/strata/"));
    };
    if rest.is_empty() {
        return Err(bad("empty name"));
    }
    if !ref_format_ok(name) {
        return Err(bad("not a valid ref name"));
    }
    Ok(())
}

/// git's ref-name rules (check-ref-format without options).
pub(crate) fn ref_format_ok(name: &str) -> bool {
    if name.is_empty() || name.ends_with('/') || name.ends_with('.') || name == "@" {
        return false;
    }
    if name.contains("..") || name.contains("@{") || name.contains("//") {
        return false;
    }
    for c in name.bytes() {
        if c < 0x20 || c == 0x7f || matches!(c, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\') {
            return false;
        }
    }
    for comp in name.split('/') {
        if comp.is_empty() || comp.starts_with('.') || comp.ends_with(".lock") {
            return false;
        }
    }
    true
}

pub(crate) fn parse_oid(out: &[u8], what: &str) -> Result<CommitOid> {
    let s = String::from_utf8_lossy(out);
    let line = s.lines().next().unwrap_or("").trim();
    CommitOid::try_from(line.to_string()).map_err(|_| {
        errd(Code::Unknown, format!("git {what} printed no commit id"), json!({ "stdout": s.to_string() }))
    })
}

impl Git {
    /// The commit `name` (under `refs/strata/`) points at, or `None`.
    pub fn read_ref(&self, name: &str) -> Result<Option<CommitOid>> {
        check_strata_ref(name)?;
        self.resolve_commit(name)
    }

    /// `rev-parse --verify --quiet <rev>^{commit}`: exit 1 → None.
    pub(crate) fn resolve_commit(&self, rev: &str) -> Result<Option<CommitOid>> {
        let mut c = self.repo_command();
        c.args(["rev-parse", "--verify", "--quiet", "--end-of-options"]).arg(format!("{rev}^{{commit}}"));
        let out = self.run(c, Opts::c())?;
        match out.status {
            Some(0) => Ok(Some(parse_oid(&out.stdout, "rev-parse")?)),
            Some(1) => Ok(None),
            _ => Err(failed("rev-parse", &out)),
        }
    }

    /// Whether `ancestor` is an ancestor of (or equal to) `descendant`.
    pub fn is_ancestor(&self, ancestor: &CommitOid, descendant: &CommitOid) -> Result<bool> {
        let mut c = self.repo_command();
        c.args(["merge-base", "--is-ancestor", ancestor.as_str(), descendant.as_str()]);
        let out = self.run(c, Opts::default())?;
        match out.status {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(self.missing_commit_or(&[ancestor, descendant], "merge-base --is-ancestor", &out)),
        }
    }

    /// A best common ancestor (the one git prints), or `None`.
    pub fn merge_base(&self, a: &CommitOid, b: &CommitOid) -> Result<Option<CommitOid>> {
        let mut c = self.repo_command();
        c.args(["merge-base", a.as_str(), b.as_str()]);
        let out = self.run(c, Opts::default())?;
        match out.status {
            Some(0) => Ok(Some(parse_oid(&out.stdout, "merge-base")?)),
            Some(1) if out.stdout.iter().all(|b| b.is_ascii_whitespace()) => Ok(None),
            _ => Err(self.missing_commit_or(&[a, b], "merge-base", &out)),
        }
    }

    /// `NoSuchCommit` for the first of `oids` that is not a commit here,
    /// else `Unknown` for the failed command.
    pub(crate) fn missing_commit_or(&self, oids: &[&CommitOid], what: &str, out: &Out) -> StrataError {
        for o in oids {
            match self.resolve_commit(o.as_str()) {
                Ok(None) => {
                    return errd(
                        Code::NoSuchCommit,
                        format!("commit {} is not in the repository", o.as_str()),
                        json!({ "commit": o.as_str() }),
                    );
                }
                Ok(Some(_)) => {}
                Err(e) => return e,
            }
        }
        failed(what, out)
    }

    /// Compare-and-swap `name` from `old` (None = absent) to `new`.
    pub fn update_ref(&self, name: &str, new: &CommitOid, old: Option<&CommitOid>) -> Result<()> {
        check_strata_ref(name)?;
        let mut c = self.repo_command();
        c.args(["update-ref", "-m", "strata", "--end-of-options", name, new.as_str()])
            .arg(old.map(|o| o.as_str()).unwrap_or(ZERO_OID));
        let out = self.run(c, Opts::c())?;
        if out.ok() {
            return Ok(());
        }
        Err(self.cas_failure(name, old, "update-ref", &out))
    }

    /// Compare-and-swap delete of `name` (must currently be `old`).
    pub fn delete_ref(&self, name: &str, old: &CommitOid) -> Result<()> {
        check_strata_ref(name)?;
        let mut c = self.repo_command();
        c.args(["update-ref", "-m", "strata", "-d", "--end-of-options", name, old.as_str()]);
        let out = self.run(c, Opts::c())?;
        if out.ok() {
            return Ok(());
        }
        Err(self.cas_failure(name, Some(old), "update-ref -d", &out))
    }

    fn cas_failure(&self, name: &str, expected: Option<&CommitOid>, what: &str, out: &Out) -> StrataError {
        let stderr = out.stderr_str();
        let actual = match self.read_ref(name) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let moved = stderr.contains("but expected")
            || stderr.contains("reference already exists")
            || actual.as_ref() != expected;
        if !moved {
            return failed(what, out);
        }
        errd(
            Code::HeadMoved,
            format!("{name} moved"),
            json!({
                "ref": name,
                "expected": expected.map(|o| o.as_str()),
                "actual": actual.as_ref().map(|o| o.as_str()),
            }),
        )
    }
}


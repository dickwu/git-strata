//! Bounded background fetch: network into a staging ref with no daemon
//! lock, then a try-locked fast-forward CAS against the pre-fetch value.
use crate::{
    DaemonWarning, Result,
    main_sync::{Job, with_deps},
    startup::Core,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};
use strata_gitio::{Git, RefUpdate, remote_ref};
use strata_publish::main_follow::{self, TempTree};
use strata_types::{Code, CommitOid, RepoName, StrataError};

#[derive(Debug, PartialEq, Eq)]
pub enum FetchStep {
    Skipped,
    Fetched,
}
pub fn staging_ref(branch: &str) -> String {
    format!("refs/strata/fetch/{branch}")
}

pub fn fetch_due(core: &Core, repo: &RepoName, now: SystemTime) -> Result<FetchStep> {
    fetch_due_inner(core, repo, now, &mut |git, branch| {
        fetch_staged(core, git, branch)
    })
}
#[cfg(feature = "test-seams")]
pub fn fetch_due_with(
    core: &Core,
    repo: &RepoName,
    now: SystemTime,
    fetch: &mut dyn FnMut(&Git, &str) -> Result<CommitOid>,
) -> Result<FetchStep> {
    fetch_due_inner(core, repo, now, fetch)
}
fn fetch_due_inner(
    core: &Core,
    repo: &RepoName,
    now: SystemTime,
    fetch: &mut dyn FnMut(&Git, &str) -> Result<CommitOid>,
) -> Result<FetchStep> {
    let cfg = core.repo_cfg(repo)?;
    if cfg.sync.fetch_secs == 0 || cfg.publish.branch != cfg.default_branch {
        return Ok(FetchStep::Skipped);
    }
    let Some(_job) = Job::take(&core.main_sync.fetching, repo) else {
        return Ok(FetchStep::Skipped);
    };
    if core.store.repo_fetch(repo)?.is_some_and(|f| {
        now.duration_since(f.at).unwrap_or_default() < Duration::from_secs(cfg.sync.fetch_secs)
    }) {
        return Ok(FetchStep::Skipped);
    }
    let git = core.git(repo)?;
    let tracking = remote_ref("origin", &cfg.default_branch);
    // Existing request fetches hold this lock. Do not start a background
    // fetch while one runs; concurrent starts still cannot overwrite via CAS.
    let expected = match core
        .locks
        .try_repo(repo, "periodic_fetch", || git.read_ref(&tracking))
    {
        Ok(old) => old,
        Err(e) if e.code == Code::Busy => return Ok(FetchStep::Skipped),
        Err(e) => return Err(e),
    };
    core.store
        .record_fetch(repo, false, Some("fetching"), now)?;
    let result = (|| {
        let fetched = fetch(git, &cfg.default_branch)?;
        if let Some(old) = &expected
            && !git.is_ancestor(old, &fetched)?
        {
            return Err(StrataError::new(Code::HeadMoved,"periodic fetch is not a fast-forward").with_detail(serde_json::json!({"reason":"not_fast_forward","expected":old,"fetched":fetched})));
        }
        with_deps(core, repo, |ctx| {
            // Prepare both possible main tips without the repo lock. A chain
            // changed concurrently is discarded under the lock, never hashed there.
            let mut prepared = vec![main_follow::prepare_at(ctx, &fetched)?];
            if cfg.pin_unpushed
                && let Some(top) = git.read_ref(&strata_publish::chain_ref(&cfg.default_branch))?
                && top != fetched
            {
                prepared.push(main_follow::prepare_at(ctx, &top)?);
            }
            core.locks.try_repo(repo, "periodic_fetch", || {
                git.ref_transaction(&[
                    RefUpdate::verify(staging_ref(&cfg.default_branch), Some(fetched.clone())),
                    RefUpdate::set(&tracking, fetched.clone(), expected.clone()),
                ])?;
                crate::fetch_after::advance_anchor_locked(git, &cfg.default_branch)?;
                let tip = strata_deps::main_tip(ctx.cfg, git)?;
                if let Some(p) = prepared.iter().find(|p| Some(&p.head) == tip.as_ref()) {
                    // Observation is required even when source following is off.
                    strata_deps::observe_main_locked(ctx, &p.tree, &p.head, &p.inputs, &p.deps)?;
                    main_follow::reconcile_locked(ctx, p)?;
                }
                Ok(())
            })
        })
    })();
    core.store.record_fetch(
        repo,
        result.is_ok(),
        result.as_ref().err().map(|e| e.message.as_str()),
        now,
    )?;
    match result {
        Ok(()) => {
            core.warnings
                .remove(|w| matches!(w,DaemonWarning::FetchFailed{repo:r} if r==repo));
            Ok(FetchStep::Fetched)
        }
        Err(e) => {
            core.warnings
                .add(DaemonWarning::FetchFailed { repo: repo.clone() });
            Err(e)
        }
    }
}

// gitio's public fetch always updates the tracking ref. This bounded runner
// stays in the daemon lane and uses the same sanitized environment/flags,
// but writes only the staging ref. Network never runs inside a daemon lock.
fn fetch_staged(core: &Core, git: &Git, branch: &str) -> Result<CommitOid> {
    let temp = TempTree::new(&core.config.paths.root)?;
    let log = temp.path();
    let stderr = File::create(&log).map_err(|e| StrataError::new(Code::Unknown, e.to_string()))?;
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("GIT_") || upper == "STRATA_PUBLISH" {
            cmd.env_remove(key);
        }
    }
    cmd.current_dir(git.common_dir())
        .args([
            "--no-replace-objects",
            "-c",
            "core.fsmonitor=false",
            "--git-dir=.",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--no-write-commit-graph",
            "--no-prune",
            "--no-recurse-submodules",
            "--refmap=",
            "--end-of-options",
            "origin",
        ])
        .arg(format!("+refs/heads/{branch}:{}", staging_ref(branch)))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0);
    let mut child = cmd.spawn().map_err(|e| {
        StrataError::new(Code::Unknown, format!("cannot start periodic fetch: {e}"))
    })?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < Duration::from_secs(120) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            result => {
                // SAFETY: this is the process group created for our own child.
                unsafe {
                    libc::killpg(child.id() as i32, libc::SIGKILL);
                }
                let _ = child.wait();
                return Err(StrataError::new(
                    Code::Unknown,
                    format!("periodic fetch timed out or could not be waited: {result:?}"),
                ));
            }
        }
    };
    if !status.success() {
        let mut tail = String::new();
        if let Ok(mut f) = File::open(&log) {
            let _ = f
                .seek(SeekFrom::End(-4096))
                .or_else(|_| f.seek(SeekFrom::Start(0)));
            let _ = f.read_to_string(&mut tail);
        }
        return Err(StrataError::new(
            Code::Unknown,
            format!("periodic fetch failed: {tail}"),
        ));
    }
    git.read_ref(&staging_ref(branch))?
        .ok_or_else(|| StrataError::new(Code::Unknown, "periodic fetch left no staging ref"))
}

//! The one place git children are built and spawned (§5.1).
//!
//! Provenance: the environment scrub (`GIT_*` removed by upper-cased key)
//! and the `current_dir(common) + --git-dir=.` convention follow tgrep's
//! `generations/git.rs` as described in docs/modules/gitio.md §5.1; the
//! tgrep sources were not available to this builder, so the code here is a
//! fresh implementation of the spec (see THIRD-PARTY-NOTICES).

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use strata_types::Code;

use crate::err::{errd, lossy};
use crate::seams::Seams;
use crate::{Git, Result};

/// Bytes of stderr kept for classification and `detail` (§5.1 step 6).
const STDERR_TAIL: usize = 4096;
/// Timeout of the read-only queries that read live shadow config (§5.1 step 5).
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout of `git init` of a shadow repository. It only writes a few
/// small files, but a volume busy with a large `clonefile` (a seed cache of
/// hundreds of GB) or a pending macOS permission prompt stalls those
/// metadata writes for tens of seconds; 10 s failed `new` (incident
/// 2026-10-27, s-kooeepw3).
pub(crate) const INIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Timeout of attach's read-tree and refresh.
pub(crate) const ATTACH_TIMEOUT: Duration = Duration::from_secs(600);
/// How long after git exits its pipes may stay open when the run has no
/// timeout (a hook's background child holding them, review P1-11).
const DRAIN_GRACE: Duration = Duration::from_secs(10);
/// The least time draining gets after git exits, even past the deadline.
const DRAIN_MIN: Duration = Duration::from_millis(500);
/// After killing the process group, how long to wait for the pipes to close.
const DRAIN_AFTER_KILL: Duration = Duration::from_secs(1);

/// Options for one run.
#[derive(Default)]
pub(crate) struct Opts {
    /// Set `LC_ALL=C` (commands whose stderr gitio classifies).
    pub lc_all: bool,
    /// Bytes to pipe to stdin; otherwise stdin is `/dev/null`.
    pub stdin: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
}

impl Opts {
    pub(crate) fn c() -> Opts {
        Opts { lc_all: true, ..Opts::default() }
    }
    pub(crate) fn timeout(mut self, t: Duration) -> Opts {
        self.timeout = Some(t);
        self
    }
    pub(crate) fn stdin(mut self, b: Vec<u8>) -> Opts {
        self.stdin = Some(b);
        self
    }
}

#[derive(Debug)]
pub(crate) struct Out {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Out {
    pub(crate) fn ok(&self) -> bool {
        self.status == Some(0)
    }
    pub(crate) fn stderr_str(&self) -> String {
        lossy(&self.stderr)
    }
}

/// A sanitized `git` command with the given extra environment applied after
/// the scrub (§5.1 steps 1–2).
pub(crate) fn base_command(extra_env: &[(OsString, Option<OsString>)]) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        let upper = k.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("GIT_") || upper == "STRATA_PUBLISH" {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("--no-replace-objects").arg("-c").arg("core.fsmonitor=false");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_NO_LAZY_FETCH", "1");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    for (k, v) in extra_env {
        match v {
            Some(v) => {
                cmd.env(k, v);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    cmd
}

/// Spawn, feed stdin, collect output, enforce the timeout by killing the
/// process group. Every spawn is recorded first.
pub(crate) fn run(seams: &Seams, mut cmd: Command, opts: Opts) -> Result<Out> {
    if opts.lc_all {
        cmd.env("LC_ALL", "C");
    }
    let piped = opts.stdin.is_some();
    cmd.stdin(if piped { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    seams.record_call(&cmd, piped);
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let mut child = cmd.spawn().map_err(|e| {
        errd(
            Code::Unknown,
            format!("cannot run git: {e}"),
            json!({ "reason": "spawn", "args": args, "errno": e.raw_os_error() }),
        )
    })?;
    let pid = child.id() as libc::pid_t;

    let stdin_thread = match (opts.stdin, child.stdin.take()) {
        (Some(bytes), Some(mut sink)) => Some(thread::spawn(move || {
            let _ = sink.write_all(&bytes);
            drop(sink);
        })),
        _ => None,
    };
    let mut so = child.stdout.take().expect("stdout piped");
    let mut se = child.stderr.take().expect("stderr piped");
    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let out_thread = {
        let buf = stdout_buf.clone();
        thread::spawn(move || {
            let mut chunk = [0u8; 64 * 1024];
            loop {
                match so.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.lock().unwrap_or_else(|p| p.into_inner()).extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
    };
    let err_thread = {
        let buf = stderr_buf.clone();
        thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match se.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut tail = buf.lock().unwrap_or_else(|p| p.into_inner());
                        tail.extend_from_slice(&chunk[..n]);
                        if tail.len() > STDERR_TAIL * 2 {
                            let cut = tail.len() - STDERR_TAIL;
                            tail.drain(..cut);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
    };

    let mut timed_out = false;
    let deadline = opts.timeout.map(|t| Instant::now() + t);
    let status = match deadline {
        None => child.wait(),
        Some(deadline) => {
            let mut nap = Duration::from_millis(1);
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Ok(s),
                    Ok(None) => {}
                    Err(e) => break Err(e),
                }
                let now = Instant::now();
                if now >= deadline {
                    timed_out = true;
                    // SAFETY: plain syscall on the child's own process group.
                    unsafe {
                        libc::killpg(pid, libc::SIGKILL);
                    }
                    break child.wait();
                }
                thread::sleep(nap.min(deadline - now));
                nap = (nap * 2).min(Duration::from_millis(50));
            }
        }
    };
    // Review P1-11: git has exited, but a hook may have left a background
    // child holding stdout/stderr (or stdin). Draining is bounded too: by
    // the run's own deadline (at least DRAIN_MIN past git's exit), or by
    // DRAIN_GRACE when the run has none. Past it the rest of git's process
    // group is killed, and after DRAIN_AFTER_KILL the output read so far is
    // used and the reader threads are left to end on their own (a straggler
    // that left the group can still hold a pipe).
    let exited = Instant::now();
    let drain_by = match deadline {
        Some(d) if !timed_out => d.max(exited + DRAIN_MIN),
        Some(_) => exited + DRAIN_MIN,
        None => exited + DRAIN_GRACE,
    };
    let mut threads: Vec<thread::JoinHandle<()>> = vec![out_thread, err_thread];
    threads.extend(stdin_thread);
    if !join_by(&mut threads, drain_by) {
        // SAFETY: plain syscall on the child's own process group.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
        join_by(&mut threads, Instant::now() + DRAIN_AFTER_KILL);
    }
    let stdout = std::mem::take(&mut *stdout_buf.lock().unwrap_or_else(|p| p.into_inner()));
    let mut stderr = std::mem::take(&mut *stderr_buf.lock().unwrap_or_else(|p| p.into_inner()));
    if stderr.len() > STDERR_TAIL {
        let cut = stderr.len() - STDERR_TAIL;
        stderr.drain(..cut);
    }
    if timed_out {
        return Err(errd(
            Code::Unknown,
            format!("git {} timed out", args.iter().find(|a| !a.starts_with('-') && !a.contains('=')).cloned().unwrap_or_default()),
            json!({ "reason": "timeout", "args": args, "stderr": lossy(&stderr) }),
        ));
    }
    let status = status.map_err(|e| {
        errd(Code::Unknown, format!("waiting for git failed: {e}"), json!({ "reason": "wait" }))
    })?;
    Ok(Out { status: status.code(), stdout, stderr })
}

/// Join every thread in `ts` that finishes by `by`; `true` when all did.
/// Unfinished handles stay in `ts` (dropping one detaches the thread).
fn join_by(ts: &mut Vec<thread::JoinHandle<()>>, by: Instant) -> bool {
    loop {
        let mut i = 0;
        while i < ts.len() {
            if ts[i].is_finished() {
                let _ = ts.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
        if ts.is_empty() {
            return true;
        }
        let now = Instant::now();
        if now >= by {
            return false;
        }
        thread::sleep((by - now).min(Duration::from_millis(5)));
    }
}

/// `Unknown` for a git that exited non-zero where no specific class applies.
pub(crate) fn failed(what: &str, out: &Out) -> strata_types::StrataError {
    errd(
        Code::Unknown,
        format!("git {what} failed: {}", out.stderr_str().trim_end()),
        json!({ "exit": out.status, "stderr": out.stderr_str() }),
    )
}

impl Git {
    pub(crate) fn env_snapshot(&self) -> Vec<(OsString, Option<OsString>)> {
        self.inner.env.lock().unwrap().clone()
    }

    /// A sanitized git command (§5.1) carrying this handle's extra env.
    pub(crate) fn command(&self) -> Command {
        base_command(&self.env_snapshot())
    }

    /// A repository-level command: cwd = common dir, `--git-dir=.`.
    pub(crate) fn repo_command(&self) -> Command {
        let mut c = self.command();
        c.current_dir(&self.inner.common_dir).arg("--git-dir=.");
        c
    }

    /// A worktree-level command (no discovery): explicit git dir and tree,
    /// cwd = tree. With `index`, sets `GIT_INDEX_FILE` and disables hooks.
    pub(crate) fn tree_command(&self, git_dir: &Path, tree: &Path, index: Option<&Path>) -> Command {
        let mut c = self.command();
        c.current_dir(tree);
        if let Some(ix) = index {
            c.arg("-c").arg("core.hooksPath=/dev/null");
            c.env("GIT_INDEX_FILE", ix);
        }
        c.arg(flag("--git-dir=", git_dir)).arg(flag("--work-tree=", tree));
        c
    }

    pub(crate) fn run(&self, cmd: Command, opts: Opts) -> Result<Out> {
        run(&self.inner.seams, cmd, opts)
    }

    /// Value of one of this handle's env overrides, falling back to the
    /// process environment (used for HOME / XDG_CONFIG_HOME, §4.3).
    pub(crate) fn env_var(&self, key: &str) -> Option<OsString> {
        let env = self.inner.env.lock().unwrap();
        for (k, v) in env.iter().rev() {
            if k == key {
                return v.clone();
            }
        }
        drop(env);
        std::env::var_os(key)
    }
}

pub(crate) fn flag(prefix: &str, p: &Path) -> OsString {
    let mut s = OsString::from(prefix);
    s.push(p.as_os_str());
    s
}


#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(script);
        c
    }

    /// Review P1-11: a background child holding stdout after the command
    /// exits (a hook's `cmd &`) no longer hangs the run: the output read so
    /// far and the exit status come back, and the straggler is killed.
    #[test]
    fn a_background_child_holding_the_pipes_does_not_hang_the_run() {
        let seams = Seams::default();
        let marker = std::env::temp_dir().join(format!("gitio-drain-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = format!("echo out; echo err >&2; (sleep 3; touch {}) & exit 0", marker.display());
        let t = Instant::now();
        let out = run(&seams, sh(&script), Opts::default().timeout(Duration::from_secs(1))).unwrap();
        assert!(t.elapsed() < Duration::from_secs(3), "bounded by the deadline: {:?}", t.elapsed());
        assert_eq!(out.status, Some(0));
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        std::thread::sleep(Duration::from_secs(4));
        assert!(!marker.exists(), "the straggler was killed with the process group");
    }

    #[test]
    fn without_a_timeout_draining_is_bounded_by_the_grace() {
        let seams = Seams::default();
        let t = Instant::now();
        let out = run(&seams, sh("echo out; sleep 60 & exit 3"), Opts::default()).unwrap();
        let took = t.elapsed();
        assert!(took >= DRAIN_GRACE && took < DRAIN_GRACE + Duration::from_secs(5), "{took:?}");
        assert_eq!(out.status, Some(3));
        assert_eq!(out.stdout, b"out\n");
    }

    #[test]
    fn a_plain_run_still_returns_promptly_with_all_output() {
        let seams = Seams::default();
        let t = Instant::now();
        let out = run(&seams, sh("head -c 300000 /dev/zero; echo done >&2"), Opts::default()).unwrap();
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(out.stdout.len(), 300000);
        assert_eq!(out.stderr, b"done\n");
    }
}

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
/// Timeout of attach's read-tree and refresh.
pub(crate) const ATTACH_TIMEOUT: Duration = Duration::from_secs(600);

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
    let out_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = so.read_to_end(&mut buf);
        buf
    });
    let err_thread = thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match se.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    tail.extend_from_slice(&chunk[..n]);
                    if tail.len() > STDERR_TAIL * 2 {
                        let cut = tail.len() - STDERR_TAIL;
                        tail.drain(..cut);
                    }
                }
            }
        }
        if tail.len() > STDERR_TAIL {
            let cut = tail.len() - STDERR_TAIL;
            tail.drain(..cut);
        }
        tail
    });

    let mut timed_out = false;
    let status = match opts.timeout {
        None => child.wait(),
        Some(t) => {
            let deadline = Instant::now() + t;
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
    if timed_out {
        // Make sure stragglers that kept the pipes open are gone too.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    if let Some(h) = stdin_thread {
        let _ = h.join();
    }
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
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


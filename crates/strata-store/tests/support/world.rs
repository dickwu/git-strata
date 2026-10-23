//! A small session fixture shared by the concurrency and crash tests:
//! deps `a`, generation app/1/a, one active session tracking `app`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use strata_store::*;
use strata_types::{
    BaseGen, BaseManifest, BlobId, Checkpoint, CheckpointId, CheckpointReason, CommitOid, DepsSet, DepsSetId,
    EntryKind, OverlayEntry, RepoName, RepoPath, SessionId, SessionManifest, TreeOid, WorkspaceState,
};

use super::{CHILD_ENV, Env};

pub fn repo() -> RepoName {
    RepoName::try_from("app".to_string()).unwrap()
}
pub fn gen1() -> BaseGen {
    BaseGen {
        repo: repo(),
        tree: TreeOid::try_from("1".repeat(40)).unwrap(),
        deps: DepsSetId::try_from("a".repeat(64)).unwrap(),
    }
}
pub fn sid() -> SessionId {
    SessionId::try_from("s-crashtst".to_string()).unwrap()
}
pub fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}
pub fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}
pub fn cid(n: u64) -> CheckpointId {
    CheckpointId::try_from(n).unwrap()
}

/// Deps `a`, generation app/1/a and an active session `s-crashtst`.
pub fn world(env: &Env, s: &Store) {
    let set = DepsSet {
        id: gen1().deps,
        repo: repo(),
        inputs_hash: "0".repeat(64),
        platform: "darwin-arm64".into(),
        toolchain: "none".into(),
        dirs: vec![],
    };
    s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
    let m = BaseManifest { tree: gen1().tree, entries: BTreeMap::new() };
    s.record_base_gen(&gen1(), &m, t(1)).unwrap();
    let root = env.root.join("sessions").join(sid().as_str());
    let mut repos = BTreeMap::new();
    repos.insert(
        repo(),
        WorkspaceState { base: gen1(), head: CommitOid::try_from("1".repeat(40)).unwrap(), path: root.join("app"), tracked: true },
    );
    s.create_session(&SessionManifest { id: sid(), ticket: None, root, repos }, t(10)).unwrap();
    assert!(s.set_lifecycle(&sid(), Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
}

/// A checkpoint of `app` adding `paths` with `blobs`; reason is a tool call
/// with `call` so replays are idempotent.
pub fn tool_cp(call: &str, entries: Vec<OverlayEntry>) -> Checkpoint {
    Checkpoint {
        id: cid(999),
        session: sid(),
        repo: repo(),
        base: gen1(),
        at: t(50),
        reason: CheckpointReason::ToolCall { id: call.into() },
        entries,
    }
}

pub fn added(p: &str, b: &BlobId) -> OverlayEntry {
    OverlayEntry { path: rp(p), kind: EntryKind::Added { blob: b.clone() }, mode: 0o100644 }
}
pub fn deleted(p: &str) -> OverlayEntry {
    OverlayEntry { path: rp(p), kind: EntryKind::Deleted, mode: 0 }
}

/// A child process re-running `test` of this binary, with piped stdio.
pub struct ChildProc {
    pub child: Child,
    pub out: BufReader<ChildStdout>,
}

pub fn spawn_child(test: &str, envs: &[(&str, &str)]) -> ChildProc {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1", "--include-ignored"])
        .env(CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn child");
    let out = BufReader::new(child.stdout.take().unwrap());
    ChildProc { child, out }
}

impl ChildProc {
    /// Read stdout lines until one starts with `marker`; returns that line.
    pub fn wait_for(&mut self, marker: &str) -> String {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.out.read_line(&mut line).unwrap();
            assert!(n > 0, "child exited before printing {marker}");
            // libtest prints "test name ... " without a newline first, so
            // the marker may appear mid-line.
            if let Some(i) = line.find(marker) {
                return line[i..].trim_end().to_string();
            }
        }
    }

    /// Tell the child to go on (it reads one line from stdin).
    pub fn go(&mut self) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(b"go\n").unwrap();
        stdin.flush().unwrap();
    }

    pub fn sigkill(mut self) {
        self.child.kill().unwrap(); // SIGKILL on unix
        self.child.wait().unwrap();
    }

    pub fn finish(mut self) {
        let st = self.child.wait().unwrap();
        assert!(st.success(), "child failed: {st:?}");
    }
}

impl Drop for ChildProc {
    /// Never orphan a child (e.g. when a parent assertion panics).
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// In a child: print a marker line and flush.
pub fn say(line: &str) {
    // Write to the raw stdout handle: `println!` may be captured by libtest.
    let mut o = std::io::stdout().lock();
    o.write_all(format!("\n{line}\n").as_bytes()).unwrap();
    o.flush().unwrap();
}

/// In a child: block until the parent writes a line.
pub fn wait_go() {
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).unwrap();
}

pub fn env_path(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap())
}

/// Store env vars for a child.
pub fn child_env(env: &Env) -> Vec<(&'static str, String)> {
    vec![("STRATA_ROOT", env.root.to_str().unwrap().to_string()), ("STRATA_CHECKOUT", env.checkout.to_str().unwrap().to_string())]
}

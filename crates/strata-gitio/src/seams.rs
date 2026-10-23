//! Test seams (§3 "Test seams"): recorded git calls, recorded sync events
//! and one-shot pause points. Without the `test-seams` feature every hook is
//! a no-op and nothing is recorded.

use std::path::Path;
use std::process::Command;

#[cfg(feature = "test-seams")]
use std::collections::{BTreeMap, HashMap};
#[cfg(feature = "test-seams")]
use std::ffi::{OsStr, OsString};
#[cfg(feature = "test-seams")]
use std::path::PathBuf;
#[cfg(feature = "test-seams")]
use std::sync::{Arc, Condvar, Mutex};

/// One git child process as spawned: argv after `git`, the effective
/// environment, the working directory, and whether stdin was piped.
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCall {
    pub args: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
    pub stdin_piped: bool,
}

/// One durability step, in the order it happened.
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    Fsync(PathBuf),
    FullFsync(PathBuf),
    RenameExcl { from: PathBuf, to: PathBuf },
}

#[cfg(feature = "test-seams")]
#[derive(Default)]
struct PauseState {
    m: Mutex<(bool, bool)>,
    cv: Condvar,
}

/// A one-shot pause point. The next operation that reaches it blocks until
/// `release` (or drop).
#[cfg(feature = "test-seams")]
pub struct Pause {
    state: Arc<PauseState>,
}

#[cfg(feature = "test-seams")]
impl Pause {
    /// Block until an operation has reached the pause point.
    pub fn wait_until_paused(&self) {
        let mut g = self.state.m.lock().unwrap();
        while !g.0 {
            g = self.state.cv.wait(g).unwrap();
        }
    }

    /// Let the paused operation continue.
    pub fn release(self) {
        drop(self);
    }
}

#[cfg(feature = "test-seams")]
impl Drop for Pause {
    fn drop(&mut self) {
        let mut g = self.state.m.lock().unwrap();
        g.1 = true;
        self.state.cv.notify_all();
    }
}

#[derive(Default)]
pub(crate) struct Seams {
    #[cfg(feature = "test-seams")]
    calls: Mutex<Vec<GitCall>>,
    #[cfg(feature = "test-seams")]
    syncs: Mutex<Vec<SyncEvent>>,
    #[cfg(feature = "test-seams")]
    pauses: Mutex<HashMap<&'static str, Arc<PauseState>>>,
}

pub(crate) const EXPORT_BEFORE_RENAME: &str = "export_before_rename";
pub(crate) const ATTACH_BEFORE_INDEX: &str = "attach_before_index";
pub(crate) const ATTACH_BEFORE_READ_TREE: &str = "attach_before_read_tree";
pub(crate) const ATTACH_BEFORE_READBACK: &str = "attach_before_readback";
pub(crate) const PUSH_BEFORE_SEND: &str = "push_before_send";
pub(crate) const PUSH_BEFORE_RECORD: &str = "push_before_record";

impl Seams {
    #[allow(unused_variables)]
    pub(crate) fn record_call(&self, cmd: &Command, stdin_piped: bool) {
        #[cfg(feature = "test-seams")]
        {
            let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
            for (k, v) in cmd.get_envs() {
                match v {
                    Some(v) => {
                        env.insert(k.to_os_string(), v.to_os_string());
                    }
                    None => {
                        env.remove(k);
                    }
                }
            }
            let cwd = cmd
                .get_current_dir()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let args = cmd.get_args().map(OsStr::to_os_string).collect();
            self.calls.lock().unwrap().push(GitCall { args, env, cwd, stdin_piped });
        }
    }

    #[allow(unused_variables)]
    pub(crate) fn fsync(&self, p: &Path) {
        #[cfg(feature = "test-seams")]
        self.syncs.lock().unwrap().push(SyncEvent::Fsync(p.to_path_buf()));
    }

    #[allow(unused_variables)]
    pub(crate) fn full_fsync(&self, p: &Path) {
        #[cfg(feature = "test-seams")]
        self.syncs.lock().unwrap().push(SyncEvent::FullFsync(p.to_path_buf()));
    }

    #[allow(unused_variables)]
    pub(crate) fn rename_excl(&self, from: &Path, to: &Path) {
        #[cfg(feature = "test-seams")]
        self.syncs
            .lock()
            .unwrap()
            .push(SyncEvent::RenameExcl { from: from.to_path_buf(), to: to.to_path_buf() });
    }

    /// Block here if a pause is armed for `point` (one-shot).
    #[allow(unused_variables)]
    pub(crate) fn hit(&self, point: &'static str) {
        #[cfg(feature = "test-seams")]
        {
            let st = self.pauses.lock().unwrap().remove(point);
            if let Some(st) = st {
                let mut g = st.m.lock().unwrap();
                g.0 = true;
                st.cv.notify_all();
                while !g.1 {
                    g = st.cv.wait(g).unwrap();
                }
            }
        }
    }

    #[cfg(feature = "test-seams")]
    pub(crate) fn arm(&self, point: &'static str) -> Pause {
        let st = Arc::new(PauseState::default());
        self.pauses.lock().unwrap().insert(point, st.clone());
        Pause { state: st }
    }

    #[cfg(feature = "test-seams")]
    pub(crate) fn take_calls(&self) -> Vec<GitCall> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    #[cfg(feature = "test-seams")]
    pub(crate) fn take_syncs(&self) -> Vec<SyncEvent> {
        std::mem::take(&mut *self.syncs.lock().unwrap())
    }
}

#[cfg(feature = "test-seams")]
impl crate::Git {
    /// `open` with extra environment for every child git (tests point
    /// `HOME` and `XDG_CONFIG_HOME` at a sandbox). `None` removes a variable.
    pub fn open_with_env(root: &Path, env: &[(&str, Option<&OsStr>)]) -> crate::Result<crate::Git> {
        let env = env.iter().map(|(k, v)| (OsString::from(k), v.map(OsStr::to_os_string))).collect();
        crate::repo::open_impl(root, env)
    }

    /// Set (or with `None`, remove) one variable for later children.
    pub fn set_env(&self, key: &str, value: Option<&OsStr>) {
        let mut env = self.inner.env.lock().unwrap();
        env.retain(|(k, _)| k != key);
        env.push((OsString::from(key), value.map(OsStr::to_os_string)));
    }

    pub fn set_fetch_timeout(&self, timeout: std::time::Duration) {
        *self.inner.fetch_timeout.lock().unwrap() = timeout;
    }

    pub fn take_git_calls(&self) -> Vec<GitCall> {
        self.inner.seams.take_calls()
    }

    pub fn take_sync_events(&self) -> Vec<SyncEvent> {
        self.inner.seams.take_syncs()
    }

    pub fn pause_export_before_rename(&self) -> Pause {
        self.inner.seams.arm(EXPORT_BEFORE_RENAME)
    }

    pub fn pause_attach_before_index(&self) -> Pause {
        self.inner.seams.arm(ATTACH_BEFORE_INDEX)
    }

    pub fn pause_attach_before_read_tree(&self) -> Pause {
        self.inner.seams.arm(ATTACH_BEFORE_READ_TREE)
    }

    pub fn pause_attach_before_readback(&self) -> Pause {
        self.inner.seams.arm(ATTACH_BEFORE_READBACK)
    }

    /// Pause a push right before `git push` is spawned.
    pub fn pause_push_before_send(&self) -> Pause {
        self.inner.seams.arm(PUSH_BEFORE_SEND)
    }

    /// Pause `push_published` after the remote accepted, before the ref
    /// transaction that records it.
    pub fn pause_push_before_record(&self) -> Pause {
        self.inner.seams.arm(PUSH_BEFORE_RECORD)
    }
}

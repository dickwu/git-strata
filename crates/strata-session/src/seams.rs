//! Test seams (session.md §3, addendum 4 "Test seams"): compiled only with
//! the `test-seams` feature. Without it every check is a no-op.

use strata_types::{RepoName, SessionId, StrataError};

use crate::Result;

/// A seam armed at the start of a phase (§3, §6.3 phase names).
#[cfg(feature = "test-seams")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSeam {
    /// Inject an I/O error with this errno at the start of the phase.
    FailAt { phase: String, errno: i32 },
    /// Return at the start of the phase without updating any state: a crash.
    StopAt { phase: String },
    /// Stop at the start of the phase until `release`.
    PauseAt { phase: String },
}

#[cfg(feature = "test-seams")]
#[derive(Default)]
struct PauseState {
    paused: bool,
    released: bool,
}

#[derive(Default)]
pub(crate) struct Seams {
    #[cfg(feature = "test-seams")]
    armed: std::sync::Mutex<Option<SessionSeam>>,
    #[cfg(feature = "test-seams")]
    next_id: std::sync::Mutex<Option<SessionId>>,
    #[cfg(feature = "test-seams")]
    pause: std::sync::Mutex<PauseState>,
    #[cfg(feature = "test-seams")]
    cv: std::sync::Condvar,
}

/// The marker a `StopAt` error carries: callers return it as is, without
/// any failure handling (the process "crashed" here).
const STOP_MARK: &str = "session-seam-stop";

pub(crate) fn is_stop(e: &StrataError) -> bool {
    e.detail
        .as_ref()
        .and_then(|d| d.get("seam"))
        .and_then(|v| v.as_str())
        == Some(STOP_MARK)
}

impl Seams {
    /// The id `new` uses next, if a test fixed one.
    pub(crate) fn take_next_id(&self) -> Option<SessionId> {
        #[cfg(feature = "test-seams")]
        {
            self.next_id.lock().unwrap_or_else(|p| p.into_inner()).take()
        }
        #[cfg(not(feature = "test-seams"))]
        {
            None
        }
    }

    /// Check the seam at the start of `phase` (dispatched for `repo`).
    #[allow(unused_variables)]
    pub(crate) fn hit(&self, phase: &str, repo: Option<&RepoName>) -> Result<()> {
        #[cfg(feature = "test-seams")]
        {
            let seam = {
                let mut armed = self.armed.lock().unwrap_or_else(|p| p.into_inner());
                let matches = |p: &str| match p.split_once('@') {
                    Some((ph, r)) => ph == phase && repo.map(|x| x.as_str()) == Some(r),
                    None => p == phase,
                };
                let hit = match armed.as_ref() {
                    Some(SessionSeam::FailAt { phase: p, .. })
                    | Some(SessionSeam::StopAt { phase: p })
                    | Some(SessionSeam::PauseAt { phase: p }) => matches(p),
                    None => false,
                };
                if hit { armed.take() } else { None }
            };
            match seam {
                None => {}
                Some(SessionSeam::FailAt { errno, .. }) => {
                    let err = std::io::Error::from_raw_os_error(errno);
                    let mut e = strata_apfs::classify_io_error(
                        &err,
                        strata_apfs::IoSide::Write,
                        std::path::Path::new(&format!("seam:{phase}")),
                    );
                    if let Some(serde_json::Value::Object(m)) = &mut e.detail {
                        m.insert("injected".into(), serde_json::json!(phase));
                    }
                    return Err(e);
                }
                Some(SessionSeam::StopAt { .. }) => {
                    return Err(StrataError::new(
                        strata_types::Code::Unknown,
                        format!("stopped at seam {phase}"),
                    )
                    .with_detail(serde_json::json!({ "seam": STOP_MARK, "phase": phase })));
                }
                Some(SessionSeam::PauseAt { .. }) => {
                    let mut st = self.pause.lock().unwrap_or_else(|p| p.into_inner());
                    st.paused = true;
                    st.released = false;
                    self.cv.notify_all();
                    while !st.released {
                        st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner());
                    }
                    st.paused = false;
                    st.released = false;
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "test-seams")]
impl crate::Sessions<'_> {
    /// The next `new` uses this id instead of a random one.
    pub fn set_next_id(&self, id: SessionId) {
        *self.seams.next_id.lock().unwrap_or_else(|p| p.into_inner()) = Some(id);
    }

    /// Arm a seam; it fires once.
    pub fn arm(&self, seam: SessionSeam) {
        *self.seams.armed.lock().unwrap_or_else(|p| p.into_inner()) = Some(seam);
    }

    /// Block until a `PauseAt` has stopped.
    pub fn wait_paused(&self) {
        let mut st = self.seams.pause.lock().unwrap_or_else(|p| p.into_inner());
        while !st.paused {
            st = self.seams.cv.wait(st).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Let the paused step go on.
    pub fn release(&self) {
        let mut st = self.seams.pause.lock().unwrap_or_else(|p| p.into_inner());
        st.released = true;
        // Not paused any more from the caller's view: a following
        // `wait_paused` waits for the next pause, not this one.
        st.paused = false;
        self.seams.cv.notify_all();
    }
}

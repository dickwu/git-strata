//! The real [`ServiceManager`]: `/bin/launchctl` by absolute path, fixed
//! argv, empty environment, stdin from /dev/null, a deadline, capped
//! output. Three verbs exist and no others: `print` (read), `kickstart`
//! without `-k`, `bootstrap` of the legacy plist (gui-daemon-setup §13 Q3:
//! start only). The static test in `tests.rs` keeps it that way.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::ServiceManager;
use crate::error::GuiError;

pub const LAUNCHCTL: &str = "/bin/launchctl";

/// launchctl answers in milliseconds; a hung one is reported, not waited out.
const DEADLINE: Duration = Duration::from_secs(15);
/// Enough for any launchctl message; the rest is read and dropped.
const OUTPUT_CAP: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Print,
    Kickstart,
    Bootstrap,
}

impl Verb {
    pub const ALL: [Verb; 3] = [Verb::Print, Verb::Kickstart, Verb::Bootstrap];

    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Print => "print",
            Verb::Kickstart => "kickstart",
            Verb::Bootstrap => "bootstrap",
        }
    }
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn domain() -> String {
    format!("gui/{}", uid())
}

fn service(label: &str) -> String {
    format!("{}/{label}", domain())
}

/// The argv for one verb. Pure, so the tests check every command line
/// without running anything.
pub fn argv(verb: Verb, label: &str, plist: Option<&Path>) -> Vec<String> {
    match verb {
        Verb::Print | Verb::Kickstart => vec![verb.as_str().to_string(), service(label)],
        Verb::Bootstrap => vec![
            verb.as_str().to_string(),
            domain(),
            plist
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ],
    }
}

pub struct Outcome {
    pub status: Option<i32>,
    pub success: bool,
    pub text: String,
}

fn capped(mut r: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut keep = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = OUTPUT_CAP.saturating_sub(keep.len());
                    keep.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        keep
    })
}

/// Wait for `child` until `deadline`. A launchctl that outlives it is left
/// to finish on its own (a thread reaps it); the GUI sends it no signal.
fn wait(mut child: Child, deadline: Duration) -> Result<std::process::ExitStatus, Child> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st),
            Ok(None) if start.elapsed() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => return Err(child),
        }
    }
}

#[derive(Default)]
pub struct Launchctl;

impl Launchctl {
    pub fn new() -> Launchctl {
        Launchctl
    }

    fn run(&self, verb: Verb, args: Vec<String>) -> Result<Outcome, GuiError> {
        let step = verb.as_str();
        let mut child = Command::new(LAUNCHCTL)
            .args(&args)
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                GuiError::launchctl_failed(step, None, &format!("cannot run {LAUNCHCTL}: {e}"))
            })?;
        let out = child.stdout.take().map(capped);
        let err = child.stderr.take().map(capped);
        let status = match wait(child, DEADLINE) {
            Ok(st) => st,
            Err(mut child) => {
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err(GuiError::launchctl_failed(
                    step,
                    None,
                    &format!("no answer within {} s", DEADLINE.as_secs()),
                ));
            }
        };
        let join = |h: Option<thread::JoinHandle<Vec<u8>>>| {
            h.and_then(|h| h.join().ok()).unwrap_or_default()
        };
        let (out, err) = (join(out), join(err));
        let mut text = String::from_utf8_lossy(&err).into_owned();
        if text.trim().is_empty() {
            text = String::from_utf8_lossy(&out).into_owned();
        }
        Ok(Outcome {
            status: status.code(),
            success: status.success(),
            text,
        })
    }

    fn must(&self, verb: Verb, args: Vec<String>) -> Result<(), GuiError> {
        let o = self.run(verb, args)?;
        if o.success {
            Ok(())
        } else {
            Err(GuiError::launchctl_failed(verb.as_str(), o.status, &o.text))
        }
    }
}

impl ServiceManager for Launchctl {
    fn is_loaded(&self, label: &str) -> Result<bool, GuiError> {
        // Non-zero (113, "could not find service") means not loaded, as in
        // `git-strata daemon start`.
        Ok(self
            .run(Verb::Print, argv(Verb::Print, label, None))?
            .success)
    }

    fn kickstart(&self, label: &str) -> Result<(), GuiError> {
        self.must(Verb::Kickstart, argv(Verb::Kickstart, label, None))
    }

    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        self.must(
            Verb::Bootstrap,
            argv(Verb::Bootstrap, super::AGENT_LABEL, Some(plist)),
        )
    }
}

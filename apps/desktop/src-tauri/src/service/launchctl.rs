//! The real [`ServiceManager`]: `/bin/launchctl` by absolute path, fixed
//! argv, empty environment, stdin from /dev/null, a deadline, and capped
//! diagnostics. `print` and `print-disabled` read complete probe output.
//! `kickstart` (without `-k`) and `bootstrap` start the user agent.
//! `disable`, `bootout` and `enable` support the update and Move fence.
//! The coordinator checks the seal or verifies the daemon is stopped
//! before bootout and keeps the fence until unload has been verified.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use strata_types::Code;

use super::{AGENT_LABEL, ServiceManager, plist_for_home, scope};
use crate::error::GuiError;

pub const LAUNCHCTL: &str = "/bin/launchctl";

/// launchctl answers in milliseconds; a hung one is reported, not waited out.
const DEADLINE: Duration = Duration::from_secs(15);
/// Limit for diagnostics. The structured probes need the full stdout.
const OUTPUT_CAP: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Print,
    PrintDisabled,
    Kickstart,
    Bootstrap,
    Bootout,
    Disable,
    Enable,
}

impl Verb {
    pub const ALL: [Verb; 7] = [
        Verb::Print,
        Verb::PrintDisabled,
        Verb::Kickstart,
        Verb::Bootstrap,
        Verb::Bootout,
        Verb::Disable,
        Verb::Enable,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Print => "print",
            Verb::PrintDisabled => "print-disabled",
            Verb::Kickstart => "kickstart",
            Verb::Bootstrap => "bootstrap",
            Verb::Bootout => "bootout",
            Verb::Disable => "disable",
            Verb::Enable => "enable",
        }
    }

    fn has_structured_stdout(self) -> bool {
        matches!(self, Self::Print | Self::PrintDisabled)
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
        Verb::Print | Verb::Kickstart | Verb::Bootout | Verb::Disable | Verb::Enable => {
            vec![verb.as_str().to_string(), service(label)]
        }
        Verb::PrintDisabled => vec![verb.as_str().to_string(), domain()],
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
    pub stderr: String,
    pub out: String,
}

impl Outcome {
    fn checked(self, verb: Verb) -> Result<Self, GuiError> {
        if self.success {
            Ok(self)
        } else {
            let diagnostic = if self.stderr.trim().is_empty() {
                &self.out
            } else {
                &self.stderr
            };
            Err(GuiError::launchctl_failed(
                verb.as_str(),
                self.status,
                &diagnostic[..diagnostic.floor_char_boundary(OUTPUT_CAP)],
            ))
        }
    }

    pub(super) fn printed_job(self, label: &str) -> Result<Option<String>, GuiError> {
        let missing = format!(
            "Could not find service \"{label}\" in domain for user gui: {}",
            uid()
        );
        if self.status == Some(113)
            && diagnostic_lines(&self.stderr)
                .chain(self.out.lines())
                .any(|line| line.trim() == missing)
        {
            return Ok(None);
        }
        self.checked(Verb::Print).map(|outcome| Some(outcome.out))
    }

    pub(super) fn disabled(self, label: &str) -> Result<bool, GuiError> {
        let outcome = self.checked(Verb::PrintDisabled)?;
        disabled_override(&outcome.out, label).ok_or_else(|| {
            GuiError::launchctl_failed(
                Verb::PrintDisabled.as_str(),
                outcome.status,
                "unexpected or incomplete disabled services table",
            )
        })
    }
}

fn disabled_override(text: &str, label: &str) -> Option<bool> {
    let text = text.trim();
    if text == "disabled services = (no disabled services)" {
        return Some(false);
    }
    // A missing entry is the default enabled state, but only in a complete table.
    let entries = text
        .strip_prefix("disabled services = {")?
        .strip_suffix('}')?;
    let mut disabled = None;
    for entry in entries
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (key, value) = entry.split_once("=>")?;
        let key = key.trim().strip_prefix('"')?.strip_suffix('"')?;
        let value = match value.trim() {
            "disabled" | "true" => true,
            "enabled" | "false" => false,
            _ => return None,
        };
        if key == label && disabled.replace(value).is_some() {
            return None;
        }
    }
    Some(disabled.unwrap_or(false))
}

fn printed_job_fields(dump: &str) -> Option<scope::JobFields<'_>> {
    let mut lines = dump.trim().lines();
    let heading = format!("{} = {{", service(AGENT_LABEL));
    (lines.next()? == heading).then_some(())?;
    scope::job_fields(lines)
}

/// The pid of a loaded job from a complete dump. `Some(None)` means the
/// dump reports a known no-process state. `None` means the probe is inconclusive.
pub fn printed_program(dump: &str) -> Option<&str> {
    printed_job_fields(dump)?.program
}

pub fn printed_pid(dump: &str) -> Option<Option<u32>> {
    printed_job_fields(dump).map(|fields| fields.pid)
}

/// The pid of a loaded job whose dump names the expected plist.
pub(super) fn printed_owned_pid(dump: &str, expected_plist: &Path) -> Option<Option<u32>> {
    let fields = printed_job_fields(dump)?;
    scope::owned_path(fields.path?, expected_plist).ok()?;
    Some(fields.pid)
}

/// The printed job's exit timeout, in seconds in the launchctl dump.
pub fn printed_exit_timeout(dump: &str) -> Option<Duration> {
    printed_job_fields(dump)?
        .exit_timeout_s
        .map(Duration::from_secs)
}

pub(super) fn read_stdout(verb: Verb, reader: impl Read) -> io::Result<Vec<u8>> {
    read_output(reader, verb.has_structured_stdout())
}

fn read_output(mut reader: impl Read, full: bool) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    if full {
        reader.read_to_end(&mut output)?;
    } else {
        // Keep one extra byte so an incomplete diagnostic line cannot be
        // mistaken for a complete absence report.
        reader
            .by_ref()
            .take(OUTPUT_CAP as u64 + 1)
            .read_to_end(&mut output)?;
        io::copy(&mut reader, &mut io::sink())?;
    }
    Ok(output)
}

fn diagnostic_lines(text: &str) -> impl Iterator<Item = &str> {
    let complete = if text.len() <= OUTPUT_CAP {
        text
    } else {
        let prefix = &text[..text.floor_char_boundary(OUTPUT_CAP)];
        prefix.rsplit_once('\n').map_or("", |(lines, _)| lines)
    };
    complete.lines()
}

type PipeRead = thread::JoinHandle<io::Result<Vec<u8>>>;

/// Wait for `child` until `deadline`. A launchctl that outlives it is left
/// to finish on its own. The deadline also covers draining both pipes.
fn wait(
    mut child: Child,
    deadline: Duration,
    out: &PipeRead,
    err: &PipeRead,
) -> Result<std::process::ExitStatus, Child> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if out.is_finished() && err.is_finished() => return Ok(st),
            Ok(_) if start.elapsed() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => return Err(child),
        }
    }
}

/// The job identity verified by the upgrade coordinator after it takes the
/// fence. A sealed running process must still have the same pid at dispatch.
#[derive(Clone, Copy, Debug)]
pub enum BootoutTarget {
    Stopped,
    Sealed(u32),
}

#[derive(Default)]
pub struct Launchctl {
    bootout_target: Option<BootoutTarget>,
}

impl Launchctl {
    pub fn new() -> Launchctl {
        Self::default()
    }

    /// Use for an individual unload attempt after verifying the running
    /// hold or stopped job. The final print must match the verified target
    /// and the login user's plist. The unbound service refuses bootout.
    pub fn for_bootout(target: BootoutTarget) -> Launchctl {
        Self {
            bootout_target: Some(target),
        }
    }

    fn run(&self, verb: Verb, label: &str, plist: Option<&Path>) -> Result<Outcome, GuiError> {
        check_label(verb, label)?;
        let step = verb.as_str();
        let mut child = Command::new(LAUNCHCTL)
            .args(argv(verb, label, plist))
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                let mut error =
                    GuiError::launchctl_failed(step, None, &format!("cannot run {LAUNCHCTL}: {e}"));
                error.detail["dispatched"] = json!(false);
                error
            })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let out = thread::spawn(move || read_stdout(verb, stdout));
        let err = thread::spawn(move || read_output(stderr, false));
        let status = match wait(child, DEADLINE, &out, &err) {
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
        let join = |h: PipeRead| {
            h.join().ok().and_then(Result::ok).ok_or_else(|| {
                GuiError::launchctl_failed(step, None, "could not read launchctl output")
            })
        };
        let out = join(out)?;
        let out = if status.success() && verb.has_structured_stdout() {
            String::from_utf8(out).map_err(|_| {
                GuiError::launchctl_failed(step, status.code(), "launchctl output was not UTF-8")
            })?
        } else {
            String::from_utf8_lossy(&out).into_owned()
        };
        Ok(Outcome {
            status: status.code(),
            success: status.success(),
            stderr: String::from_utf8_lossy(&join(err)?).into_owned(),
            out,
        })
    }

    fn must(&self, verb: Verb, label: &str, plist: Option<&Path>) -> Result<(), GuiError> {
        self.run(verb, label, plist)?.checked(verb).map(|_| ())
    }

    fn change_fence(&self, verb: Verb, label: &str) -> Result<(), GuiError> {
        dispatch_fence(
            verb,
            label,
            self.bootout_target,
            scope::current_login_home,
            |verb| self.run(verb, label, None),
        )
    }
}

fn check_label(verb: Verb, label: &str) -> Result<(), GuiError> {
    if label != AGENT_LABEL {
        return Err(GuiError::launchctl_failed(
            verb.as_str(),
            None,
            "unexpected launch agent label",
        ));
    }
    Ok(())
}

pub(super) fn dispatch_fence(
    verb: Verb,
    label: &str,
    target: Option<BootoutTarget>,
    login_home: impl FnOnce() -> Result<PathBuf, GuiError>,
    mut run: impl FnMut(Verb) -> Result<Outcome, GuiError>,
) -> Result<(), GuiError> {
    check_label(verb, label)?;
    let plist = plist_for_home(&login_home()?);
    let target = if verb == Verb::Bootout {
        Some(target.ok_or_else(|| {
            GuiError::new(
                Code::InvalidInput,
                "bootout requires a verified launch agent target",
                json!({ "reason": "bootout_target_required" }),
            )
        })?)
    } else {
        None
    };
    if let Some(dump) = run(Verb::Print)?.printed_job(label)? {
        let fields = printed_job_fields(&dump).ok_or_else(|| {
            GuiError::launchctl_failed(Verb::Print.as_str(), Some(0), "invalid service dump")
        })?;
        let path = fields.path.ok_or_else(|| {
            GuiError::launchctl_failed(
                Verb::Print.as_str(),
                Some(0),
                "service dump has no plist path",
            )
        })?;
        scope::owned_path(path, &plist)?;
        if let Some(target) = target {
            let expected_pid = match target {
                BootoutTarget::Stopped => None,
                BootoutTarget::Sealed(pid) => Some(pid),
            };
            if expected_pid != fields.pid {
                return Err(GuiError::new(
                    Code::Busy,
                    "the job's process changed before bootout",
                    json!({
                        "reason": "bootout_target_changed",
                        "expected_pid": expected_pid,
                        "observed_pid": fields.pid,
                    }),
                ));
            }
        }
    } else if verb == Verb::Bootout {
        return Ok(());
    }
    run(verb)?.checked(verb).map(|_| ())
}

impl ServiceManager for Launchctl {
    fn print(&self, label: &str) -> Result<Option<String>, GuiError> {
        self.run(Verb::Print, label, None)?.printed_job(label)
    }

    fn kickstart(&self, label: &str) -> Result<(), GuiError> {
        self.must(Verb::Kickstart, label, None)
    }

    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError> {
        self.must(Verb::Bootstrap, AGENT_LABEL, Some(plist))
    }

    fn bootout(&self, label: &str) -> Result<(), GuiError> {
        self.change_fence(Verb::Bootout, label)
    }

    fn bootout_targeted(&self, label: &str, target: BootoutTarget) -> Result<(), GuiError> {
        Self::for_bootout(target).bootout(label)
    }

    fn disable(&self, label: &str) -> Result<(), GuiError> {
        self.change_fence(Verb::Disable, label)
    }

    fn enable(&self, label: &str) -> Result<(), GuiError> {
        self.change_fence(Verb::Enable, label)
    }

    fn print_disabled(&self, label: &str) -> Result<bool, GuiError> {
        scope::current_login_home()?;
        self.run(Verb::PrintDisabled, label, None)?.disabled(label)
    }
}

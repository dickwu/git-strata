//! Recorder for a single fixture job. There is no subprocess backend here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{WorkerFixture, refusal};
use crate::Result;
use crate::plist::Launchd;
use strata_types::{Code, StrataError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchdCall {
    Bootstrap(PathBuf),
    Bootout(PathBuf),
    Enable,
    Disable,
    Print,
    Kickstart,
}

pub struct FakeLaunchd {
    fixture: WorkerFixture,
    label: String,
    pub print_reply: String,
    calls: Mutex<Vec<LaunchdCall>>,
}

impl FakeLaunchd {
    /// Only synthetic fixture domains can be recorded. All calls on this
    /// recorder target the job selected here.
    pub fn new(fixture: &WorkerFixture, domain: &str, label: &str) -> Result<Self> {
        let name = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        if !domain.strip_prefix("fixture/").is_some_and(name)
            || !name(label)
            || label == crate::AGENT_LABEL
        {
            return Err(StrataError::new(
                Code::InvalidInput,
                "expected a fixture launchd domain and test label",
            ));
        }
        Ok(Self {
            fixture: fixture.clone(),
            label: label.into(),
            print_reply: String::new(),
            calls: Mutex::new(Vec::new()),
        })
    }

    pub fn calls(&self) -> Vec<LaunchdCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: LaunchdCall) -> Result<()> {
        if let LaunchdCall::Bootstrap(plist) | LaunchdCall::Bootout(plist) = &call {
            self.fixture.check_root()?;
            self.fixture.check_path("plist", plist)?;
            if plist.file_name() != Some(format!("{}.plist", self.label).as_ref()) {
                return Err(refusal("plist label", plist));
            }
        }
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl Launchd for FakeLaunchd {
    fn bootstrap(&self, plist: &Path) -> Result<()> {
        self.record(LaunchdCall::Bootstrap(plist.into()))
    }
    fn bootout(&self, plist: &Path) -> Result<()> {
        self.record(LaunchdCall::Bootout(plist.into()))
    }
    fn enable(&self) -> Result<()> {
        self.record(LaunchdCall::Enable)
    }
    fn disable(&self) -> Result<()> {
        self.record(LaunchdCall::Disable)
    }
    fn print(&self) -> Result<String> {
        self.record(LaunchdCall::Print)?;
        Ok(self.print_reply.clone())
    }
    fn kickstart(&self) -> Result<()> {
        self.record(LaunchdCall::Kickstart)
    }
}

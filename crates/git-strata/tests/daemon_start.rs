//! Local launch lifecycle tests through the injected launchctl interface.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use git_strata::launch::{Launchctl, start_with, stop_with};
use serde_json::json;
use strata_daemon::{AGENT_LABEL, plist::agent_plist_path};
use strata_types::Code;

struct LaunchFixture {
    home: tempfile::TempDir,
    uid: u32,
    calls: RefCell<VecDeque<(Vec<String>, i32, String)>>,
}

impl LaunchFixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let plist = agent_plist_path(home.path());
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(plist, "<plist/>").unwrap();
        Self {
            home,
            // SAFETY: getuid has no preconditions.
            uid: unsafe { libc::getuid() },
            calls: RefCell::default(),
        }
    }

    fn plist(&self) -> PathBuf {
        agent_plist_path(&fs::canonicalize(self.home.path()).unwrap())
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self) -> String {
        format!("{}/{AGENT_LABEL}", self.domain())
    }

    fn reply(&self, verb: &str, target: &str, status: i32, text: impl Into<String>) {
        self.calls.borrow_mut().push_back((
            vec![verb.to_owned(), target.to_owned()],
            status,
            text.into(),
        ));
    }

    fn loaded(&self, fields: &str) {
        let target = self.target();
        self.reply(
            "print",
            &target,
            0,
            format!(
                "{target} = {{\n\tpath = {}\n{fields}}}\n",
                self.plist().display()
            ),
        );
    }

    fn missing(&self, label: &str) {
        self.reply(
            "print",
            &self.target(),
            113,
            format!(
                "Bad request.\nCould not find service \"{label}\" in domain for user gui: {}\n",
                self.uid
            ),
        );
    }

    fn disabled_services(&self, entries: &str) {
        self.reply(
            "print-disabled",
            &self.domain(),
            0,
            format!("disabled services = {{\n{entries}\n}}\n"),
        );
    }
}

impl Launchctl for LaunchFixture {
    fn run(&self, args: &[&str]) -> git_strata::Result<(Option<i32>, String)> {
        let (expected, status, text) = self
            .calls
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected launchctl call: {args:?}"));
        assert_eq!(args, expected.as_slice());
        Ok((Some(status), text))
    }
}

#[test]
fn loaded_stopped_job_is_kickstarted_without_killing_any_process() {
    let fx = LaunchFixture::new();
    fx.loaded("\tstate = not running\n\tproperties = {\n\t\tpid = 123\n\t}\n");
    fx.reply(
        "print-disabled",
        &fx.domain(),
        0,
        format!(
            "disabled services = {{\n\t\"{AGENT_LABEL}.other\" => disabled\n}}\n\
             login item associations = {{\n\t\"{AGENT_LABEL}\" => disabled\n}}\n"
        ),
    );
    fx.reply("kickstart", &fx.target(), 0, "");
    let home = fx.home.path();
    assert_eq!(start_with(home, Some(home), &fx).unwrap().data, json!({}));
    assert!(fx.calls.borrow().is_empty());
}

#[test]
fn unreadable_disabled_services_entry_prevents_start() {
    let fx = LaunchFixture::new();
    fx.loaded("\tstate = not running\n");
    fx.disabled_services(&format!("\t\"{AGENT_LABEL}\" => enabled\n\tunreadable row"));
    let home = fx.home.path();
    let e = start_with(home, Some(home), &fx).err().unwrap();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(e.detail.unwrap()["step"], "print-disabled");
    assert!(fx.calls.borrow().is_empty());
}

#[test]
fn disabled_agent_refuses_start_in_every_job_state() {
    for (fields, state) in [
        (Some("\tstate = not running\n"), "disabled"),
        (Some("\tpid = 42\n"), "disabled"),
        (None, "true"),
    ] {
        let fx = LaunchFixture::new();
        if let Some(fields) = fields {
            fx.loaded(fields);
        } else {
            fx.missing(AGENT_LABEL);
        }
        fx.disabled_services(&format!("\t\"{AGENT_LABEL}\" => {state}"));
        let home = fx.home.path();
        let e = start_with(home, Some(home), &fx).err().unwrap();
        assert_eq!(e.code, Code::Unsupported);
        assert_eq!(e.detail.unwrap()["reason"], "agent_disabled");
        assert_eq!(
            e.message,
            "The daemon's launch agent is disabled (an app update may be in progress; open GIT STRATA)"
        );
        assert!(fx.calls.borrow().is_empty());
    }
}

#[test]
fn loaded_stopped_job_needs_a_known_plist_path_before_kickstart() {
    let fx = LaunchFixture::new();
    let target = fx.target();
    fx.reply(
        "print",
        &target,
        0,
        format!("{target} = {{\n\tstate = not running\n}}\n"),
    );
    fx.disabled_services("");
    let home = fx.home.path();
    let e = start_with(home, Some(home), &fx).err().unwrap();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(e.detail.unwrap()["step"], "print");
    assert!(
        e.message
            .contains("could not confirm the loaded agent's plist path")
    );
    assert!(fx.calls.borrow().is_empty());
}

#[test]
fn kickstart_failure_is_reported() {
    let fx = LaunchFixture::new();
    fx.loaded("\tstate = not running\n");
    fx.disabled_services("");
    fx.reply("kickstart", &fx.target(), 1, "kickstart refused");
    let home = fx.home.path();
    let e = start_with(home, Some(home), &fx).err().unwrap();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(e.detail.unwrap()["step"], "kickstart");
    assert!(fx.calls.borrow().is_empty());
}

#[test]
fn failed_initial_probe_refuses_start_and_stop() {
    let fx = LaunchFixture::new();
    fx.reply("print", &fx.target(), 1, "probe failed");
    fx.reply("print", &fx.target(), 1, "probe failed");
    let home = fx.home.path();
    for result in [
        start_with(home, Some(home), &fx),
        stop_with(home, Some(home), &fx),
    ] {
        let e = result.err().unwrap();
        assert_eq!(e.code, Code::Unknown);
        assert_eq!(e.detail.unwrap()["step"], "print");
    }
    assert!(fx.calls.borrow().is_empty());
}

#[test]
fn stop_waits_through_probe_failure_until_the_target_is_explicitly_missing() {
    let fx = LaunchFixture::new();
    fx.loaded("\tpid = 42\n");
    fx.reply("bootout", &fx.target(), 0, "");
    fx.reply("print", &fx.target(), 1, "probe failed");
    fx.missing("com.gwd.other");
    fx.missing(AGENT_LABEL);
    let home = fx.home.path();
    assert_eq!(stop_with(home, Some(home), &fx).unwrap().data, json!({}));
    assert!(fx.calls.borrow().is_empty());
}

//! confirm.md §7, Task 5: tokens are built only by confirm, and no environment variable
//! skips or blocks a prompt.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use strata_confirm::Confirmer;
use strata_confirm::seams::{FakeClock, ScriptedAuth, Verdict, bypass_env_names};
use strata_types::{Action, Code, RepoName};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/strata-confirm sits two levels below the workspace root")
        .to_path_buf()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn only_confirm_constructs_tokens() {
    let root = workspace_root();
    let allowed = root.join("crates/strata-confirm/src");
    let literal = Regex::new(r"\bConfirmToken\s*\{").unwrap();
    let definition = Regex::new(r"\bstruct\s+ConfirmToken\s*\{").unwrap();
    let mut scanned = 0;
    let mut violations = Vec::new();
    let mut crates: Vec<_> = fs::read_dir(root.join("crates"))
        .unwrap()
        .flatten()
        .collect();
    crates.sort_by_key(|e| e.path());
    for krate in crates {
        let mut files = Vec::new();
        rs_files(&krate.path().join("src"), &mut files);
        for f in files {
            scanned += 1;
            if f.starts_with(&allowed) {
                continue;
            }
            for (i, line) in fs::read_to_string(&f).unwrap().lines().enumerate() {
                if literal.is_match(line) && !definition.is_match(line) {
                    violations.push(format!("{}:{}: {}", f.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        scanned > 0,
        "scanned no source files under {}",
        root.display()
    );
    assert!(
        violations.is_empty(),
        "ConfirmToken literals outside strata-confirm/src:\n{}",
        violations.join("\n")
    );
}

fn push() -> Action {
    Action::PushDeploy {
        repo: RepoName::try_from("tool".to_string()).unwrap(),
        branch: "main".to_string(),
        url: "git@github.com:acme/tool.git".to_string(),
    }
}

/// Runs in the child process (see `confirm_reads_no_environment`): one cancelled prompt and
/// one passing prompt, each shown exactly once, whatever the environment says.
#[test]
#[ignore = "child of confirm_reads_no_environment"]
fn child_require_under_environment() {
    let auth = ScriptedAuth::new(true, Ok(()), vec![Verdict::la(-2)]);
    let log = auth.log();
    let c = Confirmer::with_seams(
        auth,
        FakeClock::new(),
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(50),
    );
    let e = c.require(push()).expect_err("cancelled");
    assert_eq!(e.code, Code::NotConfirmed);
    assert_eq!(
        e.detail
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(|r| r.as_str()),
        Some("cancelled")
    );
    assert_eq!(log.shown().len(), 1);
    assert_eq!(c.record_count(), 0);

    let auth = ScriptedAuth::new(true, Ok(()), vec![Verdict::pass()]);
    let log = auth.log();
    let c = Confirmer::with_seams(
        auth,
        FakeClock::new(),
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(50),
    );
    let t = c.require(push()).expect("passes");
    assert_eq!(log.shown().len(), 1);
    c.validate(&t, &push()).expect("valid");
    println!("CHILD_OK");
}

fn run_child(vars: &[(&str, &str)]) {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args([
        "--exact",
        "child_require_under_environment",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("CHILD_OK") && stdout.contains("1 passed"),
        "with {vars:?}: status {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn confirm_reads_no_environment() {
    // Static: nothing in confirm's source can read the environment or switch on build mode.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden = [
        "std::env",
        "env::var",
        "var_os",
        "getenv",
        "env!(",
        "option_env!(",
        "debug_assertions",
        "cfg(test)",
    ];
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(!files.is_empty());
    let mut violations = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        for (i, line) in text.lines().enumerate() {
            for bad in forbidden {
                if line.contains(bad) {
                    violations.push(format!(
                        "{}:{}: `{bad}` in {}",
                        f.display(),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
        // Only the gated seams and the production bridge may build a Confirmer around an
        // authenticator.
        let name = f.file_name().unwrap().to_str().unwrap();
        if text.contains("Confirmer::build(") && name != "seams.rs" && name != "la.rs" {
            violations.push(format!(
                "{}: builds a Confirmer around an authenticator",
                f.display()
            ));
        }
    }
    let lib = fs::read_to_string(src.join("lib.rs")).unwrap();
    assert!(
        Regex::new(r#"#\[cfg\(feature = "test-seams"\)\]\s*pub mod seams;"#)
            .unwrap()
            .is_match(&lib),
        "the seams module must be behind the test-seams feature"
    );
    let la = fs::read_to_string(src.join("la.rs")).unwrap();
    assert!(
        !la.contains("ScriptedAuth"),
        "the production bridge must not reach the scripted authenticator"
    );
    assert!(violations.is_empty(), "{}", violations.join("\n"));

    // Runtime: every name, every value, then all at once.
    let names = bypass_env_names(&workspace_root());
    eprintln!("{} names: {names:?}", names.len());
    for fixed in ["STRATA_CONFIRM", "STRATA_NO_CONFIRM", "STRATA_PUBLISH"] {
        assert!(
            names.iter().any(|n| n == fixed),
            "{fixed} missing from {names:?}"
        );
    }
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, names, "sorted and deduplicated");
    for name in &names {
        for value in ["1", "true", "yes", "on", "0", ""] {
            run_child(&[(name.as_str(), value)]);
        }
    }
    let all: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "1")).collect();
    run_child(&all);
}

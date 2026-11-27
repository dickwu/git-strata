//! Seamless upgrade U5. A duplicated custody descriptor releases its shared
//! flock for every holder if anyone explicitly unlocks it.

use std::fs;
use std::path::Path;

use regex::Regex;

struct Rules {
    unlock: Regex,
    force_kill: Regex,
}

impl Rules {
    fn new() -> Self {
        Self {
            unlock: Regex::new(
                r"(?x)\b(LOCK_UN|F_UNLCK|F_ULOCK|UnlockNonblock|Unlock)\b
                | (\.|::)\s*unlock\s*\( | \bflock\s*\([^;{}]*,\s*(8|0x0*8)\b",
            )
            .unwrap(),
            force_kill: Regex::new(
                r#"(?x)\b(SIGKILL|sigkill)\b | ::\s*Kill\b | "KILL" | (\.|\bChild\s*::)\s*(kill|start_kill)\s*\(
                | \bkill(pg)?\s*\([^;{}]*,\s*(9|0x0*9)\b | ["\s]-(9|KILL|SIGKILL)["\s]
                | \bkill_on_drop\s*\(\s*true\s*\)"#,
            )
            .unwrap(),
        }
    }

    fn scan(&self, path: &Path, check_kill: bool) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                let child = entry.unwrap().path();
                if child
                    .file_name()
                    .is_some_and(|n| n == "tests" || n == "tests.rs")
                {
                    continue;
                }
                self.scan(&child, check_kill);
            }
        } else if matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("rs" | "c" | "h" | "m" | "mm" | "cpp" | "swift" | "sh")
        ) {
            let src = fs::read_to_string(path).unwrap();
            let mut code = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            if path.ends_with("strata-daemon/src/periodic_fetch.rs") {
                // Existing git fetch timeout, scoped to the git process group
                // this worker spawned. Supervisor and App have no exemption.
                code = code.replacen("libc::killpg(child.id() as i32, libc::SIGKILL);", "", 1);
            }
            assert!(
                !self.unlock.is_match(&code),
                "explicit custody unlock in {}",
                path.display()
            );
            assert!(
                !check_kill || !self.force_kill.is_match(&code),
                "force-kill in {}",
                path.display()
            );
        }
    }
}

#[test]
fn u5_custody_is_close_only_and_supervisor_and_app_never_force_kill() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Keep the current holders anchored. Moving one requires updating the scan.
    for required in [
        "crates/strata-daemon/src/startup.rs",
        "crates/strata-daemon/src/worker.rs",
        "crates/strata-store/src/open.rs",
        "apps/desktop/src-tauri/src/main.rs",
        "apps/desktop/src-tauri/src/update/install.rs",
    ] {
        assert!(
            root.join(required).is_file(),
            "missing U5 source {required}"
        );
    }
    let rules = Rules::new();
    rules.scan(&root.join("crates/strata-store/src"), false);
    rules.scan(&root.join("crates/strata-daemon/src"), true);
    rules.scan(&root.join("apps/desktop/src-tauri/src"), true);
    rules.scan(&root.join("apps/desktop/src-tauri/build.rs"), true);
    // The design puts S in git-stratad. Also pick up a separate supervisor
    // crate if one is introduced. A discovered crate must have sources.
    for entry in fs::read_dir(root.join("crates")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().contains("supervisor") {
            let src = entry.path().join("src");
            assert!(src.is_dir(), "missing U5 source {}", src.display());
            rules.scan(&src, true);
        }
    }
}

#[test]
fn u5_recognizes_equivalent_unlock_and_kill_forms() {
    let rules = Rules::new();
    for op in [
        "flock(fd, libc::LOCK_UN)",
        "flock(fd, 8)",
        "FileExt::unlock(&fd)",
        "fd\n . unlock ()",
        "lockf(fd, F_ULOCK, 0)",
        "FlockArg::UnlockNonblock",
    ] {
        assert!(rules.unlock.is_match(op), "{op}");
    }
    for op in [
        "libc::SIGKILL",
        "killpg(group, 9)",
        "child.kill()",
        "Child::kill(child)",
        "cmd.arg(\"-KILL\")",
        "cmd.args([\"-s\", \"KILL\"])",
        "child.start_kill()",
        "cmd.kill_on_drop(true)",
    ] {
        assert!(rules.force_kill.is_match(op), "{op}");
    }
    assert!(!rules.unlock.is_match("drop(custody);"));
    assert!(!rules.force_kill.is_match("libc::kill(pid, libc::SIGTERM)"));
}

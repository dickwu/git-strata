//! cli.md D30: a bad `publish --from` PATH is reported as an input error
//! naming the path and the expected form, before the TTY gate (a guarded
//! repo in the config must not turn it into `NotTty`), and nothing but the
//! test's own setup reaches the daemon.

mod support;

use std::time::Duration;

use support::live::{Live, write};

fn live() -> Live {
    let lv = Live::new(&["app", "lib"], &[("a.txt", b"one\n"), ("src/x.ts", b"x\n")]);
    lv.write_config_guarded(&["app"]);
    lv
}

#[test]
fn publish_from_bad_paths_are_input_errors_not_not_tty() {
    let lv = live();
    lv.serve(|_core| {
        let s = lv.new_session("T-1", "app,lib");
        let ws = lv.ws(&s, "app");
        write(&ws, "a.txt", b"two\n");
        let seen = lv.requests.lock().unwrap().len();

        // A repo-prefixed path: the error names the repo and the fix.
        let prefixed = format!("{s}:app/a.txt");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &prefixed]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_repo_prefix", "{r:?}");
        let d = &r.env()["error"]["detail"];
        assert_eq!(d["repo"], "app");
        assert_eq!(d["path"], "app/a.txt");
        assert_eq!(d["expected"], format!("{s}:a.txt"));

        // A path no workspace has: the error names it and the format.
        let missing = format!("{s}:nope/b.txt");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &missing]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_path", "{r:?}");
        let e = &r.env()["error"];
        assert_eq!(e["detail"]["path"], "nope/b.txt");
        assert!(e["message"].as_str().unwrap_or_default().contains("SESSION:PATH"), "{e}");

        // In both repos: AmbiguousMatch with the candidates, still no gate.
        let both = format!("{s}:src/x.ts");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &both]);
        assert_eq!(r.err_code(), "AmbiguousMatch", "{r:?}");
        assert_eq!(r.reason(), "from_repo", "{r:?}");

        std::thread::sleep(Duration::from_millis(50));
        assert!(lv.requests.lock().unwrap()[seen..].is_empty(), "a bad --from reached the daemon: {:?}", &lv.requests.lock().unwrap()[seen..]);
    });
}

#[test]
fn publish_from_prefix_is_caught_in_a_single_repo_session() {
    let lv = live();
    lv.serve(|_core| {
        let s = lv.new_session("T-2", "lib");
        let seen = lv.requests.lock().unwrap().len();
        let prefixed = format!("{s}:lib/a.txt");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &prefixed]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_repo_prefix", "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["expected"], format!("{s}:a.txt"));
        assert!(lv.requests.lock().unwrap()[seen..].is_empty());
    });
}

/// cli.md D31: `S:PATH` in several repos goes to the one repo whose
/// checkpoints changed it; `S:REPO:PATH` names the repo outright.
#[test]
fn publish_from_prefers_the_changed_repo_and_takes_a_repo_qualified_form() {
    let lv = Live::new(&["app", "lib"], &[("a.txt", b"one\n"), ("src/x.ts", b"x\n"), ("src/y.ts", b"y\n")]);
    lv.serve(|_core| {
        let s = lv.new_session("T-3", "app,lib");
        let app = lv.ws(&s, "app");
        let lib = lv.ws(&s, "lib");

        // No checkpoint changed src/x.ts yet: ambiguous, with the S:REPO:PATH hint.
        let both = format!("{s}:src/x.ts");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &both]);
        assert_eq!(r.err_code(), "AmbiguousMatch", "{r:?}");
        let d = &r.env()["error"]["detail"];
        assert_eq!(d["expected"][0], format!("{s}:app:src/x.ts"), "{d}");
        assert!(r.env()["error"]["message"].as_str().unwrap().contains("SESSION:REPO:PATH"), "{r:?}");

        // Only app's checkpoint changes src/x.ts: S:PATH picks app.
        write(&app, "src/x.ts", b"x2\n");
        lv.run_in(&app, &["checkpoint", "-m", "x"]).ok();
        let r = lv.run_in(&lv.sb, &["publish", "--from", &both]);
        let d = r.ok();
        let commits = d["commits"].as_object().expect("commits");
        assert_eq!(commits.keys().collect::<Vec<_>>(), vec!["app"], "{d}");

        // Explicit repo: S:app:src/y.ts (in both, changed in neither checkpoint yet).
        write(&app, "src/y.ts", b"y2\n");
        let explicit = format!("{s}:app:src/y.ts");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &explicit]);
        let d = r.ok();
        assert_eq!(d["commits"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["app"], "{d}");

        // Both repos changed it: ambiguous again, changed_in lists both.
        write(&app, "a.txt", b"app\n");
        write(&lib, "a.txt", b"lib\n");
        lv.run_in(&app, &["checkpoint", "-m", "a"]).ok();
        lv.run_in(&lib, &["checkpoint", "-m", "a"]).ok();
        let r = lv.run_in(&lv.sb, &["publish", "--from", &format!("{s}:a.txt")]);
        assert_eq!(r.err_code(), "AmbiguousMatch", "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["changed_in"], serde_json::json!(["app", "lib"]));

        // REPO:PATH with a path the repo lacks, and an unknown repo.
        let r = lv.run_in(&lv.sb, &["publish", "--from", &format!("{s}:lib:nope.txt")]);
        assert_eq!(r.reason(), "from_path", "{r:?}");
        let r = lv.run_in(&lv.sb, &["publish", "--from", &format!("{s}:zzz:src/x.ts")]);
        assert_eq!(r.err_code(), "InvalidInput", "{r:?}");
        assert_eq!(r.reason(), "from_path", "{r:?}");
    });
}

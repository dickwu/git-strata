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

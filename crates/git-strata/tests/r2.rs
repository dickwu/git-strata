mod support;
use serde_json::json;
use support::{FakeDaemon, Home};

#[test]
fn required_features_refuse_old_daemon_without_sending_new_or_publish() {
    for args in [
        vec!["new", "T", "--repo", "tool", "--base", "HEAD"],
        vec!["publish", "--from", "s-aaaaaaaa:tool:a", "-m", "chosen"],
    ] {
        let home = Home::new();
        let fake = FakeDaemon::start(
            &home.socket(),
            |q| json!({"id":q["id"],"ok":true,"data":{"features":[]}}),
        );
        let r = home.run(&args);
        assert_eq!(r.err_code(), "Unsupported");
        assert_eq!(r.reason(), "daemon_too_old");
        assert!(
            fake.requests()
                .iter()
                .all(|q| q["method"] == "daemon_status")
        );
    }
}

#[test]
fn base_probe_failure_remains_daemon_down_and_input_flags_are_validated() {
    let home = Home::new();
    let r = home.run(&["new", "T", "--repo", "tool", "--base", "HEAD"]);
    assert_eq!(r.reason(), "daemon_down");
    for args in [
        vec!["new", "--repos", "tool", "--base", "HEAD"],
        vec!["new", "--repo", "tool,front", "--base", "missing=HEAD"],
    ] {
        assert_eq!(home.run(&args).reason(), "base_flags");
    }
    assert_eq!(home.run(&["checkpoint", "-m", " "]).reason(), "message");
}

#[test]
fn message_flags_roundtrip_through_plan_rpc_and_per_repo_commits() {
    use support::live::{Live, write};
    let lv = Live::new(&["front", "tool"], &[("a.txt", b"base\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-message", "front,tool");
        write(&lv.ws(&s, "front"), "a.txt", b"front\n");
        write(&lv.ws(&s, "tool"), "a.txt", b"tool\n");
        let front = format!("{s}:front:a.txt");
        let tool = format!("{s}:tool:a.txt");
        let data = lv
            .run(&[
                "publish",
                "--from",
                &front,
                "--from",
                &tool,
                "-m",
                "shared subject",
                "-m",
                "shared body",
                "--message-for",
                "tool=tool subject",
                "--message-for",
                "tool=tool body",
            ])
            .ok();
        for (repo, subject, body) in [
            ("front", "shared subject", "shared body"),
            ("tool", "tool subject", "tool body"),
        ] {
            let oid = data["commits"][repo].as_str().unwrap();
            let message = support::git(
                &lv.main(repo),
                lv.home.path(),
                &["show", "-s", "--format=%B", oid],
            );
            assert!(
                message.starts_with(&format!("{subject}\n\n{body}\n\nsessions: ")),
                "{message}"
            );
            assert!(
                message.contains("\npaths: 1\nStrata-Publish: "),
                "{message}"
            );
            assert_eq!(data["message_source"][repo], "flag");
        }
    });
}

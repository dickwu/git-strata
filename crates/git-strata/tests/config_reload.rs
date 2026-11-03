mod support;
use serde_json::{Value, json};
use support::{FakeDaemon, Home};

fn response(req: &Value, outcome: &str) -> Value {
    let data = if req["method"] == "daemon_status" {
        json!({"features":["config.reload"]})
    } else {
        assert_eq!(req["method"], "config_reload");
        assert_eq!(req["params"], json!({}));
        json!({"outcome":outcome,"generation":2,"repos":["front"],"added":[],"changed":[],"restart_required":[],"pending_relax":[],"reasons":[]})
    };
    json!({"id":req["id"],"ok":true,"data":data,"error":null})
}

#[test]
fn old_daemon_has_exit_four_and_no_reload_call() {
    let home = Home::new();
    let fake = FakeDaemon::start(
        &home.socket(),
        |r| json!({"id":r["id"],"ok":true,"data":{},"error":null}),
    );
    let run = home.run(&["config", "reload"]);
    assert_eq!(run.code, 4, "{run:?}");
    assert_eq!(run.env()["error"]["detail"]["reason"], "daemon_too_old");
    assert!(
        run.stderr
            .contains("this daemon loads config only at start; ask the owner to restart it")
    );
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn reload_outcomes_keep_report_in_data_and_set_exit_code() {
    type Reply = fn(&Value) -> Value;
    for (reply, outcome, code) in [
        ((|r| response(r, "applied")) as Reply, "applied", 0),
        ((|r| response(r, "unchanged")) as Reply, "unchanged", 0),
        ((|r| response(r, "rejected")) as Reply, "rejected", 2),
        ((|r| response(r, "pending")) as Reply, "pending", 3),
    ] {
        let home = Home::new();
        let _fake = FakeDaemon::start(&home.socket(), reply);
        let run = home.run(&["config", "reload"]);
        assert_eq!(run.code, code, "{run:?}");
        assert_eq!(run.env()["data"]["outcome"], outcome);
        assert_eq!(run.env()["data"]["repos"], json!(["front"]));
        assert_eq!(run.env()["ok"], code == 0);
    }
}

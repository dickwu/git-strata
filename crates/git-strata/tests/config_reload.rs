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

#[test]
fn feature_probe_times_out_without_sending_reload() {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};
    let home = Home::new();
    let listener = UnixListener::bind(home.socket()).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            let (stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("probe did not connect: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "daemon_status");
            // No response: a bounded probe closes this connection itself.
            let mut rest = String::new();
            reader.read_to_string(&mut rest).unwrap();
            assert!(rest.is_empty(), "no config_reload was sent");
        });
        let start = Instant::now();
        let run = home.run(&["config", "reload"]);
        let elapsed = start.elapsed();
        assert_eq!(run.code, 1, "{run:?}");
        assert!(
            elapsed >= Duration::from_secs(4),
            "probe did not wait for its deadline: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "unbounded feature probe: {elapsed:?}"
        );
        server.join().unwrap();
    });
}

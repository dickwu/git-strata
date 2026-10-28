//! status-perf C: `ls` (envelope and `--table`) and `status --brief`, from
//! the daemon's `status{summary:true}`, the store directly when no daemon
//! listens, and the store directly when the daemon predates `summary`.

mod support;

use serde_json::{Value, json};
use support::FakeDaemon;
use support::live::{Live, write};

fn old_daemon(req: &Value) -> Value {
    // An old daemon: `deny_unknown_fields` refuses `summary` before dispatch.
    json!({ "id": req["id"], "ok": false, "error": {
        "rpc": "Failed",
        "message": "invalid params: unknown field `summary`, expected one of `cwd`, `session`, `all`",
    } })
}

fn old_daemon_workspaces(req: &Value) -> Value {
    json!({ "id": req["id"], "ok": false, "error": {
        "rpc": "Failed",
        "message": "invalid params: unknown field `workspaces`, expected one of `cwd`, `session`, `all`, `chains`, `summary`",
    } })
}

fn busy_daemon(req: &Value) -> Value {
    json!({ "id": req["id"], "ok": false, "error": { "code": "Busy", "message": "busy", "detail": { "reason": "x" } } })
}

#[test]
fn ls_and_status_brief_daemon_local_and_old_daemon() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let (a, b, ws) = lv.serve(|_| {
        let a = lv.new_session("T-1", "tool");
        let b = lv.new_session("T-2", "tool");
        let ws = lv.ws(&a, "tool");
        write(&ws, "b.txt", b"beta\n");
        lv.run_in(&ws, &["checkpoint", "-m", "one"]).ok();
        // From the daemon.
        let l = lv.run(&["ls"]).ok();
        assert!(l.get("source").is_none(), "{l:#}");
        let ids: Vec<&str> = l["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        let mut want = vec![a.as_str(), b.as_str()];
        want.sort();
        assert_eq!(ids, want, "by id");
        assert_eq!(l["sessions"][0]["repos"], json!(["tool"]));
        let t = lv.run(&["ls", "--table"]);
        assert_eq!(t.code, 0, "{t:?}");
        assert!(t.stdout.starts_with("ID"), "{t:?}");
        assert!(
            t.stdout
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .eq(["ID", "TICKET", "STATE", "REPOS", "CREATED"])
        );
        assert!(
            t.stdout.contains("T-1") && t.stdout.contains(a.as_str()),
            "{t:?}"
        );
        assert!(
            serde_json::from_str::<Value>(t.stdout.trim()).is_err(),
            "a table, not an envelope"
        );
        let br = lv.run_in(&ws, &["status", "--brief"]).ok();
        assert_eq!(br["id"], json!(a.as_str()), "{br:#}");
        assert_eq!(br["ticket"], "T-1");
        assert_eq!(br["workspaces"][0]["repo"], "tool");
        assert_eq!(br["workspaces"][0]["checkpoint"], json!(1), "{br:#}");
        assert_eq!(br["workspaces"][0]["health_stored"], "Ok");
        assert!(
            br.get("drift").is_none() && br.get("source").is_none(),
            "{br:#}"
        );
        let all = lv.run(&["status", "--brief", "--all"]).ok();
        assert_eq!(all["sessions"].as_array().unwrap().len(), 2, "{all:#}");
        let one = lv.run(&["status", "--brief", "--session", "T-2"]).ok();
        assert_eq!(one["id"], json!(b.as_str()), "{one:#}");
        let r = lv.run(&["status", "--brief", "--all", "--session", "T-2"]);
        assert_eq!(
            (r.err_code().as_str(), r.reason().as_str()),
            ("InvalidInput", "both_selectors")
        );
        (a, b, ws)
    });
    // No daemon: the store directly.
    let _ = std::fs::remove_file(lv.home.socket());
    let l = lv.run(&["ls"]).ok();
    assert_eq!(l["source"], "local", "{l:#}");
    assert_eq!(l["sessions"].as_array().unwrap().len(), 2);
    let br = lv.run_in(&ws, &["status", "--brief"]).ok();
    assert_eq!(
        (br["id"].clone(), br["source"].clone()),
        (json!(a.as_str()), json!("local")),
        "{br:#}"
    );
    assert_eq!(br["workspaces"][0]["checkpoint"], json!(1), "{br:#}");
    let t = lv.run(&["ls", "--table"]);
    assert_eq!(t.code, 0, "{t:?}");
    assert!(t.stdout.contains(b.as_str()), "{t:?}");
    // A workspace directory gone: still listed, recorded health.
    let away = ws.with_file_name("tool.away");
    std::fs::rename(&ws, &away).unwrap();
    let br = lv.run(&["status", "--brief", "--session", a.as_str()]).ok();
    assert_eq!(br["workspaces"][0]["health_stored"], "Ok", "{br:#}");
    std::fs::rename(&away, &ws).unwrap();
    // An old daemon refusing `summary` / `workspaces`: the store directly.
    for reply in [old_daemon as fn(&Value) -> Value, old_daemon_workspaces] {
        let d = FakeDaemon::start(&lv.home.socket(), reply);
        let l = lv.run(&["ls"]).ok();
        assert_eq!(l["source"], "local", "{l:#}");
        let br = lv.run_in(&ws, &["status", "--brief"]).ok();
        assert_eq!(br["source"], "local", "{br:#}");
        assert!(
            d.requests()
                .iter()
                .all(|r| r["params"]["summary"] == json!(true)),
            "{:?}",
            d.requests()
        );
        drop(d);
        let _ = std::fs::remove_file(lv.home.socket());
    }
    // Any other daemon error is the answer, not a fallback.
    let d = FakeDaemon::start(&lv.home.socket(), busy_daemon);
    let r = lv.run(&["ls"]);
    assert_eq!(r.err_code(), "Busy", "{r:?}");
    let t = lv.run(&["ls", "--table"]);
    assert_eq!(t.code, 1, "{t:?}");
    assert_eq!(t.err_code(), "Busy", "an error stays an envelope: {t:?}");
    let r = lv.run_in(&ws, &["status", "--brief"]);
    assert_eq!(r.err_code(), "Busy", "{r:?}");
    drop(d);
    let _ = b;
}

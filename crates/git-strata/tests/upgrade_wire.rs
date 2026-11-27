//! U3 and the local C1 cases use a real CLI with a fixture socket.
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use support::{FakeDaemon, Home};

fn ok(q: &Value, data: Value) -> Value {
    json!({"id":q["id"], "ok":true, "data":data})
}

fn refused(q: &Value, code: &str, reason: &str, safe: Value) -> Value {
    json!({"id":q["id"], "ok":false, "error":{"code":code, "message":"refused",
        "detail":{"reason":reason,"retry_safe":safe,"handover_id":"h1","phase":"park","retry_after_ms":2000}}})
}

fn old_envelope(q: &Value) -> Value {
    json!({"id":q["id"], "ok":false, "error":{"rpc":"Failed","message":"unknown envelope field \"client\""}})
}

fn context(home: &Home) -> git_strata::ctx::Ctx {
    let config_dir = home.config_dir();
    git_strata::ctx::Ctx {
        paths: git_strata::env::Paths {
            home: home.canon(),
            config_file: config_dir.join("config.toml"),
            pins: config_dir.join("pins.json"),
            socket: home.socket(),
            config_dir,
        },
        wait: git_strata::args::WaitArgs::default(),
    }
}

#[test]
fn c1_negotiates_per_call_and_drops_client_once_on_predispatch_refusal() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), |q| match q["method"].as_str().unwrap() {
        "daemon_status" => ok(q, json!({"features":["envelope.client"]})),
        "du" if q.get("client").is_some() => old_envelope(q),
        "du" => ok(q, json!({"bytes":13})),
        _ => panic!("unexpected request {q}"),
    });
    assert_eq!(h.run(&["du"]).ok()["bytes"], 13);
    let qs = d.requests();
    assert_eq!(qs.len(), 3, "{qs:?}");
    assert_eq!(qs[0]["method"], "daemon_status");
    assert!(qs[0].get("client").is_none());
    assert_eq!(qs[1]["client"], env!("CARGO_PKG_VERSION"));
    assert!(qs[2].get("client").is_none());
    assert_eq!(qs[1]["params"], qs[2]["params"]);
    assert_eq!(qs[1]["method"], qs[2]["method"]);
}

#[test]
fn c1_only_the_exact_parser_rejection_can_replay_a_write() {
    for (advertised, error, sends) in [
        (
            false,
            json!({"rpc":"Failed","message":"unknown envelope field \"client\""}),
            1,
        ),
        (
            true,
            json!({"rpc":"Failed","message":"unknown envelope field \"client\""}),
            2,
        ),
        (
            true,
            json!({"rpc":"Failed","message":"unknown envelope field \"client\" in handler"}),
            1,
        ),
        (
            true,
            json!({"code":"InvalidInput","message":"unknown envelope field \"client\"",
                     "detail":{"reason":"rpc_failed","method":"gc","rpc":"Failed"}}),
            1,
        ),
    ] {
        let h = Home::new();
        let d = FakeDaemon::start(&h.socket(), move |q| {
            if q["method"] == "daemon_status" {
                return ok(
                    q,
                    json!({"features": if advertised {vec!["envelope.client"]} else {vec![]}}),
                );
            }
            json!({"id":q["id"],"ok":false,"error":error})
        });
        assert_eq!(h.run(&["gc"]).err_code(), "InvalidInput");
        let qs = d.requests();
        let writes: Vec<_> = qs.iter().filter(|q| q["method"] == "gc").collect();
        assert_eq!(writes.len(), sends, "{qs:?}");
        assert_eq!(writes[0].get("client").is_some(), advertised);
        if sends == 2 {
            assert!(writes[1].get("client").is_none());
        }
    }
}

#[test]
fn u3_certified_upgrade_retries_a_write_with_zero_budget() {
    let h = Home::new();
    let counter = AtomicUsize::new(0);
    let d = FakeDaemon::start(&h.socket(), move |q| {
        if q["method"] == "daemon_status" {
            return ok(q, json!({"features":[]}));
        }
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            return refused(q, "Busy", "upgrading", json!(true));
        }
        ok(q, json!({"done":true}))
    });
    let run: support::Run = h
        .cmd(&["gc", "--timeout", "0"])
        .env("GIT_STRATA_TIMEOUT", "0.01")
        .output()
        .unwrap()
        .into();
    run.ok();
    let qs = d.requests();
    let writes: Vec<_> = qs.iter().filter(|q| q["method"] == "gc").collect();
    assert_eq!(writes.len(), 2, "{qs:?}");
    assert_eq!(writes[0]["params"], writes[1]["params"]);
}

#[test]
fn u3_uncertified_or_unrelated_refusals_do_not_replay() {
    for (code, reason, safe, wait) in [
        ("Busy", "upgrading", json!(false), false),
        ("Busy", "upgrading", json!("true"), false),
        ("Unknown", "upgrading", json!(true), false),
        ("Busy", "lock_wait", json!(true), false),
        ("Busy", "starting", json!(true), false),
        ("Busy", "store_unavailable", json!(true), false),
        ("Busy", "daemon_down", json!(true), true),
    ] {
        let h = Home::new();
        let attempts = AtomicUsize::new(0);
        let d = FakeDaemon::start(&h.socket(), move |q| {
            if q["method"] == "daemon_status" {
                return ok(q, json!({"features":[]}));
            }
            if attempts.fetch_add(1, Ordering::SeqCst) > 0 {
                return ok(q, json!({"unexpected_replay":true}));
            }
            refused(q, code, reason, safe.clone())
        });
        let mut command = h.cmd(&["gc"]);
        if wait {
            command.arg("--wait");
        }
        let run: support::Run = command
            .env("GIT_STRATA_TIMEOUT", "0")
            .output()
            .unwrap()
            .into();
        assert_eq!(run.reason(), reason, "{run:?}");
        assert_eq!(run.err_code(), code);
        assert!(run.env()["error"]["detail"].get("retries").is_none());
        assert_eq!(
            d.requests().iter().filter(|q| q["method"] == "gc").count(),
            1
        );
    }
}

#[test]
fn u3_startup_wait_is_explicit_and_spent_budget_preserves_refusal() {
    for reason in ["starting", "store_unavailable", "upgrading"] {
        let h = Home::new();
        let seen = Arc::new(AtomicUsize::new(0));
        let count = seen.clone();
        let _d = FakeDaemon::start(&h.socket(), move |q| {
            if q["method"] == "daemon_status" {
                return ok(q, json!({"features":[]}));
            }
            count.fetch_add(1, Ordering::SeqCst);
            refused(q, "Busy", reason, json!(reason == "upgrading"))
        });
        let mut command = h.cmd(&["du", "--timeout", "0.12"]);
        if reason != "upgrading" {
            command.arg("--wait");
        }
        let run: support::Run = command.output().unwrap().into();
        assert_eq!(run.reason(), reason, "{run:?}");
        let detail = &run.env()["error"]["detail"];
        let retries = detail["retries"].as_u64().unwrap();
        assert_eq!(detail["handover_id"], "h1");
        assert!(detail["waited_ms"].as_u64().unwrap() >= 120);
        assert_eq!(seen.load(Ordering::SeqCst) as u64, 1 + retries);
    }
}

#[test]
fn u3_c1_only_the_refused_rpc_is_replayed_and_capabilities_are_per_call() {
    use git_strata::rpc::Kind;
    let home = Home::new();
    let ctx = context(&home);
    let probes = AtomicUsize::new(0);
    let writes = AtomicUsize::new(0);
    let d = FakeDaemon::start(&home.socket(), move |q| {
        if q["method"] == "daemon_status" {
            let old = probes.fetch_add(1, Ordering::SeqCst) == 0;
            return ok(
                q,
                json!({"features": if old { vec![] } else { vec!["envelope.client"] }}),
            );
        }
        if writes.fetch_add(1, Ordering::SeqCst) == 1 {
            return refused(q, "Busy", "upgrading", json!(true));
        }
        ok(q, json!({"checkpoint":{"repo":q["params"]["repo"]}}))
    });
    for repo in ["a", "b"] {
        let params = json!({"session":"s-abcdefgh","repo":repo,"reason":{"kind":"Manual","message":"both"},"paths":[]});
        assert_eq!(
            ctx.call("checkpoint", params, Kind::Write).unwrap()["checkpoint"]["repo"],
            repo
        );
    }
    let qs = d.operations();
    assert_eq!(qs.len(), 3);
    assert_eq!(qs[0]["params"]["repo"], "a");
    assert!(qs[0].get("client").is_none());
    assert_eq!(qs[1]["params"]["repo"], "b");
    assert_eq!(qs[1]["client"], env!("CARGO_PKG_VERSION"));
    assert_eq!(qs[2]["params"], qs[1]["params"]);
}

#[test]
fn c1_announced_daemon_below_the_floor_cannot_receive_a_write() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), |q| {
        ok(q, json!({"version":"0.2.99","features":[]}))
    });
    let run = h.run(&["gc"]);
    assert_eq!(run.err_code(), "Unsupported");
    assert_eq!(run.reason(), "daemon_too_old");
    assert!(d.operations().is_empty());
}

/// Release assets and APFS mounts exist only in the hosted macOS matrix.
#[test]
#[ignore = "GitHub-hosted cross-version contract"]
fn c1_released_binaries() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/release-contract.py");
    let status = std::process::Command::new("python3")
        .arg(script)
        .arg(support::BIN)
        .arg(strata_types::MIN_CLIENT)
        .status()
        .unwrap();
    assert!(status.success(), "release contract: {status}");
}

/// Exercise the hosted harness's command and RPC checks using only current
/// Cargo artifacts and an isolated sparse image. Never downloads a release.
#[test]
#[ignore = "requires the Cargo-built fixture worker and an APFS sparse image"]
fn c1_fixture_worker_contract() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/release-contract.py");
    assert!(
        std::process::Command::new("python3")
            .arg(script)
            .arg(support::BIN)
            .arg("--fixture-only")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn u3_post_send_timeout_after_certified_refusal_stays_in_progress() {
    use std::time::Duration;
    let h = Home::new();
    let calls = AtomicUsize::new(0);
    let d = FakeDaemon::start(&h.socket(), move |q| {
        if q["method"] == "daemon_status" {
            return ok(q, json!({"features":["envelope.client"]}));
        }
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return refused(q, "Busy", "upgrading", json!(true));
        }
        std::thread::sleep(Duration::from_millis(2100));
        ok(q, json!({"finished":true}))
    });
    let run = h.run(&["sync", "--session", "s-abcdefgh", "--timeout", "1.6"]);
    assert_eq!(run.err_code(), "Unknown", "{run:?}");
    let detail = &run.env()["error"]["detail"];
    assert_eq!(detail["reason"], "client_timeout");
    assert_eq!(detail["outcome"], "in_progress");
    assert_eq!(detail["request_sent"], true);
    assert!(detail.get("retry_safe").is_none());
    let writes = d.operations();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0]["params"], writes[1]["params"]);
}

#[test]
fn c1_interruptible_write_negotiates_and_falls_back_once() {
    use git_strata::rpc::Kind;
    let h = Home::new();
    let ctx = context(&h);
    let d = FakeDaemon::start(&h.socket(), |q| {
        if q["method"] == "daemon_status" {
            return ok(q, json!({"features":["envelope.client"]}));
        }
        if q.get("client").is_some() {
            return old_envelope(q);
        }
        ok(q, json!({"removed":true}))
    });
    let out = ctx
        .call_interruptible(
            "rm",
            json!({"session":"s-abcdefgh","discard":false}),
            Kind::Write,
        )
        .unwrap()
        .unwrap();
    assert_eq!(out["removed"], true);
    let writes = d.operations();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0]["client"], env!("CARGO_PKG_VERSION"));
    assert!(writes[1].get("client").is_none());
    assert_eq!(writes[0]["params"], writes[1]["params"]);
}

#[test]
fn c1_every_rpc_crosses_a_legacy_envelope_parser() {
    use git_strata::rpc::Kind;
    use strata_daemon::rpc::{Method, parse_request};
    let h = Home::new();
    let ctx = context(&h);
    let cases: Value = serde_json::from_str(include_str!(
        "../../strata-daemon/tests/fixtures/wire_contract.json"
    ))
    .unwrap();
    let server = FakeDaemon::start(&h.socket(), |q| {
        if q.get("client").is_some() {
            return old_envelope(q);
        }
        if let Err(rejected) = parse_request(&serde_json::to_vec(q).unwrap()) {
            return rejected.0;
        }
        if q["method"] == "daemon_status" {
            // Revert race: every probe reaches the new generation and every
            // application request is parsed by a peer with the old key set.
            ok(
                q,
                json!({"accepted":"daemon_status","features":["envelope.client"]}),
            )
        } else {
            ok(q, json!({"accepted":q["method"]}))
        }
    });
    for method in Method::ALL {
        let name = method.name();
        let kind = if method.is_read_only() {
            Kind::Read
        } else {
            Kind::Write
        };
        let before = server.requests().len();
        let result = ctx.try_call(name, cases[name].clone(), kind).unwrap();
        assert_eq!(result["accepted"], name);
        let seen = server.requests();
        let frames = &seen[before..];
        if name == "daemon_status" {
            assert_eq!(frames.len(), 1);
            assert!(frames[0].get("client").is_none());
        } else {
            assert_eq!(frames.len(), 3, "{name}: {frames:?}");
            assert_eq!(frames[0]["method"], "daemon_status");
            assert_eq!(frames[1]["client"], env!("CARGO_PKG_VERSION"));
            assert!(frames[2].get("client").is_none());
            assert_eq!(frames[2]["method"], name);
            assert_eq!(frames[1]["params"], cases[name]);
            assert_eq!(frames[2]["params"], cases[name]);
        }
    }
}

#[test]
fn c1_new_downgrades_client_and_agent_once_each() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), |q| {
        if q["method"] == "daemon_status" {
            return ok(q, json!({"features":["envelope.client","new.agent"]}));
        }
        if q.get("client").is_some() {
            return old_envelope(q);
        }
        if q["params"].get("agent").is_some() {
            return json!({"id":q["id"],"ok":false,"error":{"rpc":"Failed","message":"new: unknown field `agent`"}});
        }
        ok(
            q,
            json!({"session":{"manifest":{"id":"s-abcdefgh","repos":{}}}}),
        )
    });
    h.run(&["new", "C1", "--repo", "tool", "--agent", "aider"])
        .ok();
    let writes = d.operations();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[0]["client"], env!("CARGO_PKG_VERSION"));
    assert!(writes[1].get("client").is_none() && writes[2].get("client").is_none());
    assert_eq!(writes[1]["params"]["agent"], "aider");
    assert!(writes[2]["params"].get("agent").is_none());
}

#[test]
fn u3_sync_final_certified_retry_has_its_own_io_deadline() {
    let h = Home::new();
    let attempts = AtomicUsize::new(0);
    let server = FakeDaemon::start(&h.socket(), move |q| {
        if q["method"] == "daemon_status" {
            return ok(q, json!({"features":[]}));
        }
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return refused(q, "Busy", "upgrading", json!(true));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        ok(q, json!({"finished":true}))
    });
    // The first backoff exhausts the retry budget. GS-2 still allows the
    // attempt at the deadline; its reply fits the separate call deadline.
    assert_eq!(h.run(&["sync", "--timeout", "0.25"]).ok()["finished"], true);
    assert_eq!(server.operations().len(), 2);
}

/// Validate the exact hosted config and non-TTY preview gate without opening
/// a store, mounting the image or resolving paths from the runner's HOME.
#[test]
fn c1_matrix_config_pins_the_preview_and_both_checkouts_to_the_fixture() {
    let home = Home::new();
    let volume = home.canon().join("vol");
    let source = include_str!("../../strata-daemon/tests/fixtures/wire_contract.toml")
        .replace("$VOLUME", volume.to_str().unwrap());
    let cfg = strata_types::Config::parse_for_fixture(&source, &home.canon()).unwrap();
    assert_eq!(cfg.paths.root, volume.join("data"));
    assert_eq!(cfg.repos.len(), 2);
    for (name, repo) in &cfg.repos {
        assert_eq!(repo.path, volume.join(name.as_str()));
        assert!(repo.publish.confirm);
        assert_eq!(repo.publish.push, name.as_str() == "tool");
    }
}

#[test]
fn c1_public_daemon_status_checks_the_floor_on_its_one_bare_request() {
    let h = Home::new();
    let peer = FakeDaemon::start(&h.socket(), |q| {
        ok(q, json!({"version":"0.2.3","features":["envelope.client"]}))
    });
    let run = h.run(&["daemon", "status"]);
    assert_eq!(run.err_code(), "Unsupported");
    assert_eq!(run.reason(), "daemon_too_old");
    assert_eq!(
        run.env()["error"]["detail"]["min_daemon"],
        strata_types::MIN_DAEMON
    );
    let frames = peer.requests();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["method"], "daemon_status");
    assert!(frames[0].get("client").is_none());
}

#[test]
fn u3_probe_time_counts_toward_the_targets_retry_budget() {
    let home = Home::new();
    let peer = FakeDaemon::start(&home.socket(), |q| {
        if q["method"] == "daemon_status" {
            std::thread::sleep(std::time::Duration::from_millis(160));
            return ok(q, json!({"features":[]}));
        }
        refused(q, "Busy", "upgrading", json!(true))
    });
    let run = home.run(&["du", "--timeout", "0.05"]);
    assert_eq!(run.reason(), "upgrading");
    assert_eq!(run.env()["error"]["detail"]["retries"], 0);
    assert_eq!(peer.operations().len(), 1);
}

/// CR r2 #6: v0.2.0 answers `daemon_status` successfully but without a
/// `version`. It is below MIN_DAEMON: no application request reaches it, and
/// `daemon status` does not report it as running.
#[test]
fn c1_unversioned_or_invalid_daemon_version_is_below_the_floor() {
    // `null` makes the fake drop the key (the captured v0.2.0 shape).
    for version in [json!(null), json!("not-a-version"), json!(3)] {
        let h = Home::new();
        let shown = version.clone();
        let peer = FakeDaemon::start(&h.socket(), move |q| {
            ok(
                q,
                json!({"version":shown.clone(),"features":["envelope.client"]}),
            )
        });
        let run = h.run(&["gc"]);
        assert_eq!(run.err_code(), "Unsupported", "{version}: {run:?}");
        assert_eq!(run.reason(), "daemon_too_old", "{version}");
        assert_eq!(
            run.env()["error"]["detail"]["min_daemon"],
            strata_types::MIN_DAEMON
        );
        let status = h.run(&["daemon", "status"]);
        assert_eq!(status.err_code(), "Unsupported", "{version}: {status:?}");
        assert_eq!(status.reason(), "daemon_too_old", "{version}");
        assert!(
            peer.operations().is_empty(),
            "{version}: {:?}",
            peer.requests()
        );
        assert!(
            peer.requests()
                .iter()
                .all(|q| q["method"] == "daemon_status")
        );
    }
}

/// CR r2 #7: a feature-gated write checks the feature and sends through one
/// negotiated call. A probe refused once spends the shared budget, so the
/// refused target gets no second allowance: exactly one write, and the
/// retry count carries over from the probe.
#[test]
fn u3_feature_gate_and_its_write_share_one_retry_budget() {
    let home = Home::new();
    let probes = Arc::new(AtomicUsize::new(0));
    let seen = probes.clone();
    let peer = FakeDaemon::start(&home.socket(), move |q| {
        if q["method"] == "daemon_status" {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                return refused(q, "Busy", "upgrading", json!(true));
            }
            return ok(q, json!({"features":["config.reload"]}));
        }
        refused(q, "Busy", "upgrading", json!(true))
    });
    let run: support::Run = home
        .cmd(&["config", "reload"])
        // Long enough for the first refusal to arrive on a loaded runner; the
        // backoff after it (over a second) still spends the whole budget.
        .env("GIT_STRATA_TIMEOUT", "0.5")
        .output()
        .unwrap()
        .into();
    assert_eq!(run.reason(), "upgrading", "{run:?}");
    assert_eq!(run.env()["error"]["detail"]["retries"], 1, "{run:?}");
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    let writes = peer.operations();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0]["method"], "config_reload");
}

#[test]
fn c1_sync_keeps_its_envelope_downgrade_across_the_parameter_fallback() {
    let home = Home::new();
    let peer = FakeDaemon::start(&home.socket(), |q| {
        if q["method"] == "daemon_status" {
            return ok(q, json!({"features":["envelope.client"]}));
        }
        if q.get("client").is_some() {
            return old_envelope(q);
        }
        if q["params"].get("caller_pids").is_some() {
            return json!({"id":q["id"],"ok":false,"error":{"rpc":"Failed","message":"sync: unknown field `caller_pids`"}});
        }
        ok(q, json!({"done":true}))
    });
    assert_eq!(home.run(&["sync", "--allow-cwd"]).ok()["done"], true);
    let frames = peer.requests();
    assert_eq!(
        frames
            .iter()
            .map(|q| q["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["daemon_status", "sync", "sync", "sync"]
    );
    assert!(frames[1].get("client").is_some());
    assert!(frames[2].get("client").is_none() && frames[3].get("client").is_none());
    assert!(frames[3]["params"].get("caller_pids").is_none());
}

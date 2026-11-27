//! Socketpairs exercise the production connection loop without a worker,
//! HOME, store, data-root lock, or filesystem socket.

use super::*;
use std::io::{BufRead, Cursor, Write as _};
use std::sync::{Mutex, mpsc};
use std::thread::{self, Scope};

const WAIT: Duration = Duration::from_secs(12);
const ID: &str = "u2";
const TARGET: u32 = 4242;

type Handler<'a> = dyn Fn(Request, Option<Write>) -> Reply + Sync + 'a;

fn frame(id: Value, method: &str, params: Value) -> Vec<u8> {
    let mut bytes =
        serde_json::to_vec(&json!({ "id": id, "method": method, "params": params })).unwrap();
    bytes.push(b'\n');
    bytes
}

fn preview(id: Value) -> Vec<u8> {
    frame(
        id,
        "publish_plan",
        json!({ "group": { "id": 0, "batches": [] } }),
    )
}

fn pair<'s>(
    scope: &'s Scope<'s, '_>,
    drain: &'s Drain,
    id: u64,
    initial: &[u8],
    handle: &'s Handler<'s>,
) -> BufReader<UnixStream> {
    let (mut client, stream) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    client.set_write_timeout(Some(WAIT)).unwrap();
    // Small pipelined inputs are already present before the first socket read.
    client.write_all(initial).unwrap();
    drain.register(id, stream.try_clone().unwrap());
    scope.spawn(move || {
        let _ = drain.serve_stream(id, stream, &BusyLog::default(), &mut io::sink(), handle);
    });
    BufReader::new(client)
}

fn reply(client: &mut BufReader<UnixStream>) -> Value {
    let mut line = String::new();
    assert_ne!(client.read_line(&mut line).unwrap(), 0, "missing reply");
    serde_json::from_str(&line).unwrap()
}

fn refused(reply: &Value, id: Value, generation: bool) {
    assert_eq!(reply["id"], id);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["code"], "Busy");
    let mut expected = json!({"reason":"upgrading", "retry_safe":true, "retry_after_ms":2000,
                              "handover_id":ID, "phase":"park"});
    if generation {
        expected["generation_changed"] = json!(true);
    }
    assert_eq!(reply["error"]["detail"], expected);
}

fn should_park(method: Method) -> bool {
    !method.is_read_only()
        || matches!(
            method,
            Method::PublishPlan | Method::Diff | Method::Grep | Method::Files
        )
}

fn wait_parked(drain: &Drain, count: usize) {
    let deadline = Instant::now() + WAIT;
    while drain.status().2["parked"] != count {
        assert!(
            Instant::now() < deadline,
            "expected {count} parked, got {}",
            drain.status().2
        );
        thread::sleep(Duration::from_millis(2));
    }
}

struct AbortOnDrop<'a>(&'a Drain);
impl Drop for AbortOnDrop<'_> {
    fn drop(&mut self) {
        self.0.abort_parking(ID);
    }
}

#[test]
fn u2_count_cap_detaches_and_replays_in_sequence_with_the_exact_suffix() {
    let drain = Drain::default();
    let calls = Mutex::new(Vec::new());
    let handle = |req: Request, write: Option<Write>| {
        assert_eq!(write.is_some(), req.method == Method::PublishPlan);
        calls.lock().unwrap().push(req.id.clone());
        Reply::ok(&req.id, json!({}))
    };
    thread::scope(|s| {
        let _cleanup = AbortOnDrop(&drain);
        assert!(drain.begin_parking(ID, TARGET));
        let suffix = preview(json!("buffered"));
        let partial = preview(json!("résumé"));
        let split = partial.iter().position(|b| *b == 0xc3).unwrap() + 1;
        let mut clients = Vec::new();
        for id in 0..64 {
            let mut bytes = preview(json!(id));
            if id == 0 {
                bytes.extend(&suffix);
                bytes.extend(&partial[..split]);
            }
            clients.push(pair(s, &drain, id, &bytes, &handle));
            wait_parked(&drain, clients.len());
        }
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(drain.open_connections(), 0);
        assert_eq!(drain.held_connections(), 64);
        drain.shutdown_connections(Shutdown::Read);
        {
            let state = drain.state();
            let first = state.parking.as_ref().unwrap().entries.front().unwrap();
            assert_eq!(first.frame.as_ref(), preview(json!(0)));
            assert_eq!(
                first.reader.buffer(),
                [suffix.as_slice(), &partial[..split]].concat()
            );
        }

        let mut overflow = pair(s, &drain, 64, &preview(json!("overflow")), &handle);
        refused(&reply(&mut overflow), json!("overflow"), false);
        let read = frame(json!("read"), "daemon_status", json!({}));
        overflow.get_mut().write_all(&read).unwrap();
        assert_eq!(reply(&mut overflow)["ok"], true);
        assert!(!drain.abort_parking("another-handover"));
        assert_eq!(
            drain.status().2,
            json!({"id":ID,"phase":"park","to":TARGET,"parked":64,
            "waiting_on":{"inflight":[],"background":0}})
        );
        assert!(drain.abort_parking(ID));
        for (id, client) in clients.iter_mut().enumerate() {
            assert_eq!(reply(client), json!({"ok":true,"id":id,"data":{}}));
        }
        assert_eq!(reply(&mut clients[0])["id"], "buffered");
        clients[0].get_mut().write_all(&partial[split..]).unwrap();
        assert_eq!(reply(&mut clients[0])["id"], "résumé");
        // A fresh reply marks the end of any buffered output on that stream.
        clients[0]
            .get_mut()
            .write_all(&preview(json!("fresh")))
            .unwrap();
        assert_eq!(reply(&mut clients[0])["id"], "fresh");
        assert!(drain.status().2.is_null());
        assert_eq!(drain.open_connections(), 65);
    });
    let calls = calls.into_inner().unwrap();
    assert_eq!(calls.len(), 68);
    assert_eq!(
        calls.iter().filter_map(Value::as_u64).collect::<Vec<_>>(),
        (0..64).collect::<Vec<_>>()
    );
    assert_eq!(drain.held_connections(), 0);
}

#[test]
fn u2_every_write_class_method_parks_before_its_handler() {
    let drain = Drain::default();
    let mut corpus: Value =
        serde_json::from_str(include_str!("../fixtures/wire_contract.json")).unwrap();
    corpus["sync"]["check"] = json!(true);
    corpus["gc"]["dry_run"] = json!(true);
    let calls = Mutex::new(Vec::new());
    let handle = |req: Request, write: Option<Write>| {
        assert_eq!(write.is_some(), should_park(req.method));
        calls.lock().unwrap().push(req.method);
        Reply::ok(&req.id, json!({}))
    };
    thread::scope(|s| {
        let _cleanup = AbortOnDrop(&drain);
        assert!(drain.begin_parking(ID, TARGET));
        let mut writers = Vec::new();
        for (id, &method) in Method::ALL.iter().enumerate() {
            let bytes = frame(
                json!(method.name()),
                method.name(),
                corpus[method.name()].clone(),
            );
            let mut client = pair(s, &drain, id as u64, &bytes, &handle);
            if should_park(method) {
                writers.push((method, client));
                wait_parked(&drain, writers.len());
            } else {
                assert_eq!(reply(&mut client)["id"], method.name());
            }
        }
        assert_eq!(
            calls.lock().unwrap().len(),
            Method::ALL.len() - writers.len()
        );
        assert!(drain.abort_parking(ID));
        for (method, mut client) in writers {
            assert_eq!(reply(&mut client)["id"], method.name());
        }
    });
    let mut calls = calls.into_inner().unwrap();
    calls.sort();
    assert_eq!(calls, Method::ALL);
}

#[test]
fn u2_byte_cap_counts_delimiters_and_read_ahead_at_the_exact_boundary() {
    // Preload the reader explicitly so OS socket packetization cannot change
    // which suffix bytes have already been consumed from the kernel.
    let drain = Drain::default();
    assert!(drain.begin_parking(ID, TARGET));
    let park = |id, wire_len, suffix_len| {
        let (mut client, stream) = UnixStream::pair().unwrap();
        let mut r = BufReader::new(stream);
        drain.register(id, r.get_ref().try_clone().unwrap());
        if suffix_len != 0 {
            client.write_all(&vec![b' '; suffix_len]).unwrap();
            assert_eq!(r.fill_buf().unwrap().len(), suffix_len);
        }
        let mut line = preview(json!(id));
        line.pop();
        let req = rpc::parse_request(&line).unwrap();
        line.resize(wire_len - 1, b' ');
        drain.route(id, r, line, Instant::now(), &req, false)
    };
    const HALF: usize = 16 * 1024 * 1024;
    assert!(matches!(park(1, HALF, 0), Route::Parked(_)));
    let Route::Serve(reader, Err(error)) = park(2, HALF - 31, 32) else {
        panic!("accepted more than 32 MiB");
    };
    assert_eq!(reader.buffer().len(), 32);
    refused(&Reply::error(&json!(2), &error).0, json!(2), false);
    assert!(matches!(park(3, HALF - 32, 32), Route::Parked(_)));
    assert_eq!(
        drain.state().parking.as_ref().unwrap().bytes,
        32 * 1024 * 1024
    );
    assert!(matches!(park(4, 128, 0), Route::Serve(_, Err(_))));
    assert_eq!(drain.status().2["parked"], 2);
}

#[test]
fn u2_preview_has_one_dispatch_guard_and_parking_fences_new_background_work() {
    let drain = Drain::default();
    let (entered, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let pass = drain.background().unwrap();
    let handle = |req: Request, write: Option<Write>| {
        assert!(write.is_some(), "preview entered handler without admission");
        if req.id == "first" {
            entered.send(()).unwrap();
            let _ = released.lock().unwrap().recv_timeout(WAIT);
        }
        Reply::ok(&req.id, json!({}))
    };
    thread::scope(|s| {
        let _cleanup = AbortOnDrop(&drain);
        let mut first = pair(s, &drain, 0, &preview(json!("first")), &handle);
        ready.recv_timeout(WAIT).unwrap();
        assert!(drain.begin_parking(ID, TARGET));
        assert!(!drain.begin_parking("replacement", TARGET + 1));
        let mut second = pair(s, &drain, 1, &preview(json!("second")), &handle);
        wait_parked(&drain, 1);
        let (_, inflight, status) = drain.status();
        assert_eq!(inflight.as_array().unwrap().len(), 1);
        assert_eq!(inflight[0]["method"], "publish_plan");
        assert_eq!(
            status["waiting_on"],
            json!({"inflight":inflight,"background":1})
        );
        assert!(drain.background().is_none());
        drop(pass);
        release.send(()).unwrap();
        assert_eq!(reply(&mut first)["id"], "first");
        assert_eq!(
            drain.status().2["waiting_on"],
            json!({"inflight":[],"background":0})
        );
        assert!(drain.abort_parking(ID));
        assert_eq!(reply(&mut second)["id"], "second");
        assert!(drain.background().is_some());
    });
}

#[test]
fn u2_generation_bound_streams_refuse_every_id_through_abort_until_client_close_or_idle() {
    let drain = Drain::default();
    let methods = Mutex::new(Vec::new());
    let handle = |req: Request, _: Option<Write>| {
        methods.lock().unwrap().push(req.method);
        let data = match &req.params {
            Params::Drain(p) => drain.command(p, || false).unwrap().0,
            _ => json!({"pid": 17, "started":"fixture"}),
        };
        Reply::ok(&req.id, data)
    };
    thread::scope(|s| {
        let _cleanup = AbortOnDrop(&drain);
        let mut identity = pair(
            s,
            &drain,
            1,
            &frame(json!("probe"), "daemon_status", json!({"verify":false})),
            &handle,
        );
        assert_eq!(reply(&mut identity)["data"]["pid"], 17);
        let mut owner = pair(
            s,
            &drain,
            2,
            &frame(json!("lease"), "drain", json!({"on":true})),
            &handle,
        );
        let lease = reply(&mut owner)["data"]["lease_id"].clone();
        assert!(lease.is_string());
        assert!(drain.begin_parking(ID, TARGET));

        let writes = [
            preview(json!(21)),
            preview(json!("22")),
            frame(json!(23), "daemon_status", json!({})),
            frame(json!("unknown"), "future_method", json!({})),
        ]
        .concat();
        identity.get_mut().write_all(&writes).unwrap();
        for id in [json!(21), json!("22"), json!(23), json!("unknown")] {
            refused(&reply(&mut identity), id, true);
        }
        owner.get_mut().write_all(&preview(json!("owner"))).unwrap();
        refused(&reply(&mut owner), json!("owner"), true);
        let cancel = frame(
            json!("cancel"),
            "drain",
            json!({"on":false,"lease_id":lease}),
        );
        let mut renewed = pair(s, &drain, 3, &cancel, &handle);
        refused(&reply(&mut renewed), json!("cancel"), true);
        assert_eq!(drain.status().0["lease_id"], lease);
        assert_eq!(drain.status().2["parked"], 0);
        assert!(drain.abort_parking(ID));

        identity
            .get_mut()
            .write_all(&preview(json!("after")))
            .unwrap();
        refused(&reply(&mut identity), json!("after"), true);
        identity.get_ref().shutdown(Shutdown::Write).unwrap();
        assert!(identity.fill_buf().unwrap().is_empty());

        // One still-active partial frame crosses the idle peer's five-second
        // deadline. Each new byte must reset that stream's own idle interval.
        let active = s.spawn(move || {
            for _ in 0..20 {
                renewed.get_mut().write_all(b" ").unwrap();
                thread::sleep(Duration::from_millis(300));
            }
            renewed
                .get_mut()
                .write_all(&preview(json!("still-active")))
                .unwrap();
            refused(&reply(&mut renewed), json!("still-active"), true);
        });
        let start = Instant::now();
        owner.get_mut().write_all(b"{").unwrap();
        assert!(owner.fill_buf().unwrap().is_empty());
        assert!(
            start.elapsed() >= Duration::from_millis(4800),
            "closed an active generation-bound stream early"
        );
        active.join().unwrap();
    });
    assert_eq!(
        methods.into_inner().unwrap(),
        [Method::DaemonStatus, Method::Drain]
    );
}

/// CR r2 #3: terminal stop, not abort. A generation-bound stream is blocked
/// writing a refusal to a client that is not reading yet, with the rest of a
/// complete frame still unread on the socket. Stop must answer every frame by
/// id before read halves close, then end the stream.
#[test]
fn u2_terminal_stop_answers_frames_queued_on_a_generation_bound_stream() {
    let drain = Drain::default();
    let handle = |req: Request, _: Option<Write>| Reply::ok(&req.id, json!({"pid": 17}));
    // Replies of about 3 KiB each outgrow the client's socket buffer while
    // the frames still fit in the stream's read-ahead and socket buffer.
    let ids: Vec<Value> = (0..6)
        .map(|k| json!(format!("{k}-{}", "x".repeat(3000))))
        .collect();
    thread::scope(|s| {
        let _cleanup = AbortOnDrop(&drain);
        let mut identity = pair(
            s,
            &drain,
            1,
            &frame(json!("probe"), "daemon_status", json!({"verify":false})),
            &handle,
        );
        assert_eq!(reply(&mut identity)["data"]["pid"], 17);
        assert!(drain.begin_parking(ID, TARGET));
        let frames = ids
            .iter()
            .map(|id| preview(id.clone()))
            .collect::<Vec<_>>()
            .concat();
        let mut writer = identity.get_ref().try_clone().unwrap();
        // Every byte reaches the stream before stop begins.
        s.spawn(move || writer.write_all(&frames).unwrap())
            .join()
            .unwrap();
        thread::sleep(Duration::from_millis(300));
        let (stopped, done) = mpsc::channel();
        let drain = &drain;
        s.spawn(move || {
            drain.finish_serving(false);
            // What Daemon::finish_connections does next.
            drain.shutdown_connections(Shutdown::Read);
            stopped.send(()).unwrap();
        });
        thread::sleep(Duration::from_millis(300));
        for id in &ids {
            refused(&reply(&mut identity), id.clone(), true);
        }
        assert!(
            identity.fill_buf().unwrap().is_empty(),
            "the stream ends after its queued frames"
        );
        done.recv_timeout(WAIT).expect("stop waited for the stream");
    });
}

#[test]
fn u4_real_request_method_controls_admission_across_the_rpc_corpus() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let corpus: Value =
        serde_json::from_str(include_str!("../fixtures/wire_contract.json")).unwrap();
    assert_eq!(corpus.as_object().unwrap().len(), Method::ALL.len());
    thread::scope(|s| {
        for &method in Method::ALL {
            let bytes = frame(json!(1), method.name(), corpus[method.name()].clone());
            s.spawn(move || {
                let drain = Drain::default();
                let run = catch_unwind(AssertUnwindSafe(|| {
                    rpc::serve_connection(
                        &mut Cursor::new(bytes),
                        &mut Vec::new(),
                        MAX_FRAME,
                        &mut |req| {
                            // A helper may supply a legitimate writer's name, and a
                            // best-effort handler can catch its panic. Neither masks
                            // a write from the actual outer RPC's admission audit.
                            let _ = catch_unwind(AssertUnwindSafe(|| {
                                drain.admit("config_reload", None, None)
                            }));
                            Reply::ok(&req.id, json!({}))
                        },
                    )
                    .unwrap();
                }));
                let hidden_write = !should_park(method);
                assert_eq!(run.is_err(), hidden_write, "{}", method.name());
                assert!(
                    drain.admit("config_reload", None, None).is_ok(),
                    "leaked request scope"
                );
            });
        }
    });
    // Exercise the real socket dispatcher as well. An audit failure escapes
    // the handler's catch and releases the registry fd so the caller sees EOF.
    let drain = Drain::default();
    let (mut client, stream) = UnixStream::pair().unwrap();
    drain.register(1, stream.try_clone().unwrap());
    client
        .write_all(&frame(json!(1), "daemon_status", json!({})))
        .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    thread::scope(|s| {
        let worker = s.spawn(|| {
            drain.serve_stream(
                1,
                stream,
                &BusyLog::default(),
                &mut io::sink(),
                &|req, _| {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        drain.admit("config_reload", None, None)
                    }));
                    Reply::ok(&req.id, json!({}))
                },
            )
        });
        assert!(worker.join().is_err());
    });
    assert_eq!(drain.open_connections(), 0);
    assert_eq!(
        rpc::read_frame(&mut BufReader::new(client)).unwrap(),
        Frame::Eof
    );
}

#[test]
fn u2_stop_refuses_untouched_frames_by_id_without_dispatch() {
    for poisoned in [false, true] {
        let drain = Drain::default();
        let called = Mutex::new(Vec::new());
        let handle = |req: Request, _: Option<Write>| {
            called.lock().unwrap().push(req.id.clone());
            Reply::ok(&req.id, json!({}))
        };
        thread::scope(|s| {
            let _cleanup = AbortOnDrop(&drain);
            assert!(drain.begin_parking(ID, TARGET));
            let pipeline = [
                preview(json!(1)),
                preview(json!("buffered")),
                frame(json!(3), "daemon_status", json!({})),
                b"{\"id\":".to_vec(),
            ]
            .concat();
            let mut write = pair(s, &drain, 0, &pipeline, &handle);
            wait_parked(&drain, 1);
            let mut control = pair(
                s,
                &drain,
                1,
                &frame(json!(2), "drain", json!({"on":true})),
                &handle,
            );
            wait_parked(&drain, 2);
            write
                .get_mut()
                .write_all(b"4,\"method\":\"daemon_status\",\"params\":{}}\n")
                .unwrap();
            drain.finish_serving(poisoned);
            for (id, client) in [(1, &mut write), (2, &mut control)] {
                refused(&reply(client), json!(id), false);
            }
            refused(&reply(&mut write), json!("buffered"), false);
            refused(&reply(&mut write), json!(3), false);
            refused(&reply(&mut write), json!(4), false);
        });
        assert!(called.into_inner().unwrap().is_empty());
    }
}

#[test]
fn u2_annotated_frame_logs_once_on_receipt_and_replays_despite_log_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Full<'a>(&'a AtomicUsize);
    impl io::Write for Full<'_> {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let count = AtomicUsize::new(0);
    let drain = Drain::default();
    let (mut client, stream) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    drain.register(0, stream.try_clone().unwrap());
    thread::scope(|s| {
        let _cleanup = AbortOnDrop(&drain);
        assert!(drain.begin_parking(ID, TARGET));
        s.spawn(|| {
            drain
                .serve_stream(
                    0,
                    stream,
                    &BusyLog::default(),
                    &mut Full(&count),
                    &|req, _| Reply::ok(&req.id, json!({})),
                )
                .unwrap()
        });
        writeln!(client, "{}", json!({"id":"annotated","method":"publish_plan","params":{"group":{"id":0,"batches":[]}},"client":"0.3.2"})).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        wait_parked(&drain, 1);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(drain.abort_parking(ID));
        assert_eq!(
            reply(&mut BufReader::new(client)),
            json!({"ok":true,"id":"annotated","data":{}})
        );
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

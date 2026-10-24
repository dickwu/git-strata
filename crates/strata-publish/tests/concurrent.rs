//! Task 6: concurrent publishes of one repo (publish.md §5.1, §7). The merge
//! and the chain compare-and-swap happen once, under one repo lock hold.
//! Every remote is a local bare repository in the sandbox.

mod support;

use std::collections::BTreeMap;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use strata_publish::seams::{self, PublishFault};
use strata_publish::{Published, publish};
use strata_store::PublishPhase;
use strata_types::{Code, ReleaseGroup, SessionId};
use support::*;

const CHAIN: &str = "refs/strata/publish/main";

/// A one-repo fixture whose origin has `shared.txt`; returns the seed commit.
fn fx() -> (Fx, String) {
    let fx = Fx::with(&["tool"]);
    let seed = fx.land_on_origin("tool", "main", &[("shared.txt", b"one\ntwo\nthree\n")], "seed");
    (fx, seed)
}

fn run(fx: &Fx, g: &ReleaseGroup) -> strata_publish::Result<Published> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    publish(&ctx, g, None)
}

fn session_with(fx: &Fx, files: &[(&str, &[u8])]) -> (SessionId, ReleaseGroup) {
    let s = fx.new_session(&["tool"]);
    let g = select(fx, &s, files);
    (s, g)
}

/// Write `files` into `s`, checkpoint, select exactly them.
fn select(fx: &Fx, s: &SessionId, files: &[(&str, &[u8])]) -> ReleaseGroup {
    fx.edit(s, "tool", files);
    let c = fx.checkpoint(s, "tool");
    let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
    group(vec![batch(s, "tool", c.id, &paths)])
}

fn show(fx: &Fx, rev: &str, path: &str) -> String {
    fx.sb.git(&fx.main("tool"), &["show", &format!("{rev}:{path}")])
}

/// path → content of every file in `rev`'s tree.
fn tree_files(fx: &Fx, rev: &str) -> BTreeMap<String, String> {
    fx.sb
        .git(&fx.main("tool"), &["ls-tree", "-r", "--name-only", rev])
        .lines()
        .map(|p| (p.to_string(), show(fx, rev, p)))
        .collect()
}

/// The first-parent commits from `from` (exclusive) to the chain, oldest first.
fn chain_since(fx: &Fx, from: &str) -> Vec<String> {
    let out = fx.sb.git(&fx.main("tool"), &["rev-list", "--first-parent", "--reverse", &format!("{from}..{CHAIN}")]);
    out.lines().map(str::to_string).collect()
}

/// (session, name, path, bytes) of one publish in a round.
type Edit<'a> = (&'a SessionId, &'a str, &'a str, &'a [u8]);

fn assert_serialized(fx: &Fx) {
    let mut hs: Vec<(f64, f64)> =
        fx.lock.holds().iter().filter(|h| h.repo.as_str() == "tool").map(|h| (h.start, h.end.expect("released"))).collect();
    hs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for w in hs.windows(2) {
        assert!(w[0].1 <= w[1].0, "two repo lock holds overlap: {w:?}");
    }
}

#[test]
fn disjoint_concurrent_publishes_all_land() {
    let (fx, start) = fx();
    let files = ["one.txt", "two.txt", "three.txt", "four.txt"];
    let groups: Vec<(SessionId, ReleaseGroup)> =
        files.iter().map(|f| session_with(&fx, &[(f, format!("{f}\n").as_bytes())])).collect();
    let gate = Barrier::new(groups.len());
    let results: Vec<strata_publish::Result<Published>> = std::thread::scope(|sc| {
        let hs: Vec<_> = groups
            .iter()
            .map(|(_, g)| {
                let gate = &gate;
                let fx = &fx;
                sc.spawn(move || {
                    gate.wait();
                    run(fx, g)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let published: Vec<Published> = results.into_iter().map(|r| r.expect("every disjoint publish lands")).collect();
    assert_serialized(&fx);
    let tip = fx.read_ref("tool", CHAIN).unwrap();
    for f in files {
        assert_eq!(show(&fx, &tip, f), f, "{f} is in the final tip");
    }
    assert_eq!(show(&fx, &tip, "shared.txt"), "one\ntwo\nthree");
    // One commit per publish, chained one after another.
    let chain = chain_since(&fx, &start);
    assert_eq!(chain.len(), 4, "{chain:?}");
    let mut commits: Vec<String> = published.iter().map(|p| p.commit.as_str().to_string()).collect();
    commits.sort();
    let mut on_chain = chain.clone();
    on_chain.sort();
    assert_eq!(commits, on_chain, "each publish has its own commit on the chain");
    // One successful row per session.
    for (s, _) in &groups {
        let rows: Vec<_> = fx
            .store()
            .publishes()
            .unwrap()
            .into_iter()
            .filter(|r| r.group.batches.iter().any(|b| &b.session == s))
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].phase, PublishPhase::Done);
        assert!(rows[0].outcome.as_ref().unwrap()["commit"].is_string(), "{:?}", rows[0].outcome);
    }
}

#[test]
fn conflict_already_landed_stops_the_group() {
    let (fx, _) = fx();
    let (_a, ga) = session_with(&fx, &[("shared.txt", b"one\nTWO-A\nthree\n")]);
    // B's group: a disjoint file plus a conflicting edit of the same line.
    let b1 = fx.new_session(&["tool"]);
    let b2 = fx.new_session(&["tool"]);
    let g1 = select(&fx, &b1, &[("b-only.txt", b"b\n")]);
    let g2 = select(&fx, &b2, &[("shared.txt", b"one\nTWO-B\nthree\n")]);
    let gb = group(g1.batches.into_iter().chain(g2.batches).collect());
    // A stops inside its lock hold, after its merge, before its commit.
    let p = seams::pause(fx.git_of("tool"), PublishFault::PauseBeforeCommit);
    let (ra, rb) = std::thread::scope(|sc| {
        let ha = sc.spawn(|| run(&fx, &ga));
        p.wait_until_paused();
        let hb = sc.spawn(|| run(&fx, &gb));
        // B has its row and is waiting on the repo lock A holds.
        let deadline = Instant::now() + Duration::from_secs(30);
        while fx.store().publishes().unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "B never recorded its row");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(200));
        p.release();
        (ha.join().unwrap(), hb.join().unwrap())
    });
    let pa = ra.expect("A lands");
    let e = rb.unwrap_err();
    assert_eq!(e.code, Code::Conflict, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    let cs = d["conflicts"].as_array().expect("conflicts");
    assert!(cs.iter().any(|c| c["path"] == "shared.txt"), "{d}");
    // A's commit stays the chain tip; nothing of B's group landed.
    let tip = fx.read_ref("tool", CHAIN).unwrap();
    assert_eq!(tip, pa.commit.as_str(), "the landed commit is not overwritten");
    assert_eq!(show(&fx, &tip, "shared.txt"), "one\nTWO-A\nthree");
    let o = fx.sb.git_output(&fx.main("tool"), &["cat-file", "-e", &format!("{tip}:b-only.txt")]);
    assert!(!o.status.success(), "the disjoint batch of the stopped group is not in the chain");
    let rows = fx.store().publishes().unwrap();
    assert_eq!(rows.len(), 2);
    let rb = rows.iter().find(|r| r.group.batches.len() == 2).unwrap();
    assert_eq!(rb.phase, PublishPhase::Done);
    assert_eq!(rb.outcome.as_ref().unwrap()["stopped"], "Conflict");
    assert_serialized(&fx);
}

#[test]
fn concurrent_publish_matches_serial_oracle() {
    let (fx, start) = fx();
    let a = fx.new_session(&["tool"]);
    let b = fx.new_session(&["tool"]);
    // A status reader runs the whole time; it must never take the repo lock.
    let stop = AtomicBool::new(false);
    let mut outcomes: Vec<(String, BTreeMap<String, String>, strata_publish::Result<Published>)> = Vec::new();
    let reader = std::thread::scope(|sc| {
        let r = sc.spawn(|| {
            let me = std::thread::current().id();
            let mut n = 0u32;
            while !stop.load(Ordering::SeqCst) {
                for s in [&a, &b] {
                    fx.sessions().status(s).expect("status reads during a publish");
                    n += 1;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            (me, n)
        });
        // Round 1: disjoint files from one base; the second to land merges.
        // Round 2: the same line of shared.txt; one lands, one conflicts.
        let rounds: [[Edit; 2]; 2] = [
            [(&a, "a", "a.txt", b"from a\n"), (&b, "b", "b.txt", b"from b\n")],
            [(&a, "a", "shared.txt", b"one\nA\nthree\n"), (&b, "b", "shared.txt", b"one\nB\nthree\n")],
        ];
        for round in rounds {
            let gs: Vec<(String, BTreeMap<String, String>, ReleaseGroup)> = round
                .iter()
                .map(|(s, name, p, c)| {
                    let g = select(&fx, s, &[(p, c)]);
                    let want = BTreeMap::from([(p.to_string(), String::from_utf8_lossy(c).trim_end().to_string())]);
                    (name.to_string(), want, g)
                })
                .collect();
            let gate = Barrier::new(2);
            let rs: Vec<_> = std::thread::scope(|sc2| {
                let hs: Vec<_> = gs
                    .iter()
                    .map(|(_, _, g)| {
                        let gate = &gate;
                        let fx = &fx;
                        sc2.spawn(move || {
                            gate.wait();
                            run(fx, g)
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for ((name, want, _), r) in gs.into_iter().zip(rs) {
                outcomes.push((name, want, r));
            }
        }
        stop.store(true, Ordering::SeqCst);
        r.join().unwrap()
    });
    let (reader_thread, reads) = reader;
    assert!(reads > 0);
    assert!(
        fx.lock.holds().iter().all(|h| h.thread != reader_thread),
        "a status read took the repo write lock"
    );
    assert_serialized(&fx);

    let ok: Vec<&Published> = outcomes.iter().filter_map(|(_, _, r)| r.as_ref().ok()).collect();
    let failed: Vec<_> = outcomes.iter().filter_map(|(n, _, r)| r.as_ref().err().map(|e| (n, e))).collect();
    assert_eq!(ok.len(), 3, "round 1 both land, round 2 one lands: {failed:?}");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].1.code, Code::Conflict, "{:?}", failed[0].1);
    let chain = chain_since(&fx, &start);
    assert_eq!(chain.len(), ok.len(), "successes == new commits on the chain");

    // Serial oracle: replay the successful publishes in chain order on the start tree.
    let mut by_commit: BTreeMap<String, &BTreeMap<String, String>> = BTreeMap::new();
    for (_, want, r) in &outcomes {
        if let Ok(p) = r {
            by_commit.insert(p.commit.as_str().to_string(), want);
        }
    }
    let mut oracle = tree_files(&fx, &start);
    for c in &chain {
        for (p, v) in by_commit[c] {
            oracle.insert(p.clone(), v.clone());
        }
        assert_eq!(tree_files(&fx, c), oracle, "commit {c} equals the serial replay so far");
    }
    let tip = fx.read_ref("tool", CHAIN).unwrap();
    assert_eq!(tree_files(&fx, &tip), oracle);
}

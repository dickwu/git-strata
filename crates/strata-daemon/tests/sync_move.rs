mod support;
use std::fs;
use std::time::SystemTime;
use strata_daemon::{startup::Daemon, sync_move};
use strata_types::{Code, CommitOid, SessionId, StrataError, SyncRequest};
use support::*;

fn lines(edit: &[(usize, &str)]) -> Vec<u8> {
    let mut ls: Vec<_> = (1..=12).map(|n| format!("line {n}\n")).collect();
    for (n, s) in edit {
        ls[n - 1] = format!("{s}\n");
    }
    ls.concat().into_bytes()
}
fn fixture() -> Dfx {
    let mut fx = Dfx::new();
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .rebase
        .quiet_secs = 3600;
    fx.write_config();
    land(
        &fx,
        &[
            ("a", &lines(&[])),
            ("b", &lines(&[])),
            ("binary", b"base\0"),
        ],
    );
    fx
}
fn land(fx: &Dfx, files: &[(&str, &[u8])]) -> CommitOid {
    let main = fx.main("tool");
    let head = fx.sb.commit(&main, files, "upstream");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    CommitOid::try_from(head).unwrap()
}
fn req(s: &SessionId) -> SyncRequest {
    SyncRequest {
        session: s.clone(),
        repos: None,
        check: false,
        fetch: true,
        resolved: vec![],
        keep: vec![],
        target: None,
        generation: None,
        accept_diverged: false,
    }
}
fn with_session(f: impl FnOnce(&Dfx, &Daemon<'_>, &SessionId)) {
    with_session_quiet(None, f)
}
fn with_session_quiet(quiet: Option<u64>, f: impl FnOnce(&Dfx, &Daemon<'_>, &SessionId)) {
    let mut fx = fixture();
    if let Some(q) = quiet {
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .rebase
            .quiet_secs = q;
        fx.write_config();
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let id = d
        .sessions
        .new(None, &[repo_name("tool")])
        .unwrap()
        .manifest
        .id;
    f(&fx, &d, &id);
}
fn conflict(fx: &Dfx, d: &Daemon<'_>, s: &SessionId) -> StrataError {
    fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "ours")])).unwrap();
    land(fx, &[("a", &lines(&[(2, "theirs"), (10, "upstream")]))]);
    let e = sync_move::run(d, req(s)).unwrap_err();
    assert_eq!(e.code, Code::Conflict, "{e:?}");
    assert_eq!(reason(&e), Some("sync_conflict"));
    e
}
fn resolve(s: &SessionId, e: &StrataError) -> SyncRequest {
    let detail = e.detail.as_ref().unwrap();
    SyncRequest {
        resolved: vec![rp("a")],
        target: Some(serde_json::from_value(detail["target"]["head"].clone()).unwrap()),
        generation: detail["generation"].as_u64(),
        ..req(s)
    }
}

#[test]
fn explicit_sync_accepts_a_quiet_glob_matching_tracked_files() {
    let mut fx = fixture();
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .rebase
        .quiet_skip = vec!["a".into()];
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let s = d
        .sessions
        .new(None, &[repo_name("tool")])
        .unwrap()
        .manifest
        .id;
    fs::write(ws(&core, &s, "tool").join("a"), lines(&[(2, "ours")])).unwrap();
    let head = land(&fx, &[("b", &lines(&[(3, "upstream")]))]);
    let reply = sync_move::run(&d, req(&s)).unwrap();
    assert_eq!(reply["repos"]["tool"]["result"], "moved", "{reply}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name("tool")].head,
        head
    );
    assert_eq!(
        fs::read(ws(&core, &s, "tool").join("a")).unwrap(),
        lines(&[(2, "ours")])
    );
}

#[test]
fn sync_conflict_returns_hunks_and_does_not_move() {
    with_session(|fx, d, s| {
        let before = d.core.store.session(s).unwrap().unwrap();
        let e = conflict(fx, d, s);
        let now = d.core.store.session(s).unwrap().unwrap();
        assert_eq!(before.manifest.repos, now.manifest.repos);
        assert!(
            e.detail.as_ref().unwrap()["conflicts"][0]["hunks"][0]["theirs_len"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
        let report = d.core.store.sync_report(s, &repo_name("tool")).unwrap();
        assert_eq!(report.len(), 1);
        fs::write(ws(d.core, s, "tool").join("a"), b"edited").unwrap();
        let blob = report[0].ours_blob.as_ref().unwrap();
        assert_eq!(
            d.core.store.read_blob(blob, u64::MAX - 1).unwrap(),
            lines(&[(2, "ours")])
        );
    });
}

#[test]
fn sync_resolved_merges_nonoverlapping_upstream_hunks() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "both")])).unwrap();
        let v = sync_move::run(d, resolve(s, &e)).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "moved");
        assert_eq!(
            fs::read(ws(d.core, s, "tool").join("a")).unwrap(),
            lines(&[(2, "both"), (10, "upstream")])
        );
        assert!(
            d.core
                .store
                .sync_report(s, &repo_name("tool"))
                .unwrap()
                .is_empty()
        );
        assert!(
            d.core
                .store
                .rebase_pending_of(s, &repo_name("tool"))
                .unwrap()
                .is_none()
        );
        let rec = d.core.store.session(s).unwrap().unwrap();
        assert!(rec.workspaces[&repo_name("tool")].conflicts.is_empty());
        let n = rec.workspaces[&repo_name("tool")].generation;
        assert_eq!(
            d.core
                .store
                .move_activity(s, &repo_name("tool"), n)
                .unwrap()
                .unwrap()
                .policy,
            strata_store::MoveActivityPolicy::Agent
        );
    });
}

#[test]
fn sync_resolved_overlap_takes_agent_text() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        fs::write(
            ws(d.core, s, "tool").join("a"),
            lines(&[(2, "resolved text")]),
        )
        .unwrap();
        sync_move::run(d, resolve(s, &e)).unwrap();
        assert!(
            String::from_utf8(fs::read(ws(d.core, s, "tool").join("a")).unwrap())
                .unwrap()
                .contains("resolved text")
        );
    });
}

#[test]
fn sync_resolved_unchanged_rejected() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        let e = sync_move::run(d, resolve(s, &e)).unwrap_err();
        assert_eq!(reason(&e), Some("resolved_unchanged"));
    });
}

#[test]
fn sync_keep_path_merges_nonoverlapping_upstream() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        let mut r = resolve(s, &e);
        r.keep = std::mem::take(&mut r.resolved);
        let v = sync_move::run(d, r).unwrap();
        assert_eq!(v["repos"]["tool"]["kept"][0], "a");
        assert_eq!(
            d.core
                .store
                .sync_move_history(s, &repo_name("tool"))
                .unwrap()[0]["kept"][0],
            "a"
        );
        assert_eq!(
            fs::read(ws(d.core, s, "tool").join("a")).unwrap(),
            lines(&[(2, "ours"), (10, "upstream")])
        );
    });
}

#[test]
fn keep_requires_target_and_generation() {
    with_session(|fx, d, s| {
        conflict(fx, d, s);
        for target_only in [false, true] {
            let mut r = req(s);
            r.keep = vec![rp("a")];
            if target_only {
                r.target = Some(
                    d.core.store.sync_report(s, &repo_name("tool")).unwrap()[0]
                        .target
                        .clone(),
                );
            }
            let e = sync_move::run(d, r).unwrap_err();
            assert_eq!(reason(&e), Some("resolution_binding_required"));
        }
    });
}

#[test]
fn sync_resolved_unrelated_rejected() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        let mut r = resolve(s, &e);
        r.resolved = vec![rp("b")];
        assert_eq!(
            reason(&sync_move::run(d, r).unwrap_err()),
            Some("resolved_unrelated")
        );
    });
}

#[test]
fn sync_resolved_overlap_changed_rejected() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        fs::write(
            ws(d.core, s, "tool").join("a"),
            lines(&[(2, "both"), (3, "adjacent")]),
        )
        .unwrap();
        let e = sync_move::run(d, resolve(s, &e)).unwrap_err();
        assert_eq!(reason(&e), Some("resolved_overlap_changed"));
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
    });
}

#[test]
fn resolved_missing_reported_path_still_refuses() {
    with_session(|fx, d, s| {
        fs::write(ws(d.core, s, "tool").join("b"), lines(&[(2, "ours")])).unwrap();
        land(fx, &[("b", &lines(&[(2, "theirs")]))]);
        let e = conflict(fx, d, s);
        fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "both")])).unwrap();
        let e = sync_move::run(d, resolve(s, &e)).unwrap_err();
        assert_eq!(reason(&e), Some("sync_conflict"));
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
    });
}

#[test]
fn sync_partial_resolution_can_repeat_accepted_paths() {
    for keep in [false, true] {
        with_session(|fx, d, s| {
            fs::write(ws(d.core, s, "tool").join("b"), lines(&[(2, "ours")])).unwrap();
            land(fx, &[("b", &lines(&[(2, "theirs"), (10, "upstream b")]))]);
            let first = conflict(fx, d, s);
            let rn = repo_name("tool");
            let original = d.core.store.sync_report(s, &rn).unwrap();
            let mut r = resolve(s, &first);
            if keep {
                r.keep = std::mem::take(&mut r.resolved);
            } else {
                fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "both")])).unwrap();
            }
            for _ in 0..2 {
                let e = sync_move::run(d, r.clone()).unwrap_err();
                assert_eq!(e.code, Code::Conflict, "{e:?}");
                let detail = e.detail.as_ref().unwrap();
                assert_eq!(detail[if keep { "kept" } else { "resolved" }][0], "a");
                assert_eq!(detail["conflicts"].as_array().unwrap().len(), 1);
                assert_eq!(detail["conflicts"][0]["path"], "b");
                let report = d.core.store.sync_report(s, &rn).unwrap();
                assert_eq!(
                    report.iter().find(|row| row.path == rp("a")),
                    original.iter().find(|row| row.path == rp("a"))
                );
                assert!(d.core.store.move_intents(s).unwrap().is_empty());
            }
            fs::write(ws(d.core, s, "tool").join("b"), lines(&[(2, "both b")])).unwrap();
            r.resolved.push(rp("b"));
            let v = sync_move::run(d, r).unwrap();
            assert_eq!(v["repos"]["tool"]["result"], "moved");
            assert_eq!(
                fs::read(ws(d.core, s, "tool").join("a")).unwrap(),
                lines(&[(2, if keep { "ours" } else { "both" }), (10, "upstream")])
            );
            assert_eq!(
                fs::read(ws(d.core, s, "tool").join("b")).unwrap(),
                lines(&[(2, "both b"), (10, "upstream b")])
            );
        });
    }
}

#[test]
fn sync_resolved_stale_target_head_moved() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "both")])).unwrap();
        let latest = land(fx, &[("new", b"new upstream")]);
        let e = sync_move::run(d, resolve(s, &e)).unwrap_err();
        assert_eq!(e.code, Code::HeadMoved);
        assert_eq!(
            e.detail.as_ref().unwrap()["target"]["head"],
            latest.as_str()
        );
        assert_eq!(
            d.core.store.sync_report(s, &repo_name("tool")).unwrap()[0].target,
            latest
        );
        // NB3: the refreshed report now records the resolved bytes. Repeating
        // --resolved is unchanged; --keep explicitly acknowledges this report.
        assert_eq!(
            reason(&sync_move::run(d, resolve(s, &e)).unwrap_err()),
            Some("resolved_unchanged")
        );
        let mut r = resolve(s, &e);
        r.keep = std::mem::take(&mut r.resolved);
        sync_move::run(d, r).unwrap();
    });
}

#[test]
fn sync_resolved_binary_and_delete_whole_file() {
    with_session(|fx, d, s| {
        fs::write(ws(d.core, s, "tool").join("binary"), b"ours\0").unwrap();
        land(fx, &[("binary", b"theirs\0")]);
        let e = sync_move::run(d, req(s)).unwrap_err();
        assert_eq!(e.detail.as_ref().unwrap()["conflicts"][0]["kind"], "binary");
        fs::remove_file(ws(d.core, s, "tool").join("binary")).unwrap();
        let mut r = resolve(s, &e);
        r.resolved = vec![rp("binary")];
        let v = sync_move::run(d, r).unwrap();
        assert_eq!(v["repos"]["tool"]["whole_file"][0], "binary");
        assert!(!ws(d.core, s, "tool").join("binary").exists());
    });
}

#[test]
fn sync_agent_policy_keeps_g4_late_write_proof() {
    with_session(|fx, d, s| {
        land(fx, &[("new", b"upstream")]);
        d.sessions.arm(strata_session::SessionSeam::PauseAt {
            phase: "move.recheck@tool".into(),
        });
        std::thread::scope(|sc| {
            let h = sc.spawn(|| sync_move::run(d, req(s)));
            d.sessions.wait_paused();
            fs::write(ws(d.core, s, "tool").join("a"), b"concurrent writer").unwrap();
            d.sessions.release();
            let e = h.join().unwrap().unwrap_err();
            assert_eq!(e.detail.unwrap()["defer"]["reason"], "changed_during_move");
        });
        assert_eq!(
            fs::read(ws(d.core, s, "tool").join("a")).unwrap(),
            b"concurrent writer"
        );
        assert!(!ws(d.core, s, "tool").join("new").exists());
    });
}

#[test]
fn sync_agent_policy_g0_refuses_open_move() {
    with_session(|fx, d, s| {
        land(fx, &[("new", b"upstream")]);
        let rec = d.core.store.session(s).unwrap().unwrap();
        let w = &rec.manifest.repos[&repo_name("tool")];
        d.core
            .store
            .begin_move(
                s,
                &repo_name("tool"),
                &w.base,
                &w.head,
                &rec.manifest.root.join(".move"),
                SystemTime::now(),
            )
            .unwrap();
        let e = sync_move::run(d, req(s)).unwrap_err();
        assert_eq!(e.detail.unwrap()["defer"]["reason"], "state");
    });
}

#[test]
fn sync_legacy_conflicts_cleared_without_report_or_target() {
    for keep in [false, true] {
        with_session(|fx, d, s| {
            // The explicit legacy mover keeps our conflicting side and records it.
            fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "ours")])).unwrap();
            let head = land(fx, &[("a", &lines(&[(2, "theirs")]))]);
            let rn = repo_name("tool");
            let git = &d.core.gits[&rn];
            git.fetch("origin", "main").unwrap();
            let old = d.core.store.session(s).unwrap().unwrap().manifest.repos[&rn]
                .base
                .clone();
            let target = d
                .sessions
                .ensure_base_for(&rn, &git.tree_of(&head).unwrap(), &old.deps)
                .unwrap();
            d.core
                .locks
                .with_repo(&rn, || {
                    d.sessions.move_workspace(s, &git.repo_id(), target, head)
                })
                .unwrap();
            assert_eq!(
                d.core.store.session(s).unwrap().unwrap().workspaces[&rn].conflicts,
                vec![rp("a")]
            );
            assert!(d.core.store.sync_report(s, &rn).unwrap().is_empty());
            let mut unrelated = req(s);
            unrelated.keep = vec![rp("a"), rp("not-conflicted")];
            assert_eq!(
                reason(&sync_move::run(d, unrelated).unwrap_err()),
                Some("resolved_unrelated")
            );
            assert_eq!(
                d.core.store.session(s).unwrap().unwrap().workspaces[&rn].conflicts,
                vec![rp("a")]
            );
            let mut r = req(s);
            if keep {
                r.keep = vec![rp("a")];
            } else {
                r.resolved = vec![rp("a")];
            }
            let v = sync_move::run(d, r).unwrap();
            assert_eq!(v["repos"]["tool"]["result"], "conflicts_cleared");
            assert!(
                d.core.store.session(s).unwrap().unwrap().workspaces[&rn]
                    .conflicts
                    .is_empty()
            );
        });
    }
}

#[test]
fn sync_base_diverged_refused() {
    with_session(|fx, d, s| {
        let rn = repo_name("tool");
        let main = fx.main("tool");
        let before = d.core.store.session(s).unwrap().unwrap().manifest.repos[&rn].clone();
        let parent = fx.sb.git(&main, &["rev-parse", "HEAD^"]);
        let tree = fx.sb.tree_of(&main, "HEAD");
        let other = fx
            .sb
            .git(&main, &["commit-tree", &tree, "-p", &parent, "-m", "fork"]);
        fx.sb.git(
            &main,
            &["update-ref", "refs/strata/remotes/origin/main", &other],
        );
        let mut r = req(s);
        r.fetch = false;
        assert_eq!(
            reason(&sync_move::run(d, r.clone()).unwrap_err()),
            Some("base_diverged")
        );
        // Retaining W.head on the publish chain forbids accepting it.
        fx.sb.git(
            &main,
            &[
                "update-ref",
                "refs/strata/publish/main",
                before.head.as_str(),
            ],
        );
        r.accept_diverged = true;
        assert_eq!(
            reason(&sync_move::run(d, r.clone()).unwrap_err()),
            Some("diverged_still_on_chain")
        );
        fx.sb
            .git(&main, &["update-ref", "-d", "refs/strata/publish/main"]);
        let v = sync_move::run(d, r).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "moved");
        assert!(
            !v["repos"]["tool"]["accepted_diverged"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    });
}

#[test]
fn sync_waits_while_worker_installs() {
    with_session(|fx, d, s| {
        land(fx, &[("bun.lock", b"lock v2\n")]);
        let rn = repo_name("tool");
        let git = &d.core.gits[&rn];
        git.fetch("origin", "main").unwrap();
        let tip = strata_deps::main_tip(&d.core.config.repos[&rn], git)
            .unwrap()
            .unwrap();
        strata_daemon::resolve::observe_tip(d.core, &rn, &tip).unwrap();
        let p = strata_deps::open_proposals(&d.core.store, Some(&rn))
            .unwrap()
            .into_iter()
            .find(|p| p.state == strata_deps::ProposalState::Pending)
            .unwrap();
        d.core
            .store
            .set_deps_auto(
                p.id,
                strata_store::DepsAutoState::Installing,
                None,
                SystemTime::now(),
            )
            .unwrap();
        let count = fx.sb.install_count();
        let v = sync_move::run(d, req(s)).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "waiting");
        assert_eq!(v["repos"]["tool"]["deps"]["state"], "installing");
        assert_eq!(fx.sb.install_count(), count);
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
    });
}

#[test]
fn sync_deps_ensure_precedes_queue_and_repo_lock() {
    with_session(|fx, d, s| {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        land(fx, &[("bun.lock", b"lock v2\n")]);
        let rn = repo_name("tool");
        let sid = s.clone();
        // Test seam executes under the build lock, before the install. Attempt
        // queue -> repo from a distinct thread: neither may be held by sync.
        let seen = Arc::new(AtomicBool::new(false));
        let hit = seen.clone();
        std::thread::scope(|sc| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let lock_thread = sc.spawn(move || {
                rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
                let r = d.core.locks.with_sessions(std::slice::from_ref(&sid), || {
                    d.core.locks.with_repo(&rn, || Ok(()))
                });
                done_tx.send(r).unwrap();
            });
            strata_deps::test_seams::after_attempt_begins(Box::new(move || {
                tx.send(()).unwrap();
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap()
                    .unwrap();
                hit.store(true, Ordering::SeqCst);
            }));
            let v = sync_move::run(d, req(s)).unwrap();
            assert_eq!(v["repos"]["tool"]["result"], "moved");
            lock_thread.join().unwrap();
        });
        strata_deps::test_seams::reset();
        assert!(seen.load(Ordering::SeqCst));
        assert_eq!(
            fs::read(ws(d.core, s, "tool").join("bun.lock")).unwrap(),
            b"lock v2\n"
        );
    });
}

#[test]
fn sync_waits_for_own_publish() {
    with_session(|fx, d, s| {
        fs::write(ws(d.core, s, "tool").join("a"), b"draft").unwrap();
        let cp = checkpoint(d.core, s, "tool");
        let id = d
            .core
            .store
            .record_publish(
                &group(vec![batch(s, "tool", cp.id, &["a"])]),
                SystemTime::now(),
            )
            .unwrap();
        land(fx, &[("new", b"upstream")]);
        let v = sync_move::run(d, req(s)).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "waiting");
        assert_eq!(v["repos"]["tool"]["reason"], "publishing");
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
        d.core
            .store
            .set_publish_phase(
                id,
                strata_store::PublishPhase::Running,
                strata_store::PublishPhase::Done,
                None,
                SystemTime::now(),
            )
            .unwrap();
    });
}

#[test]
fn sync_agent_policy_deps_dir_holders_on_source_only_move() {
    with_session(|fx, d, s| {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let dir = ws(d.core, s, "tool").join("node_modules");
        fs::create_dir_all(&dir).unwrap();
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut holder = Child(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("printf 'ready\\n'; exec /bin/sleep 60")
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready = String::new();
        BufReader::new(holder.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        // Drift makes this a real C2 source-only move, retaining the old set.
        fs::write(ws(d.core, s, "tool").join("bun.lock"), b"own input\n").unwrap();
        land(
            fx,
            &[
                ("new", b"upstream"),
                (
                    "package.json",
                    b"{\"name\":\"tool\",\"dependencies\":{\"x\":\"2\"}}\n",
                ),
            ],
        );
        let e = sync_move::run(d, req(s)).unwrap_err();
        assert_eq!(e.code, Code::OpenFiles, "{e:?}");
        assert!(!ws(d.core, s, "tool").join("new").exists());
    });
}

#[test]
fn sync_agent_keeps_post_swap_writer_old_tree() {
    with_session(|fx, d, s| {
        land(fx, &[("new", b"upstream")]);
        d.sessions.arm(strata_session::SessionSeam::PauseAt {
            phase: "move.swap@tool".into(),
        });
        std::thread::scope(|sc| {
            let h = sc.spawn(|| sync_move::run(d, req(s)));
            d.sessions.wait_paused();
            fs::write(ws(d.core, s, "tool").join("late"), b"late writer").unwrap();
            d.sessions.release();
            let v = h.join().unwrap().unwrap();
            let old = v["repos"]["tool"]["old_tree"].as_str().unwrap();
            assert_eq!(
                fs::read(std::path::Path::new(old).join("late")).unwrap(),
                b"late writer"
            );
        });
        let a = d.core.store.move_activities(s).unwrap();
        assert!(a.last().unwrap().late_writes.is_some());
    });
}

#[test]
fn sync_confirm_and_drift_move_source_only() {
    for confirm in [false, true] {
        let mut fx = fixture();
        if confirm {
            fx.config
                .repos
                .get_mut(&repo_name("tool"))
                .unwrap()
                .sync
                .deps = strata_types::SyncDeps::Confirm;
            fx.write_config();
        }
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let s = d
            .sessions
            .new(None, &[repo_name("tool")])
            .unwrap()
            .manifest
            .id;
        if !confirm {
            fs::write(ws(&core, &s, "tool").join("bun.lock"), b"session lock\n").unwrap();
        }
        let old = core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name("tool")]
            .base
            .deps
            .clone();
        // Different input files avoid a content conflict in the drift case.
        land(
            &fx,
            &[(
                "package.json",
                b"{\"name\":\"tool\",\"dependencies\":{\"x\":\"2\"}}\n",
            )],
        );
        let v = sync_move::run(&d, req(&s)).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "source_only");
        assert_eq!(
            v["repos"]["tool"]["deps"]["state"],
            if confirm { "waiting_human" } else { "drift" }
        );
        assert_eq!(
            core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name("tool")]
                .base
                .deps,
            old
        );
        let again = sync_move::run(&d, req(&s)).unwrap();
        assert_eq!(again["repos"]["tool"]["result"], "waiting");
        assert_eq!(
            again["repos"]["tool"]["deps"]["state"],
            v["repos"]["tool"]["deps"]["state"]
        );
        assert_eq!(
            again["repos"]["tool"]["deps"]["proposal"],
            v["repos"]["tool"]["deps"]["proposal"]
        );
        assert!(again["repos"]["tool"]["deps"]["proposal"].is_number());
        if confirm {
            assert_eq!(
                again["repos"]["tool"]["deps"]["fix"],
                format!(
                    "ask the human: git-strata deps approve {}",
                    v["repos"]["tool"]["deps"]["proposal"]
                )
            );
        }
    }
}

#[test]
fn sync_accept_diverged_refused_while_on_chain() {
    with_session(|fx, d, s| {
        let main = fx.main("tool");
        let parent = fx.sb.git(&main, &["rev-parse", "HEAD^"]);
        let tree = fx.sb.tree_of(&main, "HEAD");
        let other = fx
            .sb
            .git(&main, &["commit-tree", &tree, "-p", &parent, "-m", "fork"]);
        let old = d.core.store.session(s).unwrap().unwrap().manifest.repos[&repo_name("tool")]
            .head
            .clone();
        fx.sb.git(
            &main,
            &["update-ref", "refs/strata/remotes/origin/main", &other],
        );
        fx.sb.git(
            &main,
            &["update-ref", "refs/strata/publish/main", old.as_str()],
        );
        let mut r = req(s);
        r.fetch = false;
        r.accept_diverged = true;
        assert_eq!(
            reason(&sync_move::run(d, r).unwrap_err()),
            Some("diverged_still_on_chain")
        );
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
    });
}

#[test]
fn sync_split_repo_not_following() {
    let mut fx = fixture();
    let cfg = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    cfg.publish.branch = "work".into();
    cfg.deps_dirs.clear();
    cfg.seed_caches.clear();
    cfg.install_inputs.clear();
    cfg.install = None;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let s = d
        .sessions
        .new(None, &[repo_name("tool")])
        .unwrap()
        .manifest
        .id;
    land(&fx, &[("new", b"upstream")]);
    let v = sync_move::run(&d, req(&s)).unwrap();
    assert_eq!(v["repos"]["tool"]["result"], "not_following");
    assert_eq!(v["fetched"], serde_json::json!({}));
    assert!(
        core.store
            .rebase_pending_of(&s, &repo_name("tool"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn sync_keeps_own_change_source_only() {
    with_session(|fx, d, s| {
        let root = ws(d.core, s, "tool");
        fs::write(root.join("bun.lock"), b"own input\n").unwrap();
        let rn = repo_name("tool");
        let old = d.core.store.session(s).unwrap().unwrap().manifest.repos[&rn]
            .base
            .clone();
        let cfg = &d.core.config.repos[&rn];
        let git = &d.core.gits[&rn];
        let ctx = strata_deps::Ctx {
            store: &d.core.store,
            paths: &d.core.config.paths,
            repo: &rn,
            cfg,
            git,
            env: &d.core.env,
            locks: &d.core.locks,
        };
        let (inputs, key) = strata_deps::key_of(&ctx, &root).unwrap();
        let pinned = strata_deps::find_set(&d.core.store, &rn, &old.deps)
            .unwrap()
            .unwrap();
        d.core
            .locks
            .with_repo(&rn, || {
                strata_deps::propose(
                    &ctx,
                    strata_deps::ProposeArgs {
                        origin: strata_deps::ProposalOrigin::Session { session: s.clone() },
                        from: old.deps.clone(),
                        from_inputs: pinned.inputs_hash,
                        from_head: None,
                        to: key,
                        to_inputs: inputs,
                        tree: None,
                        head: None,
                    },
                )
            })
            .unwrap();
        land(
            fx,
            &[(
                "package.json",
                b"{\"name\":\"tool\",\"dependencies\":{\"x\":\"2\"}}\n",
            )],
        );
        let v = sync_move::run(d, req(s)).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "source_only");
        assert_eq!(v["repos"]["tool"]["deps"]["state"], "own_change");
        assert_eq!(fs::read(root.join("bun.lock")).unwrap(), b"own input\n");
    });
}

#[test]
fn sync_delete_modify_whole_file() {
    for keep in [false, true] {
        with_session(|fx, d, s| {
            fs::write(ws(d.core, s, "tool").join("a"), b"ours\n").unwrap();
            let main = fx.main("tool");
            fx.sb.git(&main, &["rm", "a"]);
            fx.sb.git(&main, &["commit", "-qm", "delete upstream"]);
            fx.sb.git(&main, &["push", "-q", "origin", "main"]);
            let e = sync_move::run(d, req(s)).unwrap_err();
            assert_eq!(
                e.detail.as_ref().unwrap()["conflicts"][0]["kind"],
                "delete_modify"
            );
            let mut r = resolve(s, &e);
            if keep {
                r.keep = std::mem::take(&mut r.resolved);
            } else {
                fs::remove_file(ws(d.core, s, "tool").join("a")).unwrap();
            }
            let out = sync_move::run(d, r).unwrap();
            assert_eq!(out["repos"]["tool"]["whole_file"][0], "a");
            assert_eq!(ws(d.core, s, "tool").join("a").exists(), keep);
        });
    }
}

#[test]
fn sync_missing_report_returns_head_moved_with_fresh_report() {
    with_session(|fx, d, s| {
        let e = conflict(fx, d, s);
        d.core
            .store
            .delete_sync_report(s, &repo_name("tool"))
            .unwrap();
        fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "both")])).unwrap();
        let e = sync_move::run(d, resolve(s, &e)).unwrap_err();
        assert_eq!(e.code, Code::HeadMoved);
        assert!(
            !d.core
                .store
                .sync_report(s, &repo_name("tool"))
                .unwrap()
                .is_empty()
        );
        assert!(d.core.store.move_intents(s).unwrap().is_empty());
    });
}

#[test]
fn sync_enrollment_preserves_unpushed_pending_target() {
    with_session(|fx, d, s| {
        let rn = repo_name("tool");
        let first = land(fx, &[("one", b"one")]);
        d.core.gits[&rn].fetch("origin", "main").unwrap();
        strata_daemon::main_sync::reconcile(d.core, &rn).unwrap();
        let row = d.core.store.rebase_pending_of(s, &rn).unwrap().unwrap();
        d.core
            .store
            .set_pending_reason(s, &rn, row.rev, "publish_unfinished")
            .unwrap();
        d.core
            .store
            .set_pending_reason(s, &rn, row.rev, "pending")
            .unwrap();
        let pending = d.core.store.rebase_pending_of(s, &rn).unwrap().unwrap();
        assert_eq!(pending.reason, "pending");
        assert!(pending.unpushed);
        land(fx, &[("two", b"two")]);
        d.core.gits[&rn].fetch("origin", "main").unwrap();
        strata_daemon::main_sync::reconcile(d.core, &rn).unwrap();
        assert_eq!(
            d.core
                .store
                .rebase_pending_of(s, &rn)
                .unwrap()
                .unwrap()
                .head
                .as_deref(),
            Some(first.as_str())
        );
    });
}

/// impl-r1 B1: legacy kept-side conflicts from an Explicit move. Neither an
/// automatic (T4/T2) nor an Agent move that did not name them clears them.
fn legacy_conflict(fx: &Dfx, d: &Daemon<'_>, s: &SessionId) {
    fs::write(ws(d.core, s, "tool").join("a"), lines(&[(2, "ours")])).unwrap();
    let head = land(fx, &[("a", &lines(&[(2, "theirs")]))]);
    let rn = repo_name("tool");
    let git = &d.core.gits[&rn];
    git.fetch("origin", "main").unwrap();
    let old = d.core.store.session(s).unwrap().unwrap().manifest.repos[&rn]
        .base
        .clone();
    let target = d
        .sessions
        .ensure_base_for(&rn, &git.tree_of(&head).unwrap(), &old.deps)
        .unwrap();
    d.core
        .locks
        .with_repo(&rn, || {
            d.sessions.move_workspace(s, &git.repo_id(), target, head)
        })
        .unwrap();
    assert_eq!(
        d.core.store.session(s).unwrap().unwrap().workspaces[&rn].conflicts,
        vec![rp("a")]
    );
}

#[test]
fn legacy_conflicts_survive_automatic_follow() {
    // T2 runs the automatic move now; the walk's quiet window is wall-clock.
    with_session_quiet(Some(0), |fx, d, s| {
        legacy_conflict(fx, d, s);
        let rn = repo_name("tool");
        let head = land(fx, &[("b", &lines(&[(5, "unrelated")]))]);
        d.core.gits[&rn].fetch("origin", "main").unwrap();
        strata_daemon::main_sync::reconcile(d.core, &rn).unwrap();
        assert!(d.core.store.rebase_pending_of(s, &rn).unwrap().is_some());
        strata_daemon::handlers::catch_up_pass(
            d,
            SystemTime::now() + std::time::Duration::from_secs(7200),
        );
        let rec = d.core.store.session(s).unwrap().unwrap();
        assert_eq!(rec.manifest.repos[&rn].head, head, "the automatic move ran");
        assert_eq!(rec.workspaces[&rn].conflicts, vec![rp("a")]);
        // O7: still acknowledgeable once up to date.
        let mut r = req(s);
        r.fetch = false;
        r.keep = vec![rp("a")];
        assert_eq!(
            sync_move::run(d, r).unwrap()["repos"]["tool"]["result"],
            "conflicts_cleared"
        );
    });
}

#[test]
fn legacy_conflicts_survive_unacknowledged_agent_sync() {
    with_session(|fx, d, s| {
        legacy_conflict(fx, d, s);
        let rn = repo_name("tool");
        let head = land(fx, &[("b", &lines(&[(5, "unrelated")]))]);
        let v = sync_move::run(d, req(s)).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "moved", "{v}");
        let rec = d.core.store.session(s).unwrap().unwrap();
        assert_eq!(rec.manifest.repos[&rn].head, head);
        assert_eq!(rec.workspaces[&rn].conflicts, vec![rp("a")]);
        let mut r = req(s);
        r.resolved = vec![rp("a")];
        assert_eq!(
            sync_move::run(d, r).unwrap()["repos"]["tool"]["result"],
            "conflicts_cleared"
        );
        assert!(
            d.core.store.session(s).unwrap().unwrap().workspaces[&rn]
                .conflicts
                .is_empty()
        );
    });
}

#[test]
fn legacy_conflicts_acknowledged_after_recovered_move_scene() {
    with_session(|fx, d, s| {
        legacy_conflict(fx, d, s);
        let rn = repo_name("tool");
        let st = &d.core.store;
        let ws = st.session(s).unwrap().unwrap().manifest.repos[&rn].clone();
        let n = st
            .begin_move(
                s,
                &rn,
                &ws.base,
                &ws.head,
                &ws.path.with_file_name(".scene-move"),
                SystemTime::now(),
            )
            .unwrap();
        st.set_move_state(
            s,
            &rn,
            n,
            strata_store::MoveState::Preparing,
            strata_store::MoveState::Swapping,
            Some(&[rp("a")]),
            SystemTime::now(),
        )
        .unwrap();
        st.fail_move(
            s,
            &rn,
            n,
            &serde_json::json!({"failed":"swap"}),
            SystemTime::now(),
        )
        .unwrap();
        st.resolve_move(s, &rn, n, true, SystemTime::now()).unwrap();
        let rec = st.session(s).unwrap().unwrap();
        assert!(
            rec.workspaces[&rn].scene.is_some(),
            "the scene stays as history"
        );
        assert_eq!(rec.workspaces[&rn].conflicts, vec![rp("a")]);
        let mut r = req(s);
        r.fetch = false;
        r.keep = vec![rp("a")];
        assert_eq!(
            sync_move::run(d, r).unwrap()["repos"]["tool"]["result"],
            "conflicts_cleared"
        );
        assert!(
            st.session(s).unwrap().unwrap().workspaces[&rn]
                .conflicts
                .is_empty()
        );
    });
}

#[test]
fn legacy_conflicts_acknowledged_on_repo_not_following() {
    // O7 refuses publishing recorded conflicts on every repo, so a split repo
    // (no main-follow) still needs `--keep`/`--resolved` to acknowledge them.
    let mut fx = fixture();
    let cfg = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    cfg.publish.branch = "work".into();
    cfg.deps_dirs.clear();
    cfg.seed_caches.clear();
    cfg.install_inputs.clear();
    cfg.install = None;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let s = d
        .sessions
        .new(None, &[repo_name("tool")])
        .unwrap()
        .manifest
        .id;
    legacy_conflict(&fx, &d, &s);
    let rn = repo_name("tool");
    let mut r = req(&s);
    r.fetch = false;
    r.keep = vec![rp("b")];
    let e = sync_move::run(&d, r).unwrap_err();
    assert_eq!(e.detail.unwrap()["reason"], "resolved_unrelated");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().workspaces[&rn].conflicts,
        vec![rp("a")]
    );
    let mut r = req(&s);
    r.fetch = false;
    r.keep = vec![rp("a")];
    let v = sync_move::run(&d, r).unwrap();
    assert_eq!(v["repos"]["tool"]["result"], "conflicts_cleared", "{v}");
    assert_eq!(v["repos"]["tool"]["following"], false);
    assert!(
        core.store.session(&s).unwrap().unwrap().workspaces[&rn]
            .conflicts
            .is_empty()
    );
    assert_eq!(
        sync_move::run(&d, req(&s)).unwrap()["repos"]["tool"]["result"],
        "not_following"
    );
}

#[test]
fn legacy_conflicts_acknowledged_before_behind_or_diverged_move() {
    for diverged in [false, true] {
        for keep in [false, true] {
            with_session(|fx, d, s| {
                legacy_conflict(fx, d, s);
                let rn = repo_name("tool");
                let before = d.core.store.session(s).unwrap().unwrap();
                let git = &d.core.gits[&rn];
                let main = fx.main("tool");
                if diverged {
                    let parent = fx.sb.git(&main, &["rev-parse", "HEAD^"]);
                    let tree = fx.sb.tree_of(&main, "HEAD");
                    let other = fx
                        .sb
                        .git(&main, &["commit-tree", &tree, "-p", &parent, "-m", "fork"]);
                    fx.sb.git(
                        &main,
                        &["update-ref", "refs/strata/remotes/origin/main", &other],
                    );
                } else {
                    land(fx, &[("b", &lines(&[(5, "behind")]))]);
                    git.fetch("origin", "main").unwrap();
                }
                let moves = d.core.store.move_intents(s).unwrap().len();
                let mut r = req(s);
                r.fetch = false;
                if keep {
                    r.keep = vec![rp("a")];
                } else {
                    r.resolved = vec![rp("a")];
                }
                let v = sync_move::run(d, r).unwrap();
                assert_eq!(v["repos"]["tool"]["result"], "conflicts_cleared", "{v}");
                assert_eq!(v["repos"]["tool"]["following"], true);
                let after = d.core.store.session(s).unwrap().unwrap();
                assert_eq!(before.manifest.repos, after.manifest.repos);
                assert_eq!(
                    before.workspaces[&rn].generation,
                    after.workspaces[&rn].generation
                );
                assert!(after.workspaces[&rn].conflicts.is_empty());
                assert_eq!(d.core.store.move_intents(s).unwrap().len(), moves);
            });
        }
    }
}

#[test]
fn legacy_conflicts_acknowledged_with_unrelated_saved_report() {
    with_session(|fx, d, s| {
        legacy_conflict(fx, d, s);
        let rn = repo_name("tool");
        fs::write(ws(d.core, s, "tool").join("b"), lines(&[(2, "ours")])).unwrap();
        land(fx, &[("b", &lines(&[(2, "theirs")]))]);
        let conflict = sync_move::run(d, req(s)).unwrap_err();
        assert_eq!(reason(&conflict), Some("sync_conflict"));
        let report = d.core.store.sync_report(s, &rn).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].path, rp("b"));
        let before = d.core.store.session(s).unwrap().unwrap();
        let mut r = req(s);
        r.resolved = vec![rp("a")];
        let v = sync_move::run(d, r).unwrap();
        assert_eq!(v["repos"]["tool"]["result"], "conflicts_cleared");
        assert_eq!(d.core.store.sync_report(s, &rn).unwrap(), report);
        let after = d.core.store.session(s).unwrap().unwrap();
        assert_eq!(before.manifest.repos, after.manifest.repos);
        assert!(after.workspaces[&rn].conflicts.is_empty());
    });
}

#[test]
fn legacy_conflicts_acknowledged_with_follow_main_false_and_g0_refuses() {
    for open_move in [false, true] {
        let mut fx = fixture();
        let rn = repo_name("tool");
        let cfg = fx.config.repos.get_mut(&rn).unwrap();
        cfg.sync.follow_main = Some(false);
        cfg.sync.deps = strata_types::SyncDeps::Confirm;
        fx.write_config();
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let s = d
            .sessions
            .new(None, std::slice::from_ref(&rn))
            .unwrap()
            .manifest
            .id;
        legacy_conflict(&fx, &d, &s);
        let before = core.store.session(&s).unwrap().unwrap();
        if open_move {
            let w = &before.manifest.repos[&rn];
            core.store
                .begin_move(
                    &s,
                    &rn,
                    &w.base,
                    &w.head,
                    &w.path.with_file_name(".open-move"),
                    SystemTime::now(),
                )
                .unwrap();
        }
        let mut r = req(&s);
        r.keep = vec![rp("a")];
        if open_move {
            let e = sync_move::run(&d, r).unwrap_err();
            assert_eq!(e.code, Code::Busy);
            assert_eq!(e.detail.as_ref().unwrap()["defer"]["reason"], "state");
            assert_eq!(
                e.detail.as_ref().unwrap()["defer"]["detail"]["open_move"],
                true
            );
            assert_eq!(
                core.store.session(&s).unwrap().unwrap().workspaces[&rn].conflicts,
                vec![rp("a")]
            );
        } else {
            let v = sync_move::run(&d, r).unwrap();
            assert_eq!(v["repos"]["tool"]["result"], "conflicts_cleared");
            assert_eq!(v["repos"]["tool"]["following"], false);
            let after = core.store.session(&s).unwrap().unwrap();
            assert!(after.workspaces[&rn].conflicts.is_empty());
            assert_eq!(before.manifest.repos, after.manifest.repos);
        }
    }
}

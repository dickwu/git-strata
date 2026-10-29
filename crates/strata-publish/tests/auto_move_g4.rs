//! rm-followups §1.4 G4 (sub-step 2.5b): an `Auto` move's activity
//! baseline, its step-14 recheck and the late-write rule for the pre-move
//! tree (Phase F, the old-tree checker, a restart, status and rm).

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use strata_publish::{MovePolicy, Published, RebaseReport, publish, rebase_session_with};
use strata_session::{OldTreeOutcome, SessionSeam, Sessions};
use strata_types::{Action, BaseGen, Code, ConfirmToken, ReleaseGroup, SessionId};
use support::*;

fn fx() -> Fx {
    let mut fx = Fx::with(&["tool"]);
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.rebase.quiet_secs = 0;
    fx.git_of("tool").fetch("origin", "main").unwrap();
    fx
}

fn run(fx: &Fx, g: &ReleaseGroup) -> Published {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    publish(&fx.ctx(&d, &ss), g, None).unwrap()
}

/// B (with `dir/b.txt`) idle, A published `a.txt`.
fn setup(fx: &Fx) -> (SessionId, Published) {
    let b = fx.new_session(&["tool"]);
    fx.edit(&b, "tool", &[("dir/b.txt", b"bravo\n"), ("keep.txt", b"k\n")]);
    let a = fx.new_session(&["tool"]);
    fx.edit(&a, "tool", &[("a.txt", b"alpha\n")]);
    let c = fx.checkpoint(&a, "tool");
    let p = run(fx, &group(vec![batch(&a, "tool", c.id, &["a.txt"])]));
    std::thread::sleep(Duration::from_millis(20));
    (b, p)
}

fn attempt(fx: &Fx, ss: &Sessions<'_>, b: &SessionId, p: &Published, policy: MovePolicy) -> strata_publish::Result<RebaseReport> {
    let d = fx.deps_ctx("tool");
    let ctx = fx.ctx(&d, ss);
    let old = fx.base_of(b, "tool");
    let ng = BaseGen { repo: repo_name("tool"), tree: fx.git_of("tool").tree_of(&p.commit).unwrap(), deps: old.deps };
    rebase_session_with(&ctx, b.clone(), &repo_name("tool"), ng, p.commit.clone(), policy)
}

/// Pause the `Auto` move at `seam`, run `during` on the workspace path,
/// then let it finish.
fn paused(fx: &Fx, b: &SessionId, p: &Published, seam: &str, during: impl FnOnce(&PathBuf)) -> strata_publish::Result<RebaseReport> {
    let ss = fx.sessions();
    ss.arm(SessionSeam::PauseAt { phase: format!("{seam}@tool") });
    let ws = fx.ws(b, "tool");
    std::thread::scope(|sc| {
        let h = sc.spawn(|| attempt(fx, &ss, b, p, MovePolicy::Auto));
        ss.wait_paused();
        during(&ws);
        ss.release();
        h.join().unwrap()
    })
}

fn last_intent(fx: &Fx, b: &SessionId) -> strata_store::MoveIntent {
    fx.store().move_intents(b).unwrap().into_iter().filter(|i| i.repo == repo_name("tool")).max_by_key(|i| i.n).unwrap()
}

fn old_path(i: &strata_store::MoveIntent) -> PathBuf {
    i.old_tree.clone().unwrap_or_else(|| i.staging.clone())
}

fn notice(fx: &Fx, b: &SessionId) -> Option<serde_json::Value> {
    let i = last_intent(fx, b);
    fx.store().move_activity(b, &repo_name("tool"), i.n).unwrap().and_then(|a| a.late_writes)
}

fn moved(r: &RebaseReport) {
    assert!(matches!(r, RebaseReport::Moved { .. }), "{r:?}");
}

/// One background check of the move's old tree, as `old_tree_loop` does.
fn check(ss: &Sessions<'_>, fx: &Fx, b: &SessionId) -> OldTreeOutcome {
    let i = last_intent(fx, b);
    let path = old_path(&i);
    let ino = std::os::unix::fs::MetadataExt::ino(&fs::symlink_metadata(&path).unwrap());
    let key = strata_session::OldTreeKey { session: b.clone(), repo: repo_name("tool"), n: i.n };
    ss.check_old_tree(&key, &path, ino, &Arc::new(AtomicBool::new(false))).unwrap()
}

struct Holder(Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn auto_rebase_write_after_step14_keeps_old_tree() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let r = paused(&fx, &b, &p, "move.swap", |ws| fs::write(ws.join("dir/b.txt"), b"late\n").unwrap()).unwrap();
    moved(&r);
    let n = notice(&fx, &b).expect("late-write notice");
    assert_eq!(n["count"], 1, "{n}");
    assert_eq!(n["paths"][0], "dir/b.txt", "{n}");
    let old = old_path(&last_intent(&fx, &b));
    assert_eq!(fs::read(old.join("dir/b.txt")).unwrap(), b"late\n", "the late write is intact in the kept tree");
    // The checker never retires it.
    let ss = fx.sessions();
    for _ in 0..2 {
        assert!(matches!(check(&ss, &fx, &b), OldTreeOutcome::Kept { .. }));
    }
    assert!(old.exists());
}

#[test]
fn auto_rebase_idle_move_retires_old_tree() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let ss = fx.sessions();
    moved(&attempt(&fx, &ss, &b, &p, MovePolicy::Auto).unwrap());
    let i = last_intent(&fx, &b);
    let a = fx.store().move_activity(&b, &repo_name("tool"), i.n).unwrap().expect("activity row");
    assert!(a.baseline.is_some());
    assert!(a.late_writes.is_none(), "the swap's own root change does not count: {:?}", a.late_writes);
    assert!(!old_path(&i).exists(), "an idle, unheld pre-move tree is retired at once");
}

#[test]
fn auto_rebase_top_level_create_in_old_tree_detected() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let r = paused(&fx, &b, &p, "move.swap", |ws| fs::write(ws.join("new-top.txt"), b"x\n").unwrap()).unwrap();
    moved(&r);
    let n = notice(&fx, &b).expect("late-write notice");
    let paths: Vec<&str> = n["paths"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(paths.contains(&"new-top.txt"), "{n}");
    assert!(old_path(&last_intent(&fx, &b)).join("new-top.txt").exists());
}

#[test]
fn auto_rebase_held_fd_on_old_tree_kept_then_two_passes() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let mut holder = None;
    let r = paused(&fx, &b, &p, "move.swap", |ws| {
        holder = Some(Holder(Command::new("/bin/sleep").arg("300").current_dir(ws.join("dir")).spawn().unwrap()));
        std::thread::sleep(Duration::from_millis(200));
    })
    .unwrap();
    moved(&r);
    assert!(notice(&fx, &b).is_none(), "a held cwd is not a write");
    let old = old_path(&last_intent(&fx, &b));
    assert!(old.exists(), "held: kept");
    let ss = fx.sessions();
    assert!(matches!(check(&ss, &fx, &b), OldTreeOutcome::Kept { .. }), "still held");
    drop(holder);
    // Two clean passes Q (0 here) apart.
    assert!(matches!(check(&ss, &fx, &b), OldTreeOutcome::Kept { .. }), "first clean pass only records");
    let (k, _, _) = ss.next_old_tree(Duration::from_secs(2)).expect("a kept Auto tree is requeued");
    assert_eq!(k.session, b);
    let i = last_intent(&fx, &b);
    let a = fx.store().move_activity(&b, &repo_name("tool"), i.n).unwrap().unwrap();
    assert_eq!(a.passes.len(), 1, "{:?}", a.passes);
    assert!(matches!(check(&ss, &fx, &b), OldTreeOutcome::Retired), "second clean pass retires");
    assert!(!old.exists());
}

#[test]
fn auto_rebase_walk_error_counts_as_changed() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let ws = fx.ws(&b, "tool");
    let r = paused(&fx, &b, &p, "move.swap", |ws| {
        fs::set_permissions(ws.join("dir"), fs::Permissions::from_mode(0o000)).unwrap();
    })
    .unwrap();
    moved(&r);
    let old = old_path(&last_intent(&fx, &b));
    let _ = fs::set_permissions(old.join("dir"), fs::Permissions::from_mode(0o755));
    let _ = fs::set_permissions(ws.join("dir"), fs::Permissions::from_mode(0o755));
    assert!(notice(&fx, &b).is_some(), "kept with a notice");
    assert!(old.exists());
}

#[test]
fn explicit_move_has_no_activity_row() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let ss = fx.sessions();
    moved(&attempt(&fx, &ss, &b, &p, MovePolicy::Explicit).unwrap());
    assert!(fx.store().move_activities(&b).unwrap().is_empty());
}

#[test]
fn auto_move_unreadable_baseline_keeps_tree() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let mut holder = None;
    moved(
        &paused(&fx, &b, &p, "move.swap", |ws| {
            holder = Some(Holder(Command::new("/bin/sleep").arg("300").current_dir(ws).spawn().unwrap()));
            std::thread::sleep(Duration::from_millis(200));
        })
        .unwrap(),
    );
    drop(holder);
    let i = last_intent(&fx, &b);
    assert!(fx.store().damage_move_activity_for_test(&b, &repo_name("tool"), i.n).unwrap());
    let ss = fx.sessions();
    for _ in 0..3 {
        assert!(matches!(check(&ss, &fx, &b), OldTreeOutcome::Kept { .. }));
    }
    let n = notice(&fx, &b).expect("fail-closed notice");
    assert_eq!(n["reason"], "baseline_missing", "{n}");
    assert!(old_path(&i).exists());
}

#[test]
fn auto_move_restart_after_done_uses_persisted_baseline() {
    let fx = fx();
    let (b, p) = setup(&fx);
    {
        let ss = fx.sessions();
        ss.arm(SessionSeam::StopAt { phase: "move.old_tree@tool".into() });
        let e = attempt(&fx, &ss, &b, &p, MovePolicy::Auto).unwrap_err();
        assert!(e.message.contains("seam"), "{e:?}");
    }
    let old = old_path(&last_intent(&fx, &b));
    assert!(old.exists());
    // Written while "down": an ignored file and one in node_modules.
    fs::create_dir_all(old.join("node_modules/late")).unwrap();
    fs::write(old.join("node_modules/late/x.js"), b"x\n").unwrap();
    fs::write(old.join(".env"), b"A=1\n").unwrap();
    // "Restart": a fresh Sessions, the startup queue, one check.
    let ss = fx.sessions();
    ss.queue_all_old_trees().unwrap();
    let (key, path, ino) = ss.next_old_tree(Duration::from_secs(1)).expect("queued");
    let out = ss.check_old_tree(&key, &path, ino, &Arc::new(AtomicBool::new(false))).unwrap();
    assert!(matches!(out, OldTreeOutcome::Kept { .. }), "{out:?}");
    let n = notice(&fx, &b).expect("notice");
    let paths: Vec<&str> = n["paths"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(paths.iter().any(|p| p.starts_with("node_modules")), "{n}");
    assert!(paths.contains(&".env"), "{n}");
}

#[test]
fn activity_walk_sees_ignored_deps_seed_and_env_writes() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let r = paused(&fx, &b, &p, "move.swap", |ws| {
        fs::create_dir_all(ws.join("node_modules")).unwrap();
        fs::write(ws.join("node_modules/seed.js"), b"x\n").unwrap();
        fs::write(ws.join(".env.local"), b"A=1\n").unwrap();
    })
    .unwrap();
    moved(&r);
    let n = notice(&fx, &b).expect("notice");
    let paths: Vec<&str> = n["paths"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(paths.contains(&"node_modules/seed.js") || paths.contains(&"node_modules"), "{n}");
    assert!(paths.contains(&".env.local"), "{n}");
}

#[test]
fn auto_rebase_change_between_checkpoint_and_swap_refuses() {
    let fx = fx();
    let (b, p) = setup(&fx);
    let before = fx.base_of(&b, "tool");
    let r = paused(&fx, &b, &p, "move.recheck", |ws| fs::write(ws.join("dir/b.txt"), b"mid\n").unwrap()).unwrap();
    match &r {
        RebaseReport::Deferred { reason, detail, .. } => {
            assert_eq!(reason, "changed_during_move", "{detail}");
            assert_eq!(detail["paths"][0], "dir/b.txt", "{detail}");
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(fx.base_of(&b, "tool"), before);
    assert_eq!(fs::read(fx.ws(&b, "tool").join("dir/b.txt")).unwrap(), b"mid\n");
    assert!(fx.store().move_intents(&b).unwrap().iter().all(|i| i.state != strata_store::MoveState::Preparing));
}

fn late_session(fx: &Fx) -> (SessionId, PathBuf) {
    let (b, p) = setup(fx);
    moved(&paused(fx, &b, &p, "move.swap", |ws| fs::write(ws.join("dir/b.txt"), b"late\n").unwrap()).unwrap());
    (b.clone(), old_path(&last_intent(fx, &b)))
}

#[test]
fn late_write_notice_in_offline_and_daemon_status() {
    let fx = fx();
    let (b, old) = late_session(&fx);
    let ss = fx.sessions();
    let st = ss.status(&b).unwrap();
    assert_eq!(st.rm.kept_trees.len(), 1, "{:?}", st.rm);
    assert_eq!(st.rm.kept_trees[0].path, old);
    assert!(
        st.rm.notes.iter().any(|l| l.starts_with("auto-rebase moved tool at ")
            && l.contains("1 file changed in the pre-move tree afterwards, kept at")),
        "{:?}",
        st.rm.notes
    );
    let v = serde_json::to_value(&st).unwrap();
    assert_eq!(v["kept_trees"][0]["paths"][0], "dir/b.txt", "{v}");
}

#[test]
fn rm_refuses_listing_late_write_paths() {
    let fx = fx();
    let (b, old) = late_session(&fx);
    let ss = fx.sessions();
    let e = ss.remove(&b, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert!(e.message.contains("written to the pre-move tree after an auto-rebase"), "{}", e.message);
    assert!(e.message.contains("dir/b.txt"), "{}", e.message);
    assert!(e.message.contains("intact"), "{}", e.message);
    let d = e.detail.unwrap();
    assert_eq!(d["old_trees"][0]["late_writes"]["paths"][0], "dir/b.txt", "{d}");
    assert!(old.exists());
}

#[test]
fn rm_discard_names_late_write_tree_and_trashes_it() {
    let fx = fx();
    let (b, old) = late_session(&fx);
    let ss = fx.sessions();
    let action = ss.discard_action(&b).unwrap();
    let Action::RemoveDiscard { kept_trees, .. } = &action else { panic!() };
    assert_eq!(kept_trees.len(), 1);
    assert_eq!(kept_trees[0].path, old.display().to_string());
    let prompt = strata_confirm::prompt_reason(&action);
    assert!(prompt.contains("also discards 1 late-written file in the kept pre-move tree"), "{prompt}");
    // A token for the bare session does not cover the kept tree.
    let bare = Action::RemoveDiscard { session: b.clone(), kept_trees: vec![] };
    let e = ss.remove(&b, true, Some(ConfirmToken { action: bare, issued: Instant::now(), nonce: 1 })).unwrap_err();
    assert_eq!(e.code, Code::NotConfirmed, "{e:?}");
    ss.remove(&b, true, Some(ConfirmToken { action, issued: Instant::now(), nonce: 2 })).unwrap();
    assert!(!old.exists(), "moved to trash");
}

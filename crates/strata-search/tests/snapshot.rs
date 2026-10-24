//! search.md §8 Task 4: the query snapshot, a replaced root, and the edges
//! of membership (§4.4, §4.5, §5.3, §5.5, §5.6).

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use strata_overlay::{MERGE_MAX_BYTES, Overlay};
use strata_search::{GrepOpts, Hit, Search, SearchSeam, Which};
use strata_session::NewReport;
use strata_types::{BlobId, Code, CommitOid, EntryKind, OverlayEntry, SessionId, StrataError, TreeOid, ViewId};
use support::*;

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

fn tree(rep: &NewReport) -> TreeOid {
    rep.manifest.repos[&repo_name("front")].base.tree.clone()
}

fn push(fx: &Fx) {
    fx.sb.git(&fx.main("front"), &["push", "-q", "origin", "HEAD:refs/heads/main"]);
}

fn base(fx: &Fx, files: &[(&str, &[u8])]) -> String {
    let h = fx.sb.commit(&fx.main("front"), files, "fixture");
    push(fx);
    h
}

fn ws(fx: &Fx, rep: &NewReport) -> PathBuf {
    fx.ws(rep, "front").path
}

fn grep(s: &Search<'_>, v: &ViewId, pat: &str) -> strata_search::Result<Vec<Hit>> {
    s.grep(Which::One(v.clone()), pat, &GrepOpts::default())
}

fn paths(h: &[Hit]) -> Vec<String> {
    h.iter().map(|h| h.path.as_str().to_string()).collect()
}

fn files(s: &Search<'_>, v: &ViewId) -> Vec<String> {
    s.files(v.clone(), "**").unwrap().iter().map(|p| p.as_str().to_string()).collect()
}

fn busy(e: &StrataError, why: &str) {
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert_eq!(reason(e), Some(why), "{e:?}");
}

fn untracked(path: &str) -> OverlayEntry {
    OverlayEntry { path: rp(path), kind: EntryKind::Untracked { blob: BlobId::try_from("0".repeat(64)).unwrap() }, mode: 0o100644 }
}

fn deleted(path: &str) -> OverlayEntry {
    OverlayEntry { path: rp(path), kind: EntryKind::Deleted, mode: 0 }
}

fn with(mut o: Overlay, extra: Vec<OverlayEntry>) -> Overlay {
    o.entries.extend(extra);
    o.entries.sort_by(|x, y| x.path.cmp(&y.path));
    o
}

/// Replace the workspace directory by a copy of itself (a new inode at the
/// same path), then run `edit` in the copy.
fn replace_root(path: &Path, edit: impl FnOnce(&Path)) {
    let old = path.with_extension("replaced");
    fs::rename(path, &old).unwrap();
    let st = std::process::Command::new("cp").arg("-Rp").arg(&old).arg(path).status().unwrap();
    assert!(st.success());
    edit(path);
}

fn wait_paused(s: &Search<'_>) -> usize {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(p) = s.paused_reader_ptr() {
            return p;
        }
        assert!(Instant::now() < deadline, "grep never paused");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Two sessions on two trees of `front`: `a` on T1 (`a.ts` with TOKEN),
/// `b` on T2 (T1 plus `extra.ts`).
fn two_trees(fx: &Fx) -> (NewReport, NewReport) {
    base(fx, &[("a.ts", b"TOKEN in a\n")]);
    let a = fx.new_session(&["front"]);
    base(fx, &[("extra.ts", b"TOKEN in extra\n")]);
    let b = fx.new_session(&["front"]);
    assert_ne!(tree(&a), tree(&b));
    (a, b)
}

fn overlays_of(fx: &Fx, reps: &[&NewReport]) -> FakeOverlays {
    let ov = FakeOverlays::default();
    for r in reps {
        ov.set(&id(r), "front", fx.capture(&id(r), "front"));
    }
    ov
}

#[test]
fn query_keeps_reader_snapshot_across_base_swap() {
    let fx = Fx::with(&["front"]);
    let (a, b) = two_trees(&fx);
    let ov = overlays_of(&fx, &[&a, &b]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let va = s.attach(&id(&a), &fx.rn("front")).unwrap();
    s.attach(&id(&b), &fx.rn("front")).unwrap();
    let p1 = s.generation_ptr(&fx.rn("front"), &tree(&a)).unwrap();
    let p2 = s.generation_ptr(&fx.rn("front"), &tree(&b)).unwrap();
    assert_ne!(p1, p2);

    s.arm(SearchSeam::PauseBeforeVerify);
    let hits = std::thread::scope(|sc| {
        let q = sc.spawn(|| grep(&s, &va, "TOKEN"));
        assert_eq!(wait_paused(&s), p1);
        s.swap_view_reader(va.clone(), &tree(&b)).unwrap();
        s.release();
        q.join().unwrap()
    })
    .unwrap();
    assert_eq!(s.paused_reader_ptr(), Some(p1), "the paused query kept its reader");
    // Ids resolved with T1's reader: a.ts only, never T2's extra.ts.
    assert_eq!(paths(&hits), vec!["a.ts"]);

    // Same again, but the root is replaced while paused: no hits at all.
    s.swap_view_reader(va.clone(), &tree(&a)).unwrap();
    s.arm(SearchSeam::PauseBeforeVerify);
    let w = ws(&fx, &a);
    let r = std::thread::scope(|sc| {
        let q = sc.spawn(|| grep(&s, &va, "TOKEN"));
        assert_eq!(wait_paused(&s), p1);
        s.swap_view_reader(va.clone(), &tree(&b)).unwrap();
        replace_root(&w, |p| fs::write(p.join("extra.ts"), b"TOKEN new tree row\n").unwrap());
        s.release();
        q.join().unwrap()
    });
    busy(&r.unwrap_err(), "root_changed");
    assert_eq!(s.paused_reader_ptr(), Some(p1));
}

#[test]
fn new_query_after_reattach_sees_new_base() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("old.ts", b"OLD_TREE_TOKEN\n")]);
    let a = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v1 = s.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_eq!(paths(&grep(&s, &v1, "TREE_TOKEN").unwrap()), vec!["old.ts"]);

    // Upstream drops old.ts and adds new.ts; the session moves there.
    let main = fx.main("front");
    fx.sb.git(&main, &["rm", "-q", "old.ts"]);
    let h = base(&fx, &[("new.ts", b"NEW_TREE_TOKEN\n")]);
    let head = CommitOid::try_from(h).unwrap();
    let g = ss.ensure_base(&fx.rn("front"), &head).unwrap();
    ss.move_workspace(&id(&a), &fx.gits[&fx.rn("front")].repo_id(), g, head).unwrap();

    let e = grep(&s, &v1, "TREE_TOKEN").unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert!(matches!(reason(&e), Some("root_changed") | Some("base_moved")), "{e:?}");
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    let v2 = s.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_ne!(v1, v2);
    assert_eq!(paths(&grep(&s, &v2, "TREE_TOKEN").unwrap()), vec!["new.ts"]);
    assert!(!files(&s, &v2).contains(&"old.ts".to_string()));
    // The old id still refuses.
    let e = grep(&s, &v1, "TREE_TOKEN").unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
}

#[test]
fn root_replacement_refuses_old_view() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"ROOT_OLD\n")]);
    let a = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let w = ws(&fx, &a);
    replace_root(&w, |p| fs::write(p.join("a.ts"), b"ROOT_NEW\n").unwrap());

    busy(&grep(&s, &v, "NOTHING_MATCHES_THIS").unwrap_err(), "root_changed");
    busy(&grep(&s, &v, "ROOT_").unwrap_err(), "root_changed");
    busy(&s.files(v.clone(), "**").unwrap_err(), "root_changed");
    busy(&s.refresh(v.clone(), &[]).unwrap_err(), "root_changed");

    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    let v2 = s.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_ne!(v, v2);
    let h = grep(&s, &v2, "ROOT_").unwrap();
    assert_eq!(h.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(), vec!["ROOT_NEW"]);
    busy(&grep(&s, &v, "ROOT_").unwrap_err(), "root_changed");
}

#[test]
fn refresh_during_root_swap_does_not_publish() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"BEFORE_SWAP\n")]);
    let a = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let w = ws(&fx, &a);
    // Runs inside refresh, after its first root check (overlays.last).
    let w2 = w.clone();
    ov.on_next_last(move || replace_root(&w2, |p| fs::write(p.join("a.ts"), b"AFTER_SWAP\n").unwrap()));
    busy(&s.refresh(v.clone(), &[rp("a.ts")]).unwrap_err(), "root_changed");
    busy(&grep(&s, &v, "AFTER_SWAP").unwrap_err(), "root_changed");
    busy(&grep(&s, &v, "SWAP").unwrap_err(), "root_changed");
}

#[test]
fn not_ready_does_not_answer_from_base() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"BASE_TOKEN\n"), ("b.ts", b"other BASE_TOKEN\n")]);
    let a = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let w = ws(&fx, &a);
    fs::remove_file(w.join("a.ts")).unwrap();
    mkfifo(&w.join("a.ts"));

    let e = s.refresh(v.clone(), &[rp("a.ts")]).unwrap_err();
    assert_eq!((e.code, reason(&e)), (Code::Unknown, Some("not_ready")), "{e:?}");
    let e = grep(&s, &v, "BASE_TOKEN").unwrap_err();
    assert_eq!((e.code, reason(&e)), (Code::Unknown, Some("not_ready")), "{e:?}");
    let e = s.files(v.clone(), "**").unwrap_err();
    assert_eq!((e.code, reason(&e)), (Code::Unknown, Some("not_ready")), "{e:?}");

    fs::remove_file(w.join("a.ts")).unwrap();
    fs::write(w.join("a.ts"), b"FIXED_TOKEN\n").unwrap();
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v.clone(), &[rp("a.ts")]).unwrap();
    assert_eq!(paths(&grep(&s, &v, "FIXED_TOKEN").unwrap()), vec!["a.ts"]);
    assert_eq!(paths(&grep(&s, &v, "BASE_TOKEN").unwrap()), vec!["b.ts"]);
}

#[test]
fn which_all_fails_if_one_root_changes() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"ALL_TOKEN\n")]);
    let a = fx.new_session(&["front"]);
    let b = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a, &b]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    s.attach(&id(&a), &fx.rn("front")).unwrap();
    let vb = s.attach(&id(&b), &fx.rn("front")).unwrap();
    assert_eq!(s.grep(Which::All, "ALL_TOKEN", &GrepOpts::default()).unwrap().len(), 2);

    s.arm(SearchSeam::PauseBeforeVerify);
    let wb = ws(&fx, &b);
    let r = std::thread::scope(|sc| {
        let q = sc.spawn(|| s.grep(Which::All, "ALL_TOKEN", &GrepOpts::default()));
        wait_paused(&s);
        replace_root(&wb, |_| {});
        s.release();
        q.join().unwrap()
    });
    busy(&r.unwrap_err(), "root_changed");
    busy(&grep(&s, &vb, "ALL_TOKEN").unwrap_err(), "root_changed");
    // Still Busy for All, not a list holding only a's hit.
    busy(&s.grep(Which::All, "ALL_TOKEN", &GrepOpts::default()).unwrap_err(), "root_changed");
}

#[test]
fn other_session_publish_does_not_change_this_view() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"SHARED_TOKEN\n")]);
    let a = fx.new_session(&["front"]);
    let b = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a, &b]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let va = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let vb = s.attach(&id(&b), &fx.rn("front")).unwrap();
    let g0 = grep(&s, &va, "SHARED_TOKEN|B_ONLY").unwrap();
    let f0 = files(&s, &va);
    let wb = ws(&fx, &b);
    for i in 0..3 {
        let p = format!("b_{i}.ts");
        fx.sb.write(&wb, &p, format!("B_ONLY_{i} SHARED_TOKEN\n").as_bytes());
        fx.sb.write(&wb, "a.ts", format!("SHARED_TOKEN edited by b {i}\n").as_bytes());
        ov.set(&id(&b), "front", fx.capture(&id(&b), "front"));
        s.refresh(vb.clone(), &[rp(&p), rp("a.ts")]).unwrap();
        s.refresh(va.clone(), &[]).unwrap();
        assert_eq!(grep(&s, &va, "SHARED_TOKEN|B_ONLY").unwrap(), g0, "round {i}");
        assert_eq!(files(&s, &va), f0, "round {i}");
        assert_eq!(grep(&s, &va, &format!("B_ONLY_{i}")).unwrap(), vec![]);
        assert_eq!(paths(&grep(&s, &vb, &format!("B_ONLY_{i}")).unwrap()), vec![p.clone()]);
    }
}

#[test]
fn candidate_count_matches_snapshot() {
    let fx = Fx::with(&["front"]);
    let (a, b) = two_trees(&fx);
    let ov = overlays_of(&fx, &[&a, &b]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    s.attach(&id(&b), &fx.rn("front")).unwrap();
    let w = ws(&fx, &a);
    let n0 = files(&s, &v).len();

    // Same path replaced: no new member.
    fx.sb.write(&w, "a.ts", b"TOKEN replaced\n");
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v.clone(), &[rp("a.ts")]).unwrap();
    assert_eq!(files(&s, &v).len(), n0);
    // A new path: one more.
    fx.sb.write(&w, "added.ts", b"TOKEN added\n");
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v.clone(), &[rp("added.ts")]).unwrap();
    assert_eq!(files(&s, &v).len(), n0 + 1);
    // A tombstone: one less.
    fs::remove_file(w.join("package.json")).unwrap();
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v.clone(), &[rp("package.json")]).unwrap();
    assert_eq!(files(&s, &v).len(), n0);
    // A Deleted entry for a path the base never had changes nothing.
    ov.set(&id(&a), "front", with(fx.capture(&id(&a), "front"), vec![deleted("ghost.ts")]));
    s.refresh(v.clone(), &[rp("ghost.ts")]).unwrap();
    assert_eq!(files(&s, &v).len(), n0);
    let all: Vec<String> = grep(&s, &v, ".").unwrap().iter().map(|h| h.path.as_str().to_string()).collect();

    // One query counts one snapshot: a reader swap mid-query (T2 adds
    // extra.ts) does not enter it.
    s.arm(SearchSeam::PauseBeforeVerify);
    let hits = std::thread::scope(|sc| {
        let q = sc.spawn(|| grep(&s, &v, "."));
        wait_paused(&s);
        s.swap_view_reader(v.clone(), &tree(&b)).unwrap();
        s.release();
        q.join().unwrap()
    })
    .unwrap();
    let got: Vec<String> = hits.iter().map(|h| h.path.as_str().to_string()).collect();
    assert_eq!(got, all);
    assert!(!got.contains(&"extra.ts".to_string()));
    // The next snapshot has T2's members.
    assert_eq!(files(&s, &v).len(), n0 + 1);
}

#[test]
fn rebuild_preserves_answers_for_empty_short_tombstone() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("short.ts", b"ab\n"), ("empty.ts", b""), ("tomb.ts", b"TOMB ab\n")]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fs::remove_file(w.join("tomb.ts")).unwrap();
    fx.sb.write(&w, "u.ts", b"ab untracked\n");
    fx.sb.write(&w, "tiny.ts", b"z");
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let answers = |v: &ViewId| {
        (
            grep(&s, v, ".").unwrap(),
            grep(&s, v, "ab").unwrap(),
            grep(&s, v, "z").unwrap(),
            grep(&s, v, "TOMB").unwrap(),
            files(&s, v),
        )
    };
    let v1 = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let before = answers(&v1);
    assert!(before.4.contains(&"empty.ts".to_string()) && !before.4.contains(&"tomb.ts".to_string()));
    assert_eq!(paths(&before.1), vec!["short.ts", "u.ts"]);
    assert_eq!(paths(&before.2), vec!["tiny.ts"]);
    assert!(before.3.is_empty());
    s.detach(v1).unwrap();
    let v2 = s.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_eq!(answers(&v2), before);

    // An untracked file removed only leaves the members.
    fs::remove_file(w.join("u.ts")).unwrap();
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v2.clone(), &[rp("u.ts")]).unwrap();
    let f = files(&s, &v2);
    assert_eq!(f.len(), before.4.len() - 1);
    assert!(!f.contains(&"u.ts".to_string()));
    assert!(!s.private_paths(v2.clone()).unwrap().contains(&"u.ts".to_string()));
    assert_eq!(paths(&grep(&s, &v2, "ab").unwrap()), vec!["short.ts"]);
}

#[test]
fn oversized_file_listed_not_indexed() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    let o = fx.capture(&id(&a), "front");
    let line = format!("{}\n", "x".repeat(99));
    let body = |token: &str, len: u64| {
        let mut b = format!("{token}\n").into_bytes();
        while (b.len() as u64) < len {
            b.extend_from_slice(line.as_bytes());
        }
        b.truncate(len as usize);
        b
    };
    fs::write(w.join("big.txt"), body("BIG_ONLY_TOKEN", MERGE_MAX_BYTES + 1)).unwrap();
    fs::write(w.join("near.txt"), body("NEAR_TOKEN", MERGE_MAX_BYTES - 1)).unwrap();
    assert_eq!(fs::metadata(w.join("big.txt")).unwrap().len(), 64 * 1024 * 1024 + 1);
    let ov = FakeOverlays::default();
    ov.set(&id(&a), "front", with(o, vec![untracked("big.txt"), untracked("near.txt")]));
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let f = files(&s, &v);
    assert!(f.contains(&"big.txt".to_string()) && f.contains(&"near.txt".to_string()), "{f:?}");
    assert_eq!(grep(&s, &v, "BIG_ONLY_TOKEN").unwrap(), vec![]);
    assert_eq!(paths(&grep(&s, &v, "NEAR_TOKEN").unwrap()), vec!["near.txt"]);
    assert!(!s.private_paths(v).unwrap().contains(&"big.txt".to_string()));
}

#[test]
fn literal_backslash_is_normal_path() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fx.sb.write(&w, "a\\b.txt", b"BACKSLASH_TOKEN\n");
    fx.sb.write(&w, "a/b.txt", b"SLASH_TOKEN\n");
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let f = files(&s, &v);
    assert!(f.contains(&"a\\b.txt".to_string()) && f.contains(&"a/b.txt".to_string()), "{f:?}");
    assert_eq!(paths(&grep(&s, &v, "BACKSLASH_TOKEN").unwrap()), vec!["a\\b.txt"]);
    assert_eq!(paths(&grep(&s, &v, "^SLASH_TOKEN").unwrap()), vec!["a/b.txt"]);
    let got: Vec<String> = s.files(v.clone(), "a\\b.txt").unwrap().iter().map(|p| p.as_str().to_string()).collect();
    assert_eq!(got, vec!["a\\b.txt"]);
    s.refresh(v.clone(), &[rp("a\\b.txt")]).unwrap();
    assert_eq!(paths(&grep(&s, &v, "BACKSLASH_TOKEN").unwrap()), vec!["a\\b.txt"]);
}

#[test]
fn verify_does_not_follow_symlink() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"SAFE_TOKEN\n"), ("b.ts", b"PIPE_TOKEN\n")]);
    let a = fx.new_session(&["front"]);
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let w = ws(&fx, &a);
    let outside = fx.sb.root.join("outside.txt");
    fs::write(&outside, b"SAFE_TOKEN leaked from outside\n").unwrap();
    fs::remove_file(w.join("a.ts")).unwrap();
    std::os::unix::fs::symlink(&outside, w.join("a.ts")).unwrap();
    fs::remove_file(w.join("b.ts")).unwrap();
    mkfifo(&w.join("b.ts"));

    // Run with a bound: a FIFO must never block the query.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let r1 = grep(&s, &v, "SAFE_TOKEN");
            let r2 = grep(&s, &v, "PIPE_TOKEN");
            let r3 = s.refresh(v.clone(), &[rp("b.ts")]);
            let r4 = grep(&s, &v, "PIPE_TOKEN");
            tx.send((r1, r2, r3, r4)).unwrap();
        });
        let (r1, r2, r3, r4) = rx.recv_timeout(Duration::from_secs(60)).expect("query hung");
        assert_eq!(r1.unwrap(), vec![], "no bytes of the link target");
        assert_eq!(r2.unwrap(), vec![]);
        let e = r3.unwrap_err();
        assert_eq!((e.code, reason(&e)), (Code::Unknown, Some("not_ready")), "{e:?}");
        let e = r4.unwrap_err();
        assert_eq!((e.code, reason(&e)), (Code::Unknown, Some("not_ready")), "{e:?}");
    });
}

#[test]
fn files_glob_does_not_cross_slash() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fx.sb.write(&w, "a.rs", b"fn a() {}\n");
    fx.sb.write(&w, "src/a.rs", b"fn b() {}\n");
    let ov = overlays_of(&fx, &[&a]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let g = |glob: &str| -> Vec<String> { s.files(v.clone(), glob).unwrap().iter().map(|p| p.as_str().to_string()).collect() };
    assert_eq!(g("*.rs"), vec!["a.rs"]);
    assert!(g("**/*.rs").contains(&"src/a.rs".to_string()));
    assert_eq!(g("src/*.rs"), vec!["src/a.rs"]);
    assert_eq!(g("*.nomatch"), Vec::<String>::new());
    let e = s.files(v.clone(), "").unwrap_err();
    assert_eq!((e.code, reason(&e)), (Code::InvalidInput, Some("glob")), "{e:?}");
}

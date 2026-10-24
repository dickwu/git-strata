//! search.md §8 Task 3: the session overlay shadows the shared base (§4.4,
//! §5.3–§5.6).

mod support;

use std::fs;

use strata_overlay::Overlay;
use strata_search::{GrepOpts, Hit, Search, Which};
use strata_session::NewReport;
use strata_types::{BlobId, EntryKind, OverlayEntry, SessionId, ViewId};
use support::*;

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

/// Push main's HEAD to origin's default branch, where new sessions start.
fn push(fx: &Fx, repo: &str) {
    let b = fx.config.repos[&fx.rn(repo)].default_branch.clone();
    fx.sb.git(&fx.main(repo), &["push", "-q", "origin", &format!("HEAD:refs/heads/{b}")]);
}

/// Commit `files` on main and make them the base of new sessions.
fn base(fx: &Fx, files: &[(&str, &[u8])]) {
    fx.sb.commit(&fx.main("front"), files, "fixture");
    push(fx, "front");
}

fn grep(s: &Search<'_>, v: &ViewId, pat: &str) -> Vec<Hit> {
    s.grep(Which::One(v.clone()), pat, &GrepOpts::default()).unwrap()
}

fn paths(h: &[Hit]) -> Vec<String> {
    h.iter().map(|h| h.path.as_str().to_string()).collect()
}

fn files(s: &Search<'_>, v: &ViewId) -> Vec<String> {
    s.files(v.clone(), "**").unwrap().iter().map(|p| p.as_str().to_string()).collect()
}

fn kind_of<'a>(o: &'a Overlay, path: &str) -> Option<&'a EntryKind> {
    o.entries.iter().find(|e| e.path.as_str() == path).map(|e| &e.kind)
}

fn blob() -> BlobId {
    BlobId::try_from("0".repeat(64)).unwrap()
}

fn untracked(path: &str) -> OverlayEntry {
    OverlayEntry { path: rp(path), kind: EntryKind::Untracked { blob: blob() }, mode: 0o100644 }
}

fn ws(fx: &Fx, rep: &NewReport) -> std::path::PathBuf {
    fx.ws(rep, "front").path
}

#[test]
fn overlay_entry_shadows_base_path() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"const x = 1; // BASE_ONLY\n"), ("b.ts", b"clean BASE_B\n")]);
    let a = fx.new_session(&["front"]);
    let b = fx.new_session(&["front"]);
    fx.sb.write(&ws(&fx, &a), "a.ts", b"const x = 2; // SESSION_ONLY\n");
    let ov = FakeOverlays::default();
    let oa = fx.capture(&id(&a), "front");
    assert!(matches!(kind_of(&oa, "a.ts"), Some(EntryKind::Modified { .. })), "{:?}", oa.entries);
    ov.set(&id(&a), "front", oa);
    ov.set(&id(&b), "front", fx.capture(&id(&b), "front"));
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let va = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let vb = s.attach(&id(&b), &fx.rn("front")).unwrap();

    assert_eq!(grep(&s, &va, "BASE_ONLY"), vec![]);
    let h = grep(&s, &va, "SESSION_ONLY");
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].path.as_str(), "a.ts");
    assert!(h[0].text.contains("SESSION_ONLY"));
    assert_eq!(h[0].line, 1);
    assert_eq!(h[0].session, id(&a));
    assert_eq!(h[0].repo, fx.rn("front"));
    // The clean file is answered from the shared base.
    assert_eq!(paths(&grep(&s, &va, "BASE_B")), vec!["b.ts"]);

    assert_eq!(grep(&s, &vb, "SESSION_ONLY"), vec![]);
    let hb = grep(&s, &vb, "BASE_ONLY");
    assert_eq!(paths(&hb), vec!["a.ts"]);
    assert_eq!(hb[0].session, id(&b));

    // Clean files never enter a private live: only the overlaid path does.
    assert_eq!(s.private_paths(va).unwrap(), vec!["a.ts".to_string()]);
    assert!(s.private_paths(vb).unwrap().is_empty());
}

#[test]
fn tombstone_hides_base_in_this_session_only() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("old.rs", b"fn OLD_MARKER() {}\n"), ("moved.rs", b"MOVED_MARKER one\nMOVED_MARKER two\n")]);
    let a = fx.new_session(&["front"]);
    let b = fx.new_session(&["front"]);
    let wa = ws(&fx, &a);
    fs::remove_file(wa.join("old.rs")).unwrap();
    fs::rename(wa.join("moved.rs"), wa.join("there.rs")).unwrap();
    let ov = FakeOverlays::default();
    let oa = fx.capture(&id(&a), "front");
    assert!(matches!(kind_of(&oa, "old.rs"), Some(EntryKind::Deleted)), "{:?}", oa.entries);
    assert!(
        matches!(kind_of(&oa, "there.rs"), Some(EntryKind::Renamed { from, .. }) if from.as_str() == "moved.rs"),
        "{:?}",
        oa.entries
    );
    ov.set(&id(&a), "front", oa);
    ov.set(&id(&b), "front", fx.capture(&id(&b), "front"));
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let va = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let vb = s.attach(&id(&b), &fx.rn("front")).unwrap();

    let fa = files(&s, &va);
    assert!(!fa.contains(&"old.rs".to_string()) && !fa.contains(&"moved.rs".to_string()), "{fa:?}");
    assert!(fa.contains(&"there.rs".to_string()));
    assert_eq!(grep(&s, &va, "OLD_MARKER"), vec![]);
    assert_eq!(paths(&grep(&s, &va, "MOVED_MARKER")), vec!["there.rs", "there.rs"]);

    let fb = files(&s, &vb);
    assert!(fb.contains(&"old.rs".to_string()) && fb.contains(&"moved.rs".to_string()), "{fb:?}");
    assert!(!fb.contains(&"there.rs".to_string()));
    assert_eq!(paths(&grep(&s, &vb, "OLD_MARKER")), vec!["old.rs"]);
    assert_eq!(paths(&grep(&s, &vb, "MOVED_MARKER")), vec!["moved.rs", "moved.rs"]);
}

#[test]
fn grep_all_tags_each_hit_with_session() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    let b = fx.new_session(&["front"]);
    fx.sb.write(&ws(&fx, &a), "hit_a.ts", b"ALPHA_HIT from a\n");
    fx.sb.write(&ws(&fx, &b), "hit_b.ts", b"nothing\nALPHA_HIT from b\n");
    let ov = FakeOverlays::default();
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    ov.set(&id(&b), "front", fx.capture(&id(&b), "front"));
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let va = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let _vb = s.attach(&id(&b), &fx.rn("front")).unwrap();

    let all = s.grep(Which::All, "ALPHA_HIT", &GrepOpts::default()).unwrap();
    assert_eq!(all.len(), 2, "{all:?}");
    let mut want = vec![(id(&a), "hit_a.ts", 1), (id(&b), "hit_b.ts", 2)];
    want.sort();
    let got: Vec<(SessionId, &str, u32)> = all.iter().map(|h| (h.session.clone(), h.path.as_str(), h.line)).collect();
    assert_eq!(got, want);
    assert!(all.iter().all(|h| h.repo == fx.rn("front")));

    let one = grep(&s, &va, "ALPHA_HIT");
    assert_eq!(one.len(), 1);
    assert_eq!((one[0].session.clone(), one[0].repo.clone(), one[0].path.as_str()), (id(&a), fx.rn("front"), "hit_a.ts"));
}

#[test]
fn grep_zero_hits_is_empty() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    let ov = FakeOverlays::default();
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    let ss = fx.sessions();
    let empty = Search::open(&fx.config, &ss, &ov);
    assert_eq!(empty.grep(Which::All, "anything", &GrepOpts::default()).unwrap(), vec![]);
    let s = Search::open(&fx.config, &ss, &ov);
    let va = s.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_eq!(s.grep(Which::One(va), "NO_SUCH_TOKEN_[0-9]+", &GrepOpts::default()).unwrap(), vec![]);
    assert_eq!(s.grep(Which::All, "NO_SUCH_TOKEN", &GrepOpts::default()).unwrap(), vec![]);
}

#[test]
fn rename_is_tombstone_plus_new_path() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("old.ts", b"RENAME_ME body\n")]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fs::rename(w.join("old.ts"), w.join("new.ts")).unwrap();
    let ov = FakeOverlays::default();
    let o = fx.capture(&id(&a), "front");
    assert!(matches!(kind_of(&o, "new.ts"), Some(EntryKind::Renamed { .. })), "{:?}", o.entries);
    ov.set(&id(&a), "front", o);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let f = files(&s, &v);
    assert!(f.contains(&"new.ts".to_string()) && !f.contains(&"old.ts".to_string()), "{f:?}");
    assert_eq!(paths(&grep(&s, &v, "RENAME_ME")), vec!["new.ts"]);

    // refresh told only `from`: both paths are updated.
    fx.sb.write(&w, "new.ts", b"RENAME_ME body\nSECOND_EDIT\n");
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v.clone(), &[rp("old.ts")]).unwrap();
    assert_eq!(paths(&grep(&s, &v, "SECOND_EDIT")), vec!["new.ts"]);
    assert!(!files(&s, &v).contains(&"old.ts".to_string()));

    // refresh told only `path`.
    fx.sb.write(&w, "new.ts", b"THIRD_EDIT\n");
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    s.refresh(v.clone(), &[rp("new.ts")]).unwrap();
    assert_eq!(paths(&grep(&s, &v, "THIRD_EDIT")), vec!["new.ts"]);
    assert_eq!(grep(&s, &v, "RENAME_ME"), vec![]);
    assert!(!files(&s, &v).contains(&"old.ts".to_string()));
}

#[test]
fn commit_inside_session_keeps_overlay_hit() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[("a.ts", b"BASE_TEXT\n")]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fx.sb.commit(&w, &[("a.ts", b"SESSION_TEXT\n")], "inside the session");
    let ov = FakeOverlays::default();
    let o = fx.capture(&id(&a), "front");
    assert!(matches!(kind_of(&o, "a.ts"), Some(EntryKind::Modified { .. })), "{:?}", o.entries);
    ov.set(&id(&a), "front", o);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_eq!(paths(&grep(&s, &v, "SESSION_TEXT")), vec!["a.ts"]);
    assert_eq!(grep(&s, &v, "BASE_TEXT"), vec![]);

    // Back to the base bytes and out of the overlay.
    fx.sb.write(&w, "a.ts", b"BASE_TEXT\n");
    let o = fx.capture(&id(&a), "front");
    assert!(kind_of(&o, "a.ts").is_none(), "{:?}", o.entries);
    ov.set(&id(&a), "front", o);
    s.refresh(v.clone(), &[rp("a.ts")]).unwrap();
    assert_eq!(paths(&grep(&s, &v, "BASE_TEXT")), vec!["a.ts"]);
    assert_eq!(grep(&s, &v, "SESSION_TEXT"), vec![]);
    assert!(s.private_paths(v).unwrap().is_empty());
}

#[test]
fn untracked_searches_like_added() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fx.sb.write(&w, "added.ts", b"TWIN_TOKEN added\n");
    fx.sb.git(&w, &["add", "added.ts"]);
    fx.sb.write(&w, "loose.ts", b"TWIN_TOKEN untracked\n");
    let ov = FakeOverlays::default();
    let o = fx.capture(&id(&a), "front");
    assert!(matches!(kind_of(&o, "added.ts"), Some(EntryKind::Added { .. })), "{:?}", o.entries);
    assert!(matches!(kind_of(&o, "loose.ts"), Some(EntryKind::Untracked { .. })), "{:?}", o.entries);
    ov.set(&id(&a), "front", o);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let f = files(&s, &v);
    assert!(f.contains(&"added.ts".to_string()) && f.contains(&"loose.ts".to_string()), "{f:?}");
    assert_eq!(paths(&grep(&s, &v, "TWIN_TOKEN")), vec!["added.ts", "loose.ts"]);
}

#[test]
fn rename_or_symlink_does_not_fail_attach() {
    let fx = Fx::with(&["front"]);
    let main = fx.main("front");
    let outside = fx.sb.root.join("outside-secret.txt");
    fs::write(&outside, b"LINK_TARGET_SECRET\n").unwrap();
    std::os::unix::fs::symlink(&outside, main.join("link.txt")).unwrap();
    fx.sb.git(&main, &["add", "link.txt"]);
    base(&fx, &[("old.rs", b"RENAMED_BODY\n")]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    assert!(fs::symlink_metadata(w.join("link.txt")).unwrap().file_type().is_symlink());
    fs::rename(w.join("old.rs"), w.join("new.rs")).unwrap();
    let ov = FakeOverlays::default();
    let o = fx.capture(&id(&a), "front");
    assert!(matches!(kind_of(&o, "new.rs"), Some(EntryKind::Renamed { .. })), "{:?}", o.entries);
    ov.set(&id(&a), "front", o);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).expect("attach issues an id");
    let f = files(&s, &v);
    assert!(f.contains(&"new.rs".to_string()) && f.contains(&"link.txt".to_string()), "{f:?}");
    assert!(!f.contains(&"old.rs".to_string()));
    assert_eq!(grep(&s, &v, "LINK_TARGET_SECRET"), vec![]);
    assert_eq!(paths(&grep(&s, &v, "RENAMED_BODY")), vec!["new.rs"]);
}

#[test]
fn gitfile_and_strata_dir_excluded() {
    let fx = Fx::with(&["front"]);
    base(&fx, &[(".strata/lww", b"STRATA_TOKEN tracked\n"), ("ok.ts", b"STRATA_TOKEN ok\n")]);
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    let gitfile = fs::read(w.join(".git")).unwrap();
    assert!(gitfile.starts_with(b"gitdir:"), "fixture: the workspace has a gitfile");
    fx.sb.write(&w, ".strata/extra", b"STRATA_TOKEN extra\n");
    fx.sb.write(&w, "node_modules/pkg/index.js", b"STRATA_TOKEN deps\n");
    fx.sb.write(&w, ".cache/seed.bin", b"STRATA_TOKEN seed\n");
    let ov = FakeOverlays::default();
    let mut o = fx.capture(&id(&a), "front");
    // Even an overlay that names them does not bring them in.
    for p in [".strata/extra", "node_modules/pkg/index.js", ".cache/seed.bin", ".git"] {
        if kind_of(&o, p).is_none() {
            o.entries.push(untracked(p));
        }
    }
    o.entries.sort_by(|x, y| x.path.cmp(&y.path));
    ov.set(&id(&a), "front", o);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let f = files(&s, &v);
    for p in &f {
        assert!(
            p != ".git"
                && !p.starts_with(".git/")
                && !p.starts_with(".strata/")
                && !p.starts_with("node_modules/")
                && !p.starts_with(".cache/"),
            "{p} listed: {f:?}"
        );
    }
    assert!(f.contains(&"ok.ts".to_string()));
    assert_eq!(paths(&grep(&s, &v, "STRATA_TOKEN")), vec!["ok.ts"]);
    assert_eq!(grep(&s, &v, "gitdir:"), vec![]);
}

#[test]
fn tracked_ignored_file_still_listed() {
    let fx = Fx::with(&["front"]);
    base(
        &fx,
        &[(".gitignore", b".DS_Store\nnode_modules/\n.cache/\nkept.txt\nskip.txt\n"), ("kept.txt", b"KEPT_TOKEN\n")],
    );
    let a = fx.new_session(&["front"]);
    let w = ws(&fx, &a);
    fx.sb.write(&w, "skip.txt", b"KEPT_TOKEN skipped\n");
    let ss = fx.sessions();

    let ov = FakeOverlays::default();
    ov.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&a), &fx.rn("front")).unwrap();
    let f = files(&s, &v);
    assert!(f.contains(&"kept.txt".to_string()) && !f.contains(&"skip.txt".to_string()), "{f:?}");
    assert_eq!(paths(&grep(&s, &v, "KEPT_TOKEN")), vec!["kept.txt"]);

    // `.ignore` is not used.
    fx.sb.write(&w, ".ignore", b"kept.txt\n");
    let ov2 = FakeOverlays::default();
    ov2.set(&id(&a), "front", fx.capture(&id(&a), "front"));
    let s2 = Search::open(&fx.config, &ss, &ov2);
    let v2 = s2.attach(&id(&a), &fx.rn("front")).unwrap();
    let f2 = files(&s2, &v2);
    assert!(f2.contains(&"kept.txt".to_string()) && !f2.contains(&"skip.txt".to_string()), "{f2:?}");
    assert_eq!(paths(&grep(&s2, &v2, "KEPT_TOKEN")), vec!["kept.txt"]);
}

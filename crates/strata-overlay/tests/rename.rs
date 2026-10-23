//! Task 5: deterministic rename pairing (§5.3).

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use common::*;
use strata_overlay::{DEFAULT_DIRTY_CAPACITY, Overlay, Scan, Tracker, compute, reconcile_full};
use strata_types::EntryKind;

const F: u32 = 0o100644;
const L: u32 = 0o120000;

fn tracker(w: &World) -> Tracker {
    Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY)
}

/// A scan with `t`, checked against the cold scan and §4.3.
fn scan(w: &World, t: &mut Tracker) -> Scan {
    let s = compute(&w.ws_ref(), &w.base, t, None).expect("compute");
    s.overlay.validate().expect("validate");
    let c = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    assert_eq!(s.overlay, c, "compute equals the cold scan");
    s
}

fn cold(w: &World) -> Overlay {
    let o = reconcile_full(&w.ws_ref(), &w.base).expect("reconcile_full");
    o.validate().expect("validate");
    o
}

/// (path, kind, from or "", mode) of every entry.
fn entries(o: &Overlay) -> Vec<(String, &'static str, String, u32)> {
    o.entries
        .iter()
        .map(|e| {
            let (k, from) = match &e.kind {
                EntryKind::Added { .. } => ("Added", String::new()),
                EntryKind::Modified { .. } => ("Modified", String::new()),
                EntryKind::Deleted => ("Deleted", String::new()),
                EntryKind::Renamed { from, .. } => ("Renamed", from.as_str().to_owned()),
                EntryKind::Untracked { .. } => ("Untracked", String::new()),
            };
            (e.path.as_str().to_owned(), k, from, e.mode)
        })
        .collect()
}

fn e(p: &str, k: &'static str, from: &str, mode: u32) -> (String, &'static str, String, u32) {
    (p.to_owned(), k, from.to_owned(), mode)
}

fn ren(p: &str, from: &str) -> (String, &'static str, String, u32) {
    e(p, "Renamed", from, F)
}

fn blob(o: &Overlay, p: &str) -> String {
    let e = o.get(&rp(p)).unwrap_or_else(|| panic!("no entry for {p}"));
    match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Renamed { blob, .. }
        | EntryKind::Untracked { blob } => blob.as_str().to_owned(),
        EntryKind::Deleted => panic!("{p} is deleted"),
    }
}

fn base_blob(w: &World, p: &str) -> String {
    w.base.manifest.entries[&rp(p)].blob.as_str().to_owned()
}

/// `mv a b` inside the workspace, creating b's parents.
fn mv(w: &World, a: &str, b: &str) {
    let to = w.ws.join(b);
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    fs::rename(w.ws.join(a), to).unwrap_or_else(|err| panic!("mv {a} {b}: {err}"));
}

fn ignorecase(w: &World) -> bool {
    w.git.read_index(&w.ws).unwrap().ignorecase
}

#[test]
fn directory_moves_expand_per_file() {
    let w = World::with(|m| {
        put(m, "alpha/a1.txt", b"a1\n");
        put(m, "alpha/a2.txt", b"a2\n");
        put(m, "beta/b1.txt", b"b1\n");
        put(m, "gamma/g1.txt", b"g1\n");
        put(m, "gamma/keep.txt", b"keep\n");
        put(m, "gamma/sub/s1.txt", b"s1\n");
        put(m, "delta/d1.txt", b"d1\n");
        put(m, "eps/e1.txt", b"e1\n");
        put(m, "host/h.txt", b"h\n");
    });
    let mut t = tracker(&w);
    assert!(scan(&w, &mut t).overlay.is_empty());
    // A rename, a move deep down, a move into a new directory inside a base
    // directory, and a directory moved twice.
    mv(&w, "alpha", "alpha2");
    mv(&w, "beta", "deep/er/beta");
    fs::create_dir(w.ws.join("host/fresh")).unwrap();
    mv(&w, "delta", "host/fresh/delta");
    mv(&w, "eps", "tmp1");
    mv(&w, "tmp1", "final/eps");
    // A subdirectory renamed over an empty directory, then its parent removed.
    fs::create_dir(w.ws.join("rescued")).unwrap();
    mv(&w, "gamma/sub", "rescued");
    w.remove("gamma");
    // A new tree renamed as a whole.
    w.write("fresh/n1.txt", b"n1\n");
    w.write("fresh/in/n2.txt", b"n2\n");
    mv(&w, "fresh", "fresh2");
    let s = scan(&w, &mut t);
    assert_eq!(
        entries(&s.overlay),
        vec![
            ren("alpha2/a1.txt", "alpha/a1.txt"),
            ren("alpha2/a2.txt", "alpha/a2.txt"),
            ren("deep/er/beta/b1.txt", "beta/b1.txt"),
            ren("final/eps/e1.txt", "eps/e1.txt"),
            e("fresh2/in/n2.txt", "Untracked", "", F),
            e("fresh2/n1.txt", "Untracked", "", F),
            e("gamma/g1.txt", "Deleted", "", 0),
            e("gamma/keep.txt", "Deleted", "", 0),
            ren("host/fresh/delta/d1.txt", "delta/d1.txt"),
            ren("rescued/s1.txt", "gamma/sub/s1.txt"),
        ]
    );
    for (p, from) in [
        ("alpha2/a1.txt", "alpha/a1.txt"),
        ("deep/er/beta/b1.txt", "beta/b1.txt"),
        ("rescued/s1.txt", "gamma/sub/s1.txt"),
    ] {
        assert_eq!(blob(&s.overlay, p), base_blob(&w, from));
    }
}

#[test]
fn exchanged_and_inverted_directories() {
    let w = World::with(|m| {
        put(m, "left/same.txt", b"L-same\n");
        put(m, "left/only_l.txt", b"only-l\n");
        put(m, "right/same.txt", b"R-same\n");
        put(m, "right/only_r.txt", b"only-r\n");
        put(m, "p/c/f.txt", b"pcf\n");
        put(m, "p/g.txt", b"pg\n");
    });
    let mut t = tracker(&w);
    let swap = |tmp: &str| {
        mv(&w, "left", tmp);
        mv(&w, "right", "left");
        mv(&w, tmp, "right");
    };
    swap("tmp_a");
    let a = scan(&w, &mut t).overlay;
    assert_eq!(
        entries(&a),
        vec![
            ren("left/only_r.txt", "right/only_r.txt"),
            e("left/same.txt", "Modified", "", F),
            ren("right/only_l.txt", "left/only_l.txt"),
            e("right/same.txt", "Modified", "", F),
        ]
    );
    assert_eq!(blob(&a, "left/same.txt"), base_blob(&w, "right/same.txt"));
    assert_eq!(blob(&a, "right/same.txt"), base_blob(&w, "left/same.txt"));
    // Swapping back is a clean workspace; swapping through another
    // temporary name gives the same overlay.
    swap("tmp_b");
    assert!(scan(&w, &mut t).overlay.is_empty());
    swap("zz_other_tmp");
    assert_eq!(scan(&w, &mut t).overlay, a);
    swap("tmp_c");
    assert!(scan(&w, &mut t).overlay.is_empty());
    // A parent and its child inverted: p/c → c/p.
    mv(&w, "p", "tmp_p");
    mv(&w, "tmp_p/c", "c");
    mv(&w, "tmp_p", "c/p");
    assert_eq!(
        entries(&scan(&w, &mut t).overlay),
        vec![ren("c/f.txt", "p/c/f.txt"), ren("c/p/g.txt", "p/g.txt")]
    );
    // And back.
    mv(&w, "c/p", "tmp_q");
    mv(&w, "c", "tmp_q/c");
    mv(&w, "tmp_q", "p");
    assert!(scan(&w, &mut t).overlay.is_empty());
}

fn replacing_tree(m: &Path) {
    put(m, "a", b"A\n");
    put(m, "b", b"B\n");
    put(m, "d", b"D\n");
    put(m, "e", b"E\n");
    put(m, "victim", b"V\n");
    put_link(m, "lnk", "target1");
}

#[test]
fn rename_onto_freed_name_and_replacing_renames() {
    let w = World::with(replacing_tree);
    // Delete a, then rename the untouched b to a.
    w.remove("a");
    mv(&w, "b", "a");
    // Change d, then rename it over e.
    w.write("d", b"D2\n");
    mv(&w, "d", "e");
    // A new file renamed over a base file.
    w.write("new.txt", b"NEW\n");
    mv(&w, "new.txt", "victim");
    // A new symlink renamed over a base symlink.
    put_link(&w.ws, "newlink", "target2");
    mv(&w, "newlink", "lnk");
    let mut t = tracker(&w);
    let a = scan(&w, &mut t).overlay;
    assert_eq!(
        entries(&a),
        vec![
            ren("a", "b"),
            e("d", "Deleted", "", 0),
            e("e", "Modified", "", F),
            e("lnk", "Modified", "", L),
            e("victim", "Modified", "", F),
        ]
    );
    assert_eq!(blob(&a, "a"), base_blob(&w, "b"));
    assert_eq!(blob(&a, "e"), blake3_hex(b"D2\n"));
    assert_eq!(blob(&a, "victim"), blake3_hex(b"NEW\n"));
    assert_eq!(blob(&a, "lnk"), blake3_hex(b"target2"));
    // The same end state reached in another order.
    let v = World::with(replacing_tree);
    put_link(&v.ws, "newlink", "target2");
    mv(&v, "newlink", "lnk");
    v.write("new.txt", b"NEW\n");
    mv(&v, "new.txt", "victim");
    mv(&v, "d", "e");
    v.write("e", b"D2\n");
    mv(&v, "b", "tmp_b");
    v.remove("a");
    mv(&v, "tmp_b", "a");
    assert_eq!(v.base.r#gen, w.base.r#gen);
    assert_eq!(cold(&v), a);
}

#[test]
fn hard_links_are_independent_paths() {
    let w = World::with(|m| {
        put(m, "h.txt", b"H\n");
        put(m, "other.txt", b"O\n");
    });
    let mut t = tracker(&w);
    // A second name for a base file.
    fs::hard_link(w.ws.join("h.txt"), w.ws.join("h2.txt")).unwrap();
    let s = scan(&w, &mut t);
    assert_eq!(entries(&s.overlay), vec![e("h2.txt", "Untracked", "", F)]);
    assert_eq!(blob(&s.overlay, "h2.txt"), base_blob(&w, "h.txt"));
    // Written through one name: both paths record the new content.
    let ino = fs::metadata(w.ws.join("h.txt")).unwrap().ino();
    fs::write(w.ws.join("h2.txt"), b"H2\n").unwrap();
    assert_eq!(fs::metadata(w.ws.join("h.txt")).unwrap().ino(), ino);
    assert_eq!(w.read("h.txt"), b"H2\n");
    let s = scan(&w, &mut t);
    assert_eq!(
        entries(&s.overlay),
        vec![
            e("h.txt", "Modified", "", F),
            e("h2.txt", "Untracked", "", F)
        ]
    );
    assert_eq!(blob(&s.overlay, "h.txt"), blake3_hex(b"H2\n"));
    assert_eq!(blob(&s.overlay, "h2.txt"), blake3_hex(b"H2\n"));
    // Removing one name changes only it.
    w.remove("h2.txt");
    let s = scan(&w, &mut t);
    assert_eq!(entries(&s.overlay), vec![e("h.txt", "Modified", "", F)]);
    assert_eq!(s.changed, vec![rp("h2.txt")]);
    // A link name moved across directories is a rename (pairing takes the
    // smallest path with the content: a/other.txt before z/…).
    fs::create_dir(w.ws.join("z")).unwrap();
    fs::hard_link(w.ws.join("other.txt"), w.ws.join("z/other-link.txt")).unwrap();
    mv(&w, "other.txt", "a/other.txt");
    let s = scan(&w, &mut t);
    assert_eq!(
        entries(&s.overlay),
        vec![
            ren("a/other.txt", "other.txt"),
            e("h.txt", "Modified", "", F),
            e("z/other-link.txt", "Untracked", "", F),
        ]
    );
}

fn case_tree(m: &Path) {
    put(m, "src/Foo.ts", b"foo\n");
    put(m, "Lib/a.ts", b"la\n");
    put(m, "Lib/b.ts", b"lb\n");
    put(m, "\u{c9}cole.ts", b"ecole\n");
}

#[test]
fn rename_case_only_recorded_as_rename() {
    let w = World::with(case_tree);
    assert!(ignorecase(&w), "the fixture has core.ignorecase = true");
    let mut t = tracker(&w);
    // mv src/Foo.ts src/foo.ts: exactly one Renamed with the base blob.
    mv(&w, "src/Foo.ts", "src/foo.ts");
    let s = scan(&w, &mut t);
    assert_eq!(entries(&s.overlay), vec![ren("src/foo.ts", "src/Foo.ts")]);
    assert_eq!(blob(&s.overlay, "src/foo.ts"), base_blob(&w, "src/Foo.ts"));
    // Renamed and changed: still that one entry, with the new content.
    w.write("src/foo.ts", b"foo 2\n");
    let s = scan(&w, &mut t);
    assert_eq!(entries(&s.overlay), vec![ren("src/foo.ts", "src/Foo.ts")]);
    assert_eq!(blob(&s.overlay, "src/foo.ts"), blake3_hex(b"foo 2\n"));
    mv(&w, "src/foo.ts", "src/Foo.ts");
    w.write("src/Foo.ts", b"foo\n");
    assert!(scan(&w, &mut t).overlay.is_empty());
    // A directory's case: one Renamed per file.
    mv(&w, "Lib", "lib");
    assert_eq!(
        entries(&scan(&w, &mut t).overlay),
        vec![ren("lib/a.ts", "Lib/a.ts"), ren("lib/b.ts", "Lib/b.ts")]
    );
    mv(&w, "lib", "Lib");
    // Non-ASCII case.
    mv(&w, "\u{c9}cole.ts", "\u{e9}cole.ts");
    assert_eq!(
        entries(&scan(&w, &mut t).overlay),
        vec![ren("\u{e9}cole.ts", "\u{c9}cole.ts")]
    );
    mv(&w, "\u{e9}cole.ts", "\u{c9}cole.ts");
    assert!(scan(&w, &mut t).overlay.is_empty());
    // core.ignorecase = false on a case-insensitive volume: paired by the
    // volume fold.
    w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
    assert!(!ignorecase(&w));
    mv(&w, "src/Foo.ts", "src/foo.ts");
    w.write("src/foo.ts", b"foo 3\n");
    let s = scan(&w, &mut t);
    assert_eq!(entries(&s.overlay), vec![ren("src/foo.ts", "src/Foo.ts")]);
    assert_eq!(blob(&s.overlay, "src/foo.ts"), blake3_hex(b"foo 3\n"));
}

#[test]
#[ignore = "needs hdiutil (a case-sensitive disk image)"]
fn rename_case_only_recorded_as_rename_case_sensitive() {
    let img = Image::case_sensitive();
    let w = World::with_in(Some(&img.mount), case_tree);
    w.git_ok(&w.ws, &["config", "core.ignorecase", "true"]);
    assert!(ignorecase(&w));
    mv(&w, "src/Foo.ts", "src/foo.ts");
    w.write("src/foo.ts", b"foo 2\n");
    let o = cold(&w);
    assert_eq!(entries(&o), vec![ren("src/foo.ts", "src/Foo.ts")]);
    assert_eq!(blob(&o, "src/foo.ts"), blake3_hex(b"foo 2\n"));
    // Both folds byte-wise: nothing pairs.
    w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
    assert_eq!(
        entries(&cold(&w)),
        vec![
            e("src/Foo.ts", "Deleted", "", 0),
            e("src/foo.ts", "Untracked", "", F),
        ]
    );
}

#[test]
fn non_ascii_case_renamed_ignored_tracked_dir_is_still_compared() {
    let w = World::with(|m| {
        put(m, "\u{c9}coles/x.ts", b"x\n");
        put(m, "\u{c9}coles/y.ts", b"y\n");
        put(m, ".gitignore", b"*.log\n");
    });
    assert!(ignorecase(&w));
    let mut t = tracker(&w);
    w.write(".gitignore", "*.log\n\u{e9}coles/\n".as_bytes());
    mv(&w, "\u{c9}coles", "\u{e9}coles");
    let index = w.git.read_index(&w.ws).unwrap();
    assert!(
        !index.tracked_under(&rp("\u{e9}coles")),
        "gitio folds ASCII only; overlay must not rely on it"
    );
    let s = scan(&w, &mut t);
    assert_eq!(
        entries(&s.overlay),
        vec![
            e(".gitignore", "Modified", "", F),
            ren("\u{e9}coles/x.ts", "\u{c9}coles/x.ts"),
            ren("\u{e9}coles/y.ts", "\u{c9}coles/y.ts"),
        ]
    );
    // core.ignorecase = false; the rule names only the new spelling.
    let w = World::with(|m| {
        put(m, "Src/a.ts", b"a\n");
        put(m, "Src/b.ts", b"b\n");
        put(m, ".gitignore", b"*.log\n");
    });
    w.git_ok(&w.ws, &["config", "core.ignorecase", "false"]);
    assert!(!ignorecase(&w));
    let mut t = tracker(&w);
    w.write(".gitignore", b"*.log\nsrc/\n");
    mv(&w, "Src", "src");
    let s = scan(&w, &mut t);
    assert_eq!(
        entries(&s.overlay),
        vec![
            e(".gitignore", "Modified", "", F),
            ren("src/a.ts", "Src/a.ts"),
            ren("src/b.ts", "Src/b.ts"),
        ]
    );
    assert_eq!(blob(&s.overlay, "src/a.ts"), base_blob(&w, "Src/a.ts"));
    assert_eq!(blob(&s.overlay, "src/b.ts"), base_blob(&w, "Src/b.ts"));
}

/// Extra (§5.3 type swaps): a directory replaced by a file with the content of
/// its only file is Deleted + Untracked, never a Renamed (and the reverse).
#[test]
fn type_swap_with_equal_content_is_not_a_rename() {
    let w = World::with(|m| {
        put(m, "d/f", b"same\n");
        put(m, "g", b"other\n");
    });
    w.remove("d");
    w.write("d", b"same\n");
    w.remove("g");
    w.write("g/inner", b"other\n");
    let mut t = tracker(&w);
    assert_eq!(
        entries(&scan(&w, &mut t).overlay),
        vec![
            e("d", "Untracked", "", F),
            e("d/f", "Deleted", "", 0),
            e("g", "Deleted", "", 0),
            e("g/inner", "Untracked", "", F),
        ]
    );
}

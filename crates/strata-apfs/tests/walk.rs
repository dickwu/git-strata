//! `walk`, `walk_with`, `list_dir` (§5.6, §8 Task 7).
mod common;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use strata_apfs::seams::{set_after_bulk_window, set_before_open};
use strata_apfs::*;
use strata_types::{Code, RepoPath, StrataError};

struct ClearHooks;
impl Drop for ClearHooks {
    fn drop(&mut self) {
        set_before_open(None);
        set_after_bulk_window(None);
    }
}

type Item = std::result::Result<(RepoPath, Attrs), StrataError>;

fn all(w: impl Iterator<Item = Item>) -> Vec<(String, Attrs)> {
    w.map(|r| r.unwrap()).map(|(p, a)| (p.as_str().to_string(), a)).collect()
}

fn names(w: impl Iterator<Item = Item>) -> Vec<String> {
    all(w).into_iter().map(|(p, _)| p).collect()
}

fn yes(_: &Path) -> bool {
    true
}

/// Everything under `root` by std, no links followed.
fn std_set(root: &Path) -> BTreeSet<String> {
    fn rec(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            out.insert(p.strip_prefix(root).unwrap().to_str().unwrap().to_string());
            if e.file_type().unwrap().is_dir() {
                rec(root, &p, out);
            }
        }
    }
    let mut s = BTreeSet::new();
    rec(root, root, &mut s);
    s
}

fn touch(p: &Path) {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(p, p.file_name().unwrap().as_encoded_bytes()).unwrap();
}

#[test]
fn walk_lists_all_entries_relative_to_root() {
    let t = Tmp::new();
    let r = t.path.clone();
    for f in ["a.txt", "img.png", "z.zip", "dir/b.rs", "dir/sub/c"] {
        touch(&r.join(f));
    }
    fs::create_dir(r.join("empty")).unwrap();
    symlink("a.txt", r.join("link")).unwrap();
    let got: BTreeSet<String> = names(walk(&r, &yes)).into_iter().collect();
    assert_eq!(got, std_set(&r));
    for want in ["img.png", "z.zip", "link", "empty", "dir", "dir/sub/c"] {
        assert!(got.contains(want), "{want}");
    }
    assert!(got.iter().all(|p| !p.starts_with('/') && !p.is_empty()));
}

fn tree_with_vendor(r: &Path) {
    for f in ["vendor/x", "vendor/y/z", "src/main.rs", "src/lib/mod.rs", "src/target/out", "node_modules/m/i.js", "target/debug/b", "README"] {
        touch(&r.join(f));
    }
}

#[test]
fn walk_excludes_configured_dirs() {
    let t = Tmp::new();
    let r = t.path.clone();
    tree_with_vendor(&r);
    let asked = Mutex::new(Vec::new());
    let f = |p: &Path| {
        asked.lock().unwrap().push(p.to_path_buf());
        p != Path::new("vendor")
    };
    let got = names(walk(&r, &f));
    assert!(got.iter().all(|p| !p.starts_with("vendor")), "{got:?}");
    for want in ["src", "src/main.rs", "src/lib", "src/lib/mod.rs"] {
        assert!(got.contains(&want.to_string()));
    }
    let asked = asked.into_inner().unwrap();
    assert!(asked.contains(&PathBuf::from("vendor")), "directories are asked too");
    assert!(asked.contains(&PathBuf::from("src/main.rs")));
}

fn excluding(names: &'static [&'static str]) -> impl Fn(&Path) -> bool + Send + Sync {
    move |p: &Path| !names.iter().any(|n| p.file_name().is_some_and(|f| f == *n))
}

#[test]
fn walk_excludes_several_dirs() {
    let t = Tmp::new();
    let r = t.path.clone();
    tree_with_vendor(&r);
    let f = excluding(&["vendor", "node_modules", "target"]);
    let got = names(walk(&r, &f));
    for p in &got {
        assert!(!p.split('/').any(|c| ["vendor", "node_modules", "target"].contains(&c)), "{p}");
    }
    let want: Vec<String> = std_set(&r)
        .into_iter()
        .filter(|p| !p.split('/').any(|c| ["vendor", "node_modules", "target"].contains(&c)))
        .collect();
    assert_eq!(got.iter().cloned().collect::<BTreeSet<_>>(), want.into_iter().collect());
}

#[test]
fn walk_exclusion_of_a_missing_dir_is_a_noop() {
    let t = Tmp::new();
    let r = t.path.clone();
    tree_with_vendor(&r);
    let f = excluding(&["no_such_dir"]);
    assert_eq!(all(walk(&r, &f)), all(walk(&r, &yes)));
}

#[test]
fn walk_exclusion_prunes_the_whole_subtree() {
    let t = Tmp::new();
    let r = t.path.clone();
    tree_with_vendor(&r);
    touch(&r.join("vendor/deep/er/still/f"));
    let inside = AtomicUsize::new(0);
    let f = |p: &Path| {
        if p.starts_with("vendor") && p != Path::new("vendor") {
            inside.fetch_add(1, Ordering::SeqCst);
        }
        p != Path::new("vendor")
    };
    let got = names(walk(&r, &f));
    assert!(got.iter().all(|p| !p.starts_with("vendor")));
    assert_eq!(inside.load(Ordering::SeqCst), 0, "the pruned directory was never read");
}

#[test]
fn walk_serves_full_and_incremental_scans() {
    let t = Tmp::new();
    let r = t.path.clone();
    for f in ["src/a.rs", "src/sub/b.rs", "src/vendor/v.rs", "vendor/x", "top"] {
        touch(&r.join(f));
    }
    symlink("a.rs", r.join("src/link")).unwrap();
    make_fifo(&r.join("src/fifo"));
    let f = |p: &Path, _: &Attrs| p.file_name().is_none_or(|n| n != "vendor");
    let full = all(walk_with(&r, &WalkOptions::default(), &f));
    let full_names: BTreeSet<_> = full.iter().map(|(p, _)| p.clone()).collect();
    let want: BTreeSet<String> = ["src", "src/a.rs", "src/sub", "src/sub/b.rs", "src/link", "src/fifo", "top"].iter().map(|s| s.to_string()).collect();
    assert_eq!(full_names, want);
    let opts = WalkOptions { start: Some(rp("src")), max_depth: Some(1), ..Default::default() };
    let inc = all(walk_with(&r, &opts, &f));
    let inc_names: Vec<_> = inc.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(inc_names, vec!["src/a.rs", "src/fifo", "src/link", "src/sub"]);
    assert_eq!(inc.iter().find(|(p, _)| p == "src/sub").unwrap().1.kind, FileKind::Dir);
    for (p, at) in full.iter().chain(inc.iter()) {
        assert_eq!(&attrs_of(&r, &rp(p)).unwrap(), at, "{p}");
    }
}

#[test]
fn walk_is_preorder_with_siblings_sorted() {
    let t = Tmp::new();
    let r = t.path.clone();
    touch(&r.join("foo/x"));
    touch(&r.join("foo-bar"));
    touch(&r.join("foo.txt"));
    let got = names(walk(&r, &yes));
    assert_eq!(got, vec!["foo", "foo/x", "foo-bar", "foo.txt"]);
    let mut full_sorted = got.clone();
    full_sorted.sort();
    assert_eq!(full_sorted, vec!["foo", "foo-bar", "foo.txt", "foo/x"]);
    assert_ne!(got, full_sorted, "the output is not globally ordered");
    let map: BTreeMap<String, Attrs> = all(walk(&r, &yes)).into_iter().collect();
    assert_eq!(map.keys().cloned().collect::<BTreeSet<_>>(), std_set(&r));
}

#[test]
fn walk_options_default_uses_the_default_buffer() {
    let d = WalkOptions::default();
    assert_eq!(d.start, None);
    assert_eq!(d.max_depth, None);
    assert_eq!(d.buffer_bytes, DEFAULT_BULK_BUFFER_BYTES);
    let t = Tmp::new();
    let r = t.path.clone();
    tree_with_vendor(&r);
    assert_eq!(all(walk(&r, &yes)), all(walk_with(&r, &WalkOptions::default(), &|_, _| true)));
}

#[test]
fn walk_is_send_and_accepts_send_sync_filters() {
    fn is_send<T: Send>() {}
    is_send::<Walk<'static>>();
    let t = Tmp::new();
    let r = t.path.clone();
    tree_with_vendor(&r);
    let here = all(walk(&r, &yes));
    let count = AtomicUsize::new(0);
    let f = |_: &Path| {
        count.fetch_add(1, Ordering::SeqCst);
        true
    };
    let w = walk(&r, &f);
    let there = std::thread::scope(|s| s.spawn(move || all(w)).join().unwrap());
    assert_eq!(here, there);
    assert_eq!(count.load(Ordering::SeqCst), here.len());
    let count2 = AtomicUsize::new(0);
    let g = |_: &Path, _: &Attrs| {
        count2.fetch_add(1, Ordering::SeqCst);
        true
    };
    let n = walk_with(&r, &WalkOptions::default(), &g).count();
    assert_eq!(count2.load(Ordering::SeqCst), n);
}

fn listing(root: &Path) -> Walk<'static> {
    let local = rp("dir");
    list_dir(root, Some(&local)) // borrows nothing: compiles only for 'static
}

#[test]
fn list_dir_lists_one_level_in_byte_order() {
    let t = Tmp::new();
    let r = t.path.clone();
    for f in ["dir/b", "dir/a", "dir/sub/inner", "dir/Foo.TXT", "top"] {
        touch(&r.join(f));
    }
    touch(&r.join("dir/e\u{301}.txt")); // NFD on disk
    let got = all(listing(&r));
    let ns: Vec<_> = got.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(ns, vec!["dir/Foo.TXT", "dir/a", "dir/b", "dir/sub", "dir/\u{e9}.txt"]);
    assert_eq!(got[3].1.kind, FileKind::Dir);
    open_rooted(&r, &rp("dir/\u{e9}.txt")).unwrap();
    assert_eq!(names(list_dir(&r, None)), vec!["dir", "top"]);
    let mut w = list_dir(&r, Some(&rp("missing")));
    assert_eq!(w.next().unwrap().unwrap_err().code, Code::NoSuchPath);
    assert!(w.next().is_none());
    symlink(r.join("dir"), r.join("ldir")).unwrap();
    let mut w = list_dir(&r, Some(&rp("ldir")));
    let e = w.next().unwrap().unwrap_err();
    assert_eq!((e.code, detail_str(&e, "reason").as_deref()), (Code::NotDirectory, Some("symlink")));
    assert!(w.next().is_none());
}

#[test]
fn walk_wide_directory_is_complete_and_stable_across_buffer_sizes() {
    let t = Tmp::new();
    let r = t.path.clone();
    let w = r.join("wide");
    fs::create_dir(&w).unwrap();
    for i in (0..1300).rev() {
        fs::write(w.join(format!("f{i:04}")), "x").unwrap();
    }
    symlink("f0001", w.join("rel_link")).unwrap();
    symlink("nowhere/at/all", w.join("dangling")).unwrap();
    touch(&w.join("nested/deeper/leaf"));
    fs::create_dir(w.join("empty1")).unwrap();
    fs::create_dir(w.join("empty2")).unwrap();
    fs::write(w.join("hl_a"), "h").unwrap();
    fs::hard_link(w.join("hl_a"), w.join("hl_b")).unwrap();
    let windows = Rc::new(Cell::new(0usize));
    let _g = ClearHooks;
    let w2 = windows.clone();
    set_after_bulk_window(Some(Box::new(move |_| w2.set(w2.get() + 1))));
    let small = all(walk_with(&r, &WalkOptions { buffer_bytes: 1024, ..Default::default() }, &|_, _| true));
    assert!(windows.get() > 1, "windows: {}", windows.get());
    let big = all(walk_with(&r, &WalkOptions::default(), &|_, _| true));
    assert_eq!(small, big);
    let names: Vec<&str> = small.iter().map(|(p, _)| p.as_str()).collect();
    let set: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(set.len(), names.len(), "every entry once");
    assert_eq!(set.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(), std_set(&r));
    let direct: Vec<&str> = names.iter().copied().filter(|p| p.starts_with("wide/") && p.matches('/').count() == 1).collect();
    let mut sorted = direct.clone();
    sorted.sort();
    assert_eq!(direct, sorted);
    let get = |p: &str| &small.iter().find(|(q, _)| q == p).unwrap().1;
    assert_eq!(get("wide/rel_link").kind, FileKind::Symlink { target: "f0001".into() });
    assert_eq!(get("wide/dangling").kind, FileKind::Symlink { target: "nowhere/at/all".into() });
    assert_eq!(get("wide/empty1").kind, FileKind::Dir);
    assert_eq!(get("wide/hl_a").inode, get("wide/hl_b").inode);
}

#[test]
fn walk_lists_the_longest_name_with_the_minimum_buffer() {
    let t = Tmp::new();
    let r = t.path.clone();
    let long = "\u{3042}".repeat(255);
    assert_eq!(long.len(), 765);
    fs::write(r.join(&long), "l").unwrap();
    fs::write(r.join("plain"), "p").unwrap();
    symlink(&long, r.join("ln")).unwrap();
    let got = names(walk_with(&r, &WalkOptions { buffer_bytes: MIN_BULK_BUFFER_BYTES, ..Default::default() }, &|_, _| true));
    let mut want = vec![long.clone(), "ln".into(), "plain".into()];
    want.sort();
    assert_eq!(got, want);
}

fn shuffled(n: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    for i in (1..n).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
    v
}

#[test]
fn walk_very_wide_directory_through_many_windows() {
    let t = Tmp::new();
    let r = t.path.clone();
    let target = |i: usize| format!("{i:05}-{}", "t".repeat(954));
    for i in shuffled(17000) {
        symlink(target(i), r.join(format!("s{i:05}"))).unwrap();
    }
    let windows = Rc::new(Cell::new(0usize));
    let _g = ClearHooks;
    let w2 = windows.clone();
    set_after_bulk_window(Some(Box::new(move |_| w2.set(w2.get() + 1))));
    let got = all(walk_with(&r, &WalkOptions { buffer_bytes: MIN_BULK_BUFFER_BYTES, ..Default::default() }, &|_, _| true));
    assert_eq!(got.len(), 17000);
    for (i, (p, a)) in got.iter().enumerate() {
        assert_eq!(p, &format!("s{i:05}"));
        assert_eq!(a.kind, FileKind::Symlink { target: target(i).into() });
    }
    assert!(windows.get() > 100, "windows: {}", windows.get());
}

#[test]
fn walk_after_mass_deletion_lists_only_survivors() {
    let t = Tmp::new();
    let r = t.path.clone();
    for i in 0..10000 {
        fs::write(r.join(format!("f{i:05}")), "").unwrap();
    }
    let mut survivors = BTreeSet::new();
    for i in 0..10000 {
        // Keep a few scattered names and a short run near the end.
        if i % 97 == 0 || (9950..9980).contains(&i) {
            survivors.insert(format!("f{i:05}"));
        } else {
            fs::remove_file(r.join(format!("f{i:05}"))).unwrap();
        }
    }
    let n = survivors.len();
    assert!((100..200).contains(&n), "{n}");
    let got = names(walk_with(&r, &WalkOptions { buffer_bytes: MIN_BULK_BUFFER_BYTES, ..Default::default() }, &|_, _| true));
    assert_eq!(got.len(), n);
    assert_eq!(got.into_iter().collect::<BTreeSet<_>>(), survivors);
}

#[test]
fn walk_returns_untouched_names_once_while_the_directory_changes() {
    let t = Tmp::new();
    let r = t.path.clone();
    let d = r.join("d");
    fs::create_dir(&d).unwrap();
    for i in 0..5000 {
        fs::write(d.join(format!("u{i:05}")), "").unwrap();
    }
    for i in 0..1000 {
        fs::write(d.join(format!("m{i:05}")), "").unwrap();
    }
    let _g = ClearHooks;
    let step = Rc::new(Cell::new(0usize));
    let (d2, s2) = (d.clone(), step.clone());
    set_after_bulk_window(Some(Box::new(move |_| {
        let k = s2.get();
        s2.set(k + 1);
        for j in 0..3 {
            let i = (k * 7 + j * 331) % 1000;
            let _ = fs::remove_file(d2.join(format!("m{i:05}")));
            let _ = fs::rename(d2.join(format!("m{:05}", (i + 500) % 1000)), d2.join(format!("r{k:05}_{j}")));
            let _ = fs::write(d2.join(format!("n{k:05}_{j}")), "");
        }
    })));
    let got = names(walk_with(&r, &WalkOptions { buffer_bytes: MIN_BULK_BUFFER_BYTES, ..Default::default() }, &|_, _| true));
    assert!(step.get() > 10, "windows: {}", step.get());
    let set: BTreeSet<&String> = got.iter().collect();
    assert_eq!(set.len(), got.len(), "no duplicates");
    for i in 0..5000 {
        let n = format!("d/u{i:05}");
        assert!(set.contains(&n), "{n} missing");
    }
}

#[test]
fn walk_never_follows_symlinks() {
    let t = Tmp::new();
    let out = Tmp::new();
    let r = t.path.clone();
    touch(&out.join("odir/secret"));
    touch(&r.join("in/f"));
    symlink(out.join("odir"), r.join("to_out")).unwrap();
    symlink("in", r.join("to_in")).unwrap();
    symlink("in/f", r.join("to_file")).unwrap();
    symlink("ghost", r.join("dangling")).unwrap();
    symlink(".", r.join("loop")).unwrap();
    let got = all(walk(&r, &yes));
    let ns: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(ns, vec!["dangling", "in", "in/f", "loop", "to_file", "to_in", "to_out"]);
    for (p, a) in &got {
        if !p.starts_with("in") {
            assert!(matches!(a.kind, FileKind::Symlink { .. }), "{p}");
        }
    }
    assert!(!ns.iter().any(|p| p.contains("secret")));
}

#[test]
fn walk_does_not_descend_into_mount_points() {
    let t = Tmp::new();
    let r = t.path.clone();
    fs::create_dir(r.join("mnt")).unwrap();
    touch(&r.join("plain"));
    let mut img = Image::create(Fs::Apfs, 32);
    fs::write(img.path().join("inside"), "i").unwrap();
    img.attach_at(&r.join("mnt"));
    assert!(r.join("mnt/inside").exists());
    let got = all(walk(&r, &yes));
    let ns: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(ns, vec!["mnt", "plain"]);
    assert_eq!(got[0].1.kind, FileKind::Dir);
    drop(img);
}

#[test]
fn walk_reports_an_unreadable_directory_and_continues() {
    let t = Tmp::new();
    let r = t.path.clone();
    touch(&r.join("a/f"));
    touch(&r.join("locked/hidden"));
    touch(&r.join("z/g"));
    set_mode(&r.join("locked"), 0o000);
    let items: Vec<Item> = walk(&r, &yes).collect();
    set_mode(&r.join("locked"), 0o755);
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for i in items {
        match i {
            Ok((p, _)) => ok.push(p.as_str().to_string()),
            Err(e) => errs.push(e),
        }
    }
    assert_eq!(ok, vec!["a", "a/f", "locked", "z", "z/g"]);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].code, Code::Unknown);
    assert_eq!(detail_str(&errs[0], "reason").as_deref(), Some("unreadable_entry"));
    assert_eq!(detail_i64(&errs[0], "errno"), Some(libc::EACCES as i64));
}

#[test]
fn walk_skips_entries_that_vanish_mid_walk() {
    let t = Tmp::new();
    let r = t.path.clone();
    touch(&r.join("d/inner"));
    touch(&r.join("e/f"));
    symlink("e/f", r.join("lnk")).unwrap();
    let _g = ClearHooks;
    let d = r.join("d");
    set_before_open(Some(Box::new(move |_, n| {
        if n == "d" {
            let _ = fs::remove_dir_all(&d);
        }
    })));
    let got = names(walk(&r, &yes));
    assert_eq!(got, vec!["d", "e", "e/f", "lnk"]);
    set_before_open(None);
    // A symlink removed between the bulk read and readlinkat is skipped.
    let fired = Rc::new(Cell::new(false));
    let (f2, l) = (fired.clone(), r.join("lnk"));
    set_after_bulk_window(Some(Box::new(move |_| {
        if !f2.get() {
            f2.set(true);
            fs::remove_file(&l).unwrap();
        }
    })));
    let got = names(list_dir(&r, None));
    assert!(fired.get());
    assert_eq!(got, vec!["e"]);
}

#[test]
fn walk_never_follows_a_directory_swapped_for_a_link() {
    let t = Tmp::new();
    let out = Tmp::new();
    let r = t.path.clone();
    touch(&r.join("d/inner"));
    touch(&r.join("z"));
    touch(&out.join("odir/secret"));
    let _g = ClearHooks;
    let (rr, od) = (r.clone(), out.join("odir"));
    set_before_open(Some(Box::new(move |_, n| {
        if n == "d" && fs::symlink_metadata(rr.join("d")).unwrap().is_dir() {
            fs::rename(rr.join("d"), rr.join("d.real")).unwrap();
            symlink(&od, rr.join("d")).unwrap();
        }
    })));
    let items: Vec<Item> = walk(&r, &yes).collect();
    assert!(items.iter().all(|i| i.is_ok()), "{items:?}");
    let ns: Vec<String> = items.into_iter().map(|i| i.unwrap().0.as_str().to_string()).collect();
    assert_eq!(ns, vec!["d", "z"]);
}

#[test]
fn walk_reports_a_swapped_root() {
    let t = Tmp::new();
    let r = t.join("root");
    touch(&r.join("a/f"));
    touch(&r.join("b"));
    // Control: untouched root, no Busy.
    assert!(walk(&r, &yes).all(|i| i.is_ok()));
    let _g = ClearHooks;
    let (rr, old) = (r.clone(), t.join("root.old"));
    set_before_open(Some(Box::new(move |_, n| {
        if n == "a" {
            fs::rename(&rr, &old).unwrap();
            fs::create_dir(&rr).unwrap();
            fs::write(rr.join("new"), "n").unwrap();
        }
    })));
    let items: Vec<Item> = walk(&r, &yes).collect();
    let (last, rest) = items.split_last().unwrap();
    let ns: Vec<String> = rest.iter().map(|i| i.as_ref().unwrap().0.as_str().to_string()).collect();
    assert_eq!(ns, vec!["a", "a/f", "b"], "entries from the pinned old directory");
    let e = last.as_ref().unwrap_err();
    assert_eq!(e.code, Code::Busy);
    assert_eq!(detail_str(e, "reason").as_deref(), Some("root_changed"));
}

#[test]
fn walk_start_is_resolved_without_following_links() {
    let t = Tmp::new();
    let out = Tmp::new();
    let r = t.path.clone();
    touch(&r.join("a/b/c"));
    touch(&r.join("a/b/d/e"));
    touch(&out.join("b/secret"));
    symlink(&out.path, r.join("l")).unwrap();
    let opts = |s: &str| WalkOptions { start: Some(rp(s)), ..Default::default() };
    assert_eq!(names(walk_with(&r, &opts("a/b"), &|_, _| true)), vec!["a/b/c", "a/b/d", "a/b/d/e"]);
    let first = |s: &str| walk_with(&r, &opts(s), &|_, _| true).collect::<Vec<_>>();
    let v = first("l/b");
    assert_eq!(v.len(), 1);
    let e = v[0].as_ref().unwrap_err();
    assert_eq!((e.code, detail_str(e, "reason").as_deref()), (Code::NotDirectory, Some("symlink")));
    let v = first("nope");
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].as_ref().unwrap_err().code, Code::NoSuchPath);
    let v = first("a/b/c");
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].as_ref().unwrap_err().code, Code::NotDirectory);
}

#[test]
fn walk_names_are_nfc_and_openable_either_way() {
    let t = Tmp::new();
    let r = t.path.clone();
    fs::write(r.join("e\u{301}"), "nfd").unwrap(); // NFD: 65 cc 81; NFC é: c3 a9
    fs::write(r.join("f"), "f").unwrap();
    fs::create_dir(r.join("cafe\u{301}")).unwrap();
    fs::write(r.join("cafe\u{301}/x"), "x").unwrap();
    let got = names(walk(&r, &yes));
    // NFC byte order: "caf\u{e9}" < "f" < "\u{e9}" (NFD order would put "e\u{301}" before "f").
    assert_eq!(got, vec!["caf\u{e9}", "caf\u{e9}/x", "f", "\u{e9}"]);
    for p in &got {
        if p != "caf\u{e9}" {
            open_rooted(&r, &rp(p)).unwrap();
        }
    }
}

#[test]
fn walk_depth_limit_is_an_error_item() {
    let t = Tmp::new();
    let r = t.path.clone();
    let mut p = r.join("a");
    for _ in 1..130 {
        p.push("a");
    }
    fs::create_dir_all(&p).unwrap();
    touch(&r.join("b/shallow"));
    let items: Vec<Item> = walk(&r, &yes).collect();
    let mut a_count = 0;
    let mut errs = Vec::new();
    for i in &items {
        match i {
            Ok((p, _)) if p.as_str().starts_with('a') => a_count += 1,
            Ok(_) => {}
            Err(e) => errs.push(e),
        }
    }
    assert_eq!(a_count, MAX_WALK_DEPTH as usize, "dirs down to level 128");
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].code, Code::TooLarge);
    assert_eq!(detail_str(errs[0], "reason").as_deref(), Some("too_deep"));
    let ok: Vec<String> = items.iter().filter_map(|i| i.as_ref().ok()).map(|(p, _)| p.as_str().to_string()).collect();
    assert!(ok.contains(&"b/shallow".to_string()));
    // Exactly MAX_WALK_DEPTH levels: complete, no error.
    let t2 = Tmp::new();
    let mut p = t2.join("a");
    for _ in 1..MAX_WALK_DEPTH {
        p.push("a");
    }
    fs::create_dir_all(&p).unwrap();
    let items: Vec<Item> = walk(&t2.path, &yes).collect();
    assert!(items.iter().all(|i| i.is_ok()));
    assert_eq!(items.len(), MAX_WALK_DEPTH as usize);
}

fn open_fds() -> usize {
    fs::read_dir("/dev/fd").unwrap().count()
}

#[test]
fn walk_fits_a_low_descriptor_limit() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "low_descriptor_limit_child", "--nocapture", "--test-threads=1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "child failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
}

#[test]
#[ignore = "child of walk_fits_a_low_descriptor_limit"]
fn low_descriptor_limit_child() {
    let lim = libc::rlimit { rlim_cur: 256, rlim_max: libc::RLIM_INFINITY };
    let mut cur: libc::rlimit = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut cur) }, 0);
    let lim = libc::rlimit { rlim_max: cur.rlim_max, ..lim };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) }, 0);
    let t = Tmp::new();
    let r = t.path.clone();
    for i in 0..412 {
        fs::create_dir_all(r.join(format!("w/d{i:03}"))).unwrap();
        fs::write(r.join(format!("w/d{i:03}/f")), "").unwrap();
    }
    let mut p = r.join("chain");
    for _ in 1..100 {
        p.push("c");
    }
    fs::create_dir_all(&p).unwrap();
    let base = open_fds();
    let mut max_over = 0usize;
    let mut dirs = Vec::new();
    for item in walk(&r, &yes) {
        let (p, a) = item.unwrap();
        let depth = p.as_str().matches('/').count() + 1;
        let now = open_fds();
        max_over = max_over.max(now.saturating_sub(base + depth));
        if a.kind == FileKind::Dir {
            dirs.push(p);
        }
    }
    assert!(dirs.len() >= 512, "{}", dirs.len());
    assert!(max_over <= 4, "descriptors beyond depth: {max_over}");
    for d in &dirs {
        assert_eq!(attrs_of(&r, d).unwrap().kind, FileKind::Dir);
    }
    assert_eq!(open_fds(), base, "attrs_of leaked a descriptor");
}

/// status-perf A1: `with_walk_stop` ends a walk on this thread at the next
/// directory with one `Busy`/`stopped` error; unset or on another thread,
/// walks run to the end; the flag goes away when the closure returns.
#[test]
fn with_walk_stop_ends_walks_at_the_next_directory() {
    let t = Tmp::new();
    let r = t.path.clone();
    for f in ["a/1", "a/b/2", "c/3", "d/4"] {
        touch(&r.join(f));
    }
    let full = names(walk(&r, &yes)).len();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (n, last) = strata_apfs::with_walk_stop(stop.clone(), || {
        let mut n = 0;
        let mut last = None;
        for item in strata_apfs::walk(&r, &yes) {
            match item {
                Ok(_) => {
                    n += 1;
                    stop.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Err(e) => last = Some(e),
            }
        }
        (n, last)
    });
    let e = last.expect("stopped");
    assert_eq!(e.code, strata_types::Code::Busy, "{e:?}");
    assert_eq!(e.detail.as_ref().unwrap()["reason"], "stopped");
    assert!(n < full, "{n} of {full}");
    // Another thread is not affected while the flag is set here.
    strata_apfs::with_walk_stop(stop.clone(), || {
        let other = std::thread::scope(|s| s.spawn(|| names(walk(&r, &yes)).len()).join().unwrap());
        assert_eq!(other, full);
    });
    assert_eq!(names(walk(&r, &yes)).len(), full, "gone after the closure");
}

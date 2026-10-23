//! Task 8: rebase (§5.8) and projection (§5.12).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use strata_overlay::{
    Base, LwwRules, Overlay, RebaseOptions, Rebased, checkpoint, project, rebase,
};
use strata_types::{BlobId, CheckpointReason, Code, EntryKind, OverlayEntry, RepoPath};

// ---------------------------------------------------------------- helpers

/// Records a checkpoint of the workspace and returns its overlay.
fn ckpt(w: &World) -> Overlay {
    let c = checkpoint(
        &w.ws_ref(),
        &w.base,
        None,
        CheckpointReason::Manual {
            message: "m".to_owned(),
        },
    )
    .expect("checkpoint");
    let o = Overlay::from_checkpoint(&c.checkpoint);
    o.validate().expect("validate");
    o
}

fn bid(b: &[u8]) -> BlobId {
    BlobId::try_from(blake3_hex(b)).unwrap()
}

fn paths(v: &[&str]) -> Vec<RepoPath> {
    v.iter().map(|s| rp(s)).collect()
}

fn get<'a>(o: &'a Overlay, p: &str) -> Option<&'a OverlayEntry> {
    o.get(&rp(p))
}

fn kind<'a>(o: &'a Overlay, p: &str) -> &'a EntryKind {
    &get(o, p).unwrap_or_else(|| panic!("no entry at {p}")).kind
}

fn move_opts(new: &Base) -> RebaseOptions {
    RebaseOptions::for_move(new, false).expect("for_move")
}

fn run(w: &World, ov: &Overlay, new: &Base, opts: &RebaseOptions) -> Rebased {
    let r = rebase(ov, &w.base, new, &w.store, opts).expect("rebase");
    r.overlay.validate().expect("rebased overlay validates");
    assert_eq!(r.overlay.base, new.r#gen);
    r
}

/// A fresh clone of `dir` under the session root (where `project` may write).
fn clone_in_session(w: &World, dir: &Path, name: &str) -> PathBuf {
    let dst = w.root.join("sessions/s-ovrlayaa").join(name);
    strata_apfs::clone_tree(dir, &dst).expect("clone base");
    dst
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Dir,
    File { bytes: Vec<u8>, exec: bool },
    Link(PathBuf),
}

/// Every path under `dir` (the root's `.git` aside): kind, content or link
/// target, executable bit.
fn tree(dir: &Path) -> BTreeMap<String, Node> {
    let mut out = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for e in fs::read_dir(dir.join(&rel)).unwrap().flatten() {
            let r = rel.join(e.file_name());
            let key = r.to_string_lossy().into_owned();
            if key == ".git" {
                continue;
            }
            let md = fs::symlink_metadata(e.path()).unwrap();
            let node = if md.file_type().is_symlink() {
                Node::Link(fs::read_link(e.path()).unwrap())
            } else if md.is_dir() {
                stack.push(r.clone());
                Node::Dir
            } else {
                Node::File {
                    bytes: fs::read(e.path()).unwrap(),
                    exec: md.permissions().mode() & 0o100 != 0,
                }
            };
            out.insert(key, node);
        }
    }
    out
}

fn file(b: &[u8]) -> Node {
    Node::File {
        bytes: b.to_vec(),
        exec: false,
    }
}

fn no_project_temps(dst: &Path) {
    for e in fs::read_dir(dst).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        assert!(!n.starts_with(".strata-project-"), "leftover {n}");
    }
}

fn lines(n: usize, edit: &[(usize, &str)]) -> Vec<u8> {
    let mut v: Vec<String> = (1..=n).map(|i| format!("line {i}")).collect();
    for (i, s) in edit {
        v[*i - 1] = (*s).to_owned();
    }
    let mut s = v.join("\n");
    s.push('\n');
    s.into_bytes()
}

fn chmod(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

/// `git merge-file -p ours base theirs`: (exit status, bytes).
fn git_merge_file(w: &World, base: &[u8], ours: &[u8], theirs: &[u8]) -> (i32, Vec<u8>) {
    let d = w.tmp.join("merge-file");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("b"), base).unwrap();
    fs::write(d.join("o"), ours).unwrap();
    fs::write(d.join("t"), theirs).unwrap();
    let out = Command::new("git")
        .args(["merge-file", "-p", "o", "b", "t"])
        .current_dir(&d)
        .output()
        .unwrap();
    (out.status.code().unwrap(), out.stdout)
}

// ---------------------------------------------------------------- tests

#[test]
fn rebase_one_sided_changes_project() {
    let w = World::with(|m| {
        put(m, "a", b"a\n");
        put(m, "b", b"b\n");
        put(m, "c", b"c\n");
        put(m, "d", b"d\n");
    });
    w.write("a", b"a session\n");
    w.write("c", b"same\n");
    w.remove("d");
    w.write("u.txt", b"untracked\n");
    let ov = ckpt(&w);
    assert!(matches!(kind(&ov, "u.txt"), EntryKind::Untracked { .. }));
    let (new, _) = w.next_base(|m| {
        put(m, "b", b"b upstream\n");
        put(m, "c", b"same\n");
        fs::remove_file(m.join("d")).unwrap();
        put(m, "u.txt", b"untracked\n");
    });
    let before = tree(&w.ws);
    let r = run(&w, &ov, &new, &move_opts(&new));
    assert_eq!(tree(&w.ws), before, "rebase never writes the workspace");
    assert_eq!(r.overlay.entries.len(), 1, "{:?}", r.overlay);
    assert_eq!(
        kind(&r.overlay, "a"),
        &EntryKind::Modified {
            blob: bid(b"a session\n")
        }
    );
    assert!(get(&r.overlay, "b").is_none());
    assert_eq!(r.dropped, paths(&["c", "d", "u.txt"]));
    assert!(r.conflicts.is_empty() && r.lww.is_empty());

    // Projected onto a clone of the new base: the session's content on top of it.
    let dst = clone_in_session(&w, &new.dir, "proj-one-sided");
    project(&w.store, &r.overlay, &dst).expect("project");
    let mut want = tree(&w.ws);
    want.insert("b".into(), file(b"b upstream\n"));
    assert_eq!(tree(&dst), want);
    no_project_temps(&dst);
}

#[test]
fn rebase_both_changed_merge_or_conflict() {
    let w = World::with(|m| {
        put(m, "m.txt", &lines(9, &[]));
        put(m, "s.txt", &lines(9, &[]));
        put(m, "del1.txt", b"del1\n");
        put(m, "del2.txt", b"del2\n");
        put(m, "bin.dat", b"\0bin\0");
        put(m, "big.txt", b"x\n");
        put(m, "f.txt", b"fff\n");
        put(m, "e.sh", b"echo\n");
        put(m, "g.txt", b"g\n");
    });
    w.write("m.txt", &lines(9, &[(1, "ours")]));
    w.write("s.txt", &lines(9, &[(5, "ours")]));
    w.remove("del1.txt");
    w.write("del2.txt", b"del2 ours\n");
    w.write("bin.dat", b"\0bin ours\0");
    // Only the session side exceeds MERGE_MAX_BYTES (one large file, not three).
    let big: Vec<u8> = b"0123456789abcde\n".repeat(65 * 1024 * 1024 / 16 + 1);
    w.write("big.txt", &big);
    fs::rename(w.ws.join("f.txt"), w.ws.join("g2.txt")).unwrap();
    chmod(&w.ws.join("e.sh"), 0o755);
    w.write("g.txt", b"G\n");
    chmod(&w.ws.join("g.txt"), 0o755);
    let ov = ckpt(&w);
    assert!(matches!(kind(&ov, "g2.txt"), EntryKind::Renamed { .. }));
    let (new, _) = w.next_base(|m| {
        put(m, "m.txt", &lines(9, &[(9, "theirs")]));
        put(m, "s.txt", &lines(9, &[(5, "theirs")]));
        put(m, "del1.txt", b"del1 theirs\n");
        fs::remove_file(m.join("del2.txt")).unwrap();
        put(m, "bin.dat", b"\0bin theirs\0");
        put(m, "big.txt", b"y\n");
        put(m, "f.txt", b"fff theirs\n");
        put(m, "e.sh", b"echo 2\n");
        put(m, "g.txt", b"G\n");
    });

    // merge = true.
    let r = run(&w, &ov, &new, &move_opts(&new));
    assert_eq!(
        r.conflicts,
        paths(&[
            "big.txt", "bin.dat", "del1.txt", "del2.txt", "f.txt", "s.txt"
        ])
    );
    // Clean merge: equals git merge-file, stored with put_bytes.
    let (code, merged) = git_merge_file(
        &w,
        &lines(9, &[]),
        &lines(9, &[(1, "ours")]),
        &lines(9, &[(9, "theirs")]),
    );
    assert_eq!(code, 0);
    assert_eq!(
        kind(&r.overlay, "m.txt"),
        &EntryKind::Modified { blob: bid(&merged) }
    );
    assert_eq!(w.store.read_blob(&bid(&merged), 1 << 20).unwrap(), merged);
    // Same region: the session's version.
    assert_eq!(
        kind(&r.overlay, "s.txt"),
        &EntryKind::Modified {
            blob: bid(&lines(9, &[(5, "ours")]))
        }
    );
    // Delete/modify both ways: the session side.
    assert_eq!(kind(&r.overlay, "del1.txt"), &EntryKind::Deleted);
    assert_eq!(
        kind(&r.overlay, "del2.txt"),
        &EntryKind::Added {
            blob: bid(b"del2 ours\n")
        }
    );
    assert_eq!(
        kind(&r.overlay, "bin.dat"),
        &EntryKind::Modified {
            blob: bid(b"\0bin ours\0")
        }
    );
    assert_eq!(
        kind(&r.overlay, "big.txt"),
        &EntryKind::Modified { blob: bid(&big) }
    );
    // Renamed f, upstream changed f: f conflicts, the rename's content stays.
    assert_eq!(kind(&r.overlay, "f.txt"), &EntryKind::Deleted);
    assert_eq!(
        kind(&r.overlay, "g2.txt"),
        &EntryKind::Added {
            blob: bid(b"fff\n")
        }
    );
    // Session's exec bit on upstream's content: the blob is stored.
    let e = get(&r.overlay, "e.sh").unwrap();
    assert_eq!(
        e.kind,
        EntryKind::Modified {
            blob: bid(b"echo 2\n")
        }
    );
    assert_eq!(e.mode, 0o100755);
    w.store.verify_blob(&bid(b"echo 2\n")).expect("blob stored");
    let g = get(&r.overlay, "g.txt").unwrap();
    assert_eq!(
        (g.kind.clone(), g.mode),
        (EntryKind::Modified { blob: bid(b"G\n") }, 0o100755)
    );
    let dst = clone_in_session(&w, &new.dir, "proj-merge");
    project(&w.store, &r.overlay, &dst).expect("project");
    let t = tree(&dst);
    assert_eq!(
        t["e.sh"],
        Node::File {
            bytes: b"echo 2\n".to_vec(),
            exec: true
        }
    );
    assert_eq!(t["m.txt"], file(&merged));
    no_project_temps(&dst);

    // merge = false: every both-sides path conflicts, no blob is written.
    let before = w.store_files();
    let r = run(&w, &ov, &new, &RebaseOptions::for_restore(false, false));
    assert_eq!(w.store_files(), before, "merge = false writes no blob");
    assert_eq!(
        r.conflicts,
        paths(&[
            "big.txt", "bin.dat", "del1.txt", "del2.txt", "e.sh", "f.txt", "g.txt", "m.txt",
            "s.txt"
        ])
    );
    let g = get(&r.overlay, "g.txt").unwrap();
    assert_eq!(
        (g.kind.clone(), g.mode),
        (EntryKind::Modified { blob: bid(b"G\n") }, 0o100755)
    );
    assert_eq!(
        get(&r.overlay, "e.sh").unwrap().kind,
        EntryKind::Modified {
            blob: bid(b"echo\n")
        }
    );

    // A base file that no longer matches its manifest: Corrupt, nothing taken.
    let p = new.dir.join("m.txt");
    chmod(&p, 0o644);
    fs::write(&p, b"tampered\n").unwrap();
    let e = rebase(&ov, &w.base, &new, &w.store, &move_opts(&new)).unwrap_err();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");
}

#[test]
fn rebase_moves_on_both_sides_do_not_conflict() {
    let w = World::with(|m| {
        put(m, "d/1.txt", b"one\n");
        put(m, "d/2.txt", b"two\n");
        put(m, "a.txt", b"aaa\n");
        put(m, "keep.txt", b"k\n");
        put(m, "src/Foo.ts", b"foo\n");
    });
    fs::rename(w.ws.join("d"), w.ws.join("x")).unwrap();
    fs::rename(w.ws.join("a.txt"), w.ws.join("b.txt")).unwrap();
    let ov = ckpt(&w);
    let (new1, _) = w.next_base(|m| {
        fs::rename(m.join("d"), m.join("y")).unwrap();
        put(m, "keep.txt", b"k2\n");
    });
    let r = run(&w, &ov, &new1, &move_opts(&new1));
    assert!(r.conflicts.is_empty(), "{:?}", r.conflicts);
    for p in ["x/1.txt", "x/2.txt"] {
        assert!(
            matches!(kind(&r.overlay, p), EntryKind::Added { .. }),
            "{p}"
        );
    }
    assert_eq!(r.dropped, paths(&["d/1.txt", "d/2.txt"]));
    assert_eq!(
        kind(&r.overlay, "b.txt"),
        &EntryKind::Renamed {
            from: rp("a.txt"),
            blob: bid(b"aaa\n")
        }
    );
    let (new2, _) = w.next_base(|m| fs::remove_file(m.join("a.txt")).unwrap());
    let r = run(&w, &ov, &new2, &move_opts(&new2));
    assert!(r.conflicts.is_empty());
    assert_eq!(
        kind(&r.overlay, "b.txt"),
        &EntryKind::Added {
            blob: bid(b"aaa\n")
        }
    );

    // Case-only rename with new content, upstream untouched.
    let blob = w.store.put_bytes(b"foo 2\n").unwrap();
    let case = Overlay {
        base: w.base.r#gen.clone(),
        entries: vec![OverlayEntry {
            path: rp("src/foo.ts"),
            kind: EntryKind::Renamed {
                from: rp("src/Foo.ts"),
                blob: blob.clone(),
            },
            mode: 0o100644,
        }],
    };
    let opts = |ignorecase, volume_case_sensitive| RebaseOptions {
        lww: LwwRules::default(),
        ignorecase,
        volume_case_sensitive,
        merge: true,
    };
    for (ic, cs) in [(false, false), (true, true)] {
        let r = run(&w, &case, &new1, &opts(ic, cs));
        assert_eq!(r.overlay.entries, case.entries, "ignorecase={ic} cs={cs}");
        assert!(r.conflicts.is_empty() && r.dropped.is_empty());
    }
    let r = run(&w, &case, &new1, &opts(false, true));
    assert_eq!(kind(&r.overlay, "src/Foo.ts"), &EntryKind::Deleted);
    assert_eq!(kind(&r.overlay, "src/foo.ts"), &EntryKind::Added { blob });
}

#[test]
fn lww_newest_wins_no_conflict() {
    let w = World::with(|m| {
        put(m, "bun.lock", &lines(9, &[]));
        put(m, "generated/api.ts", &lines(9, &[]));
        put(m, "src/a.ts", &lines(9, &[]));
        put(m, "src/b.ts", &lines(9, &[]));
    });
    let ours_lock = lines(9, &[(1, "session")]);
    let ours_api = lines(9, &[(5, "session")]);
    let ours_a = lines(9, &[(5, "session")]);
    w.write("bun.lock", &ours_lock);
    w.write("generated/api.ts", &ours_api);
    w.write("src/a.ts", &ours_a);
    w.write("src/b.ts", &lines(9, &[(1, "session")]));
    let ov = ckpt(&w);
    let (new, _) = w.next_base(|m| {
        put(m, ".strata/lww", b"*.lock\ngenerated/**\n");
        put(m, "bun.lock", &lines(9, &[(9, "upstream")]));
        put(m, "generated/api.ts", &lines(9, &[(5, "upstream")]));
        put(m, "src/a.ts", &lines(9, &[(5, "upstream")]));
        put(m, "src/b.ts", &lines(9, &[(9, "upstream")]));
    });
    let r = run(&w, &ov, &new, &move_opts(&new));
    assert_eq!(r.lww, paths(&["bun.lock", "generated/api.ts"]));
    assert_eq!(r.conflicts, paths(&["src/a.ts"]));
    let blob_at = |r: &Rebased, p: &str| match kind(&r.overlay, p) {
        EntryKind::Modified { blob } => blob.clone(),
        k => panic!("{p}: {k:?}"),
    };
    assert_eq!(blob_at(&r, "bun.lock"), bid(&ours_lock), "no merge");
    assert_eq!(blob_at(&r, "generated/api.ts"), bid(&ours_api));
    assert_eq!(blob_at(&r, "src/a.ts"), bid(&ours_a));
    let merged = lines(9, &[(1, "session"), (9, "upstream")]);
    assert_eq!(blob_at(&r, "src/b.ts"), bid(&merged));

    let r = run(&w, &ov, &new, &RebaseOptions::for_restore(false, false));
    assert!(r.lww.is_empty());
    assert!(r.conflicts.contains(&rp("bun.lock")));
}

#[test]
fn rebase_prefix_collisions_keep_the_session_side() {
    let w = World::with(|m| {
        put(m, "d/a.ts", b"a\n");
        put(m, "lib/a.ts", b"la\n");
        put(m, "src/s.ts", b"s\n");
    });
    fs::remove_dir_all(w.ws.join("d")).unwrap();
    w.write("d", b"dfile\n");
    w.write("lib/x.ts", b"x\n");
    let ov = ckpt(&w);
    let session_kind = |p: &str| kind(&ov, p).clone();

    // Upstream adds d/new.ts and replaces lib/ with a file.
    let (new1, _) = w.next_base(|m| {
        put(m, "d/new.ts", b"new\n");
        fs::remove_dir_all(m.join("lib")).unwrap();
        put(m, "lib", b"libfile\n");
    });
    // Then replaces the file lib with a symlink to src.
    let (new2, _) = w.next_base(|m| {
        fs::remove_file(m.join("lib")).unwrap();
        symlink("src", m.join("lib")).unwrap();
    });
    for (n, new) in [(1, &new1), (2, &new2)] {
        let r = run(&w, &ov, new, &move_opts(new));
        assert_eq!(
            r.conflicts,
            paths(&["d", "d/new.ts", "lib", "lib/x.ts"]),
            "{n}"
        );
        assert_eq!(kind(&r.overlay, "d"), &session_kind("d"));
        assert_eq!(kind(&r.overlay, "d/new.ts"), &EntryKind::Deleted);
        assert_eq!(kind(&r.overlay, "d/a.ts"), &EntryKind::Deleted);
        assert_eq!(kind(&r.overlay, "lib"), &EntryKind::Deleted);
        assert_eq!(kind(&r.overlay, "lib/x.ts"), &session_kind("lib/x.ts"));
        let dst = clone_in_session(&w, &new.dir, &format!("proj-prefix-{n}"));
        project(&w.store, &r.overlay, &dst).expect("project");
        let mut want = BTreeMap::new();
        want.insert("d".to_owned(), file(b"dfile\n"));
        want.insert("lib".to_owned(), Node::Dir);
        want.insert("lib/x.ts".to_owned(), file(b"x\n"));
        want.insert("src".to_owned(), Node::Dir);
        want.insert("src/s.ts".to_owned(), file(b"s\n"));
        // The symlink in new2 was removed, never followed: src/ has no x.ts.
        assert_eq!(tree(&dst), want, "{n}");
        no_project_temps(&dst);
    }
}

#[test]
fn project_materializes_entries_onto_a_clone() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "docs/a.md", b"doc\n");
    });
    w.write("src/lib.rs", b"pub fn b() {}\n");
    chmod(&w.ws.join("empty.txt"), 0o755);
    put_exec(&w.ws, "tools/new.sh", b"#!/bin/sh\n");
    w.write("new.rs", b"fn main() {}\n");
    w.git_ok(&w.ws, &["add", "new.rs"]);
    w.write("notes.txt", b"notes\n");
    put_link(&w.ws, "link2", "src");
    w.remove("bin");
    fs::rename(w.ws.join("README.md"), w.ws.join("README2.md")).unwrap();
    w.remove("docs");
    w.write("docs", b"docs file\n");
    w.remove("src/deep");
    let ov = ckpt(&w);
    for (p, want) in [
        ("src/lib.rs", "Modified"),
        ("empty.txt", "Modified"),
        ("tools/new.sh", "Untracked"),
        ("new.rs", "Added"),
        ("notes.txt", "Untracked"),
        ("link2", "Untracked"),
        ("bin/run.sh", "Deleted"),
        ("README2.md", "Renamed"),
        ("docs", "Untracked"),
        ("docs/a.md", "Deleted"),
        ("src/deep/x/y.txt", "Deleted"),
    ] {
        let k = format!("{:?}", kind(&ov, p));
        assert!(k.starts_with(want), "{p}: {k}");
    }

    let dst = clone_in_session(&w, &w.base.dir, "proj-all");
    project(&w.store, &ov, &dst).expect("project");
    assert_eq!(tree(&dst), tree(&w.ws));
    assert!(!dst.join("src/deep").exists() && !dst.join("bin").exists());
    no_project_temps(&dst);

    // A base file to remove is missing: Corrupt, never taken as new.
    let dst = clone_in_session(&w, &w.base.dir, "proj-missing");
    fs::remove_file(dst.join("bin/run.sh")).unwrap();
    let e = project(&w.store, &ov, &dst).unwrap_err();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");

    // A parent replaced by a symlink leading outside dst: NotDirectory.
    let outside = w.tmp.join("outside");
    put(&outside, "deep/x/y.txt", b"outside\n");
    put(&outside, "lib.rs", b"outside\n");
    let before = tree(&outside);
    let dst = clone_in_session(&w, &w.base.dir, "proj-link");
    fs::remove_dir_all(dst.join("src")).unwrap();
    symlink(&outside, dst.join("src")).unwrap();
    let e = project(&w.store, &ov, &dst).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(tree(&outside), before, "nothing written outside dst");

    // dst outside <store root>/sessions/: InvalidInput, nothing written.
    let elsewhere = w.tmp.join("elsewhere");
    strata_apfs::clone_tree(&w.base.dir, &elsewhere).unwrap();
    let before = tree(&elsewhere);
    let e = project(&w.store, &ov, &elsewhere).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(e.detail.unwrap()["reason"], "dst_outside_sessions");
    assert_eq!(tree(&elsewhere), before);
}

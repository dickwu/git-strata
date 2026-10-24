//! Task 2: path-level stage and merge (publish.md §5.3, D4, D5, D12).

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;
use strata_publish::{Staged, stage};
use strata_types::{Checkpoint, CommitOid, Code, EntryKind, ReleaseGroup, SessionId, StrataError};
use support::*;

const ORIGIN: &str = "refs/strata/remotes/origin/main";

/// Land `files` on origin (and fetch) so sessions made after pin them; B.
fn seed(fx: &Fx, files: &[(&str, &[u8])]) -> CommitOid {
    fx.land_on_origin("tool", "main", files, "seed");
    b(fx)
}

fn b(fx: &Fx) -> CommitOid {
    CommitOid::try_from(fx.read_ref("tool", ORIGIN).unwrap()).unwrap()
}

fn run(fx: &Fx, grp: &ReleaseGroup, base: &CommitOid) -> Result<Staged, StrataError> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let c = fx.ctx(&d, &ss);
    stage(&c, grp, base)
}

fn ws(fx: &Fx, s: &SessionId) -> PathBuf {
    fx.ws(s, "tool")
}

fn rm(fx: &Fx, s: &SessionId, p: &str) {
    let full = ws(fx, s).join(p);
    if fs::symlink_metadata(&full).unwrap().is_dir() {
        fs::remove_dir_all(full).unwrap();
    } else {
        fs::remove_file(full).unwrap();
    }
}

fn mv(fx: &Fx, s: &SessionId, a: &str, to: &str) {
    let w = ws(fx, s);
    if let Some(p) = Path::new(to).parent() {
        fs::create_dir_all(w.join(p)).unwrap();
    }
    fs::rename(w.join(a), w.join(to)).unwrap();
}

fn chmod(fx: &Fx, s: &SessionId, p: &str, mode: u32) {
    fs::set_permissions(ws(fx, s).join(p), fs::Permissions::from_mode(mode)).unwrap();
}

fn cp(fx: &Fx, s: &SessionId) -> Checkpoint {
    fx.checkpoint(s, "tool")
}

fn one(s: &SessionId, c: &Checkpoint, paths: &[&str]) -> ReleaseGroup {
    group(vec![batch(s, "tool", c.id, paths)])
}

/// `(mode, path)` of every blob in `tree`, in git's order.
fn ls(fx: &Fx, tree: &str) -> Vec<(String, String)> {
    fx.sb
        .git(&fx.main("tool"), &["ls-tree", "-r", tree])
        .lines()
        .map(|l| {
            let (meta, path) = l.split_once('\t').unwrap();
            (meta.split(' ').next().unwrap().to_string(), path.to_string())
        })
        .collect()
}

fn paths(fx: &Fx, tree: &str) -> Vec<String> {
    ls(fx, tree).into_iter().map(|(_, p)| p).collect()
}

fn has(fx: &Fx, tree: &str, p: &str) -> bool {
    paths(fx, tree).iter().any(|x| x == p)
}

fn mode(fx: &Fx, tree: &str, p: &str) -> String {
    ls(fx, tree).into_iter().find(|(_, x)| x == p).unwrap().0
}

fn cat(fx: &Fx, tree: &str, p: &str) -> Vec<u8> {
    let out = fx.sb.git_output(&fx.main("tool"), &["cat-file", "blob", &format!("{tree}:{p}")]);
    assert!(out.status.success(), "no {p} in {tree}");
    out.stdout
}

fn conflicts(e: &StrataError) -> Vec<Value> {
    assert_eq!(e.code, Code::Conflict, "{e:?}");
    e.detail.as_ref().unwrap()["conflicts"].as_array().unwrap().clone()
}

fn kinds(e: &StrataError) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = conflicts(e)
        .iter()
        .map(|c| (c["path"].as_str().unwrap().to_string(), c["kind"].as_str().unwrap().to_string()))
        .collect();
    v.sort();
    v
}

/// Layout roots under every session root.
fn layouts(fx: &Fx) -> Vec<PathBuf> {
    let sessions = fx.root().join("sessions");
    let mut out = Vec::new();
    for s in fs::read_dir(sessions).into_iter().flatten().flatten() {
        for e in fs::read_dir(s.path()).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(".strata-publish-") {
                out.push(e.path());
            }
        }
    }
    out
}

fn index_writers(fx: &Fx) -> Vec<Vec<String>> {
    fx.git_of("tool")
        .take_git_calls()
        .into_iter()
        .map(|c| c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>())
        .filter(|a| a.iter().any(|x| x == "update-index" || x == "read-tree" || x == "write-tree"))
        .collect()
}

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn with_line(n: usize, at: usize, text: &str) -> String {
    (1..=n).map(|i| if i == at { format!("{text}\n") } else { format!("line {i}\n") }).collect()
}

#[test]
fn stage_only_selected_paths() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("a.txt", b"a\n"), ("b.txt", b"b\n"), ("r.txt", b"rename me\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("a.txt", b"a2\n"), ("b.txt", b"b2\n")]);
    mv(&fx, &s, "r.txt", "r2.txt");
    let c = cp(&fx, &s);
    fx.git_of("tool").take_git_calls();
    let st = run(&fx, &one(&s, &c, &["a.txt", "r2.txt"]), &base).unwrap();
    assert_eq!(st.paths, rps(&["a.txt", "r.txt", "r2.txt"]));
    let t = st.tree.as_str();
    assert_eq!(cat(&fx, t, "a.txt"), b"a2\n");
    assert_eq!(cat(&fx, t, "b.txt"), b"b\n");
    assert!(!has(&fx, t, "r.txt"));
    assert_eq!(cat(&fx, t, "r2.txt"), b"rename me\n");
    for a in index_writers(&fx) {
        assert!(!a.iter().any(|x| x == "b.txt"), "{a:?}");
    }
    // Selecting the `from` half selects both.
    let st2 = {
        fs::remove_dir_all(st.layout_root()).unwrap();
        run(&fx, &one(&s, &c, &["r.txt"]), &base).unwrap()
    };
    assert_eq!(st2.paths, rps(&["r.txt", "r2.txt"]));
}

#[test]
fn two_sessions_nonoverlapping_hunks_merge_clean() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("f.txt", lines(10).as_bytes())]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    fx.edit(&s1, "tool", &[("f.txt", with_line(10, 2, "two").as_bytes())]);
    fx.edit(&s2, "tool", &[("f.txt", with_line(10, 8, "eight").as_bytes())]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![batch(&s1, "tool", c1.id, &["f.txt"]), batch(&s2, "tool", c2.id, &["f.txt"])]);
    let st = run(&fx, &g, &base).unwrap();
    let got = String::from_utf8(cat(&fx, st.tree.as_str(), "f.txt")).unwrap();
    assert!(got.contains("two\n") && got.contains("eight\n"), "{got}");
    assert_eq!(got.lines().count(), 10);
}

#[test]
fn same_hunk_conflict_stops_whole_group() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("f.txt", lines(10).as_bytes()), ("g.txt", b"g\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    fx.edit(&s1, "tool", &[("f.txt", with_line(10, 5, "one").as_bytes()), ("g.txt", b"g1\n")]);
    fx.edit(&s2, "tool", &[("f.txt", with_line(10, 5, "other").as_bytes())]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![batch(&s1, "tool", c1.id, &["f.txt", "g.txt"]), batch(&s2, "tool", c2.id, &["f.txt"])]);
    fx.git_of("tool").take_git_calls();
    let e = run(&fx, &g, &base).unwrap_err();
    let cs = conflicts(&e);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0]["path"], "f.txt");
    assert_eq!(cs[0]["kind"], "text");
    let h = &cs[0]["hunks"][0];
    assert!(h["ours"].as_str().unwrap().contains("other"), "{h}");
    assert!(h["theirs"].as_str().unwrap().contains("one"), "{h}");
    assert!(layouts(&fx).is_empty());
    let calls: Vec<Vec<String>> = fx
        .git_of("tool")
        .take_git_calls()
        .into_iter()
        .map(|c| c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect())
        .collect();
    for a in &calls {
        for bad in ["update-index", "read-tree", "write-tree", "commit-tree", "push", "checkout-index"] {
            assert!(!a.iter().any(|x| x == bad), "{a:?}");
        }
    }
    assert_eq!(fx.read_ref("tool", &strata_publish::chain_ref("main")), None);
}

#[test]
fn same_bytes_do_not_conflict() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("f.txt", b"old\n")]);
    let mut bin = vec![0u8; 400 * 1024];
    for (i, x) in bin.iter_mut().enumerate() {
        *x = (i * 31 % 251) as u8;
    }
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    for s in [&s1, &s2] {
        fx.edit(s, "tool", &[("f.txt", b"same\n"), ("bin.dat", &bin)]);
    }
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![
        batch(&s1, "tool", c1.id, &["f.txt", "bin.dat"]),
        batch(&s2, "tool", c2.id, &["f.txt", "bin.dat"]),
    ]);
    let st = run(&fx, &g, &base).unwrap();
    assert_eq!(cat(&fx, st.tree.as_str(), "f.txt"), b"same\n");
    assert_eq!(cat(&fx, st.tree.as_str(), "bin.dat"), bin);
}

#[test]
fn rename_both_sides_keeps_both_names_not_hardlinks() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("a.txt", b"content a\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    mv(&fx, &s1, "a.txt", "x.txt");
    mv(&fx, &s2, "a.txt", "y.txt");
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    assert!(matches!(c1.entries.iter().find(|e| e.path.as_str() == "x.txt").unwrap().kind, EntryKind::Renamed { .. }));
    let g = group(vec![batch(&s1, "tool", c1.id, &["x.txt"]), batch(&s2, "tool", c2.id, &["y.txt"])]);
    let st = run(&fx, &g, &base).unwrap();
    let t = st.tree.as_str();
    assert!(!has(&fx, t, "a.txt"));
    assert_eq!(cat(&fx, t, "x.txt"), b"content a\n");
    assert_eq!(cat(&fx, t, "y.txt"), b"content a\n");
    for n in ["x.txt", "y.txt"] {
        assert_eq!(fs::symlink_metadata(st.worktree.join(n)).unwrap().nlink(), 1);
    }
}

#[test]
fn delete_against_edit_conflicts() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("f.txt", b"f\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    rm(&fx, &s1, "f.txt");
    fx.edit(&s2, "tool", &[("f.txt", b"edited\n")]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    for g in [
        group(vec![batch(&s1, "tool", c1.id, &["f.txt"]), batch(&s2, "tool", c2.id, &["f.txt"])]),
        group(vec![batch(&s2, "tool", c2.id, &["f.txt"]), batch(&s1, "tool", c1.id, &["f.txt"])]),
    ] {
        let e = run(&fx, &g, &base).unwrap_err();
        assert_eq!(kinds(&e), vec![("f.txt".into(), "modify_delete".into())]);
    }
    assert!(layouts(&fx).is_empty());
}

#[test]
fn delete_directory_against_move_keeps_moved() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("d1/a.txt", b"da\n"), ("d1/b.txt", b"db\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    rm(&fx, &s1, "d1");
    mv(&fx, &s2, "d1", "moved");
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let del = batch(&s1, "tool", c1.id, &["d1"]);
    let mov = batch(&s2, "tool", c2.id, &["moved"]);
    for g in [group(vec![del.clone(), mov.clone()]), group(vec![mov, del])] {
        let st = run(&fx, &g, &base).unwrap();
        let t = st.tree.as_str();
        assert!(!has(&fx, t, "d1/a.txt") && !has(&fx, t, "d1/b.txt"));
        assert_eq!(cat(&fx, t, "moved/a.txt"), b"da\n");
        assert_eq!(cat(&fx, t, "moved/b.txt"), b"db\n");
        fs::remove_dir_all(st.layout_root()).unwrap();
    }
}

#[test]
fn delete_against_disjoint_add_keeps_add() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("d1/old.txt", b"old\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    rm(&fx, &s1, "d1");
    fx.edit(&s2, "tool", &[("d1/new.txt", b"new\n")]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![batch(&s1, "tool", c1.id, &["d1"]), batch(&s2, "tool", c2.id, &["d1/new.txt"])]);
    let st = run(&fx, &g, &base).unwrap();
    let under: Vec<String> = paths(&fx, st.tree.as_str()).into_iter().filter(|p| p.starts_with("d1/")).collect();
    assert_eq!(under, vec!["d1/new.txt".to_string()]);
}

#[test]
fn one_sided_exec_bit_kept() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("run.sh", b"#!/bin/sh\n"), ("other.txt", b"o\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    chmod(&fx, &s1, "run.sh", 0o700);
    fx.edit(&s2, "tool", &[("other.txt", b"o2\n")]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![batch(&s1, "tool", c1.id, &["run.sh"]), batch(&s2, "tool", c2.id, &["other.txt"])]);
    let st = run(&fx, &g, &base).unwrap();
    let t = st.tree.as_str();
    assert_eq!(mode(&fx, t, "run.sh"), "100755");
    assert_eq!(cat(&fx, t, "run.sh"), b"#!/bin/sh\n");
    assert_eq!(cat(&fx, t, "other.txt"), b"o2\n");
}

#[test]
fn chmod_and_content_merge() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("f.sh", b"echo 1\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    chmod(&fx, &s1, "f.sh", 0o755);
    fx.edit(&s2, "tool", &[("f.sh", b"echo 2\n")]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let a = batch(&s1, "tool", c1.id, &["f.sh"]);
    let bb = batch(&s2, "tool", c2.id, &["f.sh"]);
    for g in [group(vec![a.clone(), bb.clone()]), group(vec![bb, a])] {
        let st = run(&fx, &g, &base).unwrap();
        assert_eq!(mode(&fx, st.tree.as_str(), "f.sh"), "100755");
        assert_eq!(cat(&fx, st.tree.as_str(), "f.sh"), b"echo 2\n");
        fs::remove_dir_all(st.layout_root()).unwrap();
    }
}

#[test]
fn merge_root_oracle_path_level() {
    let fx = Fx::with(&["tool"]);
    let tm = lines(10);
    let base = seed(
        &fx,
        &[
            ("dj/a.txt", b"a\n"),
            ("dj/b.txt", b"b\n"),
            ("tm/f.txt", tm.as_bytes()),
            ("tc/f.txt", tm.as_bytes()),
            ("md/f.txt", b"md\n"),
            ("mv1/f.txt", b"mv1\n"),
            ("mv2/f.txt", b"mv2\n"),
            ("me/f.txt", b"me\n"),
            ("rp/a.txt", b"rpa\n"),
            ("rp/b.txt", b"rpb\n"),
            ("df/x.txt", b"dfx\n"),
            ("df/y.txt", b"dfy\n"),
            ("dg/x.txt", b"dgx\n"),
            ("dd/a.txt", b"dda\n"),
            ("dd/b.txt", b"ddb\n"),
            ("be.txt", b"both move\n"),
        ],
    );
    // Clean rows: one group of two sessions.
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    // disjoint
    fx.edit(&s1, "tool", &[("dj/a.txt", b"a1\n")]);
    fx.edit(&s2, "tool", &[("dj/b.txt", b"b2\n")]);
    // text-merge
    fx.edit(&s1, "tool", &[("tm/f.txt", with_line(10, 2, "two").as_bytes())]);
    fx.edit(&s2, "tool", &[("tm/f.txt", with_line(10, 9, "nine").as_bytes())]);
    // move-directory, both directions
    mv(&fx, &s1, "mv1", "mv1n");
    mv(&fx, &s2, "mv2", "mv2n");
    // revive-deleted-parent
    rm(&fx, &s1, "rp");
    fx.edit(&s1, "tool", &[("rp/c.txt", b"rpc\n")]);
    fx.edit(&s2, "tool", &[("rp/d.txt", b"rpd\n")]);
    // directory-becomes-file (paired)
    rm(&fx, &s1, "df");
    fx.edit(&s1, "tool", &[("df", b"now a file\n")]);
    // add-add-same
    fx.edit(&s1, "tool", &[("aa/same.txt", b"same\n")]);
    fx.edit(&s2, "tool", &[("aa/same.txt", b"same\n")]);
    // boundary names
    fx.edit(&s1, "tool", &[("sb/n1/f", b"1\n"), ("sb/n1.y", b"2\n"), ("sb/n1-z", b"3\n"), ("sb/n1x.txt", b"4\n")]);
    // delete-directory-against-delete-inside
    rm(&fx, &s1, "dd");
    rm(&fx, &s2, "dd/a.txt");
    // both-move-elsewhere
    mv(&fx, &s1, "be.txt", "be1.txt");
    mv(&fx, &s2, "be.txt", "be2.txt");
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![
        batch(&s1, "tool", c1.id, &[
            "dj/a.txt", "tm/f.txt", "mv1n", "rp", "df", "df/x.txt", "df/y.txt", "aa/same.txt", "sb", "dd", "be1.txt",
        ]),
        batch(&s2, "tool", c2.id, &["dj/b.txt", "tm/f.txt", "mv2n", "rp/d.txt", "aa/same.txt", "dd/a.txt", "be2.txt"]),
    ]);
    let st = run(&fx, &g, &base).unwrap();
    let t = st.tree.as_str();
    assert_eq!(cat(&fx, t, "dj/a.txt"), b"a1\n");
    assert_eq!(cat(&fx, t, "dj/b.txt"), b"b2\n");
    let m = String::from_utf8(cat(&fx, t, "tm/f.txt")).unwrap();
    assert!(m.contains("two\n") && m.contains("nine\n"));
    assert!(!has(&fx, t, "mv1/f.txt") && !has(&fx, t, "mv2/f.txt"));
    assert_eq!(cat(&fx, t, "mv1n/f.txt"), b"mv1\n");
    assert_eq!(cat(&fx, t, "mv2n/f.txt"), b"mv2\n");
    let rp: Vec<String> = paths(&fx, t).into_iter().filter(|p| p.starts_with("rp/")).collect();
    assert_eq!(rp, vec!["rp/c.txt".to_string(), "rp/d.txt".to_string()]);
    assert_eq!(cat(&fx, t, "df"), b"now a file\n");
    assert!(!has(&fx, t, "df/x.txt"));
    assert_eq!(cat(&fx, t, "aa/same.txt"), b"same\n");
    let sb_names: Vec<String> =
        fx.sb.git(&fx.main("tool"), &["ls-tree", "--name-only", &format!("{t}:sb")]).lines().map(String::from).collect();
    assert_eq!(sb_names, vec!["n1-z", "n1.y", "n1", "n1x.txt"]);
    assert!(paths(&fx, t).iter().all(|p| !p.starts_with("dd/")));
    assert!(!has(&fx, t, "be.txt"));
    for n in ["be1.txt", "be2.txt"] {
        assert_eq!(cat(&fx, t, n), b"both move\n");
        assert_eq!(fs::symlink_metadata(st.worktree.join(n)).unwrap().nlink(), 1);
    }

    // Conflict rows: one group, every path listed.
    let s3 = fx.new_session(&["tool"]);
    let s4 = fx.new_session(&["tool"]);
    fx.edit(&s3, "tool", &[("tc/f.txt", with_line(10, 4, "three").as_bytes())]);
    fx.edit(&s4, "tool", &[("tc/f.txt", with_line(10, 4, "four").as_bytes())]);
    rm(&fx, &s3, "md/f.txt");
    fx.edit(&s4, "tool", &[("md/f.txt", b"md edited\n")]);
    mv(&fx, &s3, "me", "me2");
    fx.edit(&s4, "tool", &[("me/f.txt", b"me edited\n")]);
    fx.edit(&s3, "tool", &[("ad/f.txt", b"one\n")]);
    fx.edit(&s4, "tool", &[("ad/f.txt", b"two\n")]);
    let (c3, c4) = (cp(&fx, &s3), cp(&fx, &s4));
    let g = group(vec![
        batch(&s3, "tool", c3.id, &["tc/f.txt", "md/f.txt", "me2", "ad/f.txt"]),
        batch(&s4, "tool", c4.id, &["tc/f.txt", "md/f.txt", "me/f.txt", "ad/f.txt"]),
    ]);
    let e = run(&fx, &g, &base).unwrap_err();
    assert_eq!(
        kinds(&e),
        vec![
            ("ad/f.txt".into(), "add_add".into()),
            ("md/f.txt".into(), "modify_delete".into()),
            ("me/f.txt".into(), "modify_delete".into()),
            ("tc/f.txt".into(), "text".into()),
        ]
    );

    // directory-becomes-file with half the pair.
    let s5 = fx.new_session(&["tool"]);
    rm(&fx, &s5, "dg");
    fx.edit(&s5, "tool", &[("dg", b"file\n")]);
    let c5 = cp(&fx, &s5);
    let e = run(&fx, &one(&s5, &c5, &["dg"]), &base).unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    assert_eq!(reason(&e), Some("type_pair"));
}

fn lww_fixture(fx: &Fx) -> CommitOid {
    seed(
        fx,
        &[
            (".strata/lww", b"%include /.strata/lww-more\n*.lock\n"),
            (".strata/lww-more", b"gen/\n"),
            ("deps.lock", b"v0\n"),
            ("gen/out.json", b"{}\n"),
            ("plain.txt", b"p\n"),
        ],
    )
}

#[test]
fn lww_later_batch_wins() {
    let fx = Fx::with(&["tool"]);
    let base = lww_fixture(&fx);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    fx.edit(&s1, "tool", &[("deps.lock", b"v1\n"), ("gen/out.json", b"{\"a\":1}\n"), ("plain.txt", b"p1\n")]);
    fx.edit(&s2, "tool", &[("deps.lock", b"v2\n"), ("gen/out.json", b"{\"b\":2}\n")]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let a = batch(&s1, "tool", c1.id, &["deps.lock", "gen/out.json", "plain.txt"]);
    let bb = batch(&s2, "tool", c2.id, &["deps.lock", "gen/out.json"]);
    let st = run(&fx, &group(vec![a.clone(), bb.clone()]), &base).unwrap();
    let t = st.tree.as_str();
    assert_eq!(cat(&fx, t, "deps.lock"), b"v2\n");
    assert_eq!(cat(&fx, t, "gen/out.json"), b"{\"b\":2}\n");
    assert_eq!(cat(&fx, t, "plain.txt"), b"p1\n");
    fs::remove_dir_all(st.layout_root()).unwrap();
    let st = run(&fx, &group(vec![bb, a]), &base).unwrap();
    assert_eq!(cat(&fx, st.tree.as_str(), "deps.lock"), b"v1\n");
    fs::remove_dir_all(st.layout_root()).unwrap();
    // Only one side changed: that side, nothing decided.
    let s3 = fx.new_session(&["tool"]);
    fx.edit(&s3, "tool", &[("plain.txt", b"p3\n")]);
    let c3 = cp(&fx, &s3);
    let g = group(vec![batch(&s1, "tool", c1.id, &["deps.lock"]), batch(&s3, "tool", c3.id, &["plain.txt"])]);
    let st = run(&fx, &g, &base).unwrap();
    assert_eq!(cat(&fx, st.tree.as_str(), "deps.lock"), b"v1\n");
}

#[test]
fn lww_later_checkpoint_wins_within_session() {
    let fx = Fx::with(&["tool"]);
    let base = lww_fixture(&fx);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("deps.lock", b"first\n")]);
    let c1 = cp(&fx, &s);
    fx.edit(&s, "tool", &[("deps.lock", b"second\n")]);
    let c2 = cp(&fx, &s);
    assert!(c2.id > c1.id);
    let g = group(vec![batch(&s, "tool", c2.id, &["deps.lock"]), batch(&s, "tool", c1.id, &["deps.lock"])]);
    let st = run(&fx, &g, &base).unwrap();
    assert_eq!(cat(&fx, st.tree.as_str(), "deps.lock"), b"second\n");
}

#[test]
fn deps_dirs_are_not_merge_units() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("README", b"r\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("node_modules/x/index.js", b"dep\n"), ("src/index.ts", b"export const a = 1\n")]);
    let c = cp(&fx, &s);
    assert!(c.entries.iter().all(|e| !e.path.as_str().starts_with("node_modules")));
    let st = run(&fx, &one(&s, &c, &["src/index.ts"]), &base).unwrap();
    assert!(paths(&fx, st.tree.as_str()).iter().all(|p| !p.starts_with("node_modules")));
    let e = run(&fx, &one(&s, &c, &["node_modules"]), &base).unwrap_err();
    assert_eq!(e.code, Code::NoMatch, "{e:?}");
}

#[test]
fn type_transition_pair_required() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("tp/a.txt", b"a\n"), ("tp/b.txt", b"b\n")]);
    let s = fx.new_session(&["tool"]);
    rm(&fx, &s, "tp");
    fx.edit(&s, "tool", &[("tp", b"file now\n")]);
    let c = cp(&fx, &s);
    let e = run(&fx, &one(&s, &c, &["tp"]), &base).unwrap_err();
    assert_eq!(e.code, Code::Conflict);
    assert_eq!(reason(&e), Some("type_pair"));
    assert!(layouts(&fx).is_empty());
    let st = run(&fx, &one(&s, &c, &["tp", "tp/a.txt", "tp/b.txt"]), &base).unwrap();
    let t = st.tree.as_str();
    assert_eq!(cat(&fx, t, "tp"), b"file now\n");
    assert!(!has(&fx, t, "tp/a.txt") && !has(&fx, t, "tp/b.txt"));
}

#[test]
fn file_to_symlink_is_one_modified() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("l.txt", b"was a file\n"), ("target.txt", b"t\n")]);
    let s = fx.new_session(&["tool"]);
    rm(&fx, &s, "l.txt");
    std::os::unix::fs::symlink("target.txt", ws(&fx, &s).join("l.txt")).unwrap();
    let c = cp(&fx, &s);
    let e: Vec<_> = c.entries.iter().filter(|e| e.path.as_str() == "l.txt").collect();
    assert_eq!(e.len(), 1);
    assert!(matches!(e[0].kind, EntryKind::Modified { .. }), "{:?}", e[0]);
    assert_eq!(e[0].mode, 0o120000);
    let st = run(&fx, &one(&s, &c, &["l.txt"]), &base).unwrap();
    assert_eq!(mode(&fx, st.tree.as_str(), "l.txt"), "120000");
    assert_eq!(cat(&fx, st.tree.as_str(), "l.txt"), b"target.txt");
}

#[test]
fn untracked_publishes_as_added() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("README", b"r\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("new.txt", b"brand new\n")]);
    let c = cp(&fx, &s);
    let e = c.entries.iter().find(|e| e.path.as_str() == "new.txt").unwrap();
    let EntryKind::Untracked { blob } = &e.kind else { panic!("{e:?}") };
    let st = run(&fx, &one(&s, &c, &["new.txt"]), &base).unwrap();
    let untracked_tree = st.tree.clone();
    fs::remove_dir_all(st.layout_root()).unwrap();
    // The same entry as `Added` gives the same tree.
    let mut added = c.clone();
    for x in &mut added.entries {
        if x.path.as_str() == "new.txt" {
            x.kind = EntryKind::Added { blob: blob.clone() };
        }
    }
    let id = fx.store().record_checkpoint(added).unwrap();
    let st = run(&fx, &group(vec![batch(&s, "tool", id, &["new.txt"])]), &base).unwrap();
    assert_eq!(st.tree, untracked_tree);
    assert_eq!(cat(&fx, st.tree.as_str(), "new.txt"), b"brand new\n");
}

#[test]
fn empty_directory_absent_from_tree() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("a.txt", b"a\n")]);
    let s = fx.new_session(&["tool"]);
    fs::create_dir(ws(&fx, &s).join("emptyd")).unwrap();
    fx.edit(&s, "tool", &[("a.txt", b"a2\n")]);
    let c = cp(&fx, &s);
    let e = run(&fx, &one(&s, &c, &["emptyd"]), &base).unwrap_err();
    assert_eq!(e.code, Code::NoMatch);
    let st = run(&fx, &one(&s, &c, &["a.txt"]), &base).unwrap();
    assert!(paths(&fx, st.tree.as_str()).iter().all(|p| !p.starts_with("emptyd")));
    assert!(ws(&fx, &s).join("emptyd").is_dir());
}

#[test]
fn binary_and_oversize_conflict_without_byte_diff() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("bin.dat", b"\0base\n"), ("big.txt", b"small\n")]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    let big = vec![b'a'; (strata_overlay::MERGE_MAX_BYTES + 1) as usize];
    fx.edit(&s1, "tool", &[("bin.dat", b"\0one\n"), ("big.txt", &big)]);
    fx.edit(&s2, "tool", &[("bin.dat", b"\0two\n"), ("big.txt", b"small edit\n")]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![
        batch(&s1, "tool", c1.id, &["bin.dat", "big.txt"]),
        batch(&s2, "tool", c2.id, &["bin.dat", "big.txt"]),
    ]);
    let e = run(&fx, &g, &base).unwrap_err();
    assert_eq!(kinds(&e), vec![("big.txt".into(), "too_large".into()), ("bin.dat".into(), "binary".into())]);
    for c in conflicts(&e) {
        let h = c["hunks"].as_array().unwrap();
        assert_eq!(h.len(), 1, "{c}");
        assert_eq!(h[0]["whole_file"], true);
        assert_eq!(h[0]["ours"], "");
    }
}

#[test]
fn hunk_text_cap_keeps_small_hunk_exact() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("f.txt", lines(30).as_bytes())]);
    let s1 = fx.new_session(&["tool"]);
    let s2 = fx.new_session(&["tool"]);
    let xs = "x".repeat(5000);
    let ys = "é".repeat(3000);
    let mk = |small: &str, large: &str| -> String {
        (1..=30)
            .map(|i| match i {
                3 => format!("{small}\n"),
                20 => format!("{large}\n"),
                _ => format!("line {i}\n"),
            })
            .collect()
    };
    fx.edit(&s1, "tool", &[("f.txt", mk("A", &xs).as_bytes())]);
    fx.edit(&s2, "tool", &[("f.txt", mk("B", &ys).as_bytes())]);
    let (c1, c2) = (cp(&fx, &s1), cp(&fx, &s2));
    let g = group(vec![batch(&s1, "tool", c1.id, &["f.txt"]), batch(&s2, "tool", c2.id, &["f.txt"])]);
    let e = run(&fx, &g, &base).unwrap_err();
    let cs = conflicts(&e);
    let hunks = cs[0]["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 2, "{hunks:?}");
    assert_eq!(hunks[0]["ours"], "B\n");
    assert_eq!(hunks[0]["theirs"], "A\n");
    assert_eq!(hunks[0]["truncated"], false);
    assert_eq!(hunks[1]["truncated"], true);
    for side in ["ours", "theirs", "base"] {
        assert!(hunks[1][side].as_str().unwrap().len() <= 4096);
    }
}

#[test]
fn no_commit_when_identical_to_base() {
    let fx = Fx::with(&["tool"]);
    seed(&fx, &[("a.txt", b"a\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("a.txt", b"landed\n")]);
    let c = cp(&fx, &s);
    // The same bytes landed upstream: B already is the result.
    fx.land_on_origin("tool", "main", &[("a.txt", b"landed\n")], "same");
    let base = b(&fx);
    let e = run(&fx, &one(&s, &c, &["a.txt"]), &base).unwrap_err();
    assert_eq!(e.code, Code::NoCommit, "{e:?}");
    assert!(layouts(&fx).is_empty());
    assert_eq!(fx.read_ref("tool", &strata_publish::chain_ref("main")), None);
}

#[test]
fn special_mode_bits_never_written() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("suid", b"s\n"), ("sgid", b"g\n"), ("d/f", b"f\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("suid", b"s2\n"), ("sgid", b"g2\n"), ("d/f", b"f2\n")]);
    chmod(&fx, &s, "suid", 0o4755);
    chmod(&fx, &s, "sgid", 0o2644);
    chmod(&fx, &s, "d", 0o1777);
    let c = cp(&fx, &s);
    let st = run(&fx, &one(&s, &c, &["suid", "sgid", "d/f"]), &base).unwrap();
    let t = st.tree.as_str();
    assert_eq!(mode(&fx, t, "suid"), "100755");
    assert_eq!(mode(&fx, t, "sgid"), "100644");
    for (m, _) in ls(&fx, t) {
        assert!(m == "100644" || m == "100755" || m == "120000", "{m}");
    }
    for n in ["suid", "sgid", "d/f"] {
        assert_eq!(fs::symlink_metadata(st.worktree.join(n)).unwrap().mode() & 0o7000, 0, "{n}");
    }
}

#[test]
fn staging_refuses_to_follow_a_symlink() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("a.txt", b"a\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("a.txt", b"a2\n")]);
    let c = cp(&fx, &s);
    let sroot = fx.store().session(&s).unwrap().unwrap().manifest.root;
    let g = one(&s, &c, &["a.txt"]);

    // The layout root itself is a symlink.
    let elsewhere = fx.sb.root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let layout = sroot.join(".strata-publish-tool-0");
    std::os::unix::fs::symlink(&elsewhere, &layout).unwrap();
    let before = fs::metadata(&elsewhere).unwrap().mtime_nsec();
    fx.git_of("tool").take_git_calls();
    let e = run(&fx, &g, &base).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(fs::metadata(&elsewhere).unwrap().mtime_nsec(), before);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    fs::remove_file(&layout).unwrap();

    // A parent (the session root) is a symlink.
    let moved = fx.sb.root.join("moved-session");
    fs::rename(&sroot, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &sroot).unwrap();
    let before = fs::metadata(&moved).unwrap().mtime_nsec();
    let e = run(&fx, &g, &base).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(fs::metadata(&moved).unwrap().mtime_nsec(), before);
    assert!(!moved.join(".strata-publish-tool-0").exists());
    for c in fx.git_of("tool").take_git_calls() {
        assert!(!c.args.iter().any(|a| a == "commit-tree" || a == "write-tree"), "{:?}", c.args);
    }
    fs::remove_file(&sroot).unwrap();
    fs::rename(&moved, &sroot).unwrap();
}

#[test]
fn stage_goes_through_gitio_stage() {
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("a.txt", b"a\n"), ("b.txt", b"b\n")]);
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", &[("a.txt", b"a2\n")]);
    rm(&fx, &s, "b.txt");
    let c = cp(&fx, &s);
    fx.git_of("tool").take_git_calls();
    let st = run(&fx, &one(&s, &c, &["a.txt", "b.txt"]), &base).unwrap();
    let calls = fx.git_of("tool").take_git_calls();
    let args: Vec<Vec<String>> =
        calls.iter().map(|c| c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()).collect();
    assert!(args.iter().all(|a| !a.iter().any(|x| x.starts_with("--index-output"))), "{args:?}");
    let writers: Vec<&Vec<String>> =
        args.iter().filter(|a| a.iter().any(|x| x == "update-index" || x == "read-tree")).collect();
    assert!(!writers.is_empty());
    // Every index write is on the staging git dir.
    for c in &calls {
        let a: Vec<String> = c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        if a.iter().any(|x| x == "update-index" || x == "read-tree") {
            // gitio's export uses a private index; attach/stage use the staging git dir.
            let on_staging = c.env.keys().any(|k| k == "GIT_INDEX_FILE")
                || c.env.get(std::ffi::OsStr::new("GIT_DIR")).is_some_and(|v| Path::new(v) == st.gitdir)
                || a.iter().any(|x| x.contains(&*st.gitdir.to_string_lossy()));
            assert!(on_staging, "{a:?} {:?}", c.cwd);
        }
    }
    assert_eq!(st.paths, rps(&["a.txt", "b.txt"]));
    assert!(!has(&fx, st.tree.as_str(), "b.txt"));
}

#[test]
fn non_utf8_link_target_refused_before_commit() {
    use std::os::unix::ffi::OsStrExt;
    let fx = Fx::with(&["tool"]);
    let base = seed(&fx, &[("a.txt", b"a\n")]);
    let s = fx.new_session(&["tool"]);
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(b"bad\xff\xfe"), ws(&fx, &s).join("link")).unwrap();
    let c = cp(&fx, &s);
    fx.git_of("tool").take_git_calls();
    let e = run(&fx, &one(&s, &c, &["link"]), &base).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(reason(&e), Some("link_target"));
    assert!(layouts(&fx).is_empty());
    for c in fx.git_of("tool").take_git_calls() {
        assert!(!c.args.iter().any(|a| a == "commit-tree"));
    }
}

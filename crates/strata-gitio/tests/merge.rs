//! Task 10: `merge3` and `is_binary` (§5.12).

mod common;

use std::fs;
use std::path::PathBuf;

use common::*;
use strata_gitio::{Hunk, Merge3, is_binary, merge3};

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// `base` with 1-based line `i` replaced by `with` (`None` deletes it).
fn edit(base: &str, edits: &[(usize, Option<&str>)]) -> String {
    let mut out = String::new();
    for (i, l) in base.split_inclusive('\n').enumerate() {
        match edits.iter().find(|(n, _)| *n == i + 1) {
            Some((_, Some(w))) => out.push_str(w),
            Some((_, None)) => {}
            None => out.push_str(l),
        }
    }
    out
}

fn clean(m: Merge3) -> Vec<u8> {
    match m {
        Merge3::Clean(b) => b,
        other => panic!("expected Clean, got {other:?}"),
    }
}

fn hunks(m: Merge3) -> Vec<Hunk> {
    match m {
        Merge3::Conflict(h) => h,
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn merge3_nonoverlapping_hunks_clean() {
    let base = lines(10);
    // Exactly one unchanged base line (line 4) between the changes.
    let ours = edit(&base, &[(3, Some("ours 3\n"))]);
    let theirs = edit(&base, &[(5, Some("theirs 5\n"))]);
    let want = edit(&base, &[(3, Some("ours 3\n")), (5, Some("theirs 5\n"))]);
    assert_eq!(clean(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())), want.as_bytes());
    // Far apart, with insertions and deletions.
    let ours = edit(&base, &[(1, Some("a\nb\nline 1\n")), (2, None)]);
    let theirs = edit(&base, &[(8, None), (9, Some("nine\n"))]);
    let want = edit(&base, &[(1, Some("a\nb\nline 1\n")), (2, None), (8, None), (9, Some("nine\n"))]);
    assert_eq!(clean(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())), want.as_bytes());
}

#[test]
fn merge3_same_hunk_conflict() {
    let base = lines(10);
    let ours = edit(&base, &[(4, Some("ours 4\n"))]);
    let theirs = edit(&base, &[(4, Some("theirs 4a\ntheirs 4b\n"))]);
    let h = hunks(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()));
    assert_eq!(
        h,
        vec![Hunk {
            base_start: 4,
            ours_start: 4,
            theirs_start: 4,
            base: b"line 4\n".to_vec(),
            ours: b"ours 4\n".to_vec(),
            theirs: b"theirs 4a\ntheirs 4b\n".to_vec(),
        }]
    );
    // A clean change before the conflict shifts the side line numbers.
    let ours = edit(&base, &[(1, Some("x\ny\nline 1\n")), (6, Some("ours 6\n"))]);
    let theirs = edit(&base, &[(2, None), (6, Some("theirs 6\n"))]);
    let h = hunks(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()));
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].base_start, h[0].ours_start, h[0].theirs_start), (6, 8, 5));
    assert_eq!(h[0].base, b"line 6\n");
}

#[test]
fn merge3_adjacent_changes_conflict() {
    let base = lines(10);
    let ours = edit(&base, &[(4, Some("ours 4\n"))]);
    let theirs = edit(&base, &[(5, Some("theirs 5\n"))]);
    let h = hunks(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()));
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].base, b"line 4\nline 5\n");
    assert_eq!(h[0].ours, b"ours 4\nline 5\n");
    assert_eq!(h[0].theirs, b"line 4\ntheirs 5\n");
    assert_eq!((h[0].base_start, h[0].ours_start, h[0].theirs_start), (4, 4, 4));
    // Different lines appended at EOF: empty base section, start is the line after.
    let ours = format!("{base}ours tail\n");
    let theirs = format!("{base}theirs tail\n");
    let h = hunks(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()));
    assert_eq!(
        h,
        vec![Hunk {
            base_start: 11,
            ours_start: 11,
            theirs_start: 11,
            base: vec![],
            ours: b"ours tail\n".to_vec(),
            theirs: b"theirs tail\n".to_vec(),
        }]
    );
}

#[test]
fn merge3_identical_or_one_sided_changes_clean() {
    let base = lines(10);
    let both = edit(&base, &[(4, Some("both 4\n"))]);
    assert_eq!(clean(merge3(base.as_bytes(), both.as_bytes(), both.as_bytes())), both.as_bytes());
    // Same change plus a change on each side elsewhere: one copy of the shared change.
    let ours = edit(&base, &[(4, Some("both 4\n")), (9, Some("ours 9\n"))]);
    let theirs = edit(&base, &[(4, Some("both 4\n")), (1, Some("theirs 1\n"))]);
    let want = edit(&base, &[(1, Some("theirs 1\n")), (4, Some("both 4\n")), (9, Some("ours 9\n"))]);
    assert_eq!(clean(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())), want.as_bytes());
    // One-sided.
    let ours = edit(&base, &[(2, Some("ours 2\n")), (7, None)]);
    assert_eq!(clean(merge3(base.as_bytes(), ours.as_bytes(), base.as_bytes())), ours.as_bytes());
    assert_eq!(clean(merge3(base.as_bytes(), base.as_bytes(), ours.as_bytes())), ours.as_bytes());
    // add/add with an empty base.
    assert_eq!(clean(merge3(b"", b"new\nfile\n", b"new\nfile\n")), b"new\nfile\n");
    let h = hunks(merge3(b"", b"ours\n", b"theirs\n"));
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].base_start, h[0].ours_start, h[0].theirs_start), (1, 1, 1));
}

#[test]
fn merge3_preserves_line_endings() {
    let base = lines(10).replace('\n', "\r\n");
    let ours = edit(&base, &[(2, Some("ours 2\r\n"))]);
    let theirs = edit(&base, &[(8, Some("theirs 8\r\n"))]);
    let want = edit(&base, &[(2, Some("ours 2\r\n")), (8, Some("theirs 8\r\n"))]);
    assert_eq!(clean(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())), want.as_bytes());
    // No newline at the end of the last line.
    let base = lines(10);
    let base = &base[..base.len() - 1];
    let ours = edit(base, &[(2, Some("ours 2\n"))]);
    let theirs = edit(base, &[(10, Some("theirs 10"))]);
    let want = edit(base, &[(2, Some("ours 2\n")), (10, Some("theirs 10"))]);
    assert_eq!(clean(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())), want.as_bytes());
    // Adding a final newline is a change of the last line.
    let theirs = format!("{base}\n");
    let ours = edit(base, &[(10, Some("ours 10"))]);
    assert!(matches!(merge3(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()), Merge3::Conflict(_)));
}

#[test]
fn merge3_binary_is_whole_file() {
    let base = b"a\nb\0c\nd\n".to_vec();
    let ours = b"a\nB\0c\nd\n".to_vec();
    let theirs = b"a\nb\0c\nD\n".to_vec();
    let h = hunks(merge3(&base, &ours, &theirs));
    assert_eq!(
        h,
        vec![Hunk { base_start: 1, ours_start: 1, theirs_start: 1, base: base.clone(), ours: ours.clone(), theirs: theirs.clone() }]
    );
    // Binary only on one side still makes the whole file one hunk.
    let h = hunks(merge3(b"x\ny\n", b"x\0\ny\n", b"x\ny\nz\n"));
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].theirs, b"x\ny\nz\n");
    // One-sided: Clean, that side.
    assert_eq!(clean(merge3(&base, &ours, &base)), ours);
    assert_eq!(clean(merge3(&base, &base, &theirs)), theirs);
    assert_eq!(clean(merge3(&base, &ours, &ours)), ours);
}

#[test]
fn merge3_agrees_with_git_merge_file_on_corpus() {
    let sb = Sandbox::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/merge-corpus");
    let mut cases: Vec<_> = fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).collect();
    cases.sort();
    assert!(cases.len() >= 20, "corpus too small: {}", cases.len());
    let mut conflicts = 0;
    for case in &cases {
        let read = |f: &str| fs::read(case.join(f)).unwrap();
        let (base, ours, theirs) = (read("base"), read("ours"), read("theirs"));
        let out = sb
            .cmd(case)
            .args(["merge-file", "-p", "ours", "base", "theirs"])
            .output()
            .unwrap();
        let code = out.status.code().expect("git merge-file exited by signal");
        assert!((0..127).contains(&code), "{}: git merge-file failed: {code}", case.display());
        match merge3(&base, &ours, &theirs) {
            Merge3::Clean(bytes) => {
                assert_eq!(code, 0, "{}: merge3 is Clean but git found {code} conflicts", case.display());
                assert_eq!(bytes, out.stdout, "{}: clean bytes differ from git", case.display());
            }
            Merge3::Conflict(h) => {
                conflicts += 1;
                assert!(code > 0, "{}: merge3 found {} hunks but git is clean", case.display(), h.len());
            }
        }
    }
    assert!(conflicts > 0 && conflicts < cases.len());
}

#[test]
fn is_binary_matches_git_rule() {
    assert!(!is_binary(b""));
    assert!(!is_binary(b"plain text\r\n"));
    assert!(is_binary(b"\0"));
    let mut v = vec![b'a'; 8000];
    v[7999] = 0;
    assert!(is_binary(&v));
    let mut v = vec![b'a'; 8001];
    v[8000] = 0;
    assert!(!is_binary(&v));
}

/// Random edits of a file of distinct lines (so every line diff is unique)
/// against `git merge-file -p`. Slow: one git process per case.
#[test]
#[ignore = "slow: spawns git merge-file a few hundred times"]
fn merge3_fuzz_against_git_merge_file() {
    let sb = Sandbox::new();
    let dir = sb.path("fuzz");
    fs::create_dir_all(&dir).unwrap();
    let mut seed: u64 = 0x2545f4914f6cdd1d;
    let mut rnd = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let mut fresh = 0u32;
    let mut mutate = |base: &[String], rnd: &mut dyn FnMut(u64) -> u64| -> Vec<String> {
        let mut out = Vec::new();
        for l in base {
            match rnd(10) {
                0 => {}
                1 => {
                    fresh += 1;
                    out.push(format!("new {fresh}\n"));
                }
                2 => {
                    fresh += 1;
                    out.push(l.clone());
                    out.push(format!("new {fresh}\n"));
                }
                _ => out.push(l.clone()),
            }
        }
        if rnd(4) == 0 {
            fresh += 1;
            out.push(format!("new {fresh}\n"));
        }
        out
    };
    for case in 0..400 {
        let n = rnd(12) as usize;
        let base: Vec<String> = (0..n).map(|i| format!("line {i}\n")).collect();
        let ours = mutate(&base, &mut rnd);
        let theirs = if rnd(5) == 0 { ours.clone() } else { mutate(&base, &mut rnd) };
        let (b, o, t) = (base.concat(), ours.concat(), theirs.concat());
        fs::write(dir.join("base"), &b).unwrap();
        fs::write(dir.join("ours"), &o).unwrap();
        fs::write(dir.join("theirs"), &t).unwrap();
        let out = sb.cmd(&dir).args(["merge-file", "-p", "ours", "base", "theirs"]).output().unwrap();
        let code = out.status.code().unwrap();
        match merge3(b.as_bytes(), o.as_bytes(), t.as_bytes()) {
            Merge3::Clean(bytes) => {
                assert_eq!(code, 0, "case {case}: git conflicts\nbase:\n{b}\nours:\n{o}\ntheirs:\n{t}");
                assert_eq!(bytes, out.stdout, "case {case}: bytes differ");
            }
            Merge3::Conflict(_) => {
                assert!(code > 0, "case {case}: git is clean\nbase:\n{b}\nours:\n{o}\ntheirs:\n{t}");
            }
        }
    }
}

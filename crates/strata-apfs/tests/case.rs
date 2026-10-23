//! `case_fold_eq` / `case_fold_key` against real volumes (§7, §8 Task 8).
mod common;

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use common::*;
use strata_apfs::{case_fold_eq, case_fold_key, volume_caps};
use unicode_normalization::UnicodeNormalization;

/// Every §4.2 B category, plus the Unicode 16 and 17 additions.
const TABLE: &[(&str, &str)] = &[
    ("a", "A"),
    ("README.md", "readme.MD"),
    ("a", "b"),
    ("\u{df}", "SS"),
    ("\u{df}", "ss"),
    ("\u{1e9e}", "ss"),
    ("\u{fb03}", "ffi"),
    ("\u{fb03}", "FFI"),
    ("\u{130}", "i\u{307}"),
    ("\u{130}", "i"),
    ("\u{131}", "i"),
    ("\u{131}", "I"),
    ("\u{3c2}", "\u{3c3}"),
    ("\u{3c3}", "\u{3a3}"),
    ("\u{212a}", "k"),
    ("\u{212b}", "\u{e5}"),
    ("\u{212b}", "a\u{30a}"),
    ("\u{13a0}", "\u{ab70}"),
    ("\u{10a0}", "\u{2d00}"),
    ("\u{1c90}", "\u{10d0}"),
    ("\u{10400}", "\u{10428}"),
    ("\u{10d50}", "\u{10d70}"),
    ("\u{1c89}", "\u{1c8a}"),
    ("\u{a7cb}", "\u{264}"),
    ("\u{a7dc}", "\u{19b}"),
    ("\u{a7ce}", "\u{a7cf}"),
    ("\u{a7d2}", "\u{a7d3}"),
    ("\u{16ea0}", "\u{16ebb}"),
    ("\u{e9}", "e\u{301}"),
    ("\u{c9}", "e\u{301}"),
    ("\u{1e69}", "S\u{323}\u{307}"),
    ("a\u{200d}b", "ab"),
    ("a\u{200c}b", "ab"),
    ("a\u{ad}b", "ab"),
    ("\u{2764}\u{fe0f}", "\u{2764}"),
    ("\u{ff21}", "a"),
    ("\u{ff21}", "\u{ff41}"),
    ("\u{1f0}", "J\u{30c}"),
    ("\u{390}", "\u{3b9}\u{308}\u{301}"),
    ("\u{345}", "\u{3b9}"),
    ("\u{1f80}", "\u{1f00}\u{3b9}"),
    ("\u{1f88}", "\u{1f80}"),
    ("\u{587}", "\u{565}\u{582}"),
    ("\u{1e900}", "\u{1e922}"),
    ("\u{2c00}", "\u{2c30}"),
];

const RANGES: &[(u32, u32)] = &[
    (0x41, 0x24f),
    (0x250, 0x2af),
    (0x370, 0x58f),
    (0x10a0, 0x10ff),
    (0x13a0, 0x13ff),
    (0x1c80, 0x1cbf),
    (0x1e00, 0x1fff),
    (0x2100, 0x218f),
    (0x24b6, 0x24e9),
    (0x2c00, 0x2d2f),
    (0xa640, 0xa6ff),
    (0xa720, 0xa7ff),
    (0xab70, 0xabbf),
    (0xfb00, 0xfb17),
    (0xff21, 0xff5a),
    (0x10400, 0x104ff),
    (0x10570, 0x105bf),
    (0x10c80, 0x10cff),
    (0x10d50, 0x10d8f),
    (0x118a0, 0x118df),
    (0x16e40, 0x16e7f),
    (0x16ea0, 0x16edf),
    (0x1e900, 0x1e94b),
];

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n
    }
    fn char_in(&mut self, (lo, hi): (u32, u32)) -> char {
        loop {
            if let Some(c) = char::from_u32(lo + self.below((hi - lo + 1) as u64) as u32) {
                return c;
            }
        }
    }
}

fn cps(s: &str) -> String {
    s.chars().map(|c| format!("U+{:04X}", c as u32)).collect::<Vec<_>>().join(" ")
}

fn sampled(seed: u64, n: usize) -> Vec<(String, String)> {
    let mut r = Rng(seed);
    let mut out = Vec::new();
    while out.len() < n {
        let range = RANGES[r.below(RANGES.len() as u64) as usize];
        let c = r.char_in(range);
        let a = c.to_string();
        let b = match r.below(4) {
            0 => c.to_uppercase().collect(),
            1 => c.to_lowercase().collect(),
            2 => r.char_in(range).to_string(),
            _ => a.nfd().collect::<String>().to_uppercase(),
        };
        out.push((a, b));
    }
    out
}

/// Does the filesystem at `dir` treat `b` as naming the file created as `a`?
/// `None` when APFS refuses a name (`EILSEQ`: it rejects unassigned code points).
fn same_on_disk(dir: &Path, a: &str, b: &str) -> Option<bool> {
    let pa = dir.join(format!("n{a}"));
    match fs::write(&pa, "") {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EILSEQ) => return None,
        Err(e) => panic!("create [{}]: {e}", cps(a)),
    }
    let same = match fs::symlink_metadata(dir.join(format!("n{b}"))) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Some(false),
        Err(e) if e.raw_os_error() == Some(libc::EILSEQ) => None,
        Err(e) => panic!("stat [{}]: {e}", cps(b)),
    };
    fs::remove_file(&pa).unwrap();
    same
}

#[test]
fn case_fold_eq_matches_a_case_insensitive_volume() {
    let img = Image::create(Fs::Apfs, 64);
    assert!(!volume_caps(img.path()).unwrap().case_sensitive);
    let dir = img.path().join("oracle");
    fs::create_dir(&dir).unwrap();
    let mut all: Vec<(String, String)> = TABLE.iter().map(|&(a, b)| (a.into(), b.into())).collect();
    all.extend(sampled(0x0a9f_5eed, 4000));
    let mut bad = Vec::new();
    let mut same_count = 0;
    let mut refused = 0;
    for (i, (a, b)) in all.iter().enumerate() {
        let Some(disk) = same_on_disk(&dir, a, b) else {
            assert!(i >= TABLE.len(), "table pair refused: [{}] vs [{}]", cps(a), cps(b));
            refused += 1;
            continue;
        };
        same_count += disk as usize;
        let eq = case_fold_eq(a, b);
        let key = case_fold_key(a) == case_fold_key(b);
        if eq != disk || key != disk {
            bad.push(format!("[{}] vs [{}]: disk {disk}, eq {eq}, key {key}", cps(a), cps(b)));
        }
    }
    assert!(bad.is_empty(), "{} disagreements:\n{}", bad.len(), bad.join("\n"));
    assert!(same_count > 1000, "{same_count}");
    assert!(refused < all.len() / 4, "too many unassigned samples: {refused}");
}

#[test]
fn case_sensitive_volume_distinguishes_case_but_not_normalization() {
    let img = Image::create(Fs::ApfsCaseSensitive, 64);
    assert!(volume_caps(img.path()).unwrap().case_sensitive);
    let d = img.path();
    fs::write(d.join("a"), "lower").unwrap();
    fs::write(d.join("A"), "upper").unwrap();
    assert_eq!(fs::read_to_string(d.join("a")).unwrap(), "lower");
    assert_eq!(fs::read_to_string(d.join("A")).unwrap(), "upper");
    fs::write(d.join("\u{e9}"), "nfc").unwrap();
    assert_eq!(fs::read_to_string(d.join("e\u{301}")).unwrap(), "nfc");
    let e = fs::OpenOptions::new().write(true).create_new(true).open(d.join("e\u{301}")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EEXIST));
    // case_fold_eq answers the case-insensitive question; callers must not ask it here.
    assert!(case_fold_eq("a", "A"));
}

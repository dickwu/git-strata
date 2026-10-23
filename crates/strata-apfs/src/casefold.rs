//! `case_fold_key` and `case_fold_eq` (§5.8): Unicode canonical caseless
//! matching (D145), `NFD(full_case_fold(NFD(s)))`, C + F mappings, no Turkic.

use unicode_normalization::UnicodeNormalization;

use crate::casefold_table::FOLD;

fn fold_char(c: char, out: &mut String) {
    match FOLD.binary_search_by_key(&c, |&(k, _)| k) {
        Ok(i) => out.push_str(FOLD[i].1),
        Err(_) => out.push(c),
    }
}

/// The fold key of a name (or a whole `/`-joined path) on a case-insensitive
/// APFS volume. In memory only: it is not NFC and changes with the table.
pub fn case_fold_key(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut folded = String::with_capacity(s.len());
    for c in s.nfd() {
        fold_char(c, &mut folded);
    }
    folded.nfd().collect()
}

/// Whether two names are the same name on a case-insensitive APFS volume.
pub fn case_fold_eq(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    case_fold_key(a) == case_fold_key(b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::casefold_table::UNICODE_VERSION;
    use std::collections::HashSet;

    /// (a, b, same) — every expectation checked by the oracle test on a real volume.
    pub(crate) const KNOWN: &[(&str, &str, bool)] = &[
        ("", "", true),
        ("a", "A", true),
        ("README.md", "readme.MD", true),
        ("a", "b", false),
        ("ab", "a", false),
        ("\u{df}", "SS", true),
        ("\u{df}", "ss", true),
        ("stra\u{df}e", "STRASSE", true),
        ("\u{1e9e}", "ss", true),
        ("\u{fb03}", "ffi", true),
        ("\u{fb03}", "FFI", true),
        ("\u{130}", "i\u{307}", true),
        ("\u{130}", "i", false),
        ("\u{131}", "i", false),
        ("\u{3c2}", "\u{3c3}", true),
        ("\u{3c3}", "\u{3a3}", true),
        ("\u{3c2}", "\u{3a3}", true),
        ("\u{212a}", "k", true),
        ("\u{212a}", "K", true),
        ("\u{212b}", "\u{e5}", true),
        ("\u{212b}", "a\u{30a}", true),
        ("\u{13a0}", "\u{ab70}", true),
        ("\u{10a0}", "\u{2d00}", true),
        ("\u{1c90}", "\u{10d0}", true),
        ("\u{10400}", "\u{10428}", true),
        ("\u{10d50}", "\u{10d70}", true),
        ("\u{1c89}", "\u{1c8a}", true),
        ("\u{a7cb}", "\u{264}", true),
        ("\u{a7dc}", "\u{19b}", true),
        ("\u{a7ce}", "\u{a7cf}", true),
        ("\u{16ea0}", "\u{16ebb}", true),
        ("\u{e9}", "e\u{301}", true),
        ("\u{c9}", "e\u{301}", true),
        ("\u{1e69}", "S\u{323}\u{307}", true),
        ("a\u{200d}b", "ab", false),
        ("a\u{200c}b", "ab", false),
        ("a\u{ad}b", "ab", false),
        ("\u{2764}\u{fe0f}", "\u{2764}", false),
        ("\u{ff21}", "a", false),
        ("\u{ff21}", "\u{ff41}", true),
        ("\u{1f0}", "J\u{30c}", true),
        ("\u{390}", "\u{3b9}\u{308}\u{301}", true),
        ("\u{345}", "\u{3b9}", true),
        ("\u{1f80}", "\u{1f00}\u{3b9}", true),
    ];

    fn cps(s: &str) -> String {
        s.chars().map(|c| format!("U+{:04X}", c as u32)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn case_fold_eq_known_pairs() {
        assert!(UNICODE_VERSION >= (16, 0, 0));
        for &(a, b, same) in KNOWN {
            assert_eq!(case_fold_eq(a, b), same, "[{}] vs [{}]", cps(a), cps(b));
            assert_eq!(case_fold_eq(b, a), same, "[{}] vs [{}]", cps(b), cps(a));
        }
    }

    /// xorshift64*, fixed seed.
    pub(crate) struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    const POOLS: &[(u32, u32)] = &[
        (0x41, 0x5a),
        (0x61, 0x7a),
        (0xc0, 0xff),
        (0x100, 0x17f),
        (0x1e00, 0x1eff),
        (0x370, 0x3ff),
        (0x1f00, 0x1fff),
        (0x400, 0x4ff),
        (0x10a0, 0x10ff),
        (0x1c90, 0x1cbf),
        (0x13a0, 0x13f5),
        (0xab70, 0xabbf),
        (0x10400, 0x1044f),
        (0x10d50, 0x10d85),
        (0x1c89, 0x1c8a),
        (0xa7c0, 0xa7dc),
        (0x300, 0x36f),
        (0xdf, 0xdf),
        (0xfb00, 0xfb06),
        (0x130, 0x131),
        (0x212a, 0x212b),
    ];

    fn rand_char(r: &mut Rng) -> char {
        loop {
            let (lo, hi) = POOLS[r.below(POOLS.len() as u64) as usize];
            if let Some(c) = char::from_u32(lo + r.below((hi - lo + 1) as u64) as u32) {
                return c;
            }
        }
    }

    pub(crate) fn rand_str(r: &mut Rng) -> String {
        let n = r.below(8) as usize;
        let s: String = (0..n).map(|_| rand_char(r)).collect();
        if r.below(2) == 0 { s.nfd().collect() } else { s.nfc().collect() }
    }

    fn case_variant(r: &mut Rng, s: &str) -> String {
        let v: String = s
            .chars()
            .map(|c| match r.below(3) {
                0 => c.to_uppercase().collect::<String>(),
                1 => c.to_lowercase().collect(),
                _ => c.to_string(),
            })
            .collect();
        if r.below(2) == 0 { v.nfd().collect() } else { v.nfc().collect() }
    }

    pub(crate) fn pairs(seed: u64, n: usize) -> Vec<(String, String)> {
        let mut r = Rng(seed);
        (0..n)
            .map(|i| {
                let a = rand_str(&mut r);
                let b = if i % 2 == 0 { case_variant(&mut r, &a) } else { rand_str(&mut r) };
                (a, b)
            })
            .collect()
    }

    #[test]
    fn case_fold_key_agrees_with_case_fold_eq() {
        let mut all: Vec<(String, String)> = KNOWN.iter().map(|&(a, b, _)| (a.into(), b.into())).collect();
        all.extend(pairs(0x5eed_cafe, 10_000));
        let mut equal = 0;
        for (a, b) in &all {
            let eq = case_fold_eq(a, b);
            equal += eq as usize;
            assert_eq!(eq, case_fold_key(a) == case_fold_key(b), "[{}] vs [{}]", cps(a), cps(b));
        }
        assert!(equal > 4000, "the case-variant half should mostly match: {equal}");
        for s in ["", "abc", "ReadMe.TXT", "A-Z_0/9"] {
            assert_eq!(case_fold_key(s), s.to_ascii_lowercase());
        }
        assert_eq!(case_fold_key("\u{df}"), case_fold_key("SS"));
        assert_eq!(case_fold_key("\u{e9}"), "e\u{301}", "keys are NFD, not NFC");
        let set: HashSet<String> = [".env", ".ENV", ".Env", "Stra\u{df}e", "STRASSE", "x"].iter().map(|s| case_fold_key(s)).collect();
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn case_fold_key_of_a_path_is_the_slash_joined_keys() {
        let fixed: &[(&str, &str)] = &[
            ("e", "\u{301}x"),
            ("", ""),
            ("", "a"),
            ("a", ""),
            ("/a", "b/"),
            ("a/", "/b"),
            ("\u{df}", "\u{130}"),
            ("\u{fb03}", "\u{10400}"),
            ("\u{13a0}", "\u{301}"),
            ("Dir", "File.TXT"),
        ];
        let mut all: Vec<(String, String)> = fixed.iter().map(|&(a, b)| (a.into(), b.into())).collect();
        let mut r = Rng(0xfeed_f00d);
        for (a, b) in pairs(0x0bad_5eed, 10_000) {
            let a = if r.below(10) == 0 { format!("{a}/") } else { a };
            let b = if r.below(10) == 0 { format!("/{b}") } else { b };
            all.push((a, b));
        }
        for (a, b) in &all {
            let whole = case_fold_key(&format!("{a}/{b}"));
            assert_eq!(whole, format!("{}/{}", case_fold_key(a), case_fold_key(b)), "[{}] / [{}]", cps(a), cps(b));
        }
        assert_eq!(case_fold_key("e/\u{301}x"), "e/\u{301}x");
        let keys: Vec<String> = ["Dir/x", "dir/y", "DIR/z/w", "dir2/x", "dirs"].iter().map(|s| case_fold_key(s)).collect();
        let prefix = case_fold_key("Dir") + "/";
        let hits: Vec<&String> = keys.iter().filter(|k| k.starts_with(&prefix)).collect();
        assert_eq!(hits, vec!["dir/x", "dir/y", "dir/z/w"]);
    }
}

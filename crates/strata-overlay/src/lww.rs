//! `.lww` rules (§5.10): the LayerFS `/.lww` glob dialect with the leading
//! `/` dropped, read from `.strata/lww` in a tree.

use std::io::Read;
use std::path::Path;

use strata_types::{Code, RepoPath};
use unicode_normalization::UnicodeNormalization;

use crate::{LWW_FILE, Result};

/// At most this many files are read, `.strata/lww` included (§5.10).
const MAX_FILES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `*`: any run of characters within one name.
    Star,
    /// `?`: one character within a name.
    One,
    /// `**/`: zero or more whole directories.
    Dirs,
    /// A trailing `/**`: one or more characters below the prefix.
    Below,
    /// Any other `**`: any run, `/` included.
    Deep,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    negate: bool,
    /// The pattern held a `/`: matched against the path from the repo root.
    /// Otherwise it is matched against every single name.
    anchored: bool,
    toks: Vec<Tok>,
}

/// The ordered rule list of one `.lww` text (or of `.strata/lww` and its includes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LwwRules {
    rules: Vec<Rule>,
}

enum Line {
    Rule(Rule),
    Include(String),
    Nothing,
}

fn parse_line(raw: &str) -> Line {
    let line = raw.trim_end_matches('\r').trim();
    if line.is_empty() || line.starts_with('#') {
        return Line::Nothing;
    }
    if let Some(rest) = line.strip_prefix("%include")
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        let p = rest.trim();
        return if p.is_empty() {
            Line::Nothing
        } else {
            Line::Include(p.to_string())
        };
    }
    let (negate, body) = match line.strip_prefix('!') {
        Some(b) => (true, b),
        None => (false, line),
    };
    let leading = body.starts_with('/');
    let body: String = body
        .trim_start_matches('/')
        .trim_end_matches('/')
        .nfc()
        .collect();
    if body.is_empty() {
        return Line::Nothing;
    }
    let anchored = leading || body.contains('/');
    Line::Rule(Rule {
        negate,
        anchored,
        toks: tokenize(&body),
    })
}

fn tokenize(s: &str) -> Vec<Tok> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '*' if c.get(i + 1) == Some(&'*') => {
                let at_start = i == 0 || c[i - 1] == '/';
                if at_start && c.get(i + 2) == Some(&'/') {
                    out.push(Tok::Dirs);
                    i += 3;
                } else if at_start && i > 0 && i + 2 == c.len() {
                    // `a/**`: drop the `/` already pushed, require something below.
                    out.pop();
                    out.push(Tok::Below);
                    i += 2;
                } else {
                    out.push(Tok::Deep);
                    i += 2;
                    while c.get(i) == Some(&'*') {
                        i += 1;
                    }
                }
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '?' => {
                out.push(Tok::One);
                i += 1;
            }
            '\\' if i + 1 < c.len() => {
                out.push(Tok::Lit(c[i + 1]));
                i += 2;
            }
            ch => {
                out.push(Tok::Lit(ch));
                i += 1;
            }
        }
    }
    out
}

/// Does `toks` match all of `t`?
fn glob(toks: &[Tok], t: &[char]) -> bool {
    let Some((first, rest)) = toks.split_first() else {
        return t.is_empty();
    };
    match first {
        Tok::Lit(ch) => t.first() == Some(ch) && glob(rest, &t[1..]),
        Tok::One => t.first().is_some_and(|c| *c != '/') && glob(rest, &t[1..]),
        Tok::Star => {
            for k in 0..=t.len() {
                if glob(rest, &t[k..]) {
                    return true;
                }
                if k < t.len() && t[k] == '/' {
                    break;
                }
            }
            false
        }
        Tok::Deep => (0..=t.len()).any(|k| glob(rest, &t[k..])),
        Tok::Dirs => {
            if glob(rest, t) {
                return true;
            }
            (0..t.len()).any(|k| t[k] == '/' && glob(rest, &t[k + 1..]))
        }
        Tok::Below => t.len() >= 2 && t[0] == '/' && rest.is_empty(),
    }
}

impl Rule {
    /// The pattern matches the path itself or any ancestor directory.
    fn matches(&self, path: &str) -> bool {
        if self.anchored {
            let c: Vec<char> = path.chars().collect();
            (0..=c.len())
                .filter(|&k| k == c.len() || c[k] == '/')
                .any(|k| glob(&self.toks, &c[..k]))
        } else {
            path.split('/')
                .any(|name| glob(&self.toks, &name.chars().collect::<Vec<_>>()))
        }
    }
}

impl LwwRules {
    /// Rules from one text; `%include` lines are ignored here (only `load`
    /// has a tree to read them from).
    pub fn parse(text: &str) -> LwwRules {
        let rules = text
            .lines()
            .filter_map(|l| match parse_line(l) {
                Line::Rule(r) => Some(r),
                _ => None,
            })
            .collect();
        LwwRules { rules }
    }

    /// Rules from `<tree>/.strata/lww` and its `%include`s (at most 16 files).
    /// A file that is missing, not a regular file or not UTF-8 reads as empty.
    pub fn load(tree: &Path) -> Result<LwwRules> {
        let mut rules = Vec::new();
        let mut budget = MAX_FILES;
        load_into(tree, LWW_FILE, &mut budget, &mut rules)?;
        Ok(LwwRules { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

fn load_into(tree: &Path, rel: &str, budget: &mut usize, out: &mut Vec<Rule>) -> Result<()> {
    if *budget == 0 {
        return Ok(());
    }
    *budget -= 1;
    let Some(text) = read_rules_file(tree, rel)? else {
        return Ok(());
    };
    for line in text.lines() {
        match parse_line(line) {
            Line::Rule(r) => out.push(r),
            Line::Include(p) => load_into(tree, p.trim_start_matches('/'), budget, out)?,
            Line::Nothing => {}
        }
    }
    Ok(())
}

fn read_rules_file(tree: &Path, rel: &str) -> Result<Option<String>> {
    let Ok(path) = RepoPath::try_from(rel.to_string()) else {
        return Ok(None);
    };
    let mut f = match strata_apfs::open_rooted_file(tree, &path) {
        Ok(f) => f,
        Err(e)
            if matches!(
                e.code,
                Code::NoSuchPath | Code::IsDirectory | Code::NotFile | Code::NotDirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(|e| {
        strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &tree.join(rel))
    })?;
    Ok(String::from_utf8(bytes).ok())
}

/// Whether `path` is matched by the rules: the last matching rule decides,
/// a negated one clears the match (§5.10). Pure.
pub fn lww_matches(patterns: &LwwRules, path: &RepoPath) -> bool {
    let p = path.as_str();
    let mut hit = false;
    for r in &patterns.rules {
        if r.matches(p) {
            hit = !r.negate;
        }
    }
    hit
}

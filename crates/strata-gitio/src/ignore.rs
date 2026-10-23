//! Git ignore rules (§5.8): a port of the pattern handling of git's
//! `dir.c` and of `wildmatch.c`, with ASCII-only case folding.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use strata_apfs::FileKind;
use strata_types::{Code, RepoPath, StrataError, TreeOid};

use crate::command::{Opts, QUERY_TIMEOUT, flag};
use crate::err::{errd, path_str};
use crate::fsx::{self, FileKey};
use crate::{Git, Result};

// ---------------------------------------------------------------------------
// patterns

const NEGATIVE: u8 = 1;
const MUSTBEDIR: u8 = 2;
const NODIR: u8 = 4;
const ENDSWITH: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    /// The pattern without its `!` and trailing `/`.
    pat: Vec<u8>,
    /// Length of the leading part without glob specials.
    nowild: usize,
    flags: u8,
}

/// The patterns of one rule file, in file order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rules {
    pats: Vec<Pattern>,
}

fn is_glob_special(c: u8) -> bool {
    matches!(c, b'*' | b'?' | b'[' | b'\\')
}

fn simple_length(p: &[u8]) -> usize {
    p.iter().position(|&c| is_glob_special(c)).unwrap_or(p.len())
}

/// git's `trim_trailing_spaces`: unescaped trailing spaces go.
fn trim_trailing_spaces(line: &[u8]) -> &[u8] {
    let mut last_space: Option<usize> = None;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b' ' => {
                if last_space.is_none() {
                    last_space = Some(i);
                }
            }
            b'\\' => {
                i += 1;
                if i >= line.len() {
                    return line;
                }
                last_space = None;
            }
            _ => last_space = None,
        }
        i += 1;
    }
    match last_space {
        Some(n) => &line[..n],
        None => line,
    }
}

fn parse_pattern(line: &[u8]) -> Pattern {
    let mut flags = 0;
    let mut p = line;
    if p.first() == Some(&b'!') {
        flags |= NEGATIVE;
        p = &p[1..];
    }
    if p.last() == Some(&b'/') {
        p = &p[..p.len() - 1];
        flags |= MUSTBEDIR;
    }
    if !p.contains(&b'/') {
        flags |= NODIR;
    }
    let nowild = simple_length(p).min(p.len());
    if p.first() == Some(&b'*') && simple_length(&p[1..]) == p.len() - 1 {
        flags |= ENDSWITH;
    }
    Pattern { pat: p.to_vec(), nowild, flags }
}

/// Parse a rule file the way git's `add_patterns_from_buffer` does: a
/// leading BOM is skipped, `#` lines and empty lines are comments, a CR
/// before the LF goes. A line wildmatch cannot use simply never matches,
/// so it costs only itself.
pub(crate) fn parse_rules(mut b: &[u8]) -> Rules {
    if b.starts_with(b"\xef\xbb\xbf") {
        b = &b[3..];
    }
    let mut pats = Vec::new();
    let mut lines: Vec<&[u8]> = b.split(|&c| c == b'\n').collect();
    // Like git, the text after the last LF is a line too (if non-empty).
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let n = lines.len();
    for (i, mut line) in lines.into_iter().enumerate() {
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        // git strips the CR of CRLF only on lines ended by LF.
        let ended = i + 1 < n || b.ends_with(b"\n");
        if ended && line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        pats.push(parse_pattern(trim_trailing_spaces(line)));
    }
    Rules { pats }
}

fn eq_ci(a: &[u8], b: &[u8], icase: bool) -> bool {
    if icase { a.eq_ignore_ascii_case(b) } else { a == b }
}

fn match_basename(base: &[u8], p: &Pattern, icase: bool) -> bool {
    let pat = &p.pat;
    if p.nowild == pat.len() {
        eq_ci(pat, base, icase)
    } else if p.flags & ENDSWITH != 0 {
        let tail = &pat[1..];
        tail.len() <= base.len() && eq_ci(tail, &base[base.len() - tail.len()..], icase)
    } else {
        wildmatch(pat, base, false, icase)
    }
}

fn match_pathname(path: &[u8], base: &[u8], p: &Pattern, icase: bool) -> bool {
    let mut pat: &[u8] = &p.pat;
    let mut prefix = p.nowild;
    if pat.first() == Some(&b'/') {
        pat = &pat[1..];
        prefix -= 1;
    }
    let bl = base.len();
    if path.len() < bl + 1 || (bl > 0 && path[bl] != b'/') || !eq_ci(&path[..bl], base, icase) {
        return false;
    }
    let mut name = if bl > 0 { &path[bl + 1..] } else { path };
    if prefix > 0 {
        if prefix > name.len() || !eq_ci(&pat[..prefix], &name[..prefix], icase) {
            return false;
        }
        pat = &pat[prefix..];
        name = &name[prefix..];
        if pat.is_empty() && name.is_empty() {
            return true;
        }
    }
    wildmatch(pat, name, true, icase)
}

impl Rules {
    /// The last pattern matching `path` (relative to the work tree root)
    /// in a file whose directory is `base` ("" for the root and for
    /// non-`.gitignore` files): `Some(ignored)`, or `None`.
    fn decide(&self, path: &[u8], is_dir: bool, base: &[u8], icase: bool) -> Option<bool> {
        let basename = match path.iter().rposition(|&c| c == b'/') {
            Some(i) => &path[i + 1..],
            None => path,
        };
        for p in self.pats.iter().rev() {
            if p.flags & MUSTBEDIR != 0 && !is_dir {
                continue;
            }
            let hit = if p.flags & NODIR != 0 {
                match_basename(basename, p, icase)
            } else {
                match_pathname(path, base, p, icase)
            };
            if hit {
                return Some(p.flags & NEGATIVE == 0);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// wildmatch (git's wildmatch.c)

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wm {
    Match,
    NoMatch,
    AbortAll,
    AbortToStarStar,
}

pub(crate) fn wildmatch(pat: &[u8], text: &[u8], pathname: bool, icase: bool) -> bool {
    dowild(pat, 0, text, 0, pathname, icase) == Wm::Match
}

fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn dowild(pat: &[u8], mut p: usize, text: &[u8], mut t: usize, pathname: bool, icase: bool) -> Wm {
    let fold = |c: u8| if icase { c.to_ascii_lowercase() } else { c };
    while at(pat, p) != 0 {
        let mut p_ch = at(pat, p);
        let mut t_ch = at(text, t);
        if t_ch == 0 && p_ch != b'*' {
            return Wm::AbortAll;
        }
        t_ch = fold(t_ch);
        p_ch = fold(p_ch);
        match p_ch {
            b'?' => {
                if pathname && t_ch == b'/' {
                    return Wm::NoMatch;
                }
            }
            b'*' => {
                let match_slash;
                p += 1;
                if at(pat, p) == b'*' {
                    let prev_ok = p < 2 || pat[p - 2] == b'/';
                    while at(pat, p + 1) == b'*' {
                        p += 1;
                    }
                    p += 1;
                    let next = at(pat, p);
                    if (!pathname || prev_ok)
                        && (next == 0 || next == b'/' || (next == b'\\' && at(pat, p + 1) == b'/'))
                    {
                        if next == b'/' && dowild(pat, p + 1, text, t, pathname, icase) == Wm::Match {
                            return Wm::Match;
                        }
                        match_slash = true;
                    } else {
                        match_slash = !pathname;
                    }
                } else {
                    match_slash = !pathname;
                }
                let next = at(pat, p);
                if next == 0 {
                    if !match_slash && text[t.min(text.len())..].contains(&b'/') {
                        return Wm::NoMatch;
                    }
                    return Wm::Match;
                } else if !match_slash && next == b'/' {
                    match text[t.min(text.len())..].iter().position(|&c| c == b'/') {
                        None => return Wm::NoMatch,
                        Some(i) => {
                            // The slash is consumed by the loop step.
                            t += i;
                            p += 1;
                            t += 1;
                            continue;
                        }
                    }
                }
                loop {
                    if t_ch == 0 {
                        break;
                    }
                    if !is_glob_special(next) {
                        let want = fold(next);
                        loop {
                            t_ch = at(text, t);
                            if t_ch == 0 || (!match_slash && t_ch == b'/') {
                                break;
                            }
                            t_ch = fold(t_ch);
                            if t_ch == want {
                                break;
                            }
                            t += 1;
                        }
                        if t_ch != want {
                            return Wm::NoMatch;
                        }
                    }
                    let m = dowild(pat, p, text, t, pathname, icase);
                    if m != Wm::NoMatch {
                        if !match_slash || m != Wm::AbortToStarStar {
                            return m;
                        }
                    } else if !match_slash && t_ch == b'/' {
                        return Wm::AbortToStarStar;
                    }
                    t += 1;
                    t_ch = at(text, t);
                }
                return Wm::AbortAll;
            }
            b'[' => {
                p += 1;
                let mut p_ch = at(pat, p);
                if p_ch == b'^' {
                    p_ch = b'!';
                }
                let negated = p_ch == b'!';
                if negated {
                    p += 1;
                    p_ch = at(pat, p);
                }
                let mut prev_ch: u8 = 0;
                let mut matched = false;
                loop {
                    if p_ch == 0 {
                        return Wm::AbortAll;
                    }
                    if p_ch == b'\\' {
                        p += 1;
                        p_ch = at(pat, p);
                        if p_ch == 0 {
                            return Wm::AbortAll;
                        }
                        if t_ch == p_ch {
                            matched = true;
                        }
                    } else if p_ch == b'-' && prev_ch != 0 && at(pat, p + 1) != 0 && at(pat, p + 1) != b']' {
                        p += 1;
                        p_ch = at(pat, p);
                        if p_ch == b'\\' {
                            p += 1;
                            p_ch = at(pat, p);
                            if p_ch == 0 {
                                return Wm::AbortAll;
                            }
                        }
                        if t_ch <= p_ch && t_ch >= prev_ch {
                            matched = true;
                        } else if icase && t_ch.is_ascii_lowercase() {
                            let up = t_ch.to_ascii_uppercase();
                            if up <= p_ch && up >= prev_ch {
                                matched = true;
                            }
                        }
                        p_ch = 0;
                    } else if p_ch == b'[' && at(pat, p + 1) == b':' {
                        p += 2;
                        let s = p;
                        while at(pat, p) != 0 && at(pat, p) != b']' {
                            p += 1;
                        }
                        if at(pat, p) == 0 {
                            return Wm::AbortAll;
                        }
                        if p == s || pat[p - 1] != b':' {
                            // No ":]": a plain '[' in the set.
                            p = s - 2;
                            p_ch = b'[';
                            if t_ch == p_ch {
                                matched = true;
                            }
                        } else {
                            let class = &pat[s..p - 1];
                            let c = t_ch;
                            let hit = match class {
                                b"alnum" => c.is_ascii_alphanumeric(),
                                b"alpha" => c.is_ascii_alphabetic(),
                                b"blank" => c == b' ' || c == b'\t',
                                b"cntrl" => c.is_ascii_control(),
                                b"digit" => c.is_ascii_digit(),
                                b"graph" => c.is_ascii_graphic(),
                                b"lower" => c.is_ascii_lowercase(),
                                b"print" => c.is_ascii_graphic() || c == b' ',
                                b"punct" => c.is_ascii_punctuation(),
                                b"space" => matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
                                b"upper" => c.is_ascii_uppercase() || (icase && c.is_ascii_lowercase()),
                                b"xdigit" => c.is_ascii_hexdigit(),
                                _ => return Wm::AbortAll,
                            };
                            if hit {
                                matched = true;
                            }
                            p_ch = 0;
                        }
                    } else if t_ch == p_ch {
                        matched = true;
                    }
                    prev_ch = p_ch;
                    p += 1;
                    p_ch = at(pat, p);
                    if p_ch == b']' {
                        break;
                    }
                }
                if matched == negated || (pathname && t_ch == b'/') {
                    return Wm::NoMatch;
                }
            }
            _ => {
                // An escaped character is compared unfolded, as in git.
                let lit = if p_ch == b'\\' {
                    p += 1;
                    at(pat, p)
                } else {
                    p_ch
                };
                if t_ch != lit {
                    return Wm::NoMatch;
                }
            }
        }
        p += 1;
        t += 1;
    }
    if t < text.len() { Wm::NoMatch } else { Wm::Match }
}

// ---------------------------------------------------------------------------
// rule sources

/// The rule set for one work tree (or export dir).
pub(crate) struct Stack {
    /// Root the `.gitignore` files are read under (rooted, G11).
    pub root: PathBuf,
    /// `info/exclude`, then the global excludes file.
    pub tail: Vec<Arc<Rules>>,
    pub icase: bool,
}

fn rule_unknown(path: &Path, what: &str, e: Option<&StrataError>) -> StrataError {
    errd(
        Code::Unknown,
        format!("cannot read ignore rules {}: {what}", path.display()),
        json!({ "path": path_str(path), "code": e.map(|e| e.code.as_str()) }),
    )
}

impl Git {
    fn cached_rules(&self, at: &Path, f: File) -> Result<Arc<Rules>> {
        let key = FileKey::of(&f).map_err(|e| rule_unknown(at, &e.to_string(), None))?;
        if let Some((k, r)) = self.inner.cache.lock().unwrap().rules.get(at)
            && *k == key
        {
            return Ok(r.clone());
        }
        let mut buf = Vec::new();
        (&f).read_to_end(&mut buf).map_err(|e| rule_unknown(at, &e.to_string(), None))?;
        let r = Arc::new(parse_rules(&buf));
        self.inner.cache.lock().unwrap().rules.insert(at.to_path_buf(), (key, r.clone()));
        Ok(r)
    }

    /// A `.gitignore` under `root`: absent, a symlink, a directory or
    /// behind a non-directory → no rules; FIFO and the like → Unknown.
    pub(crate) fn gitignore_rules(&self, root: &Path, dir: &str) -> Result<Option<Arc<Rules>>> {
        let rel = if dir.is_empty() { ".gitignore".to_string() } else { format!("{dir}/.gitignore") };
        let at = root.join(&rel);
        let rp = fsx::rp(&rel)?;
        match strata_apfs::open_rooted_file(root, &rp) {
            Ok(f) => Ok(Some(self.cached_rules(&at, f)?)),
            Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory | Code::IsDirectory) => Ok(None),
            Err(e) if e.code == Code::NotFile => match strata_apfs::attrs_of(root, &rp) {
                Ok(a) if matches!(a.kind, FileKind::Symlink { .. } | FileKind::Dir) => Ok(None),
                Ok(_) => Err(rule_unknown(&at, "not a regular file", Some(&e))),
                Err(e2) if e2.code == Code::NoSuchPath => Ok(None),
                Err(e2) => Err(rule_unknown(&at, &e2.message, Some(&e2))),
            },
            Err(e) => Err(rule_unknown(&at, &e.message, Some(&e))),
        }
    }

    /// A file of a shadow repository, rooted and not followed.
    fn rooted_rules(&self, root: &Path, rel: &str) -> Result<Option<Arc<Rules>>> {
        let at = root.join(rel);
        match strata_apfs::open_rooted_file(root, &fsx::rp(rel)?) {
            Ok(f) => Ok(Some(self.cached_rules(&at, f)?)),
            Err(e) if matches!(e.code, Code::NoSuchPath) => Ok(None),
            Err(e) if e.code == Code::NotDirectory && crate::index::reason_of(&e) == Some("not_a_directory") => Ok(None),
            Err(e) => Err(rule_unknown(&at, &e.message, Some(&e))),
        }
    }

    /// A human file: realpath, then rooted at its directory; dangling or
    /// behind a non-directory → absent.
    pub(crate) fn human_rules(&self, p: &Path) -> Result<Option<Arc<Rules>>> {
        let real = match std::fs::canonicalize(p) {
            Ok(r) => r,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) => return Ok(None),
            Err(e) => return Err(rule_unknown(p, &e.to_string(), None)),
        };
        let (dir, name) = match (real.parent(), real.file_name().and_then(|n| n.to_str())) {
            (Some(d), Some(n)) => (d.to_path_buf(), n.to_string()),
            _ => return Err(rule_unknown(&real, "not a file", None)),
        };
        match strata_apfs::open_rooted_file(&dir, &fsx::rp(&name)?) {
            Ok(f) => Ok(Some(self.cached_rules(&real, f)?)),
            Err(e) if e.code == Code::NoSuchPath => Ok(None),
            Err(e) => Err(rule_unknown(&real, &e.message, Some(&e))),
        }
    }

    /// The global excludes file seen from `common` (cached per common dir):
    /// `core.excludesFile`, else `$XDG_CONFIG_HOME/git/ignore`, else
    /// `~/.config/git/ignore`.
    pub(crate) fn global_excludes_path(&self, common: &Path) -> Result<Option<PathBuf>> {
        if let Some(p) = self.inner.cache.lock().unwrap().global_excludes.get(common) {
            return Ok(p.clone());
        }
        let mut c = self.command();
        c.current_dir(common)
            .arg(flag("--git-dir=", common))
            .args(["config", "--path", "--get", "core.excludesFile"]);
        let out = self.run(c, Opts::default().timeout(QUERY_TIMEOUT))?;
        let found = match out.status {
            Some(0) => {
                let mut v = out.stdout.clone();
                if v.last() == Some(&b'\n') {
                    v.pop();
                }
                use std::os::unix::ffi::OsStrExt;
                let p = PathBuf::from(std::ffi::OsStr::from_bytes(&v));
                Some(if p.is_absolute() { p } else { common.join(p) })
            }
            Some(1) => {
                let xdg = self.env_var("XDG_CONFIG_HOME").filter(|v| !v.is_empty());
                match xdg {
                    Some(x) => Some(PathBuf::from(x).join("git/ignore")),
                    None => self
                        .env_var("HOME")
                        .filter(|v| !v.is_empty())
                        .map(|h| PathBuf::from(h).join(".config/git/ignore")),
                }
            }
            _ => return Err(crate::command::failed("config core.excludesFile", &out)),
        };
        self.inner.cache.lock().unwrap().global_excludes.insert(common.to_path_buf(), found.clone());
        Ok(found)
    }

    fn global_rules(&self, common: &Path) -> Result<Option<Arc<Rules>>> {
        match self.global_excludes_path(common)? {
            None => Ok(None),
            Some(p) => self.human_rules(&p),
        }
    }

    /// Whether `path` in `worktree` is ignored the way `git status` would
    /// treat it: tracked files never are (§5.8).
    pub fn is_ignored(&self, worktree: &Path, path: &RepoPath) -> Result<bool> {
        let loc = self.locate(worktree)?;
        let info = self.read_index_at(&loc, worktree)?;
        if info.is_tracked(path) {
            return Ok(false);
        }
        let mut tail = Vec::new();
        let info_exclude = if loc.human {
            self.human_rules(&loc.common.join("info/exclude"))?
        } else {
            self.rooted_rules(&loc.common, "info/exclude")?
        };
        tail.extend(info_exclude);
        tail.extend(self.global_rules(&loc.common)?);
        let stack = Stack { root: loc.wt_root.clone(), tail, icase: info.ignorecase };
        let is_dir = self.is_dir_at(&stack.root, path)?;
        self.excluded(&stack, path.as_str(), is_dir)
    }

    /// Whether git would ignore `path` once `tree` (exported in `dir`) is
    /// checked out as a workspace of this repository (§5.8, last part).
    pub fn is_ignored_in_export(&self, dir: &Path, tree: &TreeOid, path: &RepoPath) -> Result<bool> {
        fsx::require_absolute(dir, "export dir")?;
        match fsx::lstat(dir)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("{} does not exist", dir.display()),
                    json!({ "path": path_str(dir) }),
                ));
            }
            Some(m) if !m.file_type().is_dir() => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("{} is not a directory", dir.display()),
                    json!({ "path": path_str(dir) }),
                ));
            }
            Some(_) => {}
        }
        if fsx::lstat(&dir.join(".git"))?.is_some() {
            return Err(errd(
                Code::InvalidInput,
                format!("{} has a .git; use is_ignored", dir.display()),
                json!({ "path": path_str(dir), "reason": "has-git" }),
            ));
        }
        let table = self.tree_table(tree)?;
        if table.has_file(path) {
            return Ok(false);
        }
        let common = self.inner.common_dir.clone();
        let mut tail = Vec::new();
        tail.extend(self.human_rules(&common.join("info/exclude"))?);
        tail.extend(self.global_rules(&common)?);
        let stack = Stack { root: dir.to_path_buf(), tail, icase: table.icase };
        let is_dir = self.is_dir_at(dir, path)?;
        self.excluded(&stack, path.as_str(), is_dir)
    }

    /// lstat: a directory, or (missing, a symlink, anything else) a file.
    pub(crate) fn is_dir_at(&self, root: &Path, path: &RepoPath) -> Result<bool> {
        match strata_apfs::attrs_of(root, path) {
            Ok(a) => Ok(matches!(a.kind, FileKind::Dir)),
            Err(e) if matches!(e.code, Code::NoSuchPath | Code::NotDirectory) => Ok(false),
            Err(e) => Err(rule_unknown(&root.join(path.as_str()), &e.message, Some(&e))),
        }
    }

    /// Rules only: an excluded parent directory excludes everything below.
    pub(crate) fn excluded(&self, stack: &Stack, path: &str, is_dir: bool) -> Result<bool> {
        let mut end = 0;
        while let Some(i) = path[end..].find('/') {
            let dir = &path[..end + i];
            if self.decide(stack, dir, true)? {
                return Ok(true);
            }
            end += i + 1;
        }
        self.decide(stack, path, is_dir)
    }

    fn decide(&self, stack: &Stack, target: &str, is_dir: bool) -> Result<bool> {
        let mut dir = parent(target);
        loop {
            if let Some(r) = self.gitignore_rules(&stack.root, dir)?
                && let Some(v) = r.decide(target.as_bytes(), is_dir, dir.as_bytes(), stack.icase)
            {
                return Ok(v);
            }
            if dir.is_empty() {
                break;
            }
            dir = parent(dir);
        }
        for r in &stack.tail {
            if let Some(v) = r.decide(target.as_bytes(), is_dir, b"", stack.icase) {
                return Ok(v);
            }
        }
        Ok(false)
    }
}

fn parent(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[..i],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wm(p: &str, t: &str) -> bool {
        wildmatch(p.as_bytes(), t.as_bytes(), true, false)
    }

    #[test]
    fn wildmatch_basics() {
        assert!(wm("foo", "foo"));
        assert!(!wm("foo", "bar"));
        assert!(wm("*", "foo"));
        assert!(!wm("*", "foo/bar"));
        assert!(wm("**", "foo/bar"));
        assert!(wm("**/foo", "foo"));
        assert!(wm("**/foo", "a/b/foo"));
        assert!(wm("a/**/b", "a/b"));
        assert!(wm("a/**/b", "a/x/y/b"));
        assert!(wm("a/**", "a/x/y"));
        assert!(!wm("a/**", "a"));
        assert!(wm("foo/*/bar", "foo/x/bar"));
        assert!(!wm("foo/*/bar", "foo/x/y/bar"));
        assert!(wm("*.[oa]", "x.o"));
        assert!(!wm("*.[oa]", "x.c"));
        assert!(wm("[!a]x", "bx"));
        assert!(wm("[^a]x", "bx"));
        assert!(wm("[a-c]", "b"));
        assert!(wm("[[:digit:]]", "5"));
        assert!(!wm("[", "["));
        assert!(wm("\\#a", "#a"));
        assert!(wm("t?mp", "temp"));
        assert!(!wm("?", "/"));
        assert!(wildmatch(b"*.TXT", b"a.txt", true, true));
        assert!(!wildmatch("É.txt".as_bytes(), "é.txt".as_bytes(), true, true));
    }

    #[test]
    fn parse_like_git() {
        let r = parse_rules(b"\xef\xbb\xbf# c\n\n!keep\nbuild/\ntrailing\\ \nsp  \r\nlast");
        let pats: Vec<&[u8]> = r.pats.iter().map(|p| p.pat.as_slice()).collect();
        assert_eq!(pats, vec![&b"keep"[..], b"build", b"trailing\\ ", b"sp", b"last"]);
        assert_eq!(r.pats[0].flags & NEGATIVE, NEGATIVE);
        assert_eq!(r.pats[1].flags & MUSTBEDIR, MUSTBEDIR);
    }
}

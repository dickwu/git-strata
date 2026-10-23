//! In-process git config reading (§5.7 step 6): a port of the behaviour of
//! git 2.54's `config.c` (`parse_file`, `get_base_var`,
//! `get_extended_base_var`, `get_value`, `parse_value`, `get_next_char`),
//! with booleans per `git_parse_maybe_bool`. Includes are not followed.
//! Any line git would reject makes the whole file `bad-config`; so does a
//! NUL byte anywhere (stricter than git, §9).

use strata_types::StrataError;

use crate::err::unknown_reason;

/// One `name = value` occurrence. `name` is `section.key` or
/// `section.subsection.key`, section and key lowercased.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub value: Option<Vec<u8>>,
}

fn bad(why: &str) -> StrataError {
    unknown_reason(format!("config is not readable as git config: {why}"), "bad-config")
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
    eof: bool,
}

impl Reader<'_> {
    /// `get_next_char`: CRLF → LF; EOF reads as LF with `eof` set.
    fn next(&mut self) -> u8 {
        if self.i >= self.b.len() {
            self.eof = true;
            return b'\n';
        }
        let mut c = self.b[self.i];
        self.i += 1;
        if c == b'\r' && self.i < self.b.len() && self.b[self.i] == b'\n' {
            self.i += 1;
            c = b'\n';
        }
        c
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn is_keychar(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

/// Parse a whole config file.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Entry>, StrataError> {
    if bytes.contains(&0) {
        return Err(bad("NUL byte"));
    }
    const BOM: &[u8] = b"\xef\xbb\xbf";
    let mut r = Reader { b: bytes, i: 0, eof: false };
    let mut out = Vec::new();
    let mut var: Vec<u8> = Vec::new();
    let mut baselen = 0usize;
    let mut comment = false;
    let mut bom = 0usize; // bytes of BOM matched; usize::MAX when done
    loop {
        let c = r.next();
        if bom != usize::MAX {
            if !r.eof && bom < BOM.len() && c == BOM[bom] {
                bom += 1;
                if bom == BOM.len() {
                    bom = usize::MAX;
                }
                continue;
            }
            if bom != 0 {
                return Err(bad("partial byte order mark"));
            }
            bom = usize::MAX;
        }
        if c == b'\n' {
            if r.eof {
                return Ok(out);
            }
            comment = false;
            continue;
        }
        if comment {
            continue;
        }
        if is_space(c) {
            continue;
        }
        if c == b'#' || c == b';' {
            comment = true;
            continue;
        }
        if c == b'[' {
            var.clear();
            base_var(&mut r, &mut var)?;
            if var.is_empty() {
                return Err(bad("empty section name"));
            }
            var.push(b'.');
            baselen = var.len();
            continue;
        }
        if !c.is_ascii_alphabetic() {
            return Err(bad("line does not start a key or section"));
        }
        var.truncate(baselen);
        var.push(c.to_ascii_lowercase());
        let value = get_value(&mut r, &mut var)?;
        out.push(Entry { name: String::from_utf8_lossy(&var).into_owned(), value });
    }
}

fn base_var(r: &mut Reader, name: &mut Vec<u8>) -> Result<(), StrataError> {
    loop {
        let c = r.next();
        if r.eof {
            return Err(bad("unterminated section header"));
        }
        if c == b']' {
            return Ok(());
        }
        if is_space(c) {
            return extended_base_var(r, name, c);
        }
        if !is_keychar(c) && c != b'.' {
            return Err(bad("bad section name"));
        }
        name.push(c.to_ascii_lowercase());
    }
}

fn extended_base_var(r: &mut Reader, name: &mut Vec<u8>, mut c: u8) -> Result<(), StrataError> {
    loop {
        if c == b'\n' {
            return Err(bad("incomplete section header"));
        }
        c = r.next();
        if !is_space(c) {
            break;
        }
    }
    if c != b'"' {
        return Err(bad("subsection must be quoted"));
    }
    name.push(b'.');
    loop {
        let mut c = r.next();
        if c == b'\n' {
            return Err(bad("incomplete section header"));
        }
        if c == b'"' {
            break;
        }
        if c == b'\\' {
            c = r.next();
            if c == b'\n' {
                return Err(bad("incomplete section header"));
            }
        }
        name.push(c);
    }
    if r.next() != b']' {
        return Err(bad("section header not closed"));
    }
    Ok(())
}

fn get_value(r: &mut Reader, name: &mut Vec<u8>) -> Result<Option<Vec<u8>>, StrataError> {
    let mut c;
    loop {
        c = r.next();
        if r.eof {
            break;
        }
        if !is_keychar(c) {
            break;
        }
        name.push(c.to_ascii_lowercase());
    }
    while c == b' ' || c == b'\t' {
        c = r.next();
    }
    if c == b'\n' {
        return Ok(None);
    }
    if c != b'=' {
        return Err(bad("bad key"));
    }
    parse_value(r).map(Some)
}

fn parse_value(r: &mut Reader) -> Result<Vec<u8>, StrataError> {
    let mut quote = false;
    let mut comment = false;
    let mut trim_len = 0usize;
    let mut v: Vec<u8> = Vec::new();
    loop {
        let mut c = r.next();
        if c == b'\n' {
            if quote {
                return Err(bad("unterminated quote"));
            }
            if trim_len != 0 {
                v.truncate(trim_len);
            }
            return Ok(v);
        }
        if comment {
            continue;
        }
        if is_space(c) && !quote {
            if trim_len == 0 {
                trim_len = v.len();
            }
            if !v.is_empty() {
                v.push(c);
            }
            continue;
        }
        if !quote && (c == b';' || c == b'#') {
            comment = true;
            continue;
        }
        trim_len = 0;
        if c == b'\\' {
            c = r.next();
            match c {
                b'\n' => continue,
                b't' => c = b'\t',
                b'b' => c = 8,
                b'n' => c = b'\n',
                b'\\' | b'"' => {}
                _ => return Err(bad("unknown escape")),
            }
            v.push(c);
            continue;
        }
        if c == b'"' {
            quote = !quote;
            continue;
        }
        v.push(c);
    }
}

/// The boolean `name` (lowercase `section.key`): every occurrence must be a
/// valid boolean; the last wins; absent is `None`.
pub(crate) fn get_bool(entries: &[Entry], name: &str) -> Result<Option<bool>, StrataError> {
    let mut out = None;
    for e in entries.iter().filter(|e| e.name == name) {
        let v = match &e.value {
            None => None,
            Some(b) => Some(std::str::from_utf8(b).map_err(|_| bad("non-UTF-8 boolean"))?),
        };
        out = Some(parse_bool(v).ok_or_else(|| bad(&format!("{name} is not a boolean")))?);
    }
    Ok(out)
}

/// Parse bytes and read one boolean.
pub(crate) fn config_bool(bytes: &[u8], name: &str) -> Result<Option<bool>, StrataError> {
    get_bool(&parse(bytes)?, name)
}

/// `git_parse_maybe_bool_text`: true/yes/on, false/no/off, empty = false.
pub(crate) fn parse_bool_text(v: &str) -> Option<bool> {
    let l = v.to_ascii_lowercase();
    match l.as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

/// `git_parse_maybe_bool`: text forms, else an int (non-zero = true).
/// `None` value (key without `=`) is true.
pub(crate) fn parse_bool(v: Option<&str>) -> Option<bool> {
    let Some(v) = v else { return Some(true) };
    if let Some(b) = parse_bool_text(v) {
        return Some(b);
    }
    parse_int32(v).map(|n| n != 0)
}

/// `git_parse_int`: strtoimax base 0 with k/m/g units, int32 range.
pub(crate) fn parse_int32(v: &str) -> Option<i32> {
    // strtoimax skips C-locale isspace.
    let s = v.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    if s.is_empty() {
        return None;
    }
    let (neg, s) = match s.as_bytes()[0] {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(r) if r.bytes().next().is_some_and(|c| c.is_ascii_hexdigit()) => (16, r),
        _ if s.starts_with('0') => (8, s),
        _ => (10, s),
    };
    let n_digits = digits.bytes().take_while(|c| (*c as char).is_digit(radix)).count();
    let (num, rest) = digits.split_at(n_digits);
    let mut val: i128 = 0;
    if num.is_empty() {
        return None;
    }
    for c in num.bytes() {
        val = val * radix as i128 + (c as char).to_digit(radix).unwrap() as i128;
        if val > i64::MAX as i128 + 1 {
            return None; // strtoimax would set ERANGE
        }
    }
    if neg {
        val = -val;
    }
    if val > i64::MAX as i128 || val < i64::MIN as i128 {
        return None;
    }
    let factor: i128 = match rest {
        "" => 1,
        "k" | "K" => 1024,
        "m" | "M" => 1024 * 1024,
        "g" | "G" => 1024 * 1024 * 1024,
        _ => return None,
    };
    let v = val.checked_mul(factor)?;
    if v > i32::MAX as i128 || v < i32::MIN as i128 {
        return None;
    }
    Some(v as i32)
}

#[cfg(test)]
mod tests {
    use super::*;


    /// What `git config --file f --type=bool --get core.ignorecase` says:
    /// Ok(Some/None) or Err(()) when git rejects the file or value.
    fn git_says(bytes: &[u8]) -> Result<Option<bool>, ()> {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("config");
        std::fs::write(&f, bytes).unwrap();
        let mut c = std::process::Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        let o = c
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path())
            .env("LC_ALL", "C")
            .args(["config", "--file"])
            .arg(&f)
            .args(["--type=bool", "--get", "core.ignorecase"])
            .output()
            .unwrap();
        match o.status.code() {
            Some(0) => match String::from_utf8_lossy(&o.stdout).trim() {
                "true" => Ok(Some(true)),
                "false" => Ok(Some(false)),
                other => panic!("git printed {other:?}"),
            },
            Some(1) if o.stderr.is_empty() => Ok(None),
            _ => Err(()),
        }
    }

    fn ours(bytes: &[u8]) -> Result<Option<bool>, ()> {
        config_bool(bytes, "core.ignorecase").map_err(|e| {
            assert_eq!(e.code, strata_types::Code::Unknown);
            assert_eq!(e.detail.as_ref().unwrap()["reason"], "bad-config");
        })
    }

    #[test]
    fn config_values_read_like_git() {
        let values: &[&str] = &[
            "true", "\"true\"", "TRUE", "\"tr\"ue", "true # c", "true ; c", "\"tr#ue\"", "", "\"\"", "yes", "On",
            "off", "0", "1", "2", "-1", "0x10", "010", "1k", "0k", "maybe", "tr\\\"ue", "tr\\que", "\"true",
            "\" true \"", "2147483647", "2147483648", "-2147483648", "-2147483649", "3000000000", "1g", "2g",
            "-2g", "-3g", "2097151k", "2097152k", "0x7fffffff", "0x80000000", "9223372036854775808", "1K",
            "1M", "1G", "+1", "-0", "07", "08", "0x", "1kb", "1e3", "\" 1\"", "\"1 \"", "\"\\t1\"", "\x0b1",
            "1\x0b", "\x0c1", "1 \"\"", "true \"\"", "0 \"\"", "off \"\"", "\"1\" \"\"", "1 \"\" # c", "1\t\"\"",
            "1 # c", "1  ", "-2097152k", "-2097153k", "0g", "1\r", "\r1", "\"1\r\"", "tr\\\nue", "\"tr\nue\"",
            "\"tr\\\nue\"", "1 \\\n", "1 \\\n   \n", "1 \\\n# c\n", "1 \\\r\n", "1\\\n   \n", "1 \\", "1\\",
        ];
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for v in values {
            cases.push(format!("[core]\n\tignorecase = {v}\n").into_bytes());
            cases.push(format!("[core]\r\n\tignorecase = {v}\r\n").into_bytes());
            cases.push(format!("[core]\n\tignorecase = {v}").into_bytes());
        }
        let files: &[&[u8]] = &[
            b"[core]\n\tignorecase\n",
            b"[core]\n\tignorecase =\n",
            b"[core]\n\tignorecase = true\n\tignorecase = false\n",
            b"[core]\n\tignorecase = maybe\n\tignorecase = true\n",
            b"[core]\n\tignorecase = true\n\tignorecase = maybe\n",
            b"[Core]\n\tIgnoreCase = true\n",
            b"[core \"x\"]\n\tignorecase = true\n",
            b"[ core ]\n\tignorecase = true\n",
            b"[core] ignorecase = true\n",
            b"[core]\n\tignorecase # c\n",
            b"[core]\n\tignorecase ; c\n",
            b"[core]\n\tignorecase\x0b= true\n",
            b"[core]\n\tignorecase\x0c= true\n",
            b"[core]\n\x0bignorecase = true\n",
            b"[core]\n\tignore_case = 1\n\tignorecase = true\n",
            b"[core]\n\tignorecase = true\n\tignore_case = 1\n",
            b"[core]\n\t9key = 1\n\tignorecase = true\n",
            b"[user]\n\tignore_case = 1\n[core]\n\tignorecase = true\n",
            b"[core]]\n\tignorecase = true\n",
            b"[]\n\tignorecase = true\n",
            b"[co_re]\n\tignorecase = true\n",
            b"[core\"\"]\n\tignorecase = true\n",
            b"[core \"x\" ]\n\tignorecase = true\n",
            b"\xef\xbb\xbf[core]\n\tignorecase = true\n",
            b"\xef\xbb[core]\n\tignorecase = true\n",
            b"\xef\xbb\xbf\xef\xbb\xbf[core]\n\tignorecase = true\n",
            b"ignorecase = true\n[core]\n",
            b"[core][user]\n\tignorecase = true\n",
            b"[user][core]\n\tignorecase = true\n",
            b"[core \"a\\zb\"]\n\tignorecase = true\n[core]\n\tignorecase = false\n",
            b"[ \"x\"]\n\tignorecase = true\n",
            b"[\tcore]\n\tignorecase = true\n",
            b"[core\t\"x\"]\n\tignorecase = true\n[core]\nignorecase=false\n",
            b"[.]\n\tignorecase = true\n",
            b"[core.]\n\tignorecase = true\n",
            b"[core \"\"]\n\tignorecase = true\n",
            b"[core.x]\n\tignorecase = true\n",
            b"[other]\n\tignorecase = true\n",
            b"",
            b"\n\n# only comments\n",
            b"[core]\n\tbare = false\n",
            b"[core\n\tignorecase = true\n",
            b"[core \"x\n\"]\n",
            b"[include]\n\tpath = /nonexistent/x\n[core]\n\tignorecase = false\n",
            b"[core]\n\tignorecase = \"1 \\\n\"\n",
            b"[core]\n # c \\\n\tignorecase = true\n",
        ];
        for f in files {
            cases.push(f.to_vec());
        }
        let mut mismatches = Vec::new();
        for c in &cases {
            let g = git_says(c);
            let o = ours(c);
            if g != o {
                mismatches.push(format!("{:?}: git {g:?} ours {o:?}", String::from_utf8_lossy(c)));
            }
        }
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }

    #[test]
    fn nul_is_bad_config_even_where_git_accepts_it() {
        let f = b"[core]\n\tignorecase = true # \x00 c\n";
        assert_eq!(git_says(f), Ok(Some(true)));
        assert_eq!(ours(f), Err(()));
    }

    #[test]
    fn include_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let inc = dir.path().join("inc");
        std::fs::write(&inc, "[core]\n\tignorecase = true\n").unwrap();
        let f = format!("[core]\n\tignorecase = false\n[include]\n\tpath = {}\n", inc.display());
        assert_eq!(ours(f.as_bytes()), Ok(Some(false)));
    }

    #[test]
    fn bools_follow_git() {
        assert_eq!(parse_bool(None), Some(true));
        assert_eq!(parse_bool(Some("")), Some(false));
        assert_eq!(parse_bool(Some("TRUE")), Some(true));
        assert_eq!(parse_bool(Some("off")), Some(false));
        assert_eq!(parse_bool(Some("2")), Some(true));
        assert_eq!(parse_bool(Some("0x0")), Some(false));
        assert_eq!(parse_bool(Some("1k")), Some(true));
        assert_eq!(parse_bool(Some("010")), Some(true));
        assert_eq!(parse_bool(Some("0")), Some(false));
        assert_eq!(parse_bool(Some("3g")), None);
        assert_eq!(parse_bool(Some("maybe")), None);
        assert_eq!(parse_bool(Some(" 1")), Some(true));
        assert_eq!(parse_bool(Some("1 ")), None);
        assert_eq!(parse_bool(Some("0x")), None);
        assert_eq!(parse_bool(Some("08")), None);
    }
}

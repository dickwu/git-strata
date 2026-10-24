//! An independent JSON reader for the envelope tests (cli.md §7: the
//! envelope must round-trip through a parser that is not serde_json).
#![allow(dead_code)]

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<J>),
    /// Keys in document order (duplicates kept, so a test can see them).
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn keys(&self) -> Vec<String> {
        match self {
            J::Obj(kv) => kv.iter().map(|(k, _)| k.clone()).collect(),
            _ => vec![],
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn map(&self) -> BTreeMap<String, J> {
        match self {
            J::Obj(kv) => kv.iter().cloned().collect(),
            _ => BTreeMap::new(),
        }
    }
}

pub fn parse(text: &str) -> Result<J, String> {
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    let v = value(&b, &mut i)?;
    if i != b.len() {
        return Err(format!("trailing input at {i}"));
    }
    Ok(v)
}

fn value(b: &[char], i: &mut usize) -> Result<J, String> {
    match b.get(*i) {
        None => Err("end of input".into()),
        Some('{') => {
            *i += 1;
            let mut kv = Vec::new();
            if b.get(*i) == Some(&'}') {
                *i += 1;
                return Ok(J::Obj(kv));
            }
            loop {
                if b.get(*i) != Some(&'"') {
                    return Err(format!("key expected at {i}"));
                }
                let k = string(b, i)?;
                if b.get(*i) != Some(&':') {
                    return Err(format!("':' expected at {i}"));
                }
                *i += 1;
                kv.push((k, value(b, i)?));
                match b.get(*i) {
                    Some(',') => *i += 1,
                    Some('}') => {
                        *i += 1;
                        return Ok(J::Obj(kv));
                    }
                    _ => return Err(format!("',' or '}}' expected at {i}")),
                }
            }
        }
        Some('[') => {
            *i += 1;
            let mut v = Vec::new();
            if b.get(*i) == Some(&']') {
                *i += 1;
                return Ok(J::Arr(v));
            }
            loop {
                v.push(value(b, i)?);
                match b.get(*i) {
                    Some(',') => *i += 1,
                    Some(']') => {
                        *i += 1;
                        return Ok(J::Arr(v));
                    }
                    _ => return Err(format!("',' or ']' expected at {i}")),
                }
            }
        }
        Some('"') => Ok(J::Str(string(b, i)?)),
        Some('t') => lit(b, i, "true", J::Bool(true)),
        Some('f') => lit(b, i, "false", J::Bool(false)),
        Some('n') => lit(b, i, "null", J::Null),
        Some(c) if *c == '-' || c.is_ascii_digit() => {
            let s = *i;
            while let Some(c) = b.get(*i) {
                if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E') {
                    *i += 1;
                } else {
                    break;
                }
            }
            Ok(J::Num(b[s..*i].iter().collect()))
        }
        Some(c) => Err(format!("unexpected {c:?} at {i} (compact JSON has no whitespace)")),
    }
}

fn lit(b: &[char], i: &mut usize, word: &str, v: J) -> Result<J, String> {
    let w: Vec<char> = word.chars().collect();
    if b.len() >= *i + w.len() && b[*i..*i + w.len()] == w[..] {
        *i += w.len();
        Ok(v)
    } else {
        Err(format!("bad literal at {i}"))
    }
}

fn hex4(b: &[char], i: &mut usize) -> Result<u32, String> {
    let s: String = b.get(*i..*i + 4).ok_or("short \\u")?.iter().collect();
    *i += 4;
    u32::from_str_radix(&s, 16).map_err(|e| e.to_string())
}

fn string(b: &[char], i: &mut usize) -> Result<String, String> {
    *i += 1;
    let mut out = String::new();
    loop {
        let c = *b.get(*i).ok_or("unterminated string")?;
        *i += 1;
        match c {
            '"' => return Ok(out),
            '\\' => {
                let e = *b.get(*i).ok_or("bad escape")?;
                *i += 1;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hi = hex4(b, i)?;
                        let cp = if (0xD800..0xDC00).contains(&hi) {
                            if b.get(*i) != Some(&'\\') || b.get(*i + 1) != Some(&'u') {
                                return Err("lone high surrogate".into());
                            }
                            *i += 2;
                            let lo = hex4(b, i)?;
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else {
                            hi
                        };
                        out.push(char::from_u32(cp).ok_or("bad code point")?);
                    }
                    _ => return Err(format!("bad escape {e:?}")),
                }
            }
            c if (c as u32) < 0x20 => return Err(format!("raw control character {:#x} in a string", c as u32)),
            c => out.push(c),
        }
    }
}

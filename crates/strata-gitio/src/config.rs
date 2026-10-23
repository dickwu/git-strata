//! git config reading (§5.7 step 6): value parsing helpers.

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

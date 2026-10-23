use std::time::{Duration, SystemTime};

use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&format(time).map_err(serde::ser::Error::custom)?)
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).map_err(serde::de::Error::custom)
}

pub fn format(time: &SystemTime) -> Result<String, String> {
    let dur = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "time is before the unix epoch".to_owned())?;
    let days = (dur.as_secs() / 86_400) as i64;
    let sod = (dur.as_secs() % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = sod / 3600;
    let minute = (sod % 3600) / 60;
    let second = sod % 60;
    let nanos = dur.subsec_nanos();
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if nanos != 0 {
        let digits = format!("{nanos:09}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
    out.push('Z');
    Ok(out)
}

pub fn parse(text: &str) -> Result<SystemTime, String> {
    let body = text.strip_suffix('Z').ok_or("timestamp must end in Z")?;
    let (date, time) = body.split_once('T').ok_or("timestamp must contain T")?;
    let mut dates = date.split('-');
    let year: i32 = next_num(&mut dates, "year")?;
    let month: u32 = next_num(&mut dates, "month")?;
    let day: u32 = next_num(&mut dates, "day")?;
    if dates.next().is_some() {
        return Err("date has too many parts".into());
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let mut clocks = clock.split(':');
    let hour: u32 = next_num(&mut clocks, "hour")?;
    let minute: u32 = next_num(&mut clocks, "minute")?;
    let second: u32 = next_num(&mut clocks, "second")?;
    if clocks.next().is_some() {
        return Err("time has too many parts".into());
    }
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return Err("timestamp field out of range".into());
    }
    let nanos = match fraction {
        None => 0,
        Some(fraction)
            if !fraction.is_empty()
                && fraction.len() <= 9
                && fraction.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let mut value: u32 = fraction.parse().map_err(|_| "bad fraction".to_owned())?;
            for _ in fraction.len()..9 {
                value = value.saturating_mul(10);
            }
            value
        }
        Some(_) => return Err("bad fractional seconds".into()),
    };
    let days = days_from_civil(year, month, day);
    let (check_y, check_m, check_d) = civil_from_days(days);
    if check_y != year || check_m != month || check_d != day {
        return Err("impossible civil date".into());
    }
    let secs =
        days as u64 * 86_400 + u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second);
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_nanos(u64::from(nanos)))
}

fn next_num<'a, T: std::str::FromStr>(
    parts: &mut impl Iterator<Item = &'a str>,
    what: &str,
) -> Result<T, String> {
    parts
        .next()
        .ok_or_else(|| format!("missing {what}"))?
        .parse()
        .map_err(|_| format!("bad {what}"))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = (year - era * 400) as u64;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * u64::from(month_index) + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_utc_instant() {
        let parsed = parse("2026-10-22T18:04:11Z").unwrap();
        let expected = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_692_251);
        assert_eq!(parsed, expected);
        assert_eq!(format(&parsed).unwrap(), "2026-10-22T18:04:11Z");
    }
}

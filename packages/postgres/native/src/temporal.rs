//! Binary `numeric`, `money`, `date`, `time`, `timestamp`, `timestamptz` and `interval` values
//! as text.
//!
//! - `numeric` becomes its exact decimal digits (`"12.50"`, scale kept), `"NaN"`,
//!   `"Infinity"` or `"-Infinity"`: a string, since an `f64` would round it. `money` (an `i64`
//!   of cents) becomes its exact amount the same way (`"-1234.50"`), without the locale's
//!   currency symbol and grouping: two fractional digits, as in nearly every `lc_monetary`.
//! - `interval` becomes PostgreSQL's default output (`IntervalStyle` `postgres`, what a `::text`
//!   cast gives): `"1 year 2 mons 3 days 04:05:06.5"`, `"-00:00:01"`, `"00:00:00"`.
//! - Dates and times become ISO 8601: `date` `"2024-02-29"`, `time` `"13:45:00"`,
//!   `timestamp` `"2024-02-29T13:45:00.5"`, `timestamptz` (sent in UTC) with a trailing `Z`.
//!   Fractions use 3 digits when whole milliseconds, else 6, and are omitted when zero. Years
//!   outside 0000–9999 use the extended form (`"+012345-01-01"`, `"-000044-03-15"`), like
//!   JavaScript's `toISOString`. `infinity` / `-infinity` stay those words.

use std::fmt::Write;

const MICROS_PER_DAY: i64 = 86_400_000_000;
/// Days from 1970-01-01 to PostgreSQL's epoch 2000-01-01.
const PG_EPOCH_DAYS: i64 = 10_957;

fn be_i16(b: &[u8], at: usize) -> Result<i16, String> {
    b.get(at..at + 2)
        .map(|s| i16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| "truncated value".to_string())
}

/// `numeric`'s binary form (base-10000 digit groups) as a decimal string.
pub fn numeric(raw: &[u8]) -> Result<String, String> {
    let ndigits = be_i16(raw, 0)?.max(0) as usize;
    let weight = be_i16(raw, 2)? as i64;
    let sign = be_i16(raw, 4)? as u16;
    let dscale = be_i16(raw, 6)?.max(0) as usize;
    match sign {
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => {}
    }
    let digits = (0..ndigits)
        .map(|i| be_i16(raw, 8 + 2 * i))
        .collect::<Result<Vec<_>, _>>()?;
    let group = |i: i64| usize::try_from(i).ok().and_then(|i| digits.get(i)).copied();
    let mut s = String::with_capacity(ndigits * 4 + dscale + 3);
    if sign == 0x4000 {
        s.push('-');
    }
    if weight < 0 {
        s.push('0');
    } else {
        for i in 0..=weight {
            let d = group(i).unwrap_or(0);
            let _ = if i == 0 {
                write!(s, "{d}")
            } else {
                write!(s, "{d:04}")
            };
        }
    }
    if dscale > 0 {
        s.push('.');
        let start = s.len();
        let mut i = weight + 1;
        while s.len() - start < dscale {
            let _ = write!(s, "{:04}", group(i).unwrap_or(0));
            i += 1;
        }
        s.truncate(start + dscale);
    }
    Ok(s)
}

/// Year, month, day of a day count since 1970-01-01 (proleptic Gregorian; H. Hinnant's
/// `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn push_date(s: &mut String, pg_days: i64) {
    let (y, m, d) = civil(pg_days + PG_EPOCH_DAYS);
    let _ = if (0..=9999).contains(&y) {
        write!(s, "{y:04}-{m:02}-{d:02}")
    } else {
        let sign = if y < 0 { '-' } else { '+' };
        write!(s, "{sign}{:06}-{m:02}-{d:02}", y.abs())
    };
}

fn push_time(s: &mut String, micros: i64) {
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    let _ = write!(
        s,
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    let _ = if frac == 0 {
        Ok(())
    } else if frac % 1000 == 0 {
        write!(s, ".{:03}", frac / 1000)
    } else {
        write!(s, ".{frac:06}")
    };
}

fn be_i64(raw: &[u8]) -> Result<i64, String> {
    <[u8; 8]>::try_from(raw)
        .map(i64::from_be_bytes)
        .map_err(|_| "bad 8-byte value".to_string())
}

/// `date` (days since 2000-01-01, 4 bytes).
pub fn date(raw: &[u8]) -> Result<String, String> {
    let days = <[u8; 4]>::try_from(raw)
        .map(i32::from_be_bytes)
        .map_err(|_| "bad date".to_string())?;
    Ok(match days {
        i32::MAX => "infinity".into(),
        i32::MIN => "-infinity".into(),
        d => {
            let mut s = String::with_capacity(10);
            push_date(&mut s, d as i64);
            s
        }
    })
}

/// `time` (microseconds since midnight).
pub fn time(raw: &[u8]) -> Result<String, String> {
    let mut s = String::with_capacity(15);
    push_time(&mut s, be_i64(raw)?);
    Ok(s)
}

/// `timestamp` / `timestamptz` (microseconds since 2000-01-01 00:00, UTC for `timestamptz`).
pub fn timestamp(raw: &[u8], utc: bool) -> Result<String, String> {
    let micros = be_i64(raw)?;
    Ok(match micros {
        i64::MAX => "infinity".into(),
        i64::MIN => "-infinity".into(),
        m => {
            let mut s = String::with_capacity(27);
            push_date(&mut s, m.div_euclid(MICROS_PER_DAY));
            s.push('T');
            push_time(&mut s, m.rem_euclid(MICROS_PER_DAY));
            if utc {
                s.push('Z');
            }
            s
        }
    })
}

/// `money`: cents as an `i64`.
pub fn money(raw: &[u8]) -> Result<String, String> {
    let cents = be_i64(raw)?;
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    Ok(format!("{sign}{}.{:02}", abs / 100, abs % 100))
}

/// `interval`: microseconds (`i64`), days (`i32`), months (`i32`), in PostgreSQL's default text
/// style (`EncodeInterval` with `INTSTYLE_POSTGRES`): each non-zero year / month / day field,
/// a `+` on a positive field after a negative one, then the time unless it is zero and
/// something was printed.
pub fn interval(raw: &[u8]) -> Result<String, String> {
    if raw.len() != 16 {
        return Err("bad interval".to_string());
    }
    let micros = be_i64(&raw[..8])?;
    let days = i32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]) as i64;
    let months = i32::from_be_bytes([raw[12], raw[13], raw[14], raw[15]]) as i64;
    let mut s = String::new();
    let mut negative_before = false;
    for (value, unit) in [(months / 12, "year"), (months % 12, "mon"), (days, "day")] {
        if value == 0 {
            continue;
        }
        let sep = if s.is_empty() { "" } else { " " };
        let plus = if negative_before && value > 0 {
            "+"
        } else {
            ""
        };
        let plural = if value == 1 { "" } else { "s" };
        let _ = write!(s, "{sep}{plus}{value} {unit}{plural}");
        negative_before |= value < 0;
    }
    if s.is_empty() || micros != 0 {
        let sep = if s.is_empty() { "" } else { " " };
        let sign = match (micros < 0, negative_before) {
            (true, _) => "-",
            (false, true) => "+",
            (false, false) => "",
        };
        let abs = micros.unsigned_abs();
        let (secs, frac) = (abs / 1_000_000, abs % 1_000_000);
        let _ = write!(
            s,
            "{sep}{sign}{:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        );
        if frac != 0 {
            let digits = format!("{frac:06}");
            s.push('.');
            s.push_str(digits.trim_end_matches('0'));
        }
    }
    Ok(s)
}

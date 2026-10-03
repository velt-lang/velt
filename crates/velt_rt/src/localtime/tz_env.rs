//! `TZ` on Windows, where the C library doesn't read it: the values that name UTC or a fixed
//! offset, which need no time-zone database. Node (ICU) also understands region names
//! (`Europe/Berlin`); for those, and anything else, Velt keeps the system zone.

/// The fixed offset, in seconds east of UTC, that `tz` names: UTC under its usual names
/// (`UTC`, `Etc/UTC`, `GMT`, `UTC0`, ...) or `Etc/GMT±N`, whose sign is inverted by convention
/// (`Etc/GMT-2` is UTC+2). `None` for anything else.
pub(super) fn fixed_offset(tz: &str) -> Option<i32> {
    // POSIX allows a leading `:` ("implementation-defined" form).
    let tz = tz.strip_prefix(':').unwrap_or(tz);
    let name = tz.strip_prefix("Etc/").unwrap_or(tz);
    const UTC: &[&str] = &[
        "UTC",
        "UCT",
        "GMT",
        "UTC0",
        "GMT0",
        "GMT+0",
        "GMT-0",
        "Zulu",
        "Universal",
        "Greenwich",
    ];
    if UTC.contains(&name) {
        return Some(0);
    }
    let hours = tz.strip_prefix("Etc/GMT")?;
    let (sign, digits) = match hours.as_bytes().first()? {
        b'+' => (-1, &hours[1..]),
        b'-' => (1, &hours[1..]),
        _ => return None,
    };
    // tzdata spells the hours without a leading zero: `Etc/GMT+05` is no zone.
    let well_formed = matches!(digits.len(), 1 | 2)
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !(digits.len() == 2 && digits.starts_with('0'));
    if !well_formed {
        return None;
    }
    let n: i32 = digits.parse().ok()?;
    let in_range = if sign < 0 { n <= 12 } else { n <= 14 };
    in_range.then_some(sign * n * 3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_and_fixed_offsets() {
        for utc in [
            "GMT+0",
            "GMT-0",
            "UTC",
            "Etc/UTC",
            "GMT",
            "Etc/GMT",
            "UTC0",
            ":UTC",
            "Etc/GMT+0",
            "Etc/GMT-0",
        ] {
            assert_eq!(fixed_offset(utc), Some(0), "{utc}");
        }
        assert_eq!(fixed_offset("Etc/GMT-2"), Some(2 * 3600));
        assert_eq!(fixed_offset("Etc/GMT+5"), Some(-5 * 3600));
        assert_eq!(fixed_offset("Etc/GMT-14"), Some(14 * 3600));
        assert_eq!(fixed_offset("Etc/GMT+12"), Some(-12 * 3600));
        for other in [
            "",
            "Europe/Berlin",
            "Etc/GMT+13",
            "Etc/GMT-15",
            "Etc/GMT+",
            "Etc/GMT+1a",
            "Etc/GMT+05",
            "Etc/GMT-00",
            "CET-1CEST",
            "UTC+2",
        ] {
            assert_eq!(fixed_offset(other), None, "{other}");
        }
    }
}

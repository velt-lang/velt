//! `std/datetime` local time: the offset of the local time zone from UTC at a given instant
//! (daylight saving time included). Everything else about dates is computed in Velt from
//! `Date.now()` and this offset.

#[cfg(any(windows, test))]
mod tz_env;
#[cfg(windows)]
mod windows;

/// Minutes to add to UTC to get local time at `epoch_ms` (e.g. 120 for CEST). Uses the C
/// library's time zone database on Unix and, on Windows, `TZ` when it names UTC or a fixed offset,
/// else the system time zone (with its dynamic, per-year daylight saving rules).
#[no_mangle]
pub extern "C" fn velt_rt_local_offset_minutes(epoch_ms: i64) -> i32 {
    offset_seconds(epoch_ms.div_euclid(1000)) / 60
}

#[cfg(unix)]
fn offset_seconds(secs: i64) -> i32 {
    // `time_t` is 64-bit on every supported target (musl since 1.2 too); the `_` keeps the
    // alias unnamed because libc deprecates it on musl.
    let t: i64 = secs;
    // SAFETY: an all-zero `tm` is a valid value to be overwritten; `localtime_r` is thread-safe.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; `t` has `time_t`'s size and alignment.
    if unsafe { libc::localtime_r(&t as *const i64 as *const _, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i32
}

#[cfg(windows)]
fn offset_seconds(secs: i64) -> i32 {
    windows::offset_seconds(secs)
}

#[cfg(not(any(unix, windows)))]
fn offset_seconds(_secs: i64) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_is_within_a_day() {
        let now = crate::process::velt_rt_date_now();
        assert!(velt_rt_local_offset_minutes(now).abs() < 24 * 60);
        assert!(velt_rt_local_offset_minutes(-1).abs() < 24 * 60);
    }
}

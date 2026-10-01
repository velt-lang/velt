//! The local UTC offset on Windows: the system time zone (`GetDynamicTimeZoneInformation`,
//! read once per process, as the C library caches `TZ` on Unix) applied to the instant with
//! `SystemTimeToTzSpecificLocalTimeEx`, which uses the zone's dynamic per-year DST rules. The
//! offset is the difference between the local and the UTC wall-clock time.
//!
//! Declared here (kernel32 is always linked, see NATIVE_LIBS.md) rather than through a
//! bindings crate: four functions and three plain structs.

use std::sync::OnceLock;

/// `SYSTEMTIME`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

/// `FILETIME`: 100 ns intervals since 1601-01-01 UTC.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

/// `DYNAMIC_TIME_ZONE_INFORMATION` (432 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct DynamicTimeZoneInformation {
    bias: i32,
    standard_name: [u16; 32],
    standard_date: SystemTime,
    standard_bias: i32,
    daylight_name: [u16; 32],
    daylight_date: SystemTime,
    daylight_bias: i32,
    time_zone_key_name: [u16; 128],
    dynamic_daylight_time_disabled: u8,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetDynamicTimeZoneInformation(info: *mut DynamicTimeZoneInformation) -> u32;
    fn SystemTimeToTzSpecificLocalTimeEx(
        zone: *const DynamicTimeZoneInformation,
        utc: *const SystemTime,
        local: *mut SystemTime,
    ) -> i32;
    fn FileTimeToSystemTime(file_time: *const FileTime, system_time: *mut SystemTime) -> i32;
    fn SystemTimeToFileTime(system_time: *const SystemTime, file_time: *mut FileTime) -> i32;
}

/// `GetDynamicTimeZoneInformation`'s failure result.
const TIME_ZONE_ID_INVALID: u32 = u32::MAX;

/// Seconds from 1601-01-01 (the `FILETIME` epoch) to 1970-01-01.
const EPOCH_DIFF_SECS: i64 = 11_644_473_600;

/// The process's time zone, or `None` if Windows cannot report one (then local time is UTC).
fn zone() -> Option<&'static DynamicTimeZoneInformation> {
    static ZONE: OnceLock<Option<DynamicTimeZoneInformation>> = OnceLock::new();
    ZONE.get_or_init(|| {
        // SAFETY: the struct is plain data; all-zero is a valid value to be overwritten.
        let mut info: DynamicTimeZoneInformation = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is valid for writes.
        let id = unsafe { GetDynamicTimeZoneInformation(&mut info) };
        (id != TIME_ZONE_ID_INVALID).then_some(info)
    })
    .as_ref()
}

fn to_file_time(unix_secs: i64) -> Option<FileTime> {
    let ticks = u64::try_from(unix_secs.checked_add(EPOCH_DIFF_SECS)?)
        .ok()?
        .checked_mul(10_000_000)?;
    Some(FileTime {
        low: ticks as u32,
        high: (ticks >> 32) as u32,
    })
}

fn ticks(ft: FileTime) -> i64 {
    ((u64::from(ft.high) << 32) | u64::from(ft.low)) as i64
}

/// Offset in seconds at `unix_secs`; outside `SYSTEMTIME`'s range (before 1601, after 30827)
/// the zone's standard offset.
pub(super) fn offset_seconds(unix_secs: i64) -> i32 {
    let Some(zone) = zone() else {
        return 0;
    };
    let standard = -(zone.bias + zone.standard_bias) * 60;
    let Some(utc_ft) = to_file_time(unix_secs) else {
        return standard;
    };
    let (mut utc, mut local, mut local_ft) = Default::default();
    // SAFETY: every pointer is valid for the call; the functions only read/write these structs.
    let ok = unsafe {
        FileTimeToSystemTime(&utc_ft, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTimeEx(zone, &utc, &mut local) != 0
            && SystemTimeToFileTime(&local, &mut local_ft) != 0
    };
    if !ok {
        return standard;
    }
    ((ticks(local_ft) - ticks(utc_ft)) / 10_000_000) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layout_matches_windows() {
        assert_eq!(std::mem::size_of::<SystemTime>(), 16);
        assert_eq!(std::mem::size_of::<DynamicTimeZoneInformation>(), 432);
    }

    #[test]
    fn offsets_are_whole_minutes_within_a_day() {
        for secs in [0, 1_700_000_000, 1_720_000_000, -86_400 * 365 * 100] {
            let off = offset_seconds(secs);
            assert!(off.abs() < 24 * 3600 && off % 60 == 0, "{secs}: {off}");
        }
    }
}

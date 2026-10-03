//! Local time on WebAssembly: WASI has no time zone and the browser target gets none from its
//! glue, so local time is UTC (`velt:datetime`'s `localParts()`, `Date`'s local getters).

/// Minutes added to UTC to get local time at `epoch_ms`: always 0 here.
#[no_mangle]
pub extern "C" fn velt_rt_local_offset_minutes(_epoch_ms: i64) -> i32 {
    0
}

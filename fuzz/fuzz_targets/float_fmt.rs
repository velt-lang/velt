//! Runtime number formatting matches JavaScript's `String(x)` and round-trips.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    velt_fuzz::numbers::check(data);
});

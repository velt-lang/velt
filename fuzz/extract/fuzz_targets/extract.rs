//! The package manager's archive extractor: no panics; accepted trees are clean.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    velt_fuzz_extract::check(data);
});

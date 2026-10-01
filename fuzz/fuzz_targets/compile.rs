//! Whole front end (load, parse, sema, lowering, VIR verification): no panics, no ICEs.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        velt_fuzz::frontend::check(src);
    }
});

//! Runtime JSON reader/writer: no panics; stringify(parse(x)) is a fixpoint.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    velt_fuzz::json::check(data);
});

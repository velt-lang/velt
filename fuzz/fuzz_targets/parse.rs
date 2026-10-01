//! Parser + formatter: no panics; formatting a parsed file keeps its AST and is idempotent.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        velt_fuzz::syntax::check(src);
    }
});

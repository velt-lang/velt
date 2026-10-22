//! Velt's string encoding (in `velt_text`, shared with std's native libraries); `count_units`
//! reports its scan to the cost tests' work counter (`work.rs`).

pub use velt_text::wtf8::*;

/// UTF-16 code units of `bytes` (see `velt_text::wtf8::count_units`).
pub fn count_units(bytes: &[u8]) -> usize {
    super::work::scanned(bytes.len());
    velt_text::wtf8::count_units(bytes)
}

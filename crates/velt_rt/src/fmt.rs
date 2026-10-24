//! Number formatting as JavaScript prints it (in `velt_text`, shared with std's native libraries),
//! with the debug runtime's check that a formatted value was not read from a freed block.

pub use velt_text::fmt::*;

/// Append the decimal digits of `v`.
pub fn push_i64(out: &mut Vec<u8>, v: i64) {
    crate::freed::check_value(v as u64, "an integer read from a freed block");
    velt_text::fmt::push_i64(out, v)
}

/// Append `v` formatted like JavaScript's `String(v)`.
pub fn push_f64(out: &mut Vec<u8>, v: f64) {
    crate::freed::check_value(v.to_bits(), "a number read from a freed block");
    velt_text::fmt::push_f64(out, v)
}

/// Append `v` as `console.log` prints it (`-0` is `-0`).
pub fn push_inspect_f64(out: &mut Vec<u8>, v: f64) {
    crate::freed::check_value(v.to_bits(), "a number read from a freed block");
    velt_text::fmt::push_inspect_f64(out, v)
}

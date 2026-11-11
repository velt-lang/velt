//! Comparing, and the position translation of the searches written in Velt.
//!
//! `indexOf`, `lastIndexOf`, `includes`, `startsWith` and `endsWith` are Velt code
//! (std/lang/string/search.velt, #2198). Positions count UTF-16 code units (#377 phase 2b): a
//! string that is not ASCII is searched on its WTF-8 bytes, and the positions are translated at
//! entry and exit by the two functions here, on the breadcrumbs (`VeltStr::unit_to_byte`,
//! `byte_to_unit`).

use crate::str::VeltStr;

/// The byte offset of UTF-16 unit `unit` of `s` (clamped to its length), times two, plus one
/// when the unit is the low half of a pair stored as one 4-byte sequence (the offset is then
/// that sequence's).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_unit_to_byte(s: *const VeltStr, unit: u64) -> u64 {
    let pos = (*s).unit_to_byte(usize::try_from(unit).unwrap_or(usize::MAX));
    (pos.byte as u64) << 1 | pos.low_half as u64
}

/// The UTF-16 unit position of byte offset `byte` of `s`, a code point boundary (clamped to its
/// length).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_byte_to_unit(s: *const VeltStr, byte: u64) -> u64 {
    (*s).byte_to_unit(usize::try_from(byte).unwrap_or(usize::MAX)) as u64
}

/// `a == b` on strings: length check, then one `memcmp`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_eq(a: *const VeltStr, b: *const VeltStr) -> u8 {
    let (a, b) = (&*a, &*b);
    (a.as_bytes() == b.as_bytes()) as u8
}

/// Is `s` well-formed UTF-16 (no lone surrogates)? O(1) for ASCII and for a heap string whose
/// lone-surrogate count is known. std's `encodeURIComponent` refuses ill-formed text with it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_is_well_formed(s: *const VeltStr) -> u8 {
    (*s).is_well_formed() as u8
}

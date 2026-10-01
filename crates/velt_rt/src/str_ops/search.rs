//! Searching and comparing: `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `==`.

use super::{ceil_boundary, clamp_position, floor_boundary, text};
use crate::str::VeltStr;

/// `s.indexOf(needle, from)`: byte offset of the first match at or after `from` (clamped to
/// `0..=len`), or -1. An empty needle matches at the clamped `from`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_index_of(
    s: *const VeltStr,
    needle: *const VeltStr,
    from: i64,
) -> i64 {
    let (s, needle) = (text(s), text(needle));
    let from = clamp_position(from, s.len());
    if needle.is_empty() {
        return from as i64;
    }
    // Matches start on char boundaries, so starting at the next boundary loses none.
    let start = ceil_boundary(s, from);
    s[start..].find(needle).map_or(-1, |i| (start + i) as i64)
}

/// `s.lastIndexOf(needle, from)`: byte offset of the last match starting at or before `from`
/// (clamped to `0..=len`; pass `i64::MAX` when JS omits it), or -1.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_last_index_of(
    s: *const VeltStr,
    needle: *const VeltStr,
    from: i64,
) -> i64 {
    let (s, needle) = (text(s), text(needle));
    let from = clamp_position(from, s.len());
    if needle.is_empty() {
        return from as i64;
    }
    if needle.len() > s.len() {
        return -1;
    }
    // A match starting at <= from ends at <= from + needle.len(), on a char boundary.
    let end = floor_boundary(s, from.saturating_add(needle.len()));
    s[..end].rfind(needle).map_or(-1, |i| i as i64)
}

/// `s.includes(needle)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_includes(s: *const VeltStr, needle: *const VeltStr) -> u8 {
    text(s).contains(text(needle)) as u8
}

/// `s.startsWith(prefix)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_starts_with(s: *const VeltStr, prefix: *const VeltStr) -> u8 {
    (*s).as_bytes().starts_with((*prefix).as_bytes()) as u8
}

/// `s.endsWith(suffix)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_ends_with(s: *const VeltStr, suffix: *const VeltStr) -> u8 {
    (*s).as_bytes().ends_with((*suffix).as_bytes()) as u8
}

/// `a == b` on strings: length check, then one `memcmp`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_eq(a: *const VeltStr, b: *const VeltStr) -> u8 {
    let (a, b) = (&*a, &*b);
    (a.len() == b.len() && a.as_bytes() == b.as_bytes()) as u8
}

//! Searching and comparing: `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `==`.
//!
//! Positions count UTF-16 code units (#377 phase 2b). An ASCII string searches its bytes, whose
//! offsets are its positions; another string searches its WTF-8 bytes and translates the
//! positions at entry and exit (`VeltStr::unit_to_byte`, `byte_to_unit`), except for a needle that
//! can match half of a pair, which is searched in code units (`units.rs`).

use super::units::{self, is_half_needle};
use super::{bytes, ceil_boundary, clamp_position, floor_boundary, text};
use crate::str::{wtf8, VeltStr};
use memchr::memmem;

/// `s.indexOf(needle, from)`: position of the first match at or after `from` (clamped to
/// `0..=length`), or -1. An empty needle matches at the clamped `from`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_index_of(
    s: *const VeltStr,
    needle: *const VeltStr,
    from: i64,
) -> i64 {
    if !(*s).is_ascii() {
        return index_of_units(&*s, &*needle, from);
    }
    let (sb, nb) = (bytes(s), bytes(needle));
    let from = clamp_position(from, sb.len());
    if nb.is_empty() {
        return from as i64;
    }
    let start = ceil_boundary(sb, from);
    let found = if nb.len() <= SHORT_NEEDLE {
        find_short(&sb[start..], nb)
    } else {
        match (text(s), text(needle)) {
            (Ok(s), Ok(needle)) => s[start..].find(needle),
            _ => memmem::find(&sb[start..], nb),
        }
    };
    found.map_or(-1, |i| (start + i) as i64)
}

/// Needles this short are found by `memchr` on their first byte and a comparison: a searcher
/// (`str::find`'s Two-Way, `memmem`) costs more to set up than such a search (`url.indexOf("/")`).
const SHORT_NEEDLE: usize = 4;

/// Below this many bytes a byte is found eight at a time ([`find_byte_short`]): `memchr`'s
/// dispatch costs more than such a search (a URL, a header value).
const SHORT_HAY: usize = 32;

/// The first position of `b` in `hay`, which is shorter than [`SHORT_HAY`]: eight bytes per
/// step, with the classic test for a zero byte in `word ^ (b × 0x01…01)`.
fn find_byte_short(hay: &[u8], b: u8) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let pattern = LO * b as u64;
    let mut at = 0;
    while at + 8 <= hay.len() {
        let word = u64::from_le_bytes(hay[at..at + 8].try_into().expect("ICE: 8 bytes"));
        let x = word ^ pattern;
        let zero = x.wrapping_sub(LO) & !x & HI;
        if zero != 0 {
            return Some(at + (zero.trailing_zeros() / 8) as usize);
        }
        at += 8;
    }
    hay[at..].iter().position(|&c| c == b).map(|i| at + i)
}

/// The first position of the non-empty `needle` in `hay`.
fn find_short(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (first, rest) = (needle[0], &needle[1..]);
    if rest.is_empty() && hay.len() < SHORT_HAY {
        return find_byte_short(hay, first);
    }
    let mut at = 0;
    while let Some(i) = memchr::memchr(first, &hay[at..]) {
        let p = at + i;
        if hay[p + 1..].starts_with(rest) {
            return Some(p);
        }
        at = p + 1;
    }
    None
}

/// [`velt_rt_str_index_of`] on a non-ASCII string.
unsafe fn index_of_units(s: &VeltStr, needle: &VeltStr, from: i64) -> i64 {
    let from = clamp_position(from, s.units());
    let (sb, nb) = (s.as_bytes(), needle.as_bytes());
    if nb.is_empty() {
        return from as i64;
    }
    if needle.units() > s.units() - from {
        return -1;
    }
    if is_half_needle(nb) {
        let found = units::find(&units::decode(sb), &units::decode(nb), from);
        return found.map_or(-1, |k| k as i64);
    }
    // Only a half needle can start at the low half of a pair: start after the pair then.
    let pos = s.unit_to_byte(from);
    let start = pos.byte + if pos.low_half { 4 } else { 0 };
    memmem::find(&sb[start..], nb).map_or(-1, |i| s.byte_to_unit(start + i) as i64)
}

/// `s.lastIndexOf(needle, from)`: position of the last match starting at or before `from`
/// (clamped to `0..=length`; pass `i64::MAX` when JS omits it), or -1.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_last_index_of(
    s: *const VeltStr,
    needle: *const VeltStr,
    from: i64,
) -> i64 {
    if !(*s).is_ascii() {
        return last_index_of_units(&*s, &*needle, from);
    }
    let (sb, nb) = (bytes(s), bytes(needle));
    let from = clamp_position(from, sb.len());
    if nb.is_empty() {
        return from as i64;
    }
    if nb.len() > sb.len() {
        return -1;
    }
    // A match starting at <= from ends at <= from + needle.len(), on a code point boundary.
    let end = floor_boundary(sb, from.saturating_add(nb.len()));
    let found = match (text(s), text(needle)) {
        (Ok(s), Ok(needle)) => s[..end].rfind(needle),
        _ => memmem::rfind(&sb[..end], nb),
    };
    found.map_or(-1, |i| i as i64)
}

/// [`velt_rt_str_last_index_of`] on a non-ASCII string.
unsafe fn last_index_of_units(s: &VeltStr, needle: &VeltStr, from: i64) -> i64 {
    let (sb, nb) = (s.as_bytes(), needle.as_bytes());
    let Some(room) = s.units().checked_sub(needle.units()) else {
        return -1;
    };
    let from = clamp_position(from, s.units()).min(room);
    if nb.is_empty() {
        return from as i64;
    }
    if is_half_needle(nb) {
        let found = units::rfind(&units::decode(sb), &units::decode(nb), from);
        return found.map_or(-1, |k| k as i64);
    }
    // A match starting at unit `from` or before starts at its code point or before (inside a
    // pair, the pair's start is the unit before).
    let last = s.unit_to_byte(from).byte;
    let end = (last + nb.len()).min(sb.len());
    memmem::rfind(&sb[..end], nb).map_or(-1, |i| s.byte_to_unit(i) as i64)
}

/// `s.includes(needle)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_includes(s: *const VeltStr, needle: *const VeltStr) -> u8 {
    if !(*s).is_ascii() && is_half_needle(bytes(needle)) {
        return (index_of_units(&*s, &*needle, 0) >= 0) as u8;
    }
    match (text(s), text(needle)) {
        (Ok(s), Ok(needle)) => s.contains(needle) as u8,
        _ => memmem::find(bytes(s), bytes(needle)).is_some() as u8,
    }
}

/// `s.startsWith(prefix)`. A prefix ending with a lone high surrogate also matches the first half
/// of a pair (`"😀".startsWith("\uD83D")`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_starts_with(s: *const VeltStr, prefix: *const VeltStr) -> u8 {
    let (sb, pb) = ((*s).as_bytes(), (*prefix).as_bytes());
    if sb.starts_with(pb) {
        return 1;
    }
    if !wtf8::ends_with_high(pb) {
        return 0;
    }
    // The bytes before the high half match, so `k` is a code point boundary of `s` too.
    let k = pb.len() - 3;
    let ok = k < sb.len() && sb.starts_with(&pb[..k]) && {
        let (cp, n) = wtf8::decode_at(sb, k);
        n == 4 && units::high_of(cp) == wtf8::decode_at(pb, k).0 as u16
    };
    ok as u8
}

/// `s.endsWith(suffix)`. A suffix starting with a lone low surrogate also matches the second half
/// of a pair (`"😀".endsWith("\uDE00")`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_ends_with(s: *const VeltStr, suffix: *const VeltStr) -> u8 {
    let (sb, xb) = ((*s).as_bytes(), (*suffix).as_bytes());
    if sb.ends_with(xb) {
        return 1;
    }
    if !wtf8::starts_with_low(xb) {
        return 0;
    }
    // The bytes after the low half match, so `k` is a code point boundary of `s` too.
    let rest = &xb[3..];
    let ok = sb.len() > rest.len() && sb.ends_with(rest) && {
        let k = sb.len() - rest.len();
        let (cp, n) = wtf8::decode_at(sb, wtf8::start_before(sb, k));
        n == 4 && units::low_of(cp) == wtf8::decode_at(xb, 0).0 as u16
    };
    ok as u8
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

//! JS string methods over `VeltStr` (rt_abi_async.md §12.2): searching, comparing, slicing,
//! splitting, trimming, case mapping, replacing, padding and number parsing.
//!
//! Positions and lengths count UTF-16 code units, as in JavaScript (#377 phase 2b,
//! docs/internals/design/strings.md). An ASCII string (units == bytes, decided by the value) works
//! on its bytes, whose offsets are its positions, as before; another string works on its WTF-8
//! bytes and translates positions at entry and exit (`VeltStr::unit_to_byte`, `byte_to_unit`:
//! breadcrumbs for long heap strings, a scan otherwise). The places where a code unit is half of
//! a pair stored as one 4-byte sequence (slicing between the halves, a needle that starts with a
//! lone low surrogate or ends with a lone high one, `split("")`, `replaceAll("", x)`) go through
//! `units.rs`.
//!
//! A string may hold lone surrogates (#377): [`text`] gives a `&str` only for well-formed text.
//! Searching, slicing, splitting, replacing, padding and repeating work on the WTF-8 bytes
//! (exact, and the seams of a built result join halves of a pair); case mapping keeps lone
//! surrogates as they are; number parsing and collation read the text lossily (one U+FFFD per
//! lone surrogate).
//!
//! Sub-strings of a static/borrowed string borrow from it, exactly like `velt_rt_str_clone` keeps
//! static strings static; a sub-string that is the whole input shares it (one count increment);
//! other sub-strings are owned copies (inline when short).

pub mod case;
pub mod collate;
pub mod fixed;
pub mod number;
pub mod replace;
pub mod search;
pub mod slice;
pub mod split;
mod units;

use crate::str::{VeltStr, Wtf8};
use std::borrow::Cow;

/// The text of a `VeltStr`: a `&str` when it is well-formed, else its WTF-8
/// (`VeltStr::text`).
///
/// # Safety
/// `s` must be a valid `VeltStr`.
#[inline]
unsafe fn text<'a>(s: *const VeltStr) -> Result<&'a str, Wtf8<'a>> {
    (*s).text()
}

/// The text of a `VeltStr`, each lone surrogate read as U+FFFD (number parsing, collation).
///
/// # Safety
/// `s` must be a valid `VeltStr`.
unsafe fn text_lossy<'a>(s: *const VeltStr) -> Cow<'a, str> {
    (*s).text_lossy()
}

/// The WTF-8 bytes of a `VeltStr`.
///
/// # Safety
/// `s` must be a valid `VeltStr`.
#[inline]
unsafe fn bytes<'a>(s: *const VeltStr) -> &'a [u8] {
    (*s).as_bytes()
}

/// `s[start..end]` as a result string: borrowed if `s` is static, the same string (count +1) if
/// it is all of `s`, a slice sharing `s`'s buffer if it is large enough, else a copy (inline when
/// short); see `VeltStr::substring`.
unsafe fn sub_string(s: *const VeltStr, start: usize, end: usize) -> VeltStr {
    (*s).substring(start, end)
}

/// JS `WhiteSpace` + `LineTerminator` (what `trim` and number parsing skip). Unlike Rust's
/// `char::is_whitespace` this includes U+FEFF and excludes U+0085.
pub fn is_js_whitespace(c: char) -> bool {
    // TAB, LF, VT, FF, CR are the contiguous range U+0009..=U+000D.
    const TAB_TO_CR: std::ops::RangeInclusive<char> = '\t'..='\r';
    const EN_QUAD_TO_HAIR_SPACE: std::ops::RangeInclusive<char> = '\u{2000}'..='\u{200a}';
    TAB_TO_CR.contains(&c)
        || EN_QUAD_TO_HAIR_SPACE.contains(&c)
        || matches!(
            c,
            ' ' | '\u{a0}'
                | '\u{1680}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
}

/// Is `i` a code point boundary of the WTF-8 `s` (as `str::is_char_boundary`)?
#[inline]
fn is_boundary(s: &[u8], i: usize) -> bool {
    i == 0 || i >= s.len() || !crate::str::wtf8::is_continuation(s[i])
}

/// Largest code point boundary `<= i` (and `<= s.len()`).
fn floor_boundary(s: &[u8], i: usize) -> usize {
    let mut i = i.min(s.len());
    while !is_boundary(s, i) {
        i -= 1;
    }
    i
}

/// Is the code point `cp` (a surrogate code point is not) JS whitespace?
fn is_js_whitespace_cp(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(is_js_whitespace)
}

/// JS relative index (`slice`): negative counts from the end; result clamped to `0..=len` (a
/// length in code units, or in bytes for an ASCII string).
fn relative_index(i: i64, len: usize) -> usize {
    if i < 0 {
        (len as i64).saturating_add(i).max(0) as usize
    } else {
        (i as u64).min(len as u64) as usize
    }
}

/// JS position argument (`indexOf`, `lastIndexOf`): clamped to `0..=len`.
fn clamp_position(i: i64, len: usize) -> usize {
    i.clamp(0, len as i64) as usize
}

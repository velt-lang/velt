//! JS string methods over `VeltStr` (rt_abi_async.md §12.2): searching, slicing, splitting,
//! trimming, case mapping, replacing, padding and number parsing.
//!
//! POC indexing model: indexes and lengths are **byte offsets** (so they agree with `s.length`).
//! An offset that falls inside a multi-byte character is moved to a character boundary, so every
//! result stays valid UTF-8. Where JS works per UTF-16 code unit (`split("")`, `replaceAll("")`)
//! these functions work per Unicode scalar value.
//!
//! Sub-strings of a static/borrowed string borrow from it, exactly like `velt_rt_str_clone` keeps
//! static strings static; a sub-string that is the whole input shares it (one count increment);
//! other sub-strings are owned copies (inline when short).

pub mod case;
pub mod fixed;
pub mod number;
pub mod replace;
pub mod search;
pub mod slice;
pub mod split;

use crate::str::VeltStr;

/// The text of a `VeltStr`.
///
/// # Safety
/// `s` must be a valid `VeltStr`; its bytes are UTF-8 by the language invariant (every producer
/// validates or builds UTF-8), which is what makes the unchecked conversion sound.
unsafe fn text<'a>(s: *const VeltStr) -> &'a str {
    std::str::from_utf8_unchecked((*s).as_bytes())
}

/// `s[start..end]` as a result string: borrowed if `s` is static/borrowed, shared if it is all
/// of `s`, else an owned copy (see `VeltStr::substring`).
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

/// Largest char boundary `<= i` (and `<= s.len()`).
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i` (and `<= s.len()`).
fn ceil_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// JS relative index (`slice`): negative counts from the end; result clamped to `0..=len`.
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

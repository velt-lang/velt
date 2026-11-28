//! JS string methods over `VeltStr` (rt_abi_async.md §12.2): searching, comparing, slicing,
//! splitting, trimming, case mapping, replacing and padding.
//!
//! Positions and lengths count UTF-16 code units, as in JavaScript (#377 phase 2b,
//! docs/internals/design/strings.md). An ASCII string (units == bytes, decided by the value) works
//! on its bytes, whose offsets are its positions, as before; another string works on its WTF-8
//! bytes and translates positions at entry and exit (`VeltStr::unit_to_byte`, `byte_to_unit`:
//! breadcrumbs for long heap strings, a scan otherwise). The places where a code unit is half of
//! a pair stored as one 4-byte sequence (slicing between the halves, a needle that starts with a
//! lone low surrogate or ends with a lone high one, `split("")`, `replaceAll("", x)`) are std's
//! (std/lang/string/positions.velt, half_pos.velt, #2198, #2816).
//!
//! A string may hold lone surrogates (#377): [`text`] gives a `&str` only for well-formed text.
//! Searching, slicing, splitting, replacing, padding and repeating work on the WTF-8 bytes
//! (exact, and the seams of a built result join halves of a pair); case mapping keeps lone
//! surrogates as they are.
//!
//! Sub-strings of a static/borrowed string borrow from it, exactly like `velt_rt_str_clone` keeps
//! static strings static; a sub-string that is the whole input shares it (one count increment);
//! other sub-strings are owned copies (inline when short).

pub mod case;
pub mod search;
pub mod slice;

use crate::str::{VeltStr, Wtf8};

/// The text of a `VeltStr`: a `&str` when it is well-formed, else its WTF-8
/// (`VeltStr::text`).
///
/// # Safety
/// `s` must be a valid `VeltStr`.
#[inline]
unsafe fn text<'a>(s: *const VeltStr) -> Result<&'a str, Wtf8<'a>> {
    (*s).text()
}

/// `s[start..end]` as a result string: borrowed if `s` is static, the same string (count +1) if
/// it is all of `s`, a slice sharing `s`'s buffer if it is large enough, else a copy (inline when
/// short); see `VeltStr::substring`.
unsafe fn sub_string(s: *const VeltStr, start: usize, end: usize) -> VeltStr {
    (*s).substring(start, end)
}

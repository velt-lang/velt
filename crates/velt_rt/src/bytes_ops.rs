//! A range of a `u8[]` as a string (std/prelude/bytes.velt: `utf8Slice`, `toString` of a range).
//! Making, copying and slicing byte arrays are Velt code (std/lang/bytes/raw.velt), and so are
//! searching, filling and concatenating (std/lang/bytes).
//!
//! Positions follow JS: relative indices (negative counts from the end) clamped to the array.

use crate::bytes::VeltBytes;
use crate::result::{invalid_utf8, IoResult, VeltErr};
use crate::str::VeltStr;

/// JS relative index: negative counts from `len`, then clamped to `0..=len`. Clamped in `i64`
/// before converting, so an index beyond a 32-bit `usize` (wasm32) isn't truncated.
fn relative(i: i64, len: usize) -> usize {
    // A Rust allocation is at most `isize::MAX` bytes, so `len` fits in an `i64`.
    let n = len as i64;
    (if i < 0 { n + i.max(-n) } else { i.min(n) }) as usize
}

/// The bytes of `b` in `[start, end)` (relative indices).
unsafe fn range<'a>(b: *const VeltBytes, start: i64, end: i64) -> &'a [u8] {
    let s = (*b).as_bytes();
    let (start, end) = (relative(start, s.len()), relative(end, s.len()));
    &s[start..end.max(start)]
}

/// The bytes in `[start, end)` (relative indices) as a new string, validating UTF-8
/// (`INVALID_DATA` on failure): `velt_rt_bytes_to_str` of a slice, without making the slice.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_range_to_str(
    b: *const VeltBytes,
    start: i64,
    end: i64,
    out: *mut IoResult<VeltStr>,
) {
    let bytes = range(b, start, end);
    // One pass to check, one to copy: ASCII is UTF-8 with a unit a byte, and UTF-8 text holds
    // no lone surrogate, so only its units are counted (#1717).
    let r = if bytes.is_ascii() {
        // SAFETY: ASCII is UTF-8, and its UTF-16 length is its byte length.
        IoResult::ok(VeltStr::from_text_counted(
            std::str::from_utf8_unchecked(bytes),
            bytes.len(),
        ))
    } else {
        match std::str::from_utf8(bytes) {
            Ok(s) => IoResult::ok(VeltStr::from_text(s)),
            Err(_) => IoResult::err(VeltErr::from_io(&invalid_utf8("byte buffer"))),
        }
    };
    r.write_to(out);
}

/// The bytes in `[start, end)` (relative indices) as a new string, an invalid sequence as
/// U+FFFD (one per maximal invalid subpart, as `TextDecoder`): Node's `buf.utf8Slice`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bytes_range_to_str_lossy(
    b: *const VeltBytes,
    start: i64,
    end: i64,
    out: *mut VeltStr,
) {
    // UTF-8 text (repaired or not) holds no lone surrogate: only its units are counted.
    out.write(text_of(range(b, start, end)));
}

/// `bytes` decoded as UTF-8, invalid sequences as U+FFFD (as JS decodes). Valid text, nearly
/// all of it, is only validated (ASCII, the most common, only scanned) and copied once, where
/// [`String::from_utf8_lossy`] would walk it chunk by chunk at several times the cost.
pub(crate) fn text_of(bytes: &[u8]) -> VeltStr {
    if bytes.is_ascii() {
        // SAFETY: ASCII is UTF-8 with one UTF-16 unit per byte.
        return unsafe {
            VeltStr::from_text_counted(std::str::from_utf8_unchecked(bytes), bytes.len())
        };
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => VeltStr::from_text(s),
        Err(_) => VeltStr::from_text(&String::from_utf8_lossy(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn arr(v: &[u8]) -> VeltBytes {
        VeltBytes::from_vec(v.to_vec())
    }

    #[test]
    fn range_to_str_counts_units_and_validates() {
        let long_ascii = "a".repeat(40);
        let mixed = format!("{long_ascii}é𝄞x");
        for text in ["", "ab", long_ascii.as_str(), "é", mixed.as_str()] {
            let mut b = arr(text.as_bytes());
            let mut out = MaybeUninit::<IoResult<VeltStr>>::uninit();
            let mut lossy = MaybeUninit::<VeltStr>::uninit();
            // SAFETY: a valid owned buffer; the results are read once and dropped.
            unsafe {
                velt_rt_bytes_range_to_str(&b, 0, i64::MAX, out.as_mut_ptr());
                let r = out.assume_init();
                assert_eq!(r.err.code, 0, "{text:?}");
                let mut s = r.value.assume_init();
                assert_eq!(s.text().ok(), Some(text));
                assert_eq!(s.units(), text.encode_utf16().count(), "{text:?}");
                crate::str::velt_rt_str_drop(&mut s);
                velt_rt_bytes_range_to_str_lossy(&b, 0, i64::MAX, lossy.as_mut_ptr());
                let mut l = lossy.assume_init();
                assert_eq!(l.units(), text.encode_utf16().count(), "{text:?}");
                crate::str::velt_rt_str_drop(&mut l);
                drop(b.take_vec());
            }
        }
        let mut bad = arr(&[b'a', 0xff, b'b']);
        let mut out = MaybeUninit::<IoResult<VeltStr>>::uninit();
        let mut lossy = MaybeUninit::<VeltStr>::uninit();
        // SAFETY: as above.
        unsafe {
            velt_rt_bytes_range_to_str(&bad, 0, 3, out.as_mut_ptr());
            assert_ne!(out.assume_init_ref().err.code, 0);
            velt_rt_bytes_range_to_str_lossy(&bad, 0, 3, lossy.as_mut_ptr());
            let mut l = lossy.assume_init();
            assert_eq!(l.text().ok(), Some("a\u{FFFD}b"));
            assert_eq!(l.units(), 3);
            crate::str::velt_rt_str_drop(&mut l);
            drop(bad.take_vec());
        }
    }

    #[test]
    fn relative_indices_clamp_before_converting() {
        for (i, want) in [
            (0, 0),
            (3, 3),
            (99, 10),
            (-3, 7),
            (-99, 0),
            (1 << 32, 10),
            (-(1 << 32), 0),
            ((1 << 32) + 3, 10),
            (-(1 << 32) - 3, 0),
            (i64::MAX, 10),
            (i64::MIN, 0),
        ] {
            assert_eq!(relative(i, 10), want, "relative({i}, 10)");
        }
    }
}

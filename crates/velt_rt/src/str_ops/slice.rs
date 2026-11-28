//! Sub-strings and code units: a byte range of a string (`slice`, `substring`, `charAt` and `at`
//! translate their positions in Velt, std/lang/string/positions.velt `sliceWide`, and cut here,
//! #2816), and `String.fromCharCode` (`charCodeAt` past an ASCII string, `repeat`, `padStart` and
//! `padEnd` are Velt code, std/lang/string/positions.velt and fill.velt, #2198).

use super::sub_string;
use crate::str::wtf8;
use crate::str::VeltStr;

/// The `units` of [`velt_rt_str_byte_slice`] of a caller that doesn't know the piece's UTF-16
/// length: the runtime counts it.
pub const UNCOUNTED: u64 = u64::MAX;

/// Bytes `start..end` of `s`'s WTF-8 as a string, shared with `s` or copied as
/// `VeltStr::substring` decides: `utf8Substring` (velt/encoding), `trim`, `split`, and `slice`
/// past std's translation of its positions. `units` is the piece's UTF-16 length, which a caller
/// that translated the positions knows (no count then), or [`UNCOUNTED`]. The caller (std) has
/// checked `start <= end <= byte length` and that both are code point boundaries.
///
/// # Safety
/// `s` must be valid; the range and `units` as above.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_byte_slice(
    s: *const VeltStr,
    start: u64,
    end: u64,
    units: u64,
    out: *mut VeltStr,
) {
    let (start, end) = (start as usize, end as usize);
    out.write(if units == UNCOUNTED {
        sub_string(s, start, end)
    } else {
        (*s).substring_units(start, end, units as usize)
    });
}

/// `s.charCodeAt(i)` over the runtime's translation, for its tests: the UTF-16 code unit at `i`
/// (the high or the low surrogate of a pair), or -1 when out of range. The method itself is std's
/// (`charCodeAtWide`, std/lang/string/positions.velt, #2816).
///
/// # Safety
/// `s` must be valid.
#[cfg(test)]
pub(crate) unsafe fn char_code_at(s: &VeltStr, i: i64) -> i64 {
    match usize::try_from(i) {
        Ok(i) if i < s.units() => {
            let bytes = s.as_bytes();
            if s.is_ascii() {
                return bytes[i] as i64;
            }
            let pos = s.unit_to_byte(i);
            let (cp, _) = wtf8::decode_at(bytes, pos.byte);
            (match (cp >= 0x10000, pos.low_half) {
                (false, _) => cp,
                (true, false) => 0xD800 + ((cp - 0x10000) >> 10),
                (true, true) => 0xDC00 + ((cp - 0x10000) & 0x3FF),
            }) as i64
        }
        _ => -1,
    }
}

/// `s.slice(a, b)` (`0 <= a <= b <= s.units()`) as std's `sliceWide` cuts it when neither end is
/// inside a pair: both ends translated, then [`velt_rt_str_byte_slice`] with the piece's units.
/// For the runtime's tests.
///
/// # Safety
/// `s` must be valid.
#[cfg(test)]
pub(crate) unsafe fn unit_slice(s: &VeltStr, a: usize, b: usize) -> VeltStr {
    let (pa, pb) = (s.unit_to_byte(a), s.unit_to_byte(b));
    assert!(!pa.low_half && !pb.low_half, "a test cuts inside a pair");
    let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
    velt_rt_str_byte_slice(
        s,
        pa.byte as u64,
        pb.byte as u64,
        (b - a) as u64,
        out.as_mut_ptr(),
    );
    out.assume_init()
}

/// One static byte per ASCII character, so `fromCharCode` of ASCII never allocates.
static ASCII: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = i as u8;
        i += 1;
    }
    t
};

/// `String.fromCharCode(code)`: `code` is reduced to a UTF-16 code unit (`ToUint16`); a
/// surrogate is a one-unit string holding that lone surrogate, as in JS. One unit has no seam to
/// join.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_char_code(code: i64, out: *mut VeltStr) {
    let unit = code as u16;
    if unit < 0x80 {
        out.write(VeltStr::from_static(
            &ASCII[unit as usize..unit as usize + 1],
        ));
        return;
    }
    let mut buf = [0u8; 4];
    out.write(VeltStr::from_bytes(wtf8::encode(unit as u32, &mut buf)));
}

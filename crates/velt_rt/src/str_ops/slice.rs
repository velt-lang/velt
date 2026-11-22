//! Sub-strings and code units: `slice`, `charCodeAt`, `String.fromCharCode` (`repeat`, `padStart`
//! and `padEnd` are Velt code, std/lang/string/fill.velt, #2198). Positions and lengths count UTF-16 code units (#377 phase 2b).

use super::units;
use super::{relative_index, sub_string};
use crate::str::wtf8;
use crate::str::VeltStr;

/// `s.slice(start, end)` with JS rules (negative = from the end, clamped, empty if
/// `start >= end`); pass `i64::MAX` for an omitted `end`. An end between the two halves of a
/// pair keeps that half as a lone surrogate (`"😀".slice(0, 1)` is `"\uD83D"`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_slice(
    s: *const VeltStr,
    start: i64,
    end: i64,
    out: *mut VeltStr,
) {
    let st = &*s;
    if st.is_ascii() {
        let n = st.len();
        out.write(sub_string(
            s,
            relative_index(start, n),
            relative_index(end, n),
        ));
        return;
    }
    let n = st.units();
    let (a, b) = (relative_index(start, n), relative_index(end, n));
    out.write(if a < b {
        units::slice(st, a, b)
    } else {
        VeltStr::empty()
    });
}

/// `utf8Substring(s, start, end)` (velt/encoding): the text of bytes `start..end` of `s`'s
/// WTF-8, shared with `s` or copied as `slice` decides. The caller (std) has checked
/// `start <= end <= byte length` and that both are code point boundaries.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_byte_slice(
    s: *const VeltStr,
    start: u64,
    end: u64,
    out: *mut VeltStr,
) {
    out.write(sub_string(s, start as usize, end as usize));
}

/// `s.charCodeAt(i)`: the UTF-16 code unit at `i` (the high or the low surrogate of a pair), or
/// -1 when out of range (JS: NaN). Compiled code reads ASCII strings inline and calls this for
/// the others.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_char_code_at(s: *const VeltStr, i: i64) -> i64 {
    let s = &*s;
    match usize::try_from(i) {
        Ok(i) if i < s.units() => {
            let bytes = s.as_bytes();
            if s.is_ascii() {
                bytes[i] as i64
            } else {
                units::unit_at(bytes, s.unit_to_byte(i)) as i64
            }
        }
        _ => -1,
    }
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

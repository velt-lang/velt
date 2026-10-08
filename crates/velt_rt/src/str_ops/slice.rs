//! Building sub-strings and new strings: `slice`, `charCodeAt`, `String.fromCharCode`, `repeat`,
//! `padStart`, `padEnd`. Positions and lengths count UTF-16 code units (#377 phase 2b).

use super::units;
use super::{bytes, relative_index, sub_string};
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

/// `s.repeat(n)`: 1 = ok; 0 = JS `RangeError` (negative count or a result longer than a string
/// can be), with `*out` empty.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_repeat(s: *const VeltStr, n: i64, out: *mut VeltStr) -> u8 {
    let t = bytes(s);
    let total = usize::try_from(n)
        .ok()
        .and_then(|n| n.checked_mul(t.len()))
        .filter(|&total| total <= crate::str::MAX_LEN);
    let Some(total) = total else {
        out.write(VeltStr::empty());
        return 0;
    };
    let result = match n {
        _ if total == 0 => VeltStr::empty(),
        1 => sub_string(s, 0, t.len()),
        // Copies of a string that ends with a high surrogate and starts with a low one join
        // into a pair at every seam.
        _ if wtf8::ends_with_high(t) && wtf8::starts_with_low(t) => {
            let mut v = Vec::with_capacity(total);
            for _ in 0..n {
                wtf8::push_joining(&mut v, t);
            }
            VeltStr::from_vec(v)
        }
        _ => VeltStr::from_vec(t.repeat(n as usize)),
    };
    out.write(result);
    1
}

/// Shared body of `padStart` / `padEnd`: fill up to `target` code units with repetitions of
/// `fill`, the last one cut to the units still missing (a cut between the halves of a pair keeps
/// the high half as a lone surrogate, as in JS). The pieces join at their seams (fill against
/// fill, fill against the string) when a high surrogate meets a low one.
unsafe fn pad(s: *const VeltStr, target: i64, fill: *const VeltStr, at_start: bool) -> VeltStr {
    let (st, ft) = (&*s, &*fill);
    let (t, f) = (st.as_bytes(), ft.as_bytes());
    let missing = usize::try_from(target)
        .unwrap_or(0)
        .saturating_sub(st.units());
    if missing == 0 || f.is_empty() {
        return sub_string(s, 0, t.len());
    }
    let per_unit = f.len().div_ceil(ft.units());
    let mut v = Vec::with_capacity(t.len().saturating_add(missing.saturating_mul(per_unit)));
    if !at_start {
        v.extend_from_slice(t);
    }
    for _ in 0..missing / ft.units() {
        wtf8::push_joining(&mut v, f);
    }
    let rest = missing % ft.units();
    if rest > 0 {
        wtf8::push_joining(&mut v, &units::prefix_to(f, ft.unit_to_byte(rest)));
    }
    if at_start {
        wtf8::push_joining(&mut v, t);
    }
    VeltStr::from_vec(v)
}

/// `s.padStart(target, fill)` (lengths in code units; pass `" "` when JS omits `fill`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_pad_start(
    s: *const VeltStr,
    target: i64,
    fill: *const VeltStr,
    out: *mut VeltStr,
) {
    out.write(pad(s, target, fill, true));
}

/// `s.padEnd(target, fill)` (lengths in code units; pass `" "` when JS omits `fill`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_pad_end(
    s: *const VeltStr,
    target: i64,
    fill: *const VeltStr,
    out: *mut VeltStr,
) {
    out.write(pad(s, target, fill, false));
}

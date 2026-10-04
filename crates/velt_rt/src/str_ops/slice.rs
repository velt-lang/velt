//! Building sub-strings and new strings: `slice`, `charCodeAt`, `String.fromCharCode`, `repeat`,
//! `padStart`, `padEnd`.

use super::{bytes, floor_boundary, relative_index, sub_string};
use crate::str::wtf8;
use crate::str::VeltStr;

/// `s.slice(start, end)` with JS rules (negative = from the end, clamped, empty if
/// `start >= end`); pass `i64::MAX` for an omitted `end`. Offsets inside a character move back
/// to its first byte.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_slice(
    s: *const VeltStr,
    start: i64,
    end: i64,
    out: *mut VeltStr,
) {
    let t = bytes(s);
    let start = floor_boundary(t, relative_index(start, t.len()));
    let end = floor_boundary(t, relative_index(end, t.len()));
    out.write(sub_string(s, start, end));
}

/// `s.charCodeAt(i)`: the byte at offset `i` (POC), or -1 when out of range (JS: NaN).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_char_code_at(s: *const VeltStr, i: i64) -> i64 {
    let bytes = (*s).as_bytes();
    usize::try_from(i)
        .ok()
        .and_then(|i| bytes.get(i))
        .map_or(-1, |&b| b as i64)
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

/// `String.fromCharCode(code)`: `code` is reduced to a UTF-16 code unit (`ToUint16`) and
/// encoded; a surrogate code unit still becomes U+FFFD, as before #377 phase 2b (which lets
/// Velt code make lone surrogates). One unit has no seam to join.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_char_code(code: i64, out: *mut VeltStr) {
    let unit = code as u16;
    if unit < 0x80 {
        out.write(VeltStr::from_static(
            &ASCII[unit as usize..unit as usize + 1],
        ));
        return;
    }
    let c = char::from_u32(unit as u32).map_or(0xFFFD, u32::from);
    let mut buf = [0u8; 4];
    out.write(VeltStr::from_bytes(wtf8::encode(c, &mut buf)));
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

/// Shared body of `padStart` / `padEnd`: fill up to `target` bytes with repetitions of `fill`,
/// the last one cut at a code point boundary (so the result may be up to 3 bytes short). The
/// pieces join at their seams (fill against fill, fill against the string) when a high
/// surrogate meets a low one.
unsafe fn pad(s: *const VeltStr, target: i64, fill: *const VeltStr, at_start: bool) -> VeltStr {
    let (t, f) = (bytes(s), bytes(fill));
    let missing = usize::try_from(target).unwrap_or(0).saturating_sub(t.len());
    if missing == 0 || f.is_empty() {
        return sub_string(s, 0, t.len());
    }
    let mut v = Vec::with_capacity(t.len() + missing);
    if !at_start {
        v.extend_from_slice(t);
    }
    for _ in 0..missing / f.len() {
        wtf8::push_joining(&mut v, f);
    }
    wtf8::push_joining(&mut v, &f[..floor_boundary(f, missing % f.len())]);
    if at_start {
        wtf8::push_joining(&mut v, t);
    }
    VeltStr::from_vec(v)
}

/// `s.padStart(target, fill)` (lengths in bytes; pass `" "` when JS omits `fill`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_pad_start(
    s: *const VeltStr,
    target: i64,
    fill: *const VeltStr,
    out: *mut VeltStr,
) {
    out.write(pad(s, target, fill, true));
}

/// `s.padEnd(target, fill)` (lengths in bytes; pass `" "` when JS omits `fill`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_pad_end(
    s: *const VeltStr,
    target: i64,
    fill: *const VeltStr,
    out: *mut VeltStr,
) {
    out.write(pad(s, target, fill, false));
}

//! Building sub-strings and new strings: `slice`, `charCodeAt`, `String.fromCharCode`, `repeat`,
//! `padStart`, `padEnd`.

use super::{floor_boundary, relative_index, sub_string, text};
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
    let t = text(s);
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
/// UTF-8 encoded; a lone surrogate (unrepresentable in UTF-8) becomes U+FFFD.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_char_code(code: i64, out: *mut VeltStr) {
    let unit = code as u16;
    if unit < 0x80 {
        out.write(VeltStr::from_static(
            &ASCII[unit as usize..unit as usize + 1],
        ));
        return;
    }
    let c = char::from_u32(unit as u32).unwrap_or('\u{FFFD}');
    let mut buf = [0u8; 4];
    out.write(VeltStr::from_vec(
        c.encode_utf8(&mut buf).as_bytes().to_vec(),
    ));
}

/// `s.repeat(n)`: 1 = ok; 0 = JS `RangeError` (negative count or a result longer than a string
/// can be), with `*out` empty.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_repeat(s: *const VeltStr, n: i64, out: *mut VeltStr) -> u8 {
    let t = text(s);
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
        _ => VeltStr::from_vec(t.repeat(n as usize).into_bytes()),
    };
    out.write(result);
    1
}

/// Shared body of `padStart` / `padEnd`: fill up to `target` bytes with repetitions of `fill`,
/// the last one cut at a character boundary (so the result may be up to 3 bytes short).
unsafe fn pad(s: *const VeltStr, target: i64, fill: *const VeltStr, at_start: bool) -> VeltStr {
    let (t, f) = (text(s), text(fill));
    let missing = usize::try_from(target).unwrap_or(0).saturating_sub(t.len());
    if missing == 0 || f.is_empty() {
        return sub_string(s, 0, t.len());
    }
    let mut v = Vec::with_capacity(t.len() + missing);
    if !at_start {
        v.extend_from_slice(t.as_bytes());
    }
    for _ in 0..missing / f.len() {
        v.extend_from_slice(f.as_bytes());
    }
    v.extend_from_slice(&f.as_bytes()[..floor_boundary(f, missing % f.len())]);
    if at_start {
        v.extend_from_slice(t.as_bytes());
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

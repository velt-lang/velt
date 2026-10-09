//! `VeltStrBuf`: a growable string builder with the `VeltStr` layout (`velt_rt_strbuf_*`).
//!
//! The compiler uses it for template literals, chains of `+`, and generated `JSON.stringify` /
//! print glue, so building a string of `n` parts costs O(total length) instead of O(n²).
//! Because the layout is `VeltStr`'s, `finish` is a move (no copy). A builder starts inline and
//! only allocates once the text passes 23 bytes, so short template literals never touch the heap.
//! Any string can be appended to (`s += x`): in place when it is inline with room or the only
//! reference to its heap buffer, otherwise the text moves to a fresh buffer first (strings are
//! immutable values: other copies never see the append).

use crate::fmt;
use crate::json::escape::{push_json_string, push_json_string_counted};
use crate::json::text::{inspect_into, stringify_into};
use crate::json::value::Value;
use crate::str::{Summary, VeltStr};

/// Identical to `VeltStr` (size 24, align 8).
pub type VeltStrBuf = VeltStr;

/// `new StrBuf(cap)`: empty builder with room for `cap` bytes (a hint of at most 23 starts
/// inline, larger ones allocate up front: appending to a heap buffer is the fastest path). The
/// hint is clamped to the largest string: a template sized from its parts adds the widest a
/// number can be, which may exceed it for a result that still fits.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_new(cap: u64, out: *mut VeltStrBuf) {
    let cap = usize::try_from(cap)
        .unwrap_or(usize::MAX)
        .min(crate::str::MAX_CAPACITY);
    out.write(VeltStr::with_capacity(cap));
}

/// Append the bytes of `s` (the caller keeps ownership of `s`; `s` may be the builder itself or
/// lie in its buffer: `push_wtf8` copies such text out before the buffer can move).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_str(buf: *mut VeltStrBuf, s: *const VeltStr) {
    if std::ptr::eq(buf, s) {
        // Copy out first: growing the builder may move the bytes being read.
        let sum = (*s).summary();
        (*buf).push_with_summary(|v| v.extend_from_slice((*s).as_bytes()), |_| Some(sum));
        return;
    }
    (*buf).push_str(&*s);
}

/// Append the bytes at `ptr` (canonical WTF-8: generated code pushes literal text). The low half
/// of `len` is the byte count; the high half may be the UTF-16 unit count, as in a string's `w1`:
/// equal to the byte count, the text is ASCII and needs no scan; 0 means unknown.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_bytes(buf: *mut VeltStrBuf, ptr: *const u8, len: u64) {
    let (bytes, units) = (len as u32 as usize, (len >> 32) as usize);
    if bytes == 0 {
        return;
    }
    let text = std::slice::from_raw_parts(ptr, bytes);
    if units == bytes {
        push_ascii(buf, text);
    } else {
        push_counted(buf, text);
    }
}

/// Append text whose summary is unknown (counted; out of line, off the hot ASCII paths).
#[inline(never)]
unsafe fn push_counted(buf: *mut VeltStrBuf, text: &[u8]) {
    (*buf).push_wtf8(text, None);
}

/// Append ASCII text (numbers, keywords): its summary is known, so nothing is counted.
#[inline(always)]
unsafe fn push_ascii(buf: *mut VeltStrBuf, text: &[u8]) {
    (*buf).push_ascii(text);
}

/// Append a decimal `i64`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_i64(buf: *mut VeltStrBuf, v: i64) {
    let mut b = itoa::Buffer::new();
    push_ascii(buf, b.format(v).as_bytes());
}

/// Append a decimal `u64`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_u64(buf: *mut VeltStrBuf, v: u64) {
    let mut b = itoa::Buffer::new();
    push_ascii(buf, b.format(v).as_bytes());
}

/// Append an `f64` formatted like JS `String(v)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_f64(buf: *mut VeltStrBuf, v: f64) {
    // A whole number below 2^53 (`${i}`) is its integer digits, as fast as an integer's.
    if let Some(i) = fmt::whole(v) {
        return velt_rt_strbuf_push_i64(buf, i);
    }
    (*buf).push_with_summary(|b| fmt::push_f64(b, v), |n| Some(Summary::ascii(n)));
}

/// Append an `f64` the way `console.log` prints it (node's `util.inspect`: `-0` is `-0`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_inspect_f64(buf: *mut VeltStrBuf, v: f64) {
    if v != 0.0 {
        if let Some(i) = fmt::whole(v) {
            return velt_rt_strbuf_push_i64(buf, i);
        }
    }
    (*buf).push_with_summary(|b| fmt::push_inspect_f64(b, v), |n| Some(Summary::ascii(n)));
}

/// Append an `f64` the way `JSON.stringify` does: JS formatting, `null` for NaN/±Infinity.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_json_f64(buf: *mut VeltStrBuf, v: f64) {
    if !v.is_finite() {
        return push_ascii(buf, b"null");
    }
    (*buf).push_with_summary(|b| fmt::push_f64(b, v), |n| Some(Summary::ascii(n)));
}

/// Append `true` / `false`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_bool(buf: *mut VeltStrBuf, v: u8) {
    push_ascii(buf, if v != 0 { b"true" } else { b"false" });
}

/// Append one byte (ASCII punctuation in generated glue: `{`, `,`, `:`, `"`…).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_byte(buf: *mut VeltStrBuf, byte: u8) {
    if byte.is_ascii() {
        push_ascii(buf, &[byte]);
    } else {
        push_counted(buf, &[byte]);
    }
}

/// Append `s` as a JSON string literal: quoted and escaped exactly like `JSON.stringify(s)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_json_str(buf: *mut VeltStrBuf, s: *const VeltStr) {
    // Escaped into scratch space first, so `s` may be the builder itself.
    if (*s).is_ascii() {
        return (*buf).push_with_ascii(|b| push_json_string(b, (*s).as_bytes()));
    }
    // Only ASCII is escaped (into ASCII: one unit per byte added) and lone surrogates (3 bytes
    // and 1 unit each become a 6-byte escape: 3 bytes but 5 units more), so the output has the
    // input's units plus one per added byte plus two per lone surrogate, and none of them.
    let (len, units) = ((*s).len(), (*s).units());
    let lone = std::cell::Cell::new(0);
    (*buf).push_with_summary(
        |b| lone.set(push_json_string_counted(b, (*s).as_bytes())),
        |out| {
            Some(Summary {
                units: units + (out - len) + 2 * lone.get(),
                lone: 0,
            })
        },
    );
}

/// Append `s` as a string inside a container prints in `console.log` (node's `util.inspect`
/// quoting and escaping, `crate::inspect`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_inspect_str(buf: *mut VeltStrBuf, s: *const VeltStr) {
    // Escaped into scratch space first, so `s` may be the builder itself.
    (*buf).push_with(|b| crate::inspect::push_inspect_string(b, (*s).as_bytes()));
}

/// Append `s` as an object key in `console.log` (bare if it is an identifier, else quoted).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_inspect_key(buf: *mut VeltStrBuf, s: *const VeltStr) {
    (*buf).push_with(|b| crate::inspect::push_inspect_key(b, (*s).as_bytes()));
}

/// Append node's `, ... n more items` after the first entries of an array, `Map` or `Set` of
/// which `remaining` more are not shown (`maxArrayLength`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_more(buf: *mut VeltStrBuf, remaining: u64) {
    (*buf).push_with_ascii(|b| crate::inspect::push_more_items(b, remaining));
}

/// Append `JSON.stringify(value)` for a `json.Value` handle (a null handle appends `null`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_json_value(buf: *mut VeltStrBuf, h: *const Value) {
    match h.as_ref() {
        Some(v) => (*buf).push_with(|b| stringify_into(b, v)),
        None => push_ascii(buf, b"null"),
    }
}

/// Append what `console.log` prints for a `json.Value` handle: node's `util.inspect` of the
/// parsed value (`{ a: 1, b: [ 2, 'x' ] }`); a string is raw when `top != 0` (a `console.log`
/// argument) and quoted otherwise. `depth` is node's depth of the value (0 at the top level):
/// containers nested deeper than 2 print as `[Array]` / `[Object]`. A null handle appends `null`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_inspect_json(
    buf: *mut VeltStrBuf,
    h: *const Value,
    top: u8,
    depth: u32,
) {
    match h.as_ref() {
        Some(v) => {
            let start = (*buf).len();
            (*buf).push_with(|b| inspect_into(b, v, top != 0, depth));
            // A top-level value is broken across lines like the glue's (a raw string is not).
            if top != 0 && matches!(v, Value::Array(_) | Value::Object(_)) {
                crate::inspect_layout::velt_rt_strbuf_inspect_layout(buf, start as u64);
            }
        }
        None => push_ascii(buf, b"null"),
    }
}

/// Move the built string to `*out` and leave `*buf` empty (still usable). A short result built
/// in a heap buffer moves inline (the buffer is freed), so copying it later is free.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_finish(buf: *mut VeltStrBuf, out: *mut VeltStr) {
    out.write(buf.replace(VeltStr::empty()).compact());
}

/// Free an unfinished builder (e.g. when an exception abandons a template literal); zeroes it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_drop(buf: *mut VeltStrBuf) {
    (*buf).release();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn new_buf(cap: u64) -> VeltStrBuf {
        let mut b = MaybeUninit::uninit();
        unsafe { velt_rt_strbuf_new(cap, b.as_mut_ptr()) };
        unsafe { b.assume_init() }
    }

    fn finish(mut b: VeltStrBuf) -> String {
        let mut out = MaybeUninit::uninit();
        unsafe { velt_rt_strbuf_finish(&mut b, out.as_mut_ptr()) };
        let mut s = unsafe { out.assume_init() };
        let text = String::from_utf8(unsafe { s.as_bytes() }.to_vec()).unwrap();
        unsafe { s.release() };
        assert!(b.is_empty() && b.is_static());
        text
    }

    #[test]
    fn builder_matches_format() {
        let mut b = new_buf(0);
        let name = VeltStr::from_static("wörld".as_bytes());
        unsafe {
            velt_rt_strbuf_push_bytes(&mut b, b"x=".as_ptr(), 2);
            velt_rt_strbuf_push_i64(&mut b, -42);
            velt_rt_strbuf_push_byte(&mut b, b' ');
            velt_rt_strbuf_push_u64(&mut b, u64::MAX);
            velt_rt_strbuf_push_byte(&mut b, b' ');
            velt_rt_strbuf_push_f64(&mut b, 0.1 + 0.2);
            velt_rt_strbuf_push_byte(&mut b, b' ');
            velt_rt_strbuf_push_f64(&mut b, 1e21);
            velt_rt_strbuf_push_bool(&mut b, 1);
            velt_rt_strbuf_push_str(&mut b, &name);
            velt_rt_strbuf_push_bytes(&mut b, std::ptr::null(), 0);
        }
        let want = format!("x={} {} 0.30000000000000004 1e+21truewörld", -42, u64::MAX);
        assert_eq!(finish(b), want);
    }

    #[test]
    fn short_results_stay_inline() {
        let mut b = new_buf(8);
        unsafe {
            velt_rt_strbuf_push_bytes(&mut b, b"line ".as_ptr(), 5);
            velt_rt_strbuf_push_i64(&mut b, 123456);
            velt_rt_strbuf_push_bytes(&mut b, b": fizz ok".as_ptr(), 9);
        }
        assert!(b.is_inline());
        assert_eq!(finish(b), "line 123456: fizz ok");
    }

    #[test]
    fn json_pushes() {
        let mut b = new_buf(4);
        let s = VeltStr::from_static("q\"uote\\\n\t\u{1}\u{1f}\u{7f}é\u{2028}".as_bytes());
        unsafe {
            velt_rt_strbuf_push_json_str(&mut b, &s);
            velt_rt_strbuf_push_json_f64(&mut b, f64::NAN);
            velt_rt_strbuf_push_json_f64(&mut b, -f64::INFINITY);
            velt_rt_strbuf_push_json_f64(&mut b, 2.5);
            velt_rt_strbuf_push_json_value(&mut b, std::ptr::null());
        }
        assert_eq!(
            finish(b),
            "\"q\\\"uote\\\\\\n\\t\\u0001\\u001f\u{7f}é\u{2028}\"nullnull2.5null"
        );
    }

    #[test]
    fn appending_to_existing_strings() {
        // A static string is copied on first push; the builder may push itself.
        let mut s = VeltStr::from_static(b"ab");
        unsafe { velt_rt_strbuf_push_byte(&mut s, b'c') };
        assert!(!s.is_static());
        unsafe { velt_rt_strbuf_push_i64(&mut s, 7) };
        let sp: *mut VeltStr = &mut s;
        unsafe { velt_rt_strbuf_push_str(sp, sp) };
        // An uncounted view into the builder's own full buffer, which must grow (and may move).
        let mut h = VeltStr::from_bytes(&[b'h'; 40]);
        let view = unsafe { VeltStr::borrowed(h.as_bytes().as_ptr().add(30), 10) };
        unsafe { velt_rt_strbuf_push_str(&mut h, &view) };
        assert_eq!(finish(h), "h".repeat(50));
        assert_eq!(finish(s), "abc7abc7");
        let mut d = new_buf(64);
        unsafe {
            velt_rt_strbuf_push_byte(&mut d, b'z');
            velt_rt_strbuf_drop(&mut d);
        }
        assert!(d.is_static() && d.is_empty());
    }
}

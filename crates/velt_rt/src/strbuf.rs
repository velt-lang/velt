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
use crate::json::escape::push_json_string;
use crate::json::text::{inspect_into, stringify_into};
use crate::json::value::Value;
use crate::str::{Summary, VeltStr};

/// Identical to `VeltStr` (size 24, align 8).
pub type VeltStrBuf = VeltStr;

/// `new StrBuf(cap)`: empty builder with room for `cap` bytes (a hint of at most 23 starts
/// inline, larger ones allocate up front: appending to a heap buffer is the fastest path).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_new(cap: u64, out: *mut VeltStrBuf) {
    out.write(VeltStr::with_capacity(cap as usize));
}

/// Append the bytes of `s` (the caller keeps ownership of `s`; `s` may be the builder itself or
/// lie in its buffer: `push_bytes` copies such text out before the buffer can move).
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
    (*buf).push_wtf8(text, Some(Summary::ascii(text.len())));
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
    (*buf).push_with_summary(|b| fmt::push_f64(b, v), |n| Some(Summary::ascii(n)));
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
    // Escaped into scratch space first, so `s` may be the builder itself. Only ASCII is escaped
    // (into ASCII), so the output has the input's units plus one per added byte.
    let write = |b: &mut Vec<u8>| push_json_string(b, (*s).as_bytes());
    if (*s).is_ascii() {
        return (*buf).push_with_summary(write, |out| Some(Summary::ascii(out)));
    }
    let (len, sum) = ((*s).len(), (*s).summary());
    (*buf).push_with_summary(write, |out| {
        Some(Summary {
            units: sum.units + (out - len),
            lone: sum.lone,
        })
    });
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
/// argument) and quoted otherwise. A null handle appends `null`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_inspect_json(
    buf: *mut VeltStrBuf,
    h: *const Value,
    top: u8,
) {
    match h.as_ref() {
        Some(v) => (*buf).push_with(|b| inspect_into(b, v, top != 0)),
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

/// Objects being printed (`console.log`), innermost last.
struct Printing {
    /// (address, where its text starts in the builder, its `<ref *N>` number once something
    /// refers back to it, else 0).
    stack: Vec<(usize, usize, u32)>,
    /// `<ref *N>` numbers given out for the value being printed.
    refs: u32,
}

thread_local! {
    static PRINTING: std::cell::RefCell<Printing> =
        const { std::cell::RefCell::new(Printing { stack: Vec::new(), refs: 0 }) };
}

/// Start printing the object at `p` (a class instance or a recursive object): 1, or, if it is
/// already being printed (the graph has a cycle), append `[Circular *N]` as node does and
/// return 0 (the caller skips the object).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_enter(buf: *mut VeltStrBuf, p: *const u8) -> u8 {
    PRINTING.with(|s| {
        let Printing { stack, refs } = &mut *s.borrow_mut();
        if let Some(entry) = stack.iter_mut().find(|e| e.0 == p as usize) {
            if entry.2 == 0 {
                *refs += 1;
                entry.2 = *refs;
            }
            (*buf).push_wtf8(format!("[Circular *{}]", entry.2).as_bytes(), None);
            return 0;
        }
        stack.push((p as usize, (*buf).len(), 0));
        1
    })
}

/// Done printing the innermost object started with `velt_rt_strbuf_inspect_enter`: if
/// something inside referred back to it, its text gets node's `<ref *N> ` prefix.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_leave(buf: *mut VeltStrBuf) {
    let done = PRINTING.with(|s| {
        let Printing { stack, refs } = &mut *s.borrow_mut();
        let done = stack.pop();
        if stack.is_empty() {
            *refs = 0;
        }
        done
    });
    if let Some((_, start, n)) = done.filter(|d| d.2 != 0) {
        (*buf).insert_bytes(start, format!("<ref *{n}> ").as_bytes());
    }
}

#[cfg(test)]
mod inspect_cycle_tests {
    use super::*;

    unsafe fn text(b: &VeltStrBuf) -> String {
        String::from_utf8(b.as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn a_reference_back_prints_circular_and_marks_the_target() {
        unsafe {
            let mut b = VeltStr::with_capacity(0);
            let (outer, inner) = (1u8, 2u8);
            b.push_wtf8(b"x: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &outer), 1);
            b.push_wtf8(b"A { b: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &inner), 1);
            b.push_wtf8(b"B { a: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &outer), 0);
            b.push_wtf8(b" }", None);
            velt_rt_strbuf_inspect_leave(&mut b);
            b.push_wtf8(b" }", None);
            velt_rt_strbuf_inspect_leave(&mut b);
            assert_eq!(text(&b), "x: <ref *1> A { b: B { a: [Circular *1] } }");
            // Numbering starts over for the next value printed.
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &inner), 1);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &inner), 0);
            velt_rt_strbuf_inspect_leave(&mut b);
            assert!(text(&b).ends_with("<ref *1> [Circular *1]"));
            b.release();
        }
    }
}

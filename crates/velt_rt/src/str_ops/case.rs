//! Trimming and case mapping: `trim`, `trimStart`, `trimEnd`, `toUpperCase`, `toLowerCase`.

use super::{bytes, is_js_whitespace_cp, sub_string, text};
use crate::str::velt_rt_str_own;
use crate::str::wtf8;
use crate::str::VeltStr;

/// Bytes of JS whitespace at the start of the WTF-8 `t` (a lone surrogate is not whitespace).
fn trimmed_start(t: &[u8]) -> usize {
    let mut i = 0;
    while i < t.len() {
        let (cp, n) = wtf8::decode_at(t, i);
        if !is_js_whitespace_cp(cp) {
            break;
        }
        i += n;
    }
    i
}

/// The end of the WTF-8 `t` without its trailing JS whitespace (at least `start`).
fn trimmed_end(t: &[u8], start: usize) -> usize {
    let mut end = t.len();
    while end > start {
        let at = wtf8::start_before(t, end);
        if !is_js_whitespace_cp(wtf8::decode_at(t, at).0) {
            break;
        }
        end = at;
    }
    end
}

/// `s.trim()`: strips JS whitespace and line terminators from both ends.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_trim(s: *const VeltStr, out: *mut VeltStr) {
    let t = bytes(s);
    let start = trimmed_start(t);
    out.write(sub_string(s, start, trimmed_end(t, start)));
}

/// `s.trimStart()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_trim_start(s: *const VeltStr, out: *mut VeltStr) {
    let t = bytes(s);
    out.write(sub_string(s, trimmed_start(t), t.len()));
}

/// `s.trimEnd()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_trim_end(s: *const VeltStr, out: *mut VeltStr) {
    out.write(sub_string(s, 0, trimmed_end(bytes(s), 0)));
}

/// Case-map `s` with `map` (applied to well-formed text). Lone surrogates have no case and are
/// kept as they are; they also end a word for the context-dependent rules (final sigma), since
/// they are neither cased nor case-ignorable, so mapping the runs between them on their own
/// gives JS's result.
unsafe fn map_case(s: *const VeltStr, map: fn(&str) -> String) -> VeltStr {
    match text(s) {
        Ok(t) => VeltStr::from_vec(map(t).into_bytes()),
        Err(w) => VeltStr::from_vec(wtf8::map_runs(w.as_bytes(), |run, out| {
            out.extend_from_slice(map(run).as_bytes())
        })),
    }
}

fn upper(t: &str) -> String {
    if t.is_ascii() {
        t.to_ascii_uppercase()
    } else {
        t.to_uppercase()
    }
}

fn lower(t: &str) -> String {
    if t.is_ascii() {
        t.to_ascii_lowercase()
    } else {
        t.to_lowercase()
    }
}

/// Whether case mapping leaves `s` as it is: ASCII without a letter `changes` matches (most
/// header names, identifiers and keys are already in the case asked for). Then the result is
/// `s`'s own copy (`velt_rt_str_own`: a heap buffer is shared, strings being immutable), with no
/// new buffer.
unsafe fn unchanged(s: *const VeltStr, changes: fn(&u8) -> bool) -> bool {
    (*s).is_ascii() && !(*s).as_bytes().iter().any(changes)
}

/// `s.toUpperCase()`: full Unicode default case mapping (`ß` → `SS`), like JS.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_to_upper(s: *const VeltStr, out: *mut VeltStr) {
    if unchanged(s, u8::is_ascii_lowercase) {
        return velt_rt_str_own(s, out);
    }
    out.write(map_case(s, upper));
}

/// `s.toLowerCase()`: full Unicode default case mapping (final sigma included), like JS.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_to_lower(s: *const VeltStr, out: *mut VeltStr) {
    if unchanged(s, u8::is_ascii_uppercase) {
        return velt_rt_str_own(s, out);
    }
    out.write(map_case(s, lower));
}

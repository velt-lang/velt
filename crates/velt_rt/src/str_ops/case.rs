//! Trimming and case mapping: `trim`, `trimStart`, `trimEnd`, `toUpperCase`, `toLowerCase`.

use super::{is_js_whitespace, sub_string, text};
use crate::str::VeltStr;

fn trimmed_start(t: &str) -> usize {
    t.len() - t.trim_start_matches(is_js_whitespace).len()
}

fn trimmed_end(t: &str) -> usize {
    t.trim_end_matches(is_js_whitespace).len()
}

/// `s.trim()`: strips JS whitespace and line terminators from both ends.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_trim(s: *const VeltStr, out: *mut VeltStr) {
    let t = text(s);
    out.write(sub_string(s, trimmed_start(t), trimmed_end(t)));
}

/// `s.trimStart()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_trim_start(s: *const VeltStr, out: *mut VeltStr) {
    let t = text(s);
    out.write(sub_string(s, trimmed_start(t), t.len()));
}

/// `s.trimEnd()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_trim_end(s: *const VeltStr, out: *mut VeltStr) {
    out.write(sub_string(s, 0, trimmed_end(text(s))));
}

/// `s.toUpperCase()`: full Unicode default case mapping (`ß` → `SS`), like JS.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_to_upper(s: *const VeltStr, out: *mut VeltStr) {
    let t = text(s);
    let v = if t.is_ascii() {
        t.to_ascii_uppercase()
    } else {
        t.to_uppercase()
    };
    out.write(VeltStr::from_vec(v.into_bytes()));
}

/// `s.toLowerCase()`: full Unicode default case mapping (final sigma included), like JS.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_to_lower(s: *const VeltStr, out: *mut VeltStr) {
    let t = text(s);
    let v = if t.is_ascii() {
        t.to_ascii_lowercase()
    } else {
        t.to_lowercase()
    };
    out.write(VeltStr::from_vec(v.into_bytes()));
}

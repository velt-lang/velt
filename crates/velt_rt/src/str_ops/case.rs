//! Case mapping: `toUpperCase` and `toLowerCase` of text that is not ASCII (std maps ASCII text
//! itself, std/lang/string/case.velt, and trims in Velt, std/lang/string/trim.velt; #2198).

use super::text;
use crate::str::velt_rt_str_own;
use crate::str::wtf8;
use crate::str::VeltStr;

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

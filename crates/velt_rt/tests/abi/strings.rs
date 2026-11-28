//! String methods through the C ABI, checked against tables produced by node (js_table.rs), plus
//! code-unit positions on non-ASCII text checked by hand against node's results.

use super::js_table as js;
use crate::str::{velt_rt_str_drop, VeltStr};
use crate::str_ops::case::*;
use crate::str_ops::search::*;
use crate::str_ops::slice::*;
use std::mem::MaybeUninit;

/// A static (borrowed) input string.
fn lit(s: &'static str) -> VeltStr {
    VeltStr::from_static(s.as_bytes())
}

/// An owned heap string, freed with `velt_rt_str_drop` when it goes out of scope.
struct Heap(VeltStr);

impl std::ops::Deref for Heap {
    type Target = VeltStr;
    fn deref(&self) -> &VeltStr {
        &self.0
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        unsafe { velt_rt_str_drop(&mut self.0) };
    }
}

/// The same text as a heap string (even when short enough to be stored inline).
fn heap(s: &str) -> Heap {
    let mut h = VeltStr::with_capacity(64.max(s.len()));
    unsafe { h.push_wtf8(s.as_bytes(), None) };
    assert!(h.is_heap());
    Heap(h)
}

/// Run an out-param ABI function, return the text (a lone surrogate as U+FFFD, as the tables
/// write it with `toWellFormed()`) and free the result.
fn take(f: impl FnOnce(*mut VeltStr)) -> String {
    let mut out = MaybeUninit::<VeltStr>::uninit();
    f(out.as_mut_ptr());
    let mut s = unsafe { out.assume_init() };
    let text = unsafe { s.to_string_lossy() };
    unsafe { velt_rt_str_drop(&mut s) };
    text
}

/// Call `f` with both a static and a heap version of `s`; both must give the same result.
fn both(s: &'static str, f: impl Fn(&VeltStr) -> String) -> String {
    let a = f(&lit(s));
    let b = f(&heap(s));
    assert_eq!(a, b, "static vs heap input {s:?}");
    a
}

#[test]
fn case_matches_js() {
    for &(s, upper, lower) in js::CASE {
        assert_eq!(
            both(s, |v| take(|o| unsafe { velt_rt_str_to_upper(v, o) })),
            upper,
            "{s:?}"
        );
        assert_eq!(
            both(s, |v| take(|o| unsafe { velt_rt_str_to_lower(v, o) })),
            lower,
            "{s:?}"
        );
    }
}

#[test]
fn char_codes_match_js() {
    for &(s, i, want) in js::CHAR_CODE_AT {
        assert_eq!(unsafe { char_code_at(&lit(s), i) }, want);
    }
    for &(code, want) in js::FROM_CHAR_CODE {
        assert_eq!(
            take(|o| unsafe { velt_rt_str_from_char_code(code, o) }),
            want,
            "{code}"
        );
    }
}

/// The code units of `s` as a static string.
fn wtf8(units: &[u16]) -> VeltStr {
    let bytes = super::utf16_model::wtf8_encode(units);
    VeltStr::from_static(Box::leak(bytes.into_boxed_slice()))
}

#[test]
fn code_unit_positions() {
    // Expected values from node 22. "héllo": é is one unit.
    let s = lit("héllo");
    unsafe {
        assert_eq!(char_code_at(&s, 1), 233);
    }
    // `slice(1, 3)` as std cuts it: bytes 1..4, two units (no count of the piece).
    let mut t = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_byte_slice(&s, 1, 4, 2, t.as_mut_ptr()) };
    let t = unsafe { t.assume_init() };
    assert!(t.is_static());
    assert_eq!((unsafe { t.as_bytes() }, t.units()), ("él".as_bytes(), 2));
    let h = heap("héllo, wörld");
    for units in [UNCOUNTED, 3] {
        let mut t = MaybeUninit::<VeltStr>::uninit();
        unsafe { velt_rt_str_byte_slice(&*h, 1, 5, units, t.as_mut_ptr()) };
        let mut t = unsafe { t.assume_init() };
        assert_eq!((unsafe { t.as_bytes() }, t.units()), ("éll".as_bytes(), 3));
        unsafe { velt_rt_str_drop(&mut t) };
    }
}

#[test]
fn half_pair_paths_match_js() {
    // The examples of docs/internals/design/strings.md "Design" (node 22).
    let (hi, lo) = (wtf8(&[0xD83D]), wtf8(&[0xDE00]));
    let emoji = lit("😀");
    unsafe {
        assert_eq!(char_code_at(&emoji, 0), 0xD83D);
        assert_eq!(char_code_at(&emoji, 1), 0xDE00);
        assert_eq!(char_code_at(&emoji, 2), -1);
    }
    // Gluing the halves back together gives the pair (canonical: its 4-byte sequence).
    let mut glued = MaybeUninit::<VeltStr>::uninit();
    unsafe { crate::str::velt_rt_str_concat(&hi, &lo, glued.as_mut_ptr()) };
    let mut glued = unsafe { glued.assume_init() };
    assert_eq!(unsafe { glued.as_bytes() }, "😀".as_bytes());
    unsafe { velt_rt_str_drop(&mut glued) };
}

#[test]
fn static_inputs_give_borrowed_results() {
    let s = lit("  hello world  ");
    let mut t = MaybeUninit::<VeltStr>::uninit();
    // The piece std's `trim` cuts (std/lang/string/trim.velt): bytes 2..13.
    unsafe { velt_rt_str_byte_slice(&s, 2, 13, UNCOUNTED, t.as_mut_ptr()) };
    let t = unsafe { t.assume_init() };
    let at = |x: &VeltStr| unsafe { x.as_bytes() }.as_ptr();
    assert!(t.is_static() && at(&t) == unsafe { at(&s).add(2) });
    let h = heap("  hello  ");
    let mut t = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_byte_slice(&*h, 2, 7, UNCOUNTED, t.as_mut_ptr()) };
    let mut t = unsafe { t.assume_init() };
    assert!(!t.is_static() && at(&t) != unsafe { at(&h).add(2) });
    drop(h);
    unsafe {
        assert_eq!(t.as_bytes(), b"hello");
        velt_rt_str_drop(&mut t);
    }
}

#[test]
fn str_eq() {
    let eq = |a: &'static str, b: &str| unsafe { velt_rt_str_eq(&lit(a), &*heap(b)) };
    assert_eq!(eq("abc", "abc"), 1);
    assert_eq!(eq("abc", "abd"), 0);
    assert_eq!(eq("abc", "ab"), 0);
    assert_eq!(eq("", ""), 1);
    let s = lit("same");
    assert_eq!(unsafe { velt_rt_str_eq(&s, &s) }, 1);
}

#[test]
fn case_mapping_that_changes_nothing_shares_the_string() {
    let h = heap("content-type");
    let mut out = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_to_lower(&*h, out.as_mut_ptr()) };
    let mut lower = unsafe { out.assume_init() };
    let same = unsafe { lower.as_bytes().as_ptr() == h.as_bytes().as_ptr() };
    assert!(same, "an unchanged heap string is shared, not copied");
    unsafe { velt_rt_str_drop(&mut lower) };
    // A borrowed one is copied (it may point into memory that is freed first).
    let s = lit("ALREADY UPPER, LONGER THAN AN INLINE STRING");
    let mut out = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_to_upper(&s, out.as_mut_ptr()) };
    let mut upper = unsafe { out.assume_init() };
    assert!(!upper.is_static());
    assert_eq!(unsafe { upper.as_bytes() }, unsafe { s.as_bytes() });
    unsafe { velt_rt_str_drop(&mut upper) };
}

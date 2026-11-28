//! String methods through the C ABI, checked against tables produced by node (js_table.rs), plus
//! code-unit positions on non-ASCII text checked by hand against node's results.

use super::js_table as js;
use crate::str::{velt_rt_str_drop, VeltStr};
use crate::str_ops::case::*;
use crate::str_ops::number::*;
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

fn same_f64(got: f64, want_bits: u64) -> bool {
    if want_bits == js::NAN {
        got.is_nan()
    } else {
        got.to_bits() == want_bits
    }
}

#[test]
fn number_matches_js() {
    for &(s, want) in js::TO_NUMBER {
        let got = unsafe { velt_rt_str_to_number(&lit(s)) };
        assert!(
            same_f64(got, want),
            "Number({s:?}) = {got:e}, want {:e}",
            f64::from_bits(want)
        );
    }
}

#[test]
fn slice_matches_js() {
    for &(s, a, b, want) in js::SLICE {
        let got = both(s, |v| take(|o| unsafe { velt_rt_str_slice(v, a, b, o) }));
        assert_eq!(got, want, "{s:?}.slice({a}, {b})");
    }
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

/// Run an out-param ABI function, return the code units of the result and free it.
fn take_units(f: impl FnOnce(*mut VeltStr)) -> Vec<u16> {
    let mut out = MaybeUninit::<VeltStr>::uninit();
    f(out.as_mut_ptr());
    let mut s = unsafe { out.assume_init() };
    let units = super::utf16_model::wtf8_decode(unsafe { s.as_bytes() });
    unsafe { velt_rt_str_drop(&mut s) };
    units
}

/// The code units of `text` with `\u{D800}`-style lone surrogates spliced in at the `~` marks.
fn u(text: &str, lone: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut lone = lone.iter();
    for (k, part) in text.split('~').enumerate() {
        if k > 0 {
            out.push(*lone.next().expect("a lone surrogate per mark"));
        }
        out.extend(part.encode_utf16());
    }
    out
}

/// The code units of `s` as a static string (`"😀"` is given as text, its halves as `u`).
fn wtf8(units: &[u16]) -> VeltStr {
    let bytes = super::utf16_model::wtf8_encode(units);
    VeltStr::from_static(Box::leak(bytes.into_boxed_slice()))
}

#[test]
fn code_unit_positions() {
    // Expected values from node 22. "héllo": é is one unit.
    let s = lit("héllo");
    let slice = |a, b| take(|o| unsafe { velt_rt_str_slice(&s, a, b, o) });
    assert_eq!(slice(1, 3), "él");
    assert_eq!(slice(2, i64::MAX), "llo");
    assert_eq!(slice(0, 2), "hé");
    assert_eq!(slice(-4, -1), "éll");
    unsafe {
        assert_eq!(char_code_at(&s, 1), 233);
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
    let slice = |s: &VeltStr, a, b| take_units(|o| unsafe { velt_rt_str_slice(s, a, b, o) });
    assert_eq!(slice(&emoji, 0, 1), vec![0xD83D]);
    assert_eq!(slice(&lit("x😀y"), 2, i64::MAX), u("~y", &[0xDE00]));
    assert_eq!(slice(&lit("😀😀"), 1, 3), vec![0xDE00, 0xD83D]);
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
    unsafe { velt_rt_str_byte_slice(&s, 2, 13, t.as_mut_ptr()) };
    let t = unsafe { t.assume_init() };
    let at = |x: &VeltStr| unsafe { x.as_bytes() }.as_ptr();
    assert!(t.is_static() && at(&t) == unsafe { at(&s).add(2) });
    let h = heap("  hello  ");
    let mut t = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_byte_slice(&*h, 2, 7, t.as_mut_ptr()) };
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

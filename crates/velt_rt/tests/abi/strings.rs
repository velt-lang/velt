//! String methods through the C ABI, checked against tables produced by node (js_table.rs), plus
//! the documented POC deviations (byte offsets, UTF-8) checked by hand.

use super::js_table as js;
use crate::str::{velt_rt_str_drop, VeltStr};
use crate::str_array::{velt_rt_str_array_drop, VeltStrArray};
use crate::str_ops::case::*;
use crate::str_ops::number::*;
use crate::str_ops::replace::*;
use crate::str_ops::search::*;
use crate::str_ops::slice::*;
use crate::str_ops::split::*;
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

/// Run an out-param ABI function, return the text and free the result.
fn take(f: impl FnOnce(*mut VeltStr)) -> String {
    let mut out = MaybeUninit::<VeltStr>::uninit();
    f(out.as_mut_ptr());
    let mut s = unsafe { out.assume_init() };
    let text = String::from_utf8(unsafe { s.as_bytes() }.to_vec()).expect("result is UTF-8");
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
fn parse_int_matches_js() {
    for &(s, radix, want) in js::PARSE_INT {
        let got = unsafe { velt_rt_parse_int(&lit(s), radix) };
        assert!(
            same_f64(got, want),
            "parseInt({s:?}, {radix}) = {got:e}, want {:e}",
            f64::from_bits(want)
        );
    }
}

#[test]
fn parse_float_and_number_match_js() {
    for &(s, want) in js::PARSE_FLOAT {
        let got = unsafe { velt_rt_parse_float(&lit(s)) };
        assert!(
            same_f64(got, want),
            "parseFloat({s:?}) = {got:e}, want {:e}",
            f64::from_bits(want)
        );
    }
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
fn search_matches_js() {
    for &(s, n, from, want) in js::INDEX_OF {
        assert_eq!(
            unsafe { velt_rt_str_index_of(&lit(s), &lit(n), from) },
            want,
            "{s:?}.indexOf({n:?}, {from})"
        );
    }
    for &(s, n, from, want) in js::LAST_INDEX_OF {
        let got = unsafe { velt_rt_str_last_index_of(&lit(s), &lit(n), from) };
        assert_eq!(got, want, "{s:?}.lastIndexOf({n:?}, {from})");
    }
    for &(s, n, inc, starts, ends) in js::CONTAINS {
        let (s, n) = (lit(s), lit(n));
        unsafe {
            assert_eq!(velt_rt_str_includes(&s, &n) == 1, inc);
            assert_eq!(velt_rt_str_starts_with(&s, &n) == 1, starts);
            assert_eq!(velt_rt_str_ends_with(&s, &n) == 1, ends);
        }
    }
}

fn split_to_vec(s: &VeltStr, sep: &str) -> Vec<String> {
    let mut out = MaybeUninit::<VeltStrArray>::uninit();
    unsafe { velt_rt_str_split(s, &*heap(sep), out.as_mut_ptr()) };
    let mut arr = unsafe { out.assume_init() };
    let items = (0..arr.len as usize)
        .map(|i| {
            let piece = unsafe { &*arr.ptr.add(i) };
            String::from_utf8(unsafe { piece.as_bytes() }.to_vec()).unwrap()
        })
        .collect();
    unsafe { velt_rt_str_array_drop(&mut arr) };
    items
}

#[test]
fn split_matches_js() {
    for &(s, sep, want) in js::SPLIT {
        let got = both(s, |v| split_to_vec(v, sep).join("\u{0}"));
        assert_eq!(got, want.join("\u{0}"), "{s:?}.split({sep:?})");
        assert_eq!(split_to_vec(&lit(s), sep).len(), want.len());
    }
}

#[test]
fn trim_and_case_match_js() {
    for &(s, trim, start, end) in js::TRIM {
        assert_eq!(
            both(s, |v| take(|o| unsafe { velt_rt_str_trim(v, o) })),
            trim
        );
        assert_eq!(
            both(s, |v| take(|o| unsafe { velt_rt_str_trim_start(v, o) })),
            start
        );
        assert_eq!(
            both(s, |v| take(|o| unsafe { velt_rt_str_trim_end(v, o) })),
            end
        );
    }
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
fn replace_matches_js() {
    for &(s, from, to, first, all) in js::REPLACE {
        let (f, t) = (lit(from), lit(to));
        let got = both(s, |v| {
            take(|o| unsafe { velt_rt_str_replace(v, &f, &t, o) })
        });
        assert_eq!(got, first, "{s:?}.replace({from:?}, {to:?})");
        let got = both(s, |v| {
            take(|o| unsafe { velt_rt_str_replace_all(v, &f, &t, o) })
        });
        assert_eq!(got, all, "{s:?}.replaceAll({from:?}, {to:?})");
    }
}

#[test]
fn repeat_pad_char_codes_match_js() {
    for &(s, n, want) in js::REPEAT {
        let got = both(s, |v| {
            take(|o| assert_eq!(unsafe { velt_rt_str_repeat(v, n, o) }, 1))
        });
        assert_eq!(got, want);
    }
    for &(s, n, fill, start, end) in js::PAD {
        let f = lit(fill);
        assert_eq!(
            both(s, |v| take(|o| unsafe {
                velt_rt_str_pad_start(v, n, &f, o)
            })),
            start
        );
        assert_eq!(
            both(s, |v| take(|o| unsafe { velt_rt_str_pad_end(v, n, &f, o) })),
            end
        );
    }
    for &(s, i, want) in js::CHAR_CODE_AT {
        assert_eq!(unsafe { velt_rt_str_char_code_at(&lit(s), i) }, want);
    }
    for &(code, want) in js::FROM_CHAR_CODE {
        assert_eq!(
            take(|o| unsafe { velt_rt_str_from_char_code(code, o) }),
            want,
            "{code}"
        );
    }
}

#[test]
fn repeat_range_errors() {
    let s = lit("ab");
    for n in [-1, i64::MIN, i64::MAX] {
        assert_eq!(
            take(|o| assert_eq!(unsafe { velt_rt_str_repeat(&s, n, o) }, 0)),
            ""
        );
    }
    // Empty string: any non-negative count is fine.
    assert_eq!(
        take(|o| assert_eq!(unsafe { velt_rt_str_repeat(&lit(""), i64::MAX, o) }, 1)),
        ""
    );
}

#[test]
fn byte_offset_model() {
    // "héllo": h=0, é=1..3, l=3. Offsets inside é move back to its first byte.
    let s = lit("héllo");
    let slice = |a, b| take(|o| unsafe { velt_rt_str_slice(&s, a, b, o) });
    assert_eq!(slice(1, 3), "é");
    assert_eq!(slice(2, i64::MAX), "éllo");
    assert_eq!(slice(0, 2), "h");
    assert_eq!(slice(-4, -1), "éll");
    assert_eq!(unsafe { velt_rt_str_index_of(&s, &lit("l"), 0) }, 3);
    assert_eq!(unsafe { velt_rt_str_index_of(&s, &lit("é"), 2) }, -1);
    assert_eq!(unsafe { velt_rt_str_last_index_of(&s, &lit("é"), 2) }, 1);
    assert_eq!(
        unsafe { velt_rt_str_last_index_of(&s, &lit("l"), i64::MAX) },
        4
    );
    assert_eq!(unsafe { velt_rt_str_char_code_at(&s, 1) }, 0xC3);
    // Pad lengths are bytes; a multi-byte fill is cut at a character boundary.
    let fill = lit("é");
    assert_eq!(
        take(|o| unsafe { velt_rt_str_pad_start(&lit("a"), 4, &fill, o) }),
        "éa"
    );
    assert_eq!(
        take(|o| unsafe { velt_rt_str_pad_end(&lit("a"), 5, &fill, o) }),
        "aéé"
    );
    // split("") / replaceAll("") work per Unicode scalar value (JS: per UTF-16 unit).
    assert_eq!(split_to_vec(&lit("a😀"), ""), ["a", "😀"]);
    let all = take(|o| unsafe { velt_rt_str_replace_all(&lit("a😀"), &lit(""), &lit("|"), o) });
    assert_eq!(all, "|a|😀|");
}

#[test]
fn static_inputs_give_borrowed_results() {
    let s = lit("  hello world  ");
    let mut t = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_trim(&s, t.as_mut_ptr()) };
    let t = unsafe { t.assume_init() };
    let at = |x: &VeltStr| unsafe { x.as_bytes() }.as_ptr();
    assert!(t.is_static() && at(&t) == unsafe { at(&s).add(2) });
    let h = heap("  hello  ");
    let mut t = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_str_trim(&*h, t.as_mut_ptr()) };
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

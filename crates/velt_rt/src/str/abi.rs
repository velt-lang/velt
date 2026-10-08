//! The core string functions of rt_abi.md "Strings": concatenation, number formatting, copy
//! (`clone`: a count increment, never a deep copy), drop and comparison.

use super::{write_out, VeltStr};
use crate::fmt;
use crate::str_array::VeltStrArray;
use std::cmp::Ordering;

#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_concat(
    a: *const VeltStr,
    b: *const VeltStr,
    out: *mut VeltStr,
) {
    let (a, b) = (&*a, &*b);
    let s = if b.is_empty() {
        a.share()
    } else if a.is_empty() {
        b.share()
    } else {
        VeltStr::concat(a, b)
    };
    write_out(out, s);
}

/// `s += t` on the owned string `*s`, whose old value is dead after the assignment: appended in
/// place when `*s` is inline with room or the only reference to its heap buffer, which grows
/// geometrically; a shared or static `*s` is copied once into a buffer of its own. So a loop of
/// appends costs O(total length). The one runtime path for every append the compiler lowers in
/// place, so a per-string header has a single place to be kept up to date.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_append(s: *mut VeltStr, t: *const VeltStr) {
    if std::ptr::eq(s, t) {
        // `s += s`: copy the text out first (the append may grow and move it).
        let sum = (*t).summary();
        (*s).push_with_summary(|v| v.extend_from_slice((*t).as_bytes()), |_| Some(sum));
        return;
    }
    // A share or view of `*s`'s own buffer is copied out by the push where the buffer grows.
    (*s).push_str(&*t);
}

/// `Buffer.byteLength(s)`: the length of `s` in UTF-8, which is its stored byte length (a lone
/// surrogate takes 3 bytes, as the U+FFFD it is written as). O(1).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_byte_length(s: *const VeltStr) -> u64 {
    (*s).len() as u64
}

/// `parts.join(sep)`: one allocation of the right form (`str/join.rs`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_join(
    parts: *const VeltStrArray,
    sep: *const VeltStr,
    out: *mut VeltStr,
) {
    let a = &*parts;
    let parts = if a.len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(a.ptr, a.len as usize)
    };
    write_out(out, VeltStr::join(parts, &*sep));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_i64(v: i64, out: *mut VeltStr) {
    let mut b = itoa::Buffer::new();
    write_out(out, VeltStr::from_bytes(b.format(v).as_bytes()));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_u64(v: u64, out: *mut VeltStr) {
    let mut b = itoa::Buffer::new();
    write_out(out, VeltStr::from_bytes(b.format(v).as_bytes()));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_f64(v: f64, out: *mut VeltStr) {
    let mut b = Vec::with_capacity(32);
    fmt::push_f64(&mut b, v);
    write_out(out, VeltStr::from_bytes(&b));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_from_bool(v: u8, out: *mut VeltStr) {
    // Static literals: no allocation, drop is a no-op.
    let s: &'static [u8] = if v != 0 { b"true" } else { b"false" };
    write_out(out, VeltStr::from_static(s));
}

/// A copy of `s`: the same bytes for static and inline strings, a count increment for heap ones.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_clone(s: *const VeltStr, out: *mut VeltStr) {
    write_out(out, (*s).share());
}

/// A copy of `s` that owns its bytes: a borrowed string (the static form, e.g. a JSON object key
/// that points into the text being parsed) is copied; inline and heap strings as in
/// `velt_rt_str_clone`. For a value kept after the memory it may borrow from is freed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_own(s: *const VeltStr, out: *mut VeltStr) {
    let s = &*s;
    let owned = if s.is_static() {
        VeltStr::owned_counted(s.as_bytes(), s.summary())
    } else {
        s.share()
    };
    write_out(out, owned);
}

/// Release `*s` (the last reference to a heap buffer frees it), then zero `*s`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_drop(s: *mut VeltStr) {
    (*s).release();
}

/// `a < b` and friends, and `sort()` without a comparator: -1 / 0 / 1 in UTF-16 code-unit order
/// (#377 phase 2b): byte order, which is code point order, corrected where the two disagree
/// ([`super::cmp_utf16`], one extra test of the first differing bytes otherwise).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_cmp(a: *const VeltStr, b: *const VeltStr) -> i32 {
    match super::cmp_utf16((*a).as_bytes(), (*b).as_bytes()) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::str::INLINE_MAX;
    use std::mem::MaybeUninit;

    fn call(f: impl FnOnce(*mut VeltStr)) -> VeltStr {
        let mut out = MaybeUninit::<VeltStr>::uninit();
        f(out.as_mut_ptr());
        unsafe { out.assume_init() }
    }

    fn text(s: &VeltStr) -> &str {
        std::str::from_utf8(unsafe { s.as_bytes() }).unwrap()
    }

    fn data(s: &VeltStr) -> *const u8 {
        unsafe { s.as_bytes() }.as_ptr()
    }

    const LONG: &str = "a string that is longer than twenty-three bytes";

    #[test]
    fn share_keeps_every_inline_byte() {
        // Regression (wasm32): `share` rebuilt the value field by field and lost bytes 4..8,
        // the padding after a 4-byte pointer ("captured 42" came back as "capt     42").
        let s = VeltStr::from_bytes(b"captured 42");
        let t = unsafe { s.share() };
        assert_eq!(text(&t), "captured 42");
        assert_eq!(
            std::mem::size_of::<VeltStr>(),
            3 * std::mem::size_of::<u64>()
        );
    }

    #[test]
    fn owned_copies_outlive_what_they_borrowed() {
        for src in ["key1", LONG] {
            let buf = src.as_bytes().to_vec();
            let view = unsafe { VeltStr::borrowed(buf.as_ptr(), buf.len()) };
            let mut owned = MaybeUninit::<VeltStr>::uninit();
            unsafe { velt_rt_str_own(&view, owned.as_mut_ptr()) };
            let mut owned = unsafe { owned.assume_init() };
            drop(buf);
            assert!(!owned.is_static());
            assert_eq!(text(&owned), src);
            unsafe { velt_rt_str_drop(&mut owned) };
        }
        // Inline and heap strings are shared, as by `velt_rt_str_clone`.
        let mut h = VeltStr::from_bytes(LONG.as_bytes());
        let mut copy = MaybeUninit::<VeltStr>::uninit();
        unsafe { velt_rt_str_own(&h, copy.as_mut_ptr()) };
        let mut copy = unsafe { copy.assume_init() };
        assert_eq!(copy.ptr(), h.ptr());
        unsafe {
            velt_rt_str_drop(&mut copy);
            velt_rt_str_drop(&mut h);
        }
    }

    #[test]
    fn appends_grow_in_place() {
        let piece = VeltStr::from_static(b"<div class=\"lvl\">leaf</div>");
        let mut s = VeltStr::empty();
        let (mut grows, mut cap) = (0, 0);
        for _ in 0..10_000 {
            unsafe { velt_rt_str_append(&mut s, &piece) };
            if s.w2 != cap {
                (grows, cap) = (grows + 1, s.w2);
            }
        }
        assert_eq!(s.len(), 10_000 * piece.len());
        assert!(grows < 20, "{grows} regrowths for 10000 appends");
        // A shared buffer is copied once; the other copy keeps its text.
        let mut other = unsafe { s.share() };
        unsafe { velt_rt_str_append(&mut s, &piece) };
        assert_ne!(data(&s), data(&other));
        assert_eq!(other.len(), 10_000 * piece.len());
        assert_eq!(s.len(), 10_001 * piece.len());
        unsafe {
            other.release();
            s.release();
        }
    }

    #[test]
    fn appending_an_uncounted_view_of_itself() {
        // A full buffer that must grow (and may move) while the appended text points into it.
        let mut s = VeltStr::from_bytes(LONG.as_bytes());
        let view = VeltStr {
            w0: s.w0,
            w1: s.w1,
            w2: s.w2,
        };
        unsafe { velt_rt_str_append(&mut s, &view) };
        assert_eq!(text(&s), format!("{LONG}{LONG}"));
        let sp: *mut VeltStr = &mut s;
        unsafe { velt_rt_str_append(sp, sp) };
        assert_eq!(text(&s), LONG.repeat(4));
        let tail = unsafe { VeltStr::borrowed(s.ptr().add(LONG.len()), LONG.len()) };
        unsafe { velt_rt_str_append(&mut s, &tail) };
        assert_eq!(text(&s), LONG.repeat(5));
        unsafe { s.release() };
    }

    #[test]
    fn forms() {
        let e = VeltStr::empty();
        assert!(e.is_static() && e.is_empty());
        let s = VeltStr::from_bytes(b"short");
        assert!(s.is_inline() && !s.is_heap() && text(&s) == "short");
        let full = VeltStr::from_bytes(&[b'x'; INLINE_MAX]);
        assert!(full.is_inline() && full.len() == INLINE_MAX);
        let mut h = VeltStr::from_bytes(LONG.as_bytes());
        assert!(h.is_heap() && text(&h) == LONG);
        unsafe { h.release() };
        assert!(h.is_static() && h.is_empty());
    }

    #[test]
    fn concat_shares_and_copies() {
        let a = VeltStr::from_static(b"hello, ");
        let b = VeltStr::from_static("wörld".as_bytes());
        let mut c = call(|o| unsafe { velt_rt_str_concat(&a, &b, o) });
        assert!(c.is_inline() && text(&c) == "hello, wörld");
        let long = VeltStr::from_static(LONG.as_bytes());
        let mut l = call(|o| unsafe { velt_rt_str_concat(&c, &long, o) });
        assert!(l.is_heap());
        // Concatenating with "" shares the other operand.
        let mut same = call(|o| unsafe { velt_rt_str_concat(&l, &VeltStr::empty(), o) });
        assert_eq!(data(&same), data(&l));
        unsafe {
            same.release();
            l.release();
            c.release();
        }
    }

    #[test]
    fn clone_is_a_count() {
        let mut h = VeltStr::from_bytes(LONG.as_bytes());
        let mut d = call(|o| unsafe { velt_rt_str_clone(&h, o) });
        assert_eq!(data(&d), data(&h));
        unsafe { velt_rt_str_drop(&mut h) };
        assert!(h.is_empty());
        assert_eq!(text(&d), LONG);
        unsafe { velt_rt_str_drop(&mut d) };
        // Dropping twice (zeroed) is harmless.
        unsafe { velt_rt_str_drop(&mut d) };
        let s = VeltStr::from_static(b"lit");
        let c = call(|o| unsafe { velt_rt_str_clone(&s, o) });
        assert!(c.is_static() && text(&c) == "lit");
    }

    #[test]
    fn append_in_place_only_when_unique() {
        let mut s = VeltStr::from_static(b"ab");
        unsafe { s.push_wtf8(b"c", None) };
        assert!(s.is_inline() && text(&s) == "abc");
        unsafe { s.push_wtf8(LONG.as_bytes(), None) };
        assert!(s.is_heap());
        unsafe { s.push_wtf8(b"!", None) };
        let before = data(&s);
        unsafe { s.push_wtf8(b"#", None) };
        assert_eq!(data(&s), before, "unique with room: in place");
        let mut shared = unsafe { s.share() };
        unsafe { s.push_wtf8(b"?", None) };
        assert_ne!(data(&s), before, "shared: copied");
        assert!(text(&shared).ends_with("!#") && text(&s).ends_with("!#?"));
        unsafe {
            shared.release();
            s.release();
        }
    }

    #[test]
    fn substring_forms() {
        let st = VeltStr::from_static(LONG.as_bytes());
        let sub = unsafe { st.substring(2, 30) };
        assert!(sub.is_static());
        let mut h = VeltStr::from_bytes(LONG.as_bytes());
        let mut whole = unsafe { h.substring(0, LONG.len()) };
        assert!(whole.is_heap() && data(&whole) == data(&h));
        let small = unsafe { h.substring(0, 5) };
        assert!(small.is_inline() && text(&small) == "a str");
        unsafe {
            whole.release();
            h.release();
        }
    }

    #[test]
    fn from_numbers() {
        let s = call(|o| unsafe { velt_rt_str_from_i64(i64::MIN, o) });
        assert!(s.is_inline() && text(&s) == "-9223372036854775808");
        let s = call(|o| unsafe { velt_rt_str_from_u64(u64::MAX, o) });
        assert_eq!(text(&s), "18446744073709551615");
        for (v, want) in [
            (1e21, "1e+21"),
            (-0.0, "0"),
            (2.5, "2.5"),
            (f64::NAN, "NaN"),
        ] {
            let s = call(|o| unsafe { velt_rt_str_from_f64(v, o) });
            assert_eq!(text(&s), want);
        }
        let t = call(|o| unsafe { velt_rt_str_from_bool(1, o) });
        let f = call(|o| unsafe { velt_rt_str_from_bool(0, o) });
        assert_eq!((text(&t), t.is_static(), text(&f)), ("true", true, "false"));
    }

    #[test]
    fn cmp_by_code_units() {
        let cmp = |a: &'static str, b: &'static str| unsafe {
            velt_rt_str_cmp(
                &VeltStr::from_static(a.as_bytes()),
                &VeltStr::from_static(b.as_bytes()),
            )
        };
        assert_eq!(cmp("a", "b"), -1);
        assert_eq!(cmp("b", "a"), 1);
        assert_eq!(cmp("abc", "abc"), 0);
        assert_eq!(cmp("", ""), 0);
        assert_eq!(cmp("", "a"), -1);
        assert_eq!(cmp("ab", "abc"), -1);
        assert_eq!(cmp("Z", "a"), -1);
        assert_eq!(cmp("é", "z"), 1); // 0xC3 > 'z'
                                      // Code-unit order (#377 phase 2b): U+FF5E sorts after a supplementary character.
        assert_eq!(cmp("～", "😀"), 1);
        assert_eq!(cmp("a😀", "a～"), -1);
        let inline = VeltStr::from_bytes(b"abc");
        let r = unsafe { velt_rt_str_cmp(&inline, &VeltStr::from_static(b"abc")) };
        assert_eq!(r, 0);
    }

    #[test]
    fn shared_across_threads() {
        let mut h = VeltStr::from_bytes(LONG.as_bytes());
        let copies: Vec<VeltStr> = (0..8).map(|_| unsafe { h.share() }).collect();
        let threads: Vec<_> = copies
            .into_iter()
            .map(|mut c| {
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        let mut x = unsafe { c.share() };
                        unsafe { x.release() };
                    }
                    assert_eq!(text(&c), LONG);
                    unsafe { c.release() };
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(text(&h), LONG);
        unsafe { h.release() };
    }
}

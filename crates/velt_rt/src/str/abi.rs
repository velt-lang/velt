//! The core string functions of rt_abi.md "Strings": concatenation, number formatting, copy
//! (`clone`: a count increment, never a deep copy), drop and comparison.

use super::{write_out, VeltStr};
use crate::fmt;
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
        let mut s = VeltStr::with_capacity(a.len() + b.len());
        s.push_bytes(a.as_bytes());
        s.push_bytes(b.as_bytes());
        s
    };
    write_out(out, s);
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

/// Release `*s` (the last reference to a heap buffer frees it), then zero `*s`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_drop(s: *mut VeltStr) {
    (*s).release();
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_cmp(a: *const VeltStr, b: *const VeltStr) -> i32 {
    match (*a).as_bytes().cmp((*b).as_bytes()) {
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
        unsafe { s.push_bytes(b"c") };
        assert!(s.is_inline() && text(&s) == "abc");
        unsafe { s.push_bytes(LONG.as_bytes()) };
        assert!(s.is_heap());
        unsafe { s.push_bytes(b"!") };
        let before = data(&s);
        unsafe { s.push_bytes(b"#") };
        assert_eq!(data(&s), before, "unique with room: in place");
        let mut shared = unsafe { s.share() };
        unsafe { s.push_bytes(b"?") };
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
    fn cmp_bytewise() {
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

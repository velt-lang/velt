//! Shared slices (#402): which pieces share their buffer, what they cost a front-consuming
//! parser, and that everything that reads a heap string's buffer (counts, layout, lone
//! surrogates, breadcrumbs, the per-thread cursor, appends) finds a slice's. Run with
//! `VELT_RT_DEBUG_ALLOC=1` to have the allocator check that every buffer is freed once, with the
//! layout it was allocated with.

use super::*;
use crate::str_ops::slice::{velt_rt_str_char_code_at, velt_rt_str_slice};

/// Does `s` point into the buffer of `parent`'s bytes?
fn inside(s: &VeltStr, parent: &VeltStr) -> bool {
    let (p, n) = (bytes(parent).as_ptr() as usize, parent.len());
    (p..p + n).contains(&(bytes(s).as_ptr() as usize))
}

/// `s.slice(a, b)` in code units, through the ABI entry generated code calls.
fn js_slice(s: &VeltStr, a: i64, b: i64) -> VeltStr {
    let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
    unsafe {
        velt_rt_str_slice(s, a, b, out.as_mut_ptr());
        out.assume_init()
    }
}

/// UTF-16 of a string, the reference model.
fn utf16(s: &VeltStr) -> Vec<u16> {
    let text = unsafe { s.to_string_lossy() };
    text.encode_utf16().collect()
}

#[test]
fn large_pieces_share_and_small_ones_copy() {
    let text = "abcdefghij".repeat(100);
    let s = Owned(VeltStr::from_bytes(text.as_bytes()));
    let tail = Owned(unsafe { s.0.substring(10, 1000) });
    assert!(tail.0.is_slice() && inside(&tail.0, &s.0), "{:?}", tail.0);
    assert_eq!(bytes(&tail.0), &text.as_bytes()[10..]);
    // A slice of a slice shares the same buffer.
    let inner = Owned(unsafe { tail.0.substring(100, 800) });
    assert!(inner.0.is_slice() && inside(&inner.0, &s.0));
    assert_eq!(inner.0.buffer(), s.0.buffer());
    assert_eq!(bytes(&inner.0), &text.as_bytes()[110..810]);
    // Under a quarter of the buffer: a copy, so a slice never pins more than 4x its size.
    let small = Owned(unsafe { s.0.substring(0, 200) });
    assert!(!small.0.is_slice() && small.0.is_heap() && !inside(&small.0, &s.0));
    // Short pieces stay inline.
    let short = unsafe { s.0.substring(3, 9) };
    assert!(short.is_inline() && bytes(&short) == b"defghi");
}

#[test]
fn a_slice_outlives_its_parent() {
    let text = "the quick brown fox jumps over the lazy dog, ".repeat(10);
    let mut s = VeltStr::from_bytes(text.as_bytes());
    let mut tail = unsafe { s.substring(4, text.len()) };
    let copy = unsafe { tail.share() };
    unsafe { s.release() };
    assert_eq!(bytes(&tail), &text.as_bytes()[4..]);
    unsafe { tail.release() };
    // The last reference frees the buffer, with the layout it was allocated with.
    assert_eq!(bytes(&copy), &text.as_bytes()[4..]);
    drop(Owned(copy));
}

#[test]
fn appending_to_a_slice_copies_it() {
    let text = "0123456789".repeat(10);
    let s = Owned(VeltStr::from_bytes(text.as_bytes()));
    let mut tail = unsafe { s.0.substring(50, 100) };
    assert!(tail.is_slice());
    unsafe { tail.push_wtf8(b"!", None) };
    assert!(!tail.is_slice() && !inside(&tail, &s.0));
    assert_eq!(bytes(&tail), format!("{}!", &text[50..]).as_bytes());
    assert_eq!(bytes(&s.0), text.as_bytes(), "the parent is untouched");
    // An ASCII push onto a slice (the builder's fast path) copies too.
    let mut other = unsafe { s.0.substring(10, 90) };
    unsafe { other.push_ascii(b"xyz") };
    assert!(!other.is_slice());
    assert_eq!(bytes(&other), format!("{}xyz", &text[10..90]).as_bytes());
    unsafe { other.release() };
    drop(Owned(tail));
}

#[test]
fn ascii_slices_of_non_ascii_buffers_free_with_the_header() {
    // The slice is ASCII, its buffer is not: freeing the last reference through the slice
    // must use the buffer's layout (with a header), which only the slice's own bit tells.
    let text = format!("é{}", "plain ascii text ".repeat(8));
    let mut s = VeltStr::from_bytes(text.as_bytes());
    let tail = Owned(unsafe { s.substring(2, text.len()) });
    assert!(tail.0.is_slice() && tail.0.is_ascii() && tail.0.buffer_kind().1);
    unsafe { s.release() };
    assert_eq!(bytes(&tail.0), &text.as_bytes()[2..]);
    // ASCII again: the 23-byte inline limit applies.
    let t = Owned(unsafe { tail.0.substring(0, 40) });
    assert!(t.0.is_slice() && t.0.units() == 40);
}

#[test]
fn front_consuming_parsers_are_linear() {
    // Regression for #402: `rest = rest.slice(sp + 1)` copied the rest every time.
    for text in ["word ".repeat(20_000), "wörd 日本 😀 ".repeat(5_000)] {
        let model: Vec<u16> = text.encode_utf16().collect();
        let mut rest = VeltStr::from_bytes(text.as_bytes());
        let (mut at, mut words, mut copied) = (0, 0, 0);
        while let Some(sp) = model[at..].iter().position(|&u| u == b' ' as u16) {
            words += 1;
            let next = js_slice(&rest, sp as i64 + 1, i64::MAX);
            at += sp + 1;
            assert_eq!(next.units(), model.len() - at, "after {words} words");
            if next.is_heap() && !inside(&next, &rest) {
                copied += next.len();
            }
            unsafe { rest.release() };
            rest = next;
            if words % 997 == 0 {
                assert_eq!(utf16(&rest), model[at..], "after {words} words");
                invariants::check_whole(&rest);
            }
        }
        assert_eq!(words, text.split(' ').count() - 1);
        // n/4 + n/16 + ... < n/3 of the input is copied in all.
        assert!(
            copied * 3 <= text.len(),
            "copied {copied} of {}",
            text.len()
        );
        unsafe { rest.release() };
    }
}

#[test]
fn non_ascii_slices_know_their_units_without_a_count() {
    let text = "é日😀a".repeat(1000);
    let s = Owned(VeltStr::from_bytes(text.as_bytes()));
    let tail = Owned(js_slice(&s.0, 5, i64::MAX));
    assert!(tail.0.is_slice());
    // A slice of the slice (which has no breadcrumbs to build first): translating unit 5 scans
    // a few units, and the piece's own unit count is not counted.
    let before = work::total();
    let inner = Owned(js_slice(&tail.0, 5, i64::MAX));
    assert!(work::total() - before < 100, "{}", work::total() - before);
    assert!(inner.0.is_slice());
    assert_eq!(
        utf16(&inner.0),
        text.encode_utf16().skip(10).collect::<Vec<_>>()
    );
    invariants::check_whole(&tail.0);
}

#[test]
fn sequential_loops_over_a_non_ascii_slice_stay_linear() {
    // A slice has no breadcrumbs of its own; the per-thread cursor keeps a `charCodeAt` loop
    // over it linear, as over a static string.
    let text = "é日😀a".repeat(5000);
    let s = Owned(VeltStr::from_bytes(text.as_bytes()));
    // Byte 2: after the "é" (unit 1); no translation, so the buffer has no breadcrumbs.
    let tail = Owned(unsafe { s.0.substring(2, text.len()) });
    assert!(tail.0.is_slice());
    let want: Vec<u16> = text.encode_utf16().skip(1).collect();
    let n = work::least_work(|| {
        let before = work::total();
        for (i, &u) in want.iter().enumerate() {
            let got = unsafe { velt_rt_str_char_code_at(&tail.0, i as i64) };
            assert_eq!(got, u as i64, "unit {i}");
        }
        work::total() - before
    });
    assert!(n <= 3 * want.len(), "{n} units scanned for {}", want.len());
    // Being remembered marks the buffer, so freeing it forgets the positions.
    let field = unsafe { heap::crumbs(s.0.buffer()) }.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(field, heap::REMEMBERED);
}

#[test]
fn a_remembered_slice_buffer_bumps_the_epoch_when_freed() {
    let text = "é日😀a".repeat(100);
    let mut s = VeltStr::from_bytes(text.as_bytes());
    let mut tail = js_slice(&s, 1, i64::MAX);
    unsafe { tail.unit_to_byte(200) };
    unsafe { s.release() };
    let before = recent::epoch();
    unsafe { tail.release() };
    assert!(recent::epoch() > before);
    // Breadcrumbs of a marked buffer still build.
    let p = Owned(VeltStr::from_bytes(text.as_bytes()));
    let t = Owned(js_slice(&p.0, 1, i64::MAX));
    unsafe { t.0.unit_to_byte(150) };
    let want = text.encode_utf16().take(300).collect::<Vec<_>>();
    for (i, &u) in want.iter().enumerate().step_by(37) {
        assert_eq!(
            unsafe { velt_rt_str_char_code_at(&p.0, i as i64) },
            u as i64
        );
    }
}

#[test]
fn lone_surrogates_in_a_sliced_buffer() {
    // A buffer with lone surrogates: a slice can't know its own count, so it says unknown and
    // counts when asked; a buffer without them gives 0 to every slice.
    let mut text = "x".repeat(40).into_bytes();
    text.extend_from_slice(&enc3(HI));
    text.extend_from_slice("é".repeat(40).as_bytes());
    let s = Owned(VeltStr::from_bytes(&text));
    assert_eq!(unsafe { s.0.lone() }, 1);
    let with = Owned(unsafe { s.0.substring(10, text.len()) });
    assert!(with.0.is_slice());
    assert_eq!(unsafe { with.0.lone() }, wtf8::LONE_UNKNOWN);
    assert!(!unsafe { with.0.is_well_formed() });
    let without = Owned(unsafe { s.0.substring(43, text.len()) });
    assert!(without.0.is_slice() && unsafe { without.0.is_well_formed() });
    // The buffer's own count is not overwritten by a slice's.
    assert_eq!(unsafe { s.0.lone() }, 1);
    // A slice ending in a high surrogate joins a low one appended to it.
    let end = Owned(unsafe { s.0.substring(0, 43) });
    assert!(end.0.is_slice());
    let mut joined = unsafe { end.0.share() };
    unsafe { joined.push_wtf8(&enc3(LO), None) };
    assert_eq!(&bytes(&joined)[40..], "😀".as_bytes());
    assert!(unsafe { joined.is_well_formed() });
    drop(Owned(joined));
}

#[test]
fn slices_cross_threads() {
    let text = "shared between threads ".repeat(50);
    let s = Owned(VeltStr::from_bytes(text.as_bytes()));
    let slices: Vec<VeltStr> = (0..8)
        .map(|i| unsafe { s.0.substring(i, text.len() - i) })
        .collect();
    let threads: Vec<_> = slices
        .into_iter()
        .enumerate()
        .map(|(i, mut t)| {
            let want = text.as_bytes()[i..text.len() - i].to_vec();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    let c = Owned(unsafe { t.substring(1, t.len()) });
                    assert_eq!(bytes(&c.0), &want[1..]);
                }
                unsafe { t.release() };
            })
        })
        .collect();
    for t in threads {
        t.join().expect("thread");
    }
}

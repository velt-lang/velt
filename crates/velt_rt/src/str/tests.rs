//! The UTF-16 bookkeeping of the string layout (#377 phase 1): unit and lone-surrogate counts in
//! every form, the inline limits, the header on non-ASCII heap buffers and the join of surrogate
//! halves at a seam. Every string built here is also recounted in full by
//! `invariants::check_whole` (runtime tests only). Run with `VELT_RT_DEBUG_ALLOC=1` to have the
//! allocator check that buffers are freed with the size they were allocated with.

use super::*;

/// The WTF-8 of a lone surrogate (or any BMP code point).
fn enc3(cp: u32) -> [u8; 3] {
    [
        0xE0 | (cp >> 12) as u8,
        0x80 | ((cp >> 6) & 0x3F) as u8,
        0x80 | (cp & 0x3F) as u8,
    ]
}

const HI: u32 = 0xD83D;
const LO: u32 = 0xDE00;

/// The 24 bytes of a value.
fn raw(s: &VeltStr) -> [u8; 24] {
    // SAFETY: VeltStr is 24 plain bytes.
    unsafe { std::mem::transmute_copy(s) }
}

fn bytes(s: &VeltStr) -> &[u8] {
    unsafe { s.as_bytes() }
}

/// Owns a string for the test and drops it at the end.
struct Owned(VeltStr);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { self.0.release() };
    }
}

#[test]
fn units_in_every_form() {
    let texts = [
        "",
        "abc",
        "héllo",
        "日本語",
        "😀",
        "a😀b\u{10FFFF}",
        "an ASCII string longer than the inline form",
        "a non-ASCII string — longer than the inline form 😀",
    ];
    for text in texts {
        let want = text.encode_utf16().count();
        let leaked: &'static [u8] = Box::leak(text.as_bytes().to_vec().into_boxed_slice());
        let mut built = Owned(VeltStr::with_capacity(40));
        unsafe { built.0.push_wtf8(text.as_bytes(), None) };
        let forms = [
            Owned(VeltStr::from_static(leaked)),
            Owned(VeltStr::from_bytes(text.as_bytes())),
            built,
        ];
        for s in &forms {
            assert_eq!(bytes(&s.0), text.as_bytes());
            assert_eq!((s.0.units(), s.0.len()), (want, text.len()), "{:?}", s.0);
            assert_eq!(s.0.is_ascii(), text.is_ascii(), "{:?}", s.0);
            let sum = unsafe { s.0.summary() };
            // Known for ASCII and owned strings; a non-ASCII static string can't tell.
            let lone = if s.0.is_static() && !s.0.is_ascii() {
                wtf8::LONE_UNKNOWN
            } else {
                0
            };
            assert_eq!(sum, Summary { units: want, lone });
        }
    }
}

#[test]
fn inline_limits() {
    let ascii = Owned(VeltStr::from_bytes(&[b'x'; 23]));
    assert!(ascii.0.is_inline());
    assert_eq!(raw(&ascii.0)[23], 0x80 | 23);
    // Non-ASCII: byte 23 is 0xC0 | len, byte 22 the unit count, so 22 bytes fit.
    let e11 = "é".repeat(11);
    let s = Owned(VeltStr::from_bytes(e11.as_bytes()));
    assert!(s.0.is_inline() && s.0.units() == 11 && s.0.len() == 22);
    assert_eq!((raw(&s.0)[22], raw(&s.0)[23]), (11, 0xC0 | 22));
    let e11a = e11 + "a";
    let h = Owned(VeltStr::from_bytes(e11a.as_bytes()));
    assert!(h.0.is_heap() && h.0.units() == 12 && h.0.len() == 23);
    assert_eq!(raw(&h.0)[8..16], ((12u64 << 32) | 23).to_le_bytes());
    // A non-ASCII heap string of 22 bytes compacts inline, one of 23 bytes stays on the heap.
    let text = "😀".repeat(5);
    let mut b = Owned(VeltStr::with_capacity(64));
    unsafe { b.0.push_wtf8(text.as_bytes(), None) };
    assert!(b.0.is_heap() && b.0.len() == 20);
    let short = unsafe { std::mem::replace(&mut b.0, VeltStr::empty()).compact() };
    assert!(short.is_inline() && short.units() == 10);
    let long = unsafe { VeltStr::from_bytes(e11a.as_bytes()).compact() };
    assert!(long.is_heap());
    drop((Owned(short), Owned(long)));
}

#[test]
fn inline_appends_switch_form() {
    let mut s = Owned(VeltStr::from_static(b"ab"));
    unsafe { s.0.push_wtf8("é".as_bytes(), None) };
    assert!(s.0.is_inline() && !s.0.is_ascii());
    assert_eq!((s.0.len(), s.0.units()), (4, 3));
    unsafe { s.0.push_wtf8(b"0123456789abcdefgh", None) };
    assert!(s.0.is_inline() && s.0.len() == 22 && s.0.units() == 21);
    unsafe { s.0.push_wtf8(b"!", None) };
    assert!(s.0.is_heap() && s.0.len() == 23 && s.0.units() == 22);
}

#[test]
fn ascii_buffer_moves_to_a_header_at_its_first_non_ascii_byte() {
    let mut s = Owned(VeltStr::with_capacity(256));
    unsafe { s.0.push_wtf8(&[b'a'; 40], None) };
    let ascii_buffer = s.0.ptr();
    unsafe { s.0.push_wtf8(b"bc", None) };
    assert_eq!(
        s.0.ptr(),
        ascii_buffer,
        "ASCII into an ASCII buffer: in place"
    );
    // Unique and with room, but an ASCII buffer has no header: the text moves.
    unsafe { s.0.push_wtf8("€".as_bytes(), None) };
    let header_buffer = s.0.ptr();
    assert_ne!(header_buffer, ascii_buffer);
    assert!(s.0.is_heap() && !s.0.is_ascii());
    assert_eq!(s.0.w2, 256, "the builder keeps its capacity");
    unsafe { s.0.push_wtf8(b"de", None) };
    unsafe { s.0.push_wtf8("😀".as_bytes(), None) };
    assert_eq!(
        s.0.ptr(),
        header_buffer,
        "anything into a header buffer: in place"
    );
    assert_eq!((s.0.len(), s.0.units()), (51, 47));
}

#[test]
fn header_buffers_grow_share_and_free() {
    let mut s = Owned(VeltStr::from_bytes("ü".repeat(20).as_bytes()));
    for i in 0..200 {
        let piece = if i % 2 == 0 { "x" } else { "日" };
        unsafe { s.0.push_wtf8(piece.as_bytes(), None) };
    }
    assert_eq!((s.0.len(), s.0.units()), (40 + 100 + 300, 20 + 200));
    let copy = Owned(unsafe { s.0.share() });
    unsafe { s.0.push_wtf8(b"!", None) };
    assert_ne!(copy.0.ptr(), s.0.ptr(), "a shared buffer is never written");
    assert_eq!(copy.0.len() + 1, s.0.len());
    let mut whole = Owned(VeltStr::empty());
    unsafe { velt_rt_str_concat(&copy.0, &s.0, &mut whole.0) };
    assert_eq!(whole.0.units(), copy.0.units() + s.0.units());
}

#[test]
fn borrowed_sub_ranges_count_units() {
    let text: &'static str = "aé日😀 plain ASCII tail";
    let s = VeltStr::from_static(text.as_bytes());
    let piece = unsafe { s.substring(1, 10) };
    assert!(piece.is_static() && piece.units() == "é日😀".encode_utf16().count());
    let ascii = unsafe { s.substring(10, text.len()) };
    assert!(ascii.is_static() && ascii.is_ascii() && ascii.units() == text.len() - 10);
    // A piece of an ASCII string is ASCII without counting; one of a heap string is a copy.
    let h = Owned(VeltStr::from_bytes(text.as_bytes()));
    let copy = Owned(unsafe { h.0.substring(3, 10) });
    assert!(copy.0.is_inline() && copy.0.units() == 3);
}

#[test]
fn halves_join_at_a_seam() {
    let (hi, lo) = (enc3(HI), enc3(LO));
    let pair = "😀".as_bytes();
    // Inline, static and heap strings ending with a high half, each given a low half.
    let prefix = "a prefix that is longer than the inline form ".as_bytes();
    let starts: [Owned; 3] = [
        Owned(VeltStr::from_bytes(&hi)),
        Owned(VeltStr::from_static(&*Box::leak(Box::new(hi)))),
        Owned(VeltStr::from_bytes(&[prefix, &hi].concat())),
    ];
    for mut s in starts {
        let before = s.0.len();
        // A static string's lone count is unknown (it has no room for one).
        let lone = if s.0.is_static() {
            wtf8::LONE_UNKNOWN
        } else {
            1
        };
        assert_eq!(unsafe { s.0.summary() }.lone, lone);
        unsafe { s.0.push_wtf8(&[&lo[..], b"z"].concat(), None) };
        assert_eq!(&bytes(&s.0)[before - 3..], &[pair, b"z"].concat()[..]);
        assert_eq!(s.0.len(), before + 2, "bytes shrink by 2 at the join");
        assert_eq!(unsafe { s.0.summary() }.lone, 0);
    }
    // In place in a unique heap buffer with room.
    let mut b = Owned(VeltStr::with_capacity(128));
    unsafe { b.0.push_wtf8(&[prefix, &hi].concat(), None) };
    let at = b.0.ptr();
    unsafe { b.0.push_wtf8(&lo, None) };
    assert_eq!(b.0.ptr(), at);
    assert_eq!(bytes(&b.0), [prefix, pair].concat());
    assert_eq!(
        (b.0.units(), unsafe { b.0.summary() }.lone),
        (prefix.len() + 2, 0)
    );
}

#[test]
fn halves_that_do_not_meet_stay_lone() {
    let (hi, lo) = (enc3(HI), enc3(LO));
    // low + high, and a high followed by text before the low: no join.
    let mut s = Owned(VeltStr::from_bytes(&lo));
    unsafe { s.0.push_wtf8(&hi, None) };
    unsafe { s.0.push_wtf8(&[&b"x"[..], &lo].concat(), None) };
    assert_eq!(bytes(&s.0), [&lo[..], &hi, b"x", &lo].concat());
    assert_eq!((s.0.units(), unsafe { s.0.summary() }.lone), (4, 3));
    // A long one keeps its lone count in the header across growth.
    for _ in 0..20 {
        unsafe { s.0.push_wtf8(&hi, None) };
        unsafe { s.0.push_wtf8(&hi, None) };
    }
    assert!(s.0.is_heap());
    assert_eq!(unsafe { heap::lone(s.0.ptr()) }, 43);
}

#[test]
fn concat_joins_halves() {
    let (hi, lo) = (enc3(HI), enc3(LO));
    let long = [&[b'-'; 30][..], &hi].concat();
    for (a, b) in [
        (hi.to_vec(), lo.to_vec()),
        (long.clone(), [&lo[..], &[b'+'; 30]].concat()),
    ] {
        let (a, b) = (
            Owned(VeltStr::from_bytes(&a)),
            Owned(VeltStr::from_bytes(&b)),
        );
        let mut c = Owned(VeltStr::empty());
        unsafe { velt_rt_str_concat(&a.0, &b.0, &mut c.0) };
        assert_eq!(c.0.units(), a.0.units() + b.0.units());
        assert_eq!(c.0.len(), a.0.len() + b.0.len() - 2);
        assert_eq!(unsafe { c.0.summary() }.lone, 0);
    }
}

#[test]
fn strbuf_pushes_carry_units() {
    use crate::strbuf::*;
    let mut b = Owned(VeltStr::empty());
    unsafe {
        velt_rt_strbuf_new(0, &mut b.0);
        velt_rt_strbuf_push_i64(&mut b.0, -42);
        velt_rt_strbuf_push_bytes(&mut b.0, "€ ".as_ptr(), 4);
        velt_rt_strbuf_push_bool(&mut b.0, 1);
        let s = VeltStr::from_static("😀".as_bytes());
        velt_rt_strbuf_push_json_str(&mut b.0, &s);
        velt_rt_strbuf_push_str(&mut b.0, &s);
        let p: *mut VeltStr = &mut b.0;
        velt_rt_strbuf_push_str(p, p);
    }
    let text = "-42€ true\"😀\"😀";
    assert_eq!(bytes(&b.0), text.repeat(2).as_bytes());
    assert_eq!(b.0.units(), 2 * text.encode_utf16().count());
}

#[test]
fn inline_strings_flag_lone_surrogates() {
    let lone_flag = |s: &VeltStr| raw(s)[23] & INLINE_LONE != 0;
    let plain = Owned(VeltStr::from_bytes("é日".as_bytes()));
    assert!(plain.0.is_inline() && !lone_flag(&plain.0));
    let hi = Owned(VeltStr::from_bytes(&enc3(HI)));
    assert!(lone_flag(&hi.0) && unsafe { hi.0.summary() }.lone == 1);
    // Appending text with a lone surrogate sets the flag; one without keeps it clear.
    let mut s = Owned(VeltStr::from_bytes("é".as_bytes()));
    unsafe { s.0.push_str(&plain.0) };
    assert!(!lone_flag(&s.0));
    unsafe { s.0.push_str(&hi.0) };
    assert!(lone_flag(&s.0) && unsafe { s.0.summary() }.lone == 1);
    // Concatenation and pieces carry it too.
    let mut c = Owned(VeltStr::empty());
    unsafe { velt_rt_str_concat(&plain.0, &hi.0, &mut c.0) };
    assert!(c.0.is_inline() && lone_flag(&c.0));
    let piece = Owned(unsafe { c.0.substring(0, 5) });
    assert!(!lone_flag(&piece.0), "a piece without the surrogate");
    // Text that arrives as UTF-8 has none.
    let t = Owned(VeltStr::from_text("ünïcode"));
    assert!(!lone_flag(&t.0) && t.0.units() == 7);
}

#[test]
fn joins_sum_their_pieces() {
    let parts: Vec<VeltStr> = ["a", "é", "😀", "", "plain ASCII text past the inline limit"]
        .iter()
        .map(|t| VeltStr::from_text(t))
        .collect();
    let sep = VeltStr::from_static("—".as_bytes());
    let joined = Owned(unsafe { VeltStr::join(&parts, &sep) });
    let want = "a—é—😀——plain ASCII text past the inline limit";
    assert_eq!(bytes(&joined.0), want.as_bytes());
    assert!(joined.0.is_heap() && joined.0.units() == want.encode_utf16().count());
    let short = Owned(unsafe { VeltStr::join(&parts[..3], &sep) });
    assert!(short.0.is_inline() && bytes(&short.0) == "a—é—😀".as_bytes());
    let one = Owned(unsafe { VeltStr::join(&parts[4..], &sep) });
    assert_eq!(one.0.ptr(), parts[4].ptr(), "one part is shared");
    // Halves of a pair at a seam join.
    let (hi, lo) = (
        VeltStr::from_bytes(&enc3(HI)),
        VeltStr::from_bytes(&enc3(LO)),
    );
    let pair = Owned(unsafe { VeltStr::join(&[hi, lo], &VeltStr::empty()) });
    assert_eq!(bytes(&pair.0), "😀".as_bytes());
    for mut p in parts {
        unsafe { p.release() };
    }
}

#[test]
fn appending_a_view_of_itself_joins_and_grows_safely() {
    // A heap string starting with a low half and ending with a high one: appending an uncounted
    // copy of itself joins the halves at the seam, which rewrites (and may move) the buffer the
    // appended text lies in.
    let (hi, lo) = (enc3(HI), enc3(LO));
    let body = [&lo[..], &[b'x'; 30], &hi].concat();
    let mut s = VeltStr::from_bytes(&body);
    let view = VeltStr {
        w0: s.w0,
        w1: s.w1,
        w2: s.w2,
    };
    unsafe { velt_rt_str_append(&mut s, &view) };
    let want = [&lo[..], &[b'x'; 30], "😀".as_bytes(), &[b'x'; 30], &hi].concat();
    assert_eq!(bytes(&s), want);
    assert_eq!(unsafe { s.summary() }.lone, 2);
    // A full header buffer growing while the appended text is a static-form view into it.
    let tail = unsafe { VeltStr::borrowed(s.ptr().add(3), 30) };
    unsafe { velt_rt_str_append(&mut s, &tail) };
    assert_eq!(&bytes(&s)[want.len()..], &[b'x'; 30]);
    let sp: *mut VeltStr = &mut s;
    unsafe { velt_rt_str_append(sp, sp) };
    assert_eq!(s.len(), 2 * (want.len() + 30));
    unsafe { s.release() };
}

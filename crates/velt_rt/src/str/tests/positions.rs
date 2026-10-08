//! Positions (#377 phases 2a and 2b): breadcrumbs, the per-thread cursor over heap and static
//! strings, code-unit order, and the JSON reader's borrowed keys.

use super::*;

/// The byte position of every unit of `text` (WTF-8 of well-formed text), by a plain walk.
fn positions(text: &str) -> Vec<crumbs::BytePos> {
    let mut v = Vec::new();
    for (i, c) in text.char_indices() {
        v.push(crumbs::BytePos {
            byte: i,
            low_half: false,
        });
        if c.len_utf16() == 2 {
            v.push(crumbs::BytePos {
                byte: i,
                low_half: true,
            });
        }
    }
    v.push(crumbs::BytePos {
        byte: text.len(),
        low_half: false,
    });
    v
}

#[test]
fn breadcrumbs_built_by_two_threads_at_once() {
    let text: String = "aé😀日".repeat(300);
    let want = positions(&text);
    for _ in 0..50 {
        let s = Owned(VeltStr::from_bytes(text.as_bytes()));
        let copies = [unsafe { s.0.share() }, unsafe { s.0.share() }];
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = copies
            .into_iter()
            .enumerate()
            .map(|(t, c)| {
                let (want, barrier) = (want.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let c = Owned(c);
                    barrier.wait();
                    for k in 0..want.len() {
                        // The two threads walk in opposite directions.
                        let u = if t == 0 { k } else { want.len() - 1 - k };
                        assert_eq!(unsafe { c.0.unit_to_byte(u) }, want[u], "unit {u}");
                    }
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        // Built once and published: the string's own value sees the table.
        assert!(!unsafe { heap::crumbs(s.0.ptr()) }
            .load(std::sync::atomic::Ordering::Acquire)
            .is_null());
    }
}

/// A string two threads read at once without a retain (as a field of a `shared` object passed by
/// pointer to a runtime call): its count stays 1 throughout.
struct Unretained<'a>(&'a VeltStr);

// SAFETY: test only; the two threads only read the string (translations take `&self`).
unsafe impl Sync for Unretained<'_> {}

#[test]
fn breadcrumbs_of_a_count_one_string_read_by_two_threads() {
    // Regression (#377 phase 2a review): building or extending the table in place because the
    // count was 1 raced with the other reader. Run with VELT_RT_DEBUG_ALLOC=1 too: a table freed
    // or reallocated under a reader then reads poison.
    let piece = "aé😀日".repeat(40);
    for _ in 0..30 {
        let mut s = Owned(VeltStr::with_capacity(64));
        let mut text = String::new();
        // Each round grows the string, so the readers extend the table (in place while it has
        // room, by a published copy when it doesn't), all while the count is 1.
        for _ in 0..4 {
            unsafe { s.0.push_wtf8(piece.as_bytes(), None) };
            text.push_str(&piece);
            let want = positions(&text);
            let shared = Unretained(&s.0);
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                for t in 0..2 {
                    let (want, shared, barrier) = (&want, &shared, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        for k in 0..want.len() {
                            let u = if t == 0 { k } else { want.len() - 1 - k };
                            assert_eq!(unsafe { shared.0.unit_to_byte(u) }, want[u], "unit {u}");
                            if !want[u].low_half {
                                assert_eq!(unsafe { shared.0.byte_to_unit(want[u].byte) }, u);
                            }
                        }
                    });
                }
            });
            assert!(unsafe { heap::is_unique(s.0.ptr()) });
        }
    }
}

#[test]
fn breadcrumbs_follow_appends_and_are_freed_with_the_buffer() {
    // Run with VELT_RT_DEBUG_ALLOC=1 to have the allocator check the tables' frees too.
    let mut s = Owned(VeltStr::with_capacity(64));
    let mut text = String::new();
    for round in 0..40 {
        let piece = if round % 3 == 0 { "x😀é" } else { "ab日c" }.repeat(round + 1);
        unsafe { s.0.push_wtf8(piece.as_bytes(), None) };
        text.push_str(&piece);
        let want = positions(&text);
        // Shared every other round: then the table is extended by publishing a copy.
        let shared = (round % 2 == 1).then(|| Owned(unsafe { s.0.share() }));
        for u in (0..want.len()).step_by(7) {
            assert_eq!(
                unsafe { s.0.unit_to_byte(u) },
                want[u],
                "round {round}, unit {u}"
            );
            if !want[u].low_half {
                assert_eq!(unsafe { s.0.byte_to_unit(want[u].byte) }, u);
            }
        }
        drop(shared);
    }
}

#[test]
fn short_and_static_strings_are_scanned() {
    let text = "é😀".repeat(40);
    let want = positions(&text);
    let st = VeltStr::from_static(Box::leak(text.clone().into_bytes().into_boxed_slice()));
    let short = Owned(VeltStr::from_bytes("é😀日".as_bytes()));
    for (u, &pos) in want.iter().enumerate() {
        assert_eq!(unsafe { st.unit_to_byte(u) }, pos);
    }
    let want = positions("é😀日");
    for u in 0..want.len() + 2 {
        assert_eq!(
            unsafe { short.0.unit_to_byte(u) },
            want[u.min(want.len() - 1)]
        );
    }
    let ascii = VeltStr::from_static(b"plain ASCII text");
    assert_eq!(
        unsafe { ascii.unit_to_byte(5) },
        crumbs::BytePos {
            byte: 5,
            low_half: false
        }
    );
    assert_eq!(unsafe { ascii.byte_to_unit(7) }, 7);
}

#[test]
fn utf16_order_examples() {
    use std::cmp::Ordering::*;
    let c = |a: &[u8], b: &[u8]| cmp_utf16(a, b);
    // U+E000..U+FFFF sort after supplementary characters by code units (node: "～" < "😀" is
    // false), and a lone low surrogate after a supplementary character ("\uDC00" > "\u{10000}").
    assert_eq!(c("～".as_bytes(), "😀".as_bytes()), Greater);
    assert_eq!(c(&enc3(0xDC00), "\u{10000}".as_bytes()), Greater);
    // A lone high surrogate against a pair starting with it: the shorter is less.
    assert_eq!(c(&enc3(HI), "😀".as_bytes()), Less);
    assert_eq!(c(&[&enc3(HI)[..], b"a"].concat(), "😀".as_bytes()), Less);
    assert_eq!(c(b"abc", b"abd"), Less);
    assert_eq!(c(b"ab", b"ab"), Equal);
    assert_eq!(c("é".as_bytes(), b"e"), Greater);
}

#[test]
fn remembered_positions_never_outlive_their_buffer() {
    // A string freed and a new one of the same length and unit count, likely at the same
    // address: the position remembered for the first must not be used for the second (#377
    // phase 2b, `recent.rs`). Their texts put unit 101 at different bytes.
    let a_text = format!("{}{}", "é".repeat(100), "x".repeat(100));
    let b_text = format!("{}{}", "x".repeat(100), "é".repeat(100));
    for _ in 0..20 {
        let a = Owned(VeltStr::from_bytes(a_text.as_bytes()));
        assert_eq!(unsafe { a.0.unit_to_byte(100) }.byte, 200);
        drop(a);
        let b = Owned(VeltStr::from_bytes(b_text.as_bytes()));
        assert_eq!(unsafe { b.0.unit_to_byte(101) }.byte, 102);
        assert_eq!(unsafe { b.0.byte_to_unit(104) }, 102);
    }
    // An in-place append changes `w1`, so the old position is not taken for the new text, and a
    // grown (moved) buffer forgets it.
    let mut s = Owned(VeltStr::with_capacity(400));
    unsafe { s.0.push_wtf8(a_text.as_bytes(), None) };
    assert_eq!(unsafe { s.0.unit_to_byte(150) }.byte, 250);
    unsafe { s.0.push_wtf8("😀".repeat(200).as_bytes(), None) };
    assert_eq!(
        unsafe { s.0.unit_to_byte(201) },
        crumbs::BytePos {
            byte: 300,
            low_half: true
        }
    );
}

#[test]
fn sequential_loops_over_static_strings_stay_linear() {
    // Regression (#377 phase 2b review): a static string has no breadcrumbs, and every
    // translation scanned from one end, so a `charCodeAt` or `slice` loop over a long non-ASCII
    // literal was O(n²). The work is counted, not timed.
    let text = "é日😀a".repeat(5000);
    let want = positions(&text);
    let units = want.len() - 1;
    let st = VeltStr::from_static(Box::leak(text.into_bytes().into_boxed_slice()));
    let scanned = |f: &mut dyn FnMut()| {
        work::least_work(|| {
            let before = work::total();
            f();
            work::total() - before
        })
    };
    // `charCodeAt(i)` for every i.
    let n = scanned(&mut || {
        for (u, &pos) in want.iter().enumerate() {
            assert_eq!(unsafe { st.unit_to_byte(u) }, pos, "unit {u}");
        }
    });
    assert!(n <= 2 * units, "charCodeAt loop: {n} units scanned");
    // `slice(i, i + 1)`, and the same loop backward.
    let n = scanned(&mut || {
        for u in 0..units {
            let (a, b) = unsafe { st.unit_range_to_bytes(u, u + 1) };
            assert_eq!((a, b), (want[u], want[u + 1]), "unit {u}");
        }
        for u in (0..units).rev() {
            assert_eq!(unsafe { st.unit_to_byte(u) }, want[u], "unit {u}");
        }
    });
    assert!(n <= 6 * units, "slice loops: {n} units scanned");
    // `indexOf(x, pos)` hits: byte positions back to units, forward with gaps.
    let n = scanned(&mut || {
        for u in (0..units).step_by(97).filter(|&u| !want[u].low_half) {
            assert_eq!(unsafe { st.byte_to_unit(want[u].byte) }, u);
        }
    });
    assert!(n <= 4 * units, "byte_to_unit loop: {n} bytes scanned");
}

/// The first key of the JSON object `json`, read with the pull reader, and whether it borrows
/// the text (static form).
fn first_key(json: &str) -> (Owned, bool) {
    use crate::json::reader_abi::*;
    let src = Owned(VeltStr::from_bytes(json.as_bytes()));
    unsafe {
        let r = velt_rt_json_reader_new(&src.0);
        assert_eq!(velt_rt_json_reader_expect_object_start(r), 1);
        let mut key = std::mem::MaybeUninit::uninit();
        assert_eq!(velt_rt_json_reader_next_key(r, key.as_mut_ptr()), 1);
        let key = key.assume_init();
        let borrowed = key.is_static();
        // A copy that outlives the reader and the text, as a decoder keeps a key.
        let mut kept = std::mem::MaybeUninit::uninit();
        velt_rt_str_own(&key, kept.as_mut_ptr());
        velt_rt_json_reader_free(r);
        (Owned(kept.assume_init()), borrowed)
    }
}

#[test]
fn long_non_ascii_json_keys_are_copied() {
    // Threads remember positions in long non-ASCII static strings forever (`recent.rs`), which
    // is sound only for literals: a JSON key borrowed from text that is freed later must not be
    // one (#377 phase 2b review). Shorter or ASCII keys still borrow.
    let long = format!("{}{}", "é".repeat(100), "x".repeat(100));
    let cases = [
        (long.clone(), false),
        ("é".repeat(STRIDE), true),
        ("x".repeat(500), true),
        (format!("{}\\u00e9", "é".repeat(10)), false),
    ];
    for (key, borrows) in cases {
        let (_, borrowed) = first_key(&format!("{{\"{key}\":1}}"));
        assert_eq!(borrowed, borrows, "{key}");
    }
    // The copy translates like any heap string.
    let (k, _) = first_key(&format!("{{\"{long}\":1}}"));
    assert!(k.0.is_heap());
    assert_eq!(unsafe { k.0.unit_to_byte(100) }.byte, 200);
    assert_eq!(unsafe { k.0.unit_to_byte(101) }.byte, 201);
    // A heap string and a static view of the same bytes never share an entry. (The buffer is
    // leaked: a long non-ASCII view must point at bytes that are never freed.)
    let text = "é".repeat(100);
    let heap = VeltStr::from_bytes(text.as_bytes());
    let view = unsafe { VeltStr::borrowed(heap.ptr(), heap.len()) };
    unsafe { view.unit_to_byte(50) };
    assert!(recent::find(&heap).is_none());
}

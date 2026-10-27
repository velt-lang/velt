use super::*;

fn esc(s: &str) -> String {
    let mut o = Vec::new();
    push_json_string(&mut o, s.as_bytes());
    String::from_utf8(o).unwrap()
}

fn wtf8(cp: u32) -> Vec<u8> {
    let mut b = [0; 4];
    crate::wtf8::encode(cp, &mut b).to_vec()
}

/// `JSON.stringify` of WTF-8 `s`, a code point at a time: the reference for the loops.
fn reference(s: &[u8]) -> (Vec<u8>, usize) {
    let mut out = vec![b'"'];
    let mut lone = 0;
    let mut i = 0;
    while i < s.len() {
        let len = match s[i] {
            0..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        };
        let cp = match len {
            1 => s[i] as u32,
            2 => (s[i] as u32 & 0x1f) << 6 | (s[i + 1] as u32 & 0x3f),
            3 => {
                (s[i] as u32 & 0x0f) << 12
                    | (s[i + 1] as u32 & 0x3f) << 6
                    | (s[i + 2] as u32 & 0x3f)
            }
            _ => 0x10000,
        };
        match cp {
            0x22 => out.extend_from_slice(b"\\\""),
            0x5c => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            0x0a => out.extend_from_slice(b"\\n"),
            0x0d => out.extend_from_slice(b"\\r"),
            0x09 => out.extend_from_slice(b"\\t"),
            0..=0x1f | 0xd800..=0xdfff => {
                out.extend_from_slice(format!("\\u{cp:04x}").as_bytes());
                lone += (cp >= 0xd800) as usize;
            }
            _ => out.extend_from_slice(&s[i..i + len]),
        }
        i += len;
    }
    out.push(b'"');
    (out, lone)
}

/// The output of both ways of reading runs (the target's, and eight bytes at a time as on
/// wasm32), against the reference, with `s` at every start address mod 16.
fn check(s: &[u8]) {
    let (want, want_lone) = reference(s);
    let mut buf = vec![0u8; s.len() + 16];
    let shifts: &[usize] = if cfg!(miri) {
        &[0, 5]
    } else {
        &[0, 1, 3, 7, 8, 13]
    };
    for &shift in shifts {
        buf[shift..shift + s.len()].copy_from_slice(s);
        let s = &buf[shift..shift + s.len()];
        let mut out = Vec::new();
        assert_eq!(push_json_string_counted(&mut out, s), want_lone, "{s:x?}");
        assert_eq!(out, want, "{s:x?}");
        let mut out = Vec::new();
        assert_eq!(push_with::<Words>(&mut out, s), want_lone, "words {s:x?}");
        assert_eq!(out, want, "words {s:x?}");
    }
}

/// `all`, or under Miri (slow, used to run these tests on big-endian and aarch64 targets) the
/// sizes in `few` that are in it.
fn lens(all: std::ops::RangeInclusive<usize>, few: &[usize]) -> Vec<usize> {
    if cfg!(miri) {
        few.iter().copied().filter(|n| all.contains(n)).collect()
    } else {
        all.collect()
    }
}

/// Every byte value, or under Miri the ones the masks treat specially.
fn bytes() -> Vec<u8> {
    if cfg!(miri) {
        vec![
            0, 0x1F, 0x20, b'"', 0x5C, b'a', 0x7F, 0x80, 0xA0, 0xA2, 0xDC, 0xED, 0xFF,
        ]
    } else {
        (0..=255).collect()
    }
}

/// Whether the byte at `i` must end a run: it is escaped (a lone surrogate's lead byte among
/// them).
fn must_stop(s: &[u8], i: usize) -> bool {
    match ESCAPE[s[i] as usize] {
        0 => false,
        MAYBE_LONE => s.get(i + 1).is_some_and(|&b| b >= 0xA0),
        _ => true,
    }
}

/// `run_end` and `blocks` from every start. `run_end`: no byte before the end it gives must
/// stop, and it ends at the end of `s` or at a byte that `ESCAPE` marks. `blocks`: no byte
/// before its end is marked at all, and it ends at a marked byte, at the start of the block
/// that holds one, or after the last whole block.
fn check_runs<S: Scan>(s: &[u8]) {
    for i in 0..=s.len() {
        if s.len() >= S::WIDTH {
            let end = run_end::<S>(s, i);
            assert!(end >= i && end <= s.len(), "{s:x?} from {i}: {end}");
            assert!(
                (i..end).all(|k| !must_stop(s, k)),
                "{s:x?} from {i}: skipped a stop before {end}"
            );
            assert!(
                end == s.len() || ESCAPE[s[end] as usize] != 0,
                "{s:x?} from {i}: ends at plain byte {end}"
            );
        }
        let end = blocks::<S>(s, i);
        assert!(end >= i && end <= s.len(), "{s:x?} from {i}: blocks {end}");
        assert!(
            (i..end).all(|k| ESCAPE[s[k] as usize] == 0),
            "{s:x?} from {i}: blocks skipped a marked byte before {end}"
        );
        let block_start = (end - i).is_multiple_of(S::WIDTH);
        assert!(
            (end < s.len() && ESCAPE[s[end] as usize] != 0)
                || (block_start && (!S::EXACT || end + S::WIDTH > s.len())),
            "{s:x?} from {i}: blocks ends at {end}"
        );
    }
}

/// Every byte value at every position of strings of 1 to 40 bytes (16 and 8 bytes are the
/// block sizes), with the four kinds of byte that can follow an `ED`, for both ways.
#[test]
fn runs_stop_at_every_byte_that_is_escaped() {
    for len in lens(1..=40, &[7, 8, 15, 16, 17]) {
        for at in 0..len {
            for b in bytes() {
                let nexts: &[u8] = if b == 0xED {
                    &[b'a', 0x80, 0x9F, 0xA0, 0xBF]
                } else {
                    b"a"
                };
                for &next in nexts {
                    let mut s = vec![b'a'; len];
                    s[at] = b;
                    if at + 1 < len {
                        s[at + 1] = next;
                    } else if next != b'a' {
                        continue;
                    }
                    check_runs::<Native>(&s);
                    check_runs::<Words>(&s);
                }
            }
        }
    }
}

/// A stop, then a byte that a borrow from it marks wrongly in a word (0x20 above a control
/// character, `#` above `"`, `]` above `\`, `EC` above `ED`), at every offset and from every
/// start, so the stop is also among the bytes skipped at the start of the last block.
#[test]
fn runs_ignore_wrong_marks_above_a_stop() {
    for len in lens(2..=40, &[8, 9, 16, 17]) {
        for at in 0..len - 1 {
            for (b, c) in [
                (0x1F, 0x20),
                (0x00, 0x20),
                (0x01, 0x21),
                (b'"', b'#'),
                (0x5C, b']'),
                (0xED, 0xEC),
            ] {
                let mut s = vec![b'a'; len];
                s[at] = b;
                s[at + 1] = c;
                check_runs::<Native>(&s);
                check_runs::<Words>(&s);
            }
        }
    }
}

/// U+D000–U+D7FF (Hangul among it) does not end a run on x86-64 and aarch64, except in the
/// last lane of a block.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn hangul_stays_in_the_vector_loop() {
    // 한 is ED 95 9C: 5 syllables and a space fill a block with no `ED` in its last lane.
    let s = "한한한한한 ".repeat(8);
    assert_eq!(run_end::<Simd>(s.as_bytes(), 0), s.len());
    let mut s = s.into_bytes();
    s.extend_from_slice(&wtf8(0xD800));
    assert_eq!(run_end::<Simd>(&s, 0), s.len() - 3);
}

/// Every special byte at every position of strings longer than a block, against the reference.
#[test]
fn blocks_escape_like_bytes() {
    for len in [15, 16, 17, 31, 32, 33, 47, 64, 100] {
        for at in 0..len {
            for special in [b'"', b'\\', b'\n', 0x01, 0x1F, 0xED] {
                let mut s = vec![b'a'; len];
                s[at] = special;
                if special == 0xED {
                    // U+D7FF: ED 9F BF, a lead byte that is no lone surrogate.
                    if at + 2 >= len {
                        continue;
                    }
                    s[at + 1] = 0x9F;
                    s[at + 2] = 0xBF;
                }
                check(&s);
            }
        }
    }
}

#[test]
fn every_special_at_every_offset_matches_the_reference() {
    let specials: Vec<Vec<u8>> = vec![
        b"\"".to_vec(),
        b"\\".to_vec(),
        b"\n".to_vec(),
        b"\x01".to_vec(),
        b"\x7f".to_vec(),
        "é".into(),
        "\u{D55C}".into(),
        "\u{D7FF}".into(),
        "😀".into(),
        wtf8(0xD83D),
        wtf8(0xDE00),
        "\u{2028}".into(),
    ];
    let text = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    // One special at every offset of every length up to the whole text (short and long
    // strings, every position in and between blocks), then a `"`: an `ED` lead byte or an
    // escape followed by blocks that are plain again.
    for len in lens(0..=text.len(), &[0, 8, 17, 24]) {
        for at in 0..=len {
            for sp in &specials {
                let mut s = text[..len].to_vec();
                s.splice(at..at, sp.iter().copied());
                check(&s);
                s.push(b'"');
                check(&s);
            }
        }
    }
    // Two specials at every pair of offsets of a long string.
    for a in lens(0..=40, &[0, 7, 15, 16, 24, 39]) {
        for b in lens(a..=40, &[a, a + 1, a + 8, a + 9, 40]) {
            for (x, y) in [(0, 9), (2, 6), (9, 10), (6, 0), (8, 3), (7, 7), (10, 11)] {
                let mut s = text[..40].to_vec();
                s.splice(b..b, specials[y].iter().copied());
                s.splice(a..a, specials[x].iter().copied());
                check(&s);
            }
        }
    }
    // Runs of specials between plain runs.
    for n in 1..40 {
        for sp in &specials {
            let mut s = text[..20].to_vec();
            for _ in 0..n {
                s.extend_from_slice(sp);
            }
            s.extend_from_slice(&text[..20]);
            check(&s);
        }
    }
}

/// Random canonical WTF-8 strings of up to 300 bytes, mostly plain with some of every kind.
#[test]
fn random_strings_match_the_reference() {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..if cfg!(miri) { 100 } else { 20_000 } {
        let len = (next() % 120) as usize;
        let mut s = Vec::new();
        for _ in 0..len {
            let r = next();
            let cp = match r % 16 {
                0 => (r >> 8) as u32 % 0x20,
                1 => [0x22, 0x5c, 0x7f][(r >> 8) as usize % 3],
                2 => 0xD800 + (r >> 8) as u32 % 0x800,
                3 => 0xD000 + (r >> 8) as u32 % 0x800,
                4 => 0x80 + (r >> 8) as u32 % 0x780,
                5 => 0x800 + (r >> 8) as u32 % 0xF800,
                6 => 0x10000 + (r >> 8) as u32 % 0x100000,
                7 => [0x2028, 0x2029, 0xFFFF, 0xFEFF][(r >> 8) as usize % 4],
                _ => 0x20 + (r >> 8) as u32 % 0x5F,
            };
            let unit = wtf8(cp);
            // Two lone surrogates in a row that make a pair are one code point in WTF-8.
            let pairs = s.len() >= 3
                && s[s.len() - 3] == 0xED
                && s[s.len() - 2] >= 0xA0
                && s[s.len() - 2] < 0xB0
                && unit[0] == 0xED
                && unit[1] >= 0xB0;
            if !pairs {
                s.extend_from_slice(&unit);
            }
        }
        check(&s);
    }
}

#[test]
fn matches_json_stringify() {
    // Expected values from node: JSON.stringify(s).
    assert_eq!(esc(""), r#""""#);
    assert_eq!(esc("plain"), r#""plain""#);
    assert_eq!(esc("q\"uote"), r#""q\"uote""#);
    assert_eq!(esc("a\\b/c"), r#""a\\b/c""#);
    assert_eq!(esc("\u{8}\u{c}\n\r\t"), r#""\b\f\n\r\t""#);
    assert_eq!(
        esc("\u{0}\u{1}\u{b}\u{1f}"),
        r#""\u0000\u0001\u000b\u001f""#
    );
    assert_eq!(esc("\u{7f}é😀\u{2028}"), "\"\u{7f}é😀\u{2028}\"");
    // Hangul (an `ED` lead byte that is no surrogate) is copied.
    assert_eq!(esc("\u{D55C}\u{D7FF}"), "\"\u{D55C}\u{D7FF}\"");
}

#[test]
fn lone_surrogates_are_escaped_like_node() {
    // node -e 'console.log(JSON.stringify(["\uD83D", "\uDE00a", "􏿿", "\uDABC"]))'
    // prints ["\ud83d","\ude00a","\u{10FFFF}","\udabc"].
    let cases: [(Vec<u8>, &str, usize); 4] = [
        (wtf8(0xD83D), r#""\ud83d""#, 1),
        ([wtf8(0xDE00), b"a".to_vec()].concat(), r#""\ude00a""#, 1),
        ("\u{10FFFF}".as_bytes().to_vec(), "\"\u{10FFFF}\"", 0),
        (
            [wtf8(0xDABC), b"\n".to_vec(), wtf8(0xDFFF)].concat(),
            r#""\udabc\n\udfff""#,
            2,
        ),
    ];
    for (input, want, lone) in cases {
        let mut o = Vec::new();
        assert_eq!(push_json_string_counted(&mut o, &input), lone);
        assert_eq!(String::from_utf8(o).unwrap(), want);
    }
}

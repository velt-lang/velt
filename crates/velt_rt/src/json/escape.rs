//! JSON string output: quoting and escaping exactly like `JSON.stringify` (well-formed variant).
//!
//! Escaped: `"` `\\`, the short forms `\b \f \n \r \t`, and other control characters below
//! U+0020 as lowercase `\u00xx`, and lone surrogates (#377) as lowercase `\udxxx` (ES2019
//! well-formed `JSON.stringify`), so the output is always well-formed. Everything else
//! (including DEL, U+2028/U+2029 and other non-ASCII) is copied verbatim.

/// For each byte: 0 = copy as is, otherwise the character after the backslash (`u` = `\u00xx`),
/// or [`MAYBE_LONE`] for the lead byte of a lone surrogate (also of U+D000–U+D7FF).
static ESCAPE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = b'u';
        i += 1;
    }
    t[0x08] = b'b';
    t[0x09] = b't';
    t[0x0a] = b'n';
    t[0x0c] = b'f';
    t[0x0d] = b'r';
    t[b'"' as usize] = b'"';
    t[b'\\' as usize] = b'\\';
    t[0xED] = MAYBE_LONE;
    t
};

/// [`ESCAPE`] mark of the `ED` byte.
const MAYBE_LONE: u8 = 1;

const HEX: &[u8; 16] = b"0123456789abcdef";

const ONES: u64 = 0x0101_0101_0101_0101;

/// Strings this long and longer are scanned eight bytes at a time where they are plain ASCII
/// ([`push_long`]); shorter ones a byte at a time, which costs them less.
const LONG: usize = 16;

/// Whether the eight bytes of `word` are all ASCII that is written as is: no control
/// character, `"` or `\\`. A word with a byte from 0x80 up is never plain.
///
/// Per byte, the high bit of each term says: below 0x20 (the subtraction borrows), equal to
/// `"`, equal to `\\` (zero minus one), or not ASCII. A borrow reaches a byte only from a
/// lower byte that already set its own bit, so an all-ASCII word is judged exactly. (`| word`
/// is redundant, the other terms flag every byte from 0x80 up, but says what is meant.)
#[inline]
fn plain_ascii(word: u64) -> bool {
    let control = word.wrapping_sub(ONES * 0x20);
    let quote = (word ^ (ONES * b'"' as u64)).wrapping_sub(ONES);
    let backslash = (word ^ (ONES * b'\\' as u64)).wrapping_sub(ONES);
    (control | quote | backslash | word) & (ONES * 0x80) == 0
}

/// Append `s` (canonical WTF-8) as a quoted, escaped JSON string literal.
#[inline]
pub fn push_json_string(out: &mut Vec<u8>, s: &[u8]) {
    push_json_string_counted(out, s);
}

/// [`push_json_string`], returning the number of lone surrogates written as `\udxxx` (each 3
/// bytes and 1 unit in, 6 ASCII bytes out).
pub fn push_json_string_counted(out: &mut Vec<u8>, s: &[u8]) -> usize {
    if s.len() >= LONG {
        return push_long(out, s);
    }
    out.reserve(s.len() + 2);
    out.push(b'"');
    push_from(out, s, 0)
}

/// [`push_json_string_counted`] for a string of [`LONG`] bytes or more: eight bytes at a time
/// while they are plain ASCII, then a byte at a time to the end. Text that is not ASCII, or
/// dense in escapes, stops the first word or one soon after and so costs one test more.
#[inline(never)]
fn push_long(out: &mut Vec<u8>, s: &[u8]) -> usize {
    out.reserve(s.len() + 2);
    out.push(b'"');
    let mut i = 0;
    while i + 8 <= s.len() && plain_ascii(u64::from_le_bytes(s[i..i + 8].try_into().unwrap())) {
        i += 8;
    }
    push_from(out, s, i)
}

/// The rest of [`push_json_string_counted`] once the opening quote is written and `s[..i]` is
/// known to need no escape: a byte at a time, then the closing quote.
#[inline(always)]
fn push_from(out: &mut Vec<u8>, s: &[u8], mut i: usize) -> usize {
    let mut run_start = 0;
    let mut lone = 0;
    loop {
        while i < s.len() && ESCAPE[s[i] as usize] == 0 {
            i += 1;
        }
        let Some(&byte) = s.get(i) else {
            break;
        };
        let esc = ESCAPE[byte as usize];
        if esc == MAYBE_LONE {
            if s.get(i + 1).is_some_and(|&b| b >= 0xA0) {
                out.extend_from_slice(&s[run_start..i]);
                let unit = 0xD000 | ((s[i + 1] as usize & 0x3F) << 6) | (s[i + 2] as usize & 0x3F);
                out.extend_from_slice(b"\\u");
                for shift in [12, 8, 4, 0] {
                    out.push(HEX[(unit >> shift) & 0xf]);
                }
                lone += 1;
                i += 3;
                run_start = i;
            } else {
                i += 1;
            }
            continue;
        }
        out.extend_from_slice(&s[run_start..i]);
        i += 1;
        run_start = i;
        if esc == b'u' {
            out.extend_from_slice(b"\\u00");
            out.push(HEX[(byte >> 4) as usize]);
            out.push(HEX[(byte & 0xf) as usize]);
        } else {
            out.push(b'\\');
            out.push(esc);
        }
    }
    out.extend_from_slice(&s[run_start..]);
    out.push(b'"');
    lone
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str) -> String {
        let mut o = Vec::new();
        push_json_string(&mut o, s.as_bytes());
        String::from_utf8(o).unwrap()
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
        // Long runs go eight bytes at a time; an escape at any offset is still found.
        for at in 0..24 {
            let mut text = "abcdefghijklmnopqrstuvwxyz".repeat(2);
            text.insert(at, '"');
            assert_eq!(esc(&text), format!("\"{}\"", text.replace('"', "\\\"")));
            text.replace_range(at..at + 1, "\u{1f}");
            assert_eq!(
                esc(&text),
                format!("\"{}\"", text.replace('\u{1f}', "\\u001f"))
            );
            text.replace_range(at..at + 1, "\\");
            assert_eq!(esc(&text), format!("\"{}\"", text.replace('\\', "\\\\")));
            text.replace_range(at..at + 1, "é\u{7f}");
            assert_eq!(esc(&text), format!("\"{text}\""));
        }
        // Every byte value in every position of a word is judged as the table says.
        for b in 0..=255u8 {
            for at in 0..8 {
                let mut word = *b"abcdefgh";
                word[at] = b;
                let plain = b < 0x80 && ESCAPE[b as usize] == 0;
                assert_eq!(
                    plain_ascii(u64::from_le_bytes(word)),
                    plain,
                    "{b:#x} at {at}"
                );
            }
        }
        // Hangul (an `ED` lead byte that is no surrogate) is copied.
        assert_eq!(esc("\u{D55C}\u{D7FF}"), "\"\u{D55C}\u{D7FF}\"");
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

    #[test]
    fn every_special_at_every_offset_matches_the_reference() {
        let wtf8 = |cp: u32| {
            let mut b = [0; 4];
            crate::str::wtf8::encode(cp, &mut b).to_vec()
        };
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
        ];
        let text = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let check = |s: &[u8]| {
            let mut out = Vec::new();
            let lone = push_json_string_counted(&mut out, s);
            let (want, want_lone) = reference(s);
            assert_eq!(out, want, "{s:?}");
            assert_eq!(lone, want_lone, "{s:?}");
        };
        // One special at every offset of every length up to the whole text (short and long
        // strings, every position in and between words), then a `"`: an `ED` lead byte or an
        // escape followed by words that are plain again.
        for len in 0..=text.len() {
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
        for a in 0..=40 {
            for b in a..=40 {
                for (x, y) in [(0, 9), (2, 6), (9, 10), (6, 0), (8, 3)] {
                    let mut s = text[..40].to_vec();
                    s.splice(b..b, specials[y].iter().copied());
                    s.splice(a..a, specials[x].iter().copied());
                    check(&s);
                }
            }
        }
        // Runs of specials between plain runs.
        for n in 1..20 {
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

    #[test]
    fn lone_surrogates_are_escaped_like_node() {
        // node -e 'console.log(JSON.stringify(["\uD83D", "\uDE00a", "\uDBFF\uDFFF", "\uDABC"]))'
        // prints ["\ud83d","\ude00a","\u{10FFFF}","\udabc"].
        let wtf8 = |cp: u32| {
            let mut b = [0; 4];
            crate::str::wtf8::encode(cp, &mut b).to_vec()
        };
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
}

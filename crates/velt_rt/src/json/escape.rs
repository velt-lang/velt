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

/// Append `s` (canonical WTF-8) as a quoted, escaped JSON string literal.
#[inline]
pub fn push_json_string(out: &mut Vec<u8>, s: &[u8]) {
    push_json_string_counted(out, s);
}

/// [`push_json_string`], returning the number of lone surrogates written as `\udxxx` (each 3
/// bytes and 1 unit in, 6 ASCII bytes out).
pub fn push_json_string_counted(out: &mut Vec<u8>, s: &[u8]) -> usize {
    out.reserve(s.len() + 2);
    out.push(b'"');
    let mut run_start = 0;
    let mut lone = 0;
    let mut i = 0;
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
        // Hangul (an `ED` lead byte that is no surrogate) is copied.
        assert_eq!(esc("\u{D55C}\u{D7FF}"), "\"\u{D55C}\u{D7FF}\"");
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

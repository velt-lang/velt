//! JSON string output: quoting and escaping exactly like `JSON.stringify` (well-formed variant).
//!
//! Escaped: `"` `\\`, the short forms `\b \f \n \r \t`, and other control characters below
//! U+0020 as lowercase `\u00xx`. Everything else (including DEL, U+2028/U+2029 and non-ASCII) is
//! copied verbatim. Lone surrogates cannot occur because Velt strings are UTF-8.

/// For each byte: 0 = copy as is, otherwise the character after the backslash (`u` = `\u00xx`).
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
    t
};

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Append `s` as a quoted, escaped JSON string literal.
pub fn push_json_string(out: &mut Vec<u8>, s: &[u8]) {
    out.reserve(s.len() + 2);
    out.push(b'"');
    let mut run_start = 0;
    for (i, &byte) in s.iter().enumerate() {
        let esc = ESCAPE[byte as usize];
        if esc == 0 {
            continue;
        }
        out.extend_from_slice(&s[run_start..i]);
        run_start = i + 1;
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
    }
}

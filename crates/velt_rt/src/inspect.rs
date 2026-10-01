//! Strings inside containers the way node's `util.inspect` (`console.log([s])`) shows them:
//! single quotes, or double quotes when the text contains `'` but no `"`, or backticks when it
//! contains both (and no backtick); the chosen quote, `\` and control characters are escaped
//! (`\n`, `\t`, `\b`, `\f`, `\r`, else `\xHH` for C0, DEL and C1 controls). velt_vir's format
//! glue quotes compile-time strings (literal types, string enums) the same way.

/// Append `s` quoted and escaped to `out`.
/// An object key as `util.inspect` prints it: bare when it is an identifier (`a`, `_x1`,
/// `$`), else quoted like a string (`'a b'`, `'1'`).
pub fn push_inspect_key(out: &mut Vec<u8>, s: &[u8]) {
    let ident = s
        .first()
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_' || *c == b'$')
        && s.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$');
    if ident {
        out.extend_from_slice(s);
    } else {
        push_inspect_string(out, s);
    }
}

pub fn push_inspect_string(out: &mut Vec<u8>, s: &[u8]) {
    let text = String::from_utf8_lossy(s);
    let quote = pick_quote(&text);
    out.push(quote as u8);
    let mut buf = [0u8; 4];
    for c in text.chars() {
        match c {
            '\n' => out.extend_from_slice(b"\\n"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\\' => out.extend_from_slice(b"\\\\"),
            _ if c == quote => {
                out.push(b'\\');
                out.push(c as u8);
            }
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {
                out.extend_from_slice(format!("\\x{:02X}", c as u32).as_bytes())
            }
            _ => out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes()),
        }
    }
    out.push(quote as u8);
}

/// Node's choice of quote for `text`.
fn pick_quote(text: &str) -> char {
    if !text.contains('\'') {
        '\''
    } else if !text.contains('"') {
        '"'
    } else if !text.contains('`') && !text.contains("${") {
        '`'
    } else {
        '\''
    }
}

#[cfg(test)]
mod tests {
    use super::push_inspect_string;

    fn q(s: &str) -> String {
        let mut out = vec![];
        push_inspect_string(&mut out, s.as_bytes());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn quotes_and_escapes_like_node() {
        assert_eq!(q("tab\there"), "'tab\\there'");
        assert_eq!(q("back\\slash"), "'back\\\\slash'");
        assert_eq!(q("single'q"), "\"single'q\"");
        assert_eq!(q("both'\"q"), "`both'\"q`");
        assert_eq!(q("all'\"`"), "'all\\'\"`'");
        assert_eq!(q("\u{1}\u{b}\u{7f}\u{85}"), "'\\x01\\x0B\\x7F\\x85'");
        assert_eq!(q("é"), "'é'");
    }
}

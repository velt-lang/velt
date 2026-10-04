//! Strings inside containers the way node's `util.inspect` (`console.log([s])`) shows them:
//! single quotes, or double quotes when the text contains `'` but no `"`, or backticks when it
//! contains both (and no backtick); the chosen quote, `\` and control characters are escaped
//! (`\n`, `\t`, `\b`, `\f`, `\r`, else `\xHH` for C0, DEL and C1 controls). velt_vir's format
//! glue quotes compile-time strings (literal types, string enums) the same way.

/// An object key as `util.inspect` prints it: bare when it is an identifier of ASCII letters,
/// digits and `_` not starting with a digit (`a`, `_x1`), else quoted like a string (`'a b'`,
/// `'1'`, `'$'`, `'é'`, `''`), as node does.
pub fn push_inspect_key(out: &mut Vec<u8>, s: &[u8]) {
    let ident = s
        .first()
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        && s.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_');
    if ident {
        out.extend_from_slice(s);
    } else {
        push_inspect_string(out, s);
    }
}

/// Append `s` quoted and escaped to `out`.
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

/// The text of a string quoted by [`push_inspect_string`] (`quoted` includes the quotes).
pub fn unescape_inspect_string(quoted: &[u8]) -> Vec<u8> {
    let inner = &quoted[1..quoted.len().saturating_sub(1).max(1)];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let (c, next) = (inner[i], inner.get(i + 1).copied());
        if c != b'\\' || next.is_none() {
            out.push(c);
            i += 1;
            continue;
        }
        i += 2;
        match next.unwrap_or_default() {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'r' => out.push(b'\r'),
            b'x' => {
                let hex = inner
                    .get(i..i + 2)
                    .and_then(|h| std::str::from_utf8(h).ok());
                let code = hex.and_then(|h| u32::from_str_radix(h, 16).ok());
                if let Some(ch) = code.and_then(char::from_u32) {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    i += 2;
                }
            }
            other => out.push(other),
        }
    }
    out
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
    use super::{push_inspect_key, push_inspect_string};

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

    fn key(s: &str) -> String {
        let mut out = vec![];
        push_inspect_key(&mut out, s.as_bytes());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn unescape_reverses_quoting() {
        let texts = [
            "plain",
            "tab\there",
            "a\nb\n",
            "single'q",
            "both'\"q",
            "all'\"`",
        ];
        for s in texts.into_iter().chain(["\u{1}\u{85}é\\x"]) {
            assert_eq!(
                super::unescape_inspect_string(q(s).as_bytes()),
                s.as_bytes()
            );
        }
    }

    #[test]
    fn keys_bare_only_when_identifiers_like_node() {
        // As `node -e 'console.log({a: 1, _x1: 2, $: 3, "a b": 4, "1": 5, "é": 6, "": 7})'`.
        for bare in ["a", "_x1", "A_9", "_"] {
            assert_eq!(key(bare), bare);
        }
        assert_eq!(key("$"), "'$'");
        assert_eq!(key("$a"), "'$a'");
        assert_eq!(key("a b"), "'a b'");
        assert_eq!(key("1"), "'1'");
        assert_eq!(key("1a"), "'1a'");
        assert_eq!(key("é"), "'é'");
        assert_eq!(key(""), "''");
        assert_eq!(key("it's"), "\"it's\"");
        assert_eq!(key("a\nb"), "'a\\nb'");
    }
}

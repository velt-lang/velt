//! Strings inside containers the way node's `util.inspect` (`console.log([s])`) shows them:
//! single quotes, or double quotes when the text contains `'` but no `"`, or backticks when it
//! contains both (and no backtick); the chosen quote, `\` and control characters are escaped
//! (`\n`, `\t`, `\b`, `\f`, `\r`, else `\xHH` for C0, DEL and C1 controls), and a lone surrogate
//! as `\udxxx` (#377; a top-level string prints it as U+FFFD instead, like every output).
//! velt_vir's format glue quotes compile-time strings (literal types, string enums) the same way.
//! Node's default limits ([`DEPTH`], [`MAX_ARRAY_LENGTH`]) are shared with the format glue.

use crate::str::wtf8;

/// Node's `util.inspect` default `depth`: a container nested deeper than this prints as
/// `[Object]`, `[Array]`, `[Name]` (the format glue applies the same limit).
pub const DEPTH: u32 = 2;

/// Node's default `maxArrayLength`: an array, `Map` or `Set` prints this many entries, then
/// `... n more items`.
pub const MAX_ARRAY_LENGTH: usize = 100;

/// Append node's `, ... n more items` (`... 1 more item`) after the shown entries of a
/// container with `remaining` more.
pub fn push_more_items(out: &mut Vec<u8>, remaining: u64) {
    out.extend_from_slice(b", ... ");
    out.extend_from_slice(itoa::Buffer::new().format(remaining).as_bytes());
    out.extend_from_slice(if remaining == 1 {
        b" more item"
    } else {
        b" more items"
    });
}

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

/// Append `s` (canonical WTF-8) quoted and escaped to `out`.
pub fn push_inspect_string(out: &mut Vec<u8>, s: &[u8]) {
    let quote = pick_quote(s);
    out.push(quote);
    let mut i = 0;
    while i < s.len() {
        let (c, n) = wtf8::decode_at(s, i);
        match c {
            0x0A => out.extend_from_slice(b"\\n"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            0x0D => out.extend_from_slice(b"\\r"),
            0x5C => out.extend_from_slice(b"\\\\"),
            _ if c == quote as u32 => {
                out.push(b'\\');
                out.push(quote);
            }
            0x00..=0x1F | 0x7F..=0x9F => out.extend_from_slice(format!("\\x{c:02X}").as_bytes()),
            0xD800..=0xDFFF => out.extend_from_slice(format!("\\u{c:04x}").as_bytes()),
            _ => out.extend_from_slice(&s[i..i + n]),
        }
        i += n;
    }
    out.push(quote);
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
fn pick_quote(text: &[u8]) -> u8 {
    let has = |c: u8| text.contains(&c);
    if !has(b'\'') {
        b'\''
    } else if !has(b'"') {
        b'"'
    } else if !has(b'`') && !text.windows(2).any(|w| w == b"${") {
        b'`'
    } else {
        b'\''
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

    #[test]
    fn lone_surrogates_are_escaped_like_node() {
        // node -e 'console.log(["\uD83D", "a\uDE00b", "\uD83D\uDE00", "\uD83D\u0001"])'
        // prints [ '\ud83d', 'a\ude00b', '😀', '\ud83d\x01' ].
        let enc = |cp: u32| {
            let mut b = [0; 4];
            crate::str::wtf8::encode(cp, &mut b).to_vec()
        };
        let show = |b: Vec<u8>| {
            let mut out = vec![];
            push_inspect_string(&mut out, &b);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(show(enc(0xD83D)), "'\\ud83d'");
        assert_eq!(
            show([b"a".to_vec(), enc(0xDE00), b"b".to_vec()].concat()),
            "'a\\ude00b'"
        );
        assert_eq!(show("😀".as_bytes().to_vec()), "'😀'");
        assert_eq!(show([enc(0xD83D), vec![1]].concat()), "'\\ud83d\\x01'");
        let mut key = vec![];
        push_inspect_key(&mut key, &enc(0xD800));
        assert_eq!(String::from_utf8(key).unwrap(), "'\\ud800'");
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

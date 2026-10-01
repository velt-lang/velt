//! Facts the AST does not keep but the formatter must reproduce, recovered from the source text:
//! literal spellings (`0xff`, `1_000`, quote style), `===` versus `==`, the parameter names of
//! function types, and where the author left blank lines.

use velt_common::Span;

/// The source text of `span`.
pub(crate) fn slice(src: &str, span: Span) -> &str {
    src.get(span.lo as usize..span.hi as usize).unwrap_or("")
}

/// A string literal in the house style: double quotes, unless the text contains a `"` (then the
/// original single quotes are kept). Escapes are kept as written, except `\'` which is not
/// needed inside double quotes.
pub(crate) fn string_literal(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) else {
        return raw.to_string();
    };
    if inner.contains('"') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    out.push('"');
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\'') => out.push('\''),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out.push('"');
    out
}

/// Is there an empty line in the whitespace directly before `pos`?
pub(crate) fn blank_line_before(src: &str, pos: u32) -> bool {
    let before = src.get(..pos as usize).unwrap_or("");
    let gap = before.trim_end_matches([' ', '\t', '\r', '\n']);
    before[gap.len()..].matches('\n').count() >= 2
}

/// The source spelling of `==`/`!=` between two operands: `===`/`!==` if written that way.
pub(crate) fn strict_equality(src: &str, lhs_hi: u32, rhs_lo: u32) -> bool {
    src.get(lhs_hi as usize..rhs_lo as usize)
        .is_some_and(|gap| gap.contains("===") || gap.contains("!=="))
}

/// The literal tokens (numbers, strings, `true`/`false`/`null`) inside a pattern's span, in order.
/// Pattern literals keep no spelling in the AST, so it is re-read from the source.
pub(crate) fn literal_tokens(src: &str, span: Span) -> Vec<String> {
    let text = slice(src, span);
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        if c == b'"' || c == b'\'' {
            i = string_end(bytes, i);
            out.push(string_literal(&text[start..i]));
        } else if c.is_ascii_digit()
            || (c == b'.'
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                && (i == 0 || bytes[i - 1] != b'.'))
        {
            i = number_end(bytes, i);
            out.push(text[start..i].to_string());
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            while i < bytes.len() && is_word_byte(bytes[i]) {
                i += 1;
            }
            out.push(text[start..i].to_string());
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = text[i..].find("*/").map_or(bytes.len(), |n| i + n + 2);
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
        } else {
            i += 1;
        }
    }
    out
}

fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            c if c == bytes[start] => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// End of a number literal: digits, `_`, radix letters, exponent (with sign), fraction and suffix.
fn number_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        let c = bytes[i];
        let exp_sign = matches!(c, b'+' | b'-') && matches!(bytes[i - 1], b'e' | b'E');
        let fraction = c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
        if is_word_byte(c) || exp_sign || fraction {
            i += 1;
        } else {
            break;
        }
    }
    i
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// The `name:` written before a function-type parameter whose type starts at `type_lo`
/// (the AST drops these names; they are kept for readability).
pub(crate) fn fn_type_param_name(src: &str, type_lo: u32) -> Option<&str> {
    let before = src.get(..type_lo as usize)?.trim_end();
    let before = before.strip_suffix(':')?.trim_end();
    let name_start = before
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .map_or(0, |i| i + 1);
    let name = &before[name_start..];
    if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    #[test]
    fn quotes() {
        assert_eq!(string_literal("'abc'"), "\"abc\"");
        assert_eq!(string_literal("'it\\'s'"), "\"it's\"");
        assert_eq!(string_literal("'say \"hi\"'"), "'say \"hi\"'");
        assert_eq!(string_literal("'a\\\\'"), "\"a\\\\\"");
        assert_eq!(string_literal("''"), "\"\"");
        assert_eq!(string_literal("\"x\""), "\"x\"");
    }

    #[test]
    fn blank_lines() {
        assert!(blank_line_before("a;\n\n  b;", 6));
        assert!(!blank_line_before("a;\n  b;", 5));
    }

    #[test]
    fn pattern_literals() {
        let src = "-1_000..=0x1fu8 | 'a' /* 2 */ 1.5e-3";
        let span = Span::new(FileId(0), 0, src.len() as u32);
        assert_eq!(
            literal_tokens(src, span),
            ["1_000", "0x1fu8", "\"a\"", "1.5e-3"]
        );
        let src = "1..10";
        let span = Span::new(FileId(0), 0, src.len() as u32);
        assert_eq!(literal_tokens(src, span), ["1", "10"]);
    }

    #[test]
    fn fn_type_names() {
        let src = "(acc: i64, x: T, U) => R";
        assert_eq!(fn_type_param_name(src, 6), Some("acc"));
        assert_eq!(fn_type_param_name(src, 14), Some("x"));
        assert_eq!(fn_type_param_name(src, 17), None);
    }
}

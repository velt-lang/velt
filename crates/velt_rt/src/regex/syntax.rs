//! JavaScript regular-expression syntax → Rust `regex` syntax.
//!
//! The two agree on almost everything a JS program writes (classes, quantifiers, anchors,
//! alternation, named groups `(?<n>…)`, `\p{…}` escapes). The differences handled here:
//! - `\d \D \w \W \b \B` are ASCII-only in JS; Rust's are Unicode-aware. They become explicit
//!   ASCII classes (`\b`/`\B` become `(?-u:\b)`/`(?-u:\B)`; inside a class JS `\b` is backspace).
//! - Inside a JS class `[`, `&&`, `--` and `~~` are literal; Rust reads them as nested classes and
//!   set operators, so `[`, `&`, `~` (and a `-` that follows another `-`) are escaped.
//! - JS `[^]` matches any character and `[]` matches nothing.
//! - JS line terminators are `\n`, `\r`, U+2028 and U+2029, so without the `s` flag `.` becomes
//!   a class that excludes all four. (With `m`, the caller turns on CRLF mode so `^`/`$` also
//!   stop at `\r`; U+2028/2029 are not anchor boundaries, a known gap.)
//! - `\0` (NUL) and `\cX` (control letter) have no Rust spelling.
//! - Annex B: a `{` that doesn't start a valid quantifier (`x{`, `a{,3}`) and a lone `}` are
//!   literal text.
//!
//! Lookaround and backreferences have no Rust equivalent; the regex crate rejects them and the
//! caller reports its error as a `SyntaxError`-like message.

/// ASCII word characters (JS `\w`).
const WORD: &str = "0-9A-Za-z_";

/// `.` without the `s` flag: any character except a JS line terminator.
const DOT: &str = "[^\\n\\r\\x{2028}\\x{2029}]";

/// Rewrites a JS pattern for the `regex` crate (see the module docs). `dot_all` is the `s` flag.
pub fn translate(js: &str, dot_all: bool) -> String {
    let mut out = String::with_capacity(js.len() + 16);
    let mut chars = js.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(e) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match e {
                    '0' if !chars.peek().is_some_and(char::is_ascii_digit) => out.push_str("\\x00"),
                    'c' if chars.peek().is_some_and(char::is_ascii_alphabetic) => {
                        let letter = chars.next().expect("ICE: peeked");
                        out.push_str(&format!("\\x{:02X}", letter as u32 % 32));
                    }
                    _ => push_escape(&mut out, e, in_class),
                }
            }
            '.' if !in_class && !dot_all => out.push_str(DOT),
            '{' if !in_class => match quantifier(&mut chars) {
                Some(q) => out.push_str(&q),
                None => out.push_str("\\{"),
            },
            '}' if !in_class => out.push_str("\\}"),
            '[' if !in_class => {
                in_class = true;
                let negated = chars.peek() == Some(&'^');
                if negated {
                    chars.next();
                }
                if chars.peek() == Some(&']') {
                    chars.next();
                    in_class = false;
                    // `[^]` = any character, `[]` = nothing.
                    out.push_str(if negated {
                        "(?s:.)"
                    } else {
                        "[^\\x00-\\x{10FFFF}]"
                    });
                    continue;
                }
                out.push_str(if negated { "[^" } else { "[" });
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '[' | '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
            }
            '-' if in_class && out.ends_with('-') => out.push_str("\\-"),
            _ => out.push(c),
        }
    }
    out
}

/// After a `{`: consumes and returns a valid quantifier (`{n}`, `{n,}`, `{n,m}`) including its
/// braces, or consumes nothing and returns `None` (the `{` is then literal).
fn quantifier(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let rest: String = chars.clone().take_while(|&c| c != '}').collect();
    let (min, max) = rest.split_once(',').unwrap_or((&rest, "0"));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let closed = chars.clone().nth(rest.chars().count()) == Some('}');
    if !closed || min.is_empty() || !digits(min) || !digits(max) {
        return None;
    }
    for _ in 0..=rest.chars().count() {
        chars.next();
    }
    Some(format!("{{{rest}}}"))
}

fn push_escape(out: &mut String, e: char, in_class: bool) {
    match (e, in_class) {
        ('d', false) => out.push_str("[0-9]"),
        ('d', true) => out.push_str("0-9"),
        ('D', _) => out.push_str("[^0-9]"),
        ('w', false) => {
            out.push('[');
            out.push_str(WORD);
            out.push(']');
        }
        ('w', true) => out.push_str(WORD),
        ('W', _) => {
            out.push_str("[^");
            out.push_str(WORD);
            out.push(']');
        }
        ('b', false) => out.push_str("(?-u:\\b)"),
        ('B', false) => out.push_str("(?-u:\\B)"),
        ('b', true) => out.push_str("\\x08"),
        ('/', _) => out.push('/'),
        _ => {
            out.push('\\');
            out.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::translate;

    fn translate_js(p: &str) -> String {
        translate(p, false)
    }

    #[test]
    fn ascii_shorthands() {
        assert_eq!(translate_js(r"\d+"), "[0-9]+");
        assert_eq!(translate_js(r"[\d.]"), "[0-9.]");
        assert_eq!(translate_js(r"[^\w]"), "[^0-9A-Za-z_]");
        assert_eq!(translate_js(r"\bx\B"), r"(?-u:\b)x(?-u:\B)");
        assert_eq!(translate_js(r"[\b]"), r"[\x08]");
        assert_eq!(translate_js(r"a\/b"), "a/b");
    }

    #[test]
    fn class_literals_rust_treats_as_operators() {
        assert_eq!(translate_js("[a[&&~]"), r"[a\[\&\&\~]");
        assert_eq!(translate_js("[--]"), r"[-\-]");
        assert_eq!(translate_js("[]a]"), r"[^\x00-\x{10FFFF}]a]");
        assert_eq!(translate_js("[^]"), "(?s:.)");
        assert_eq!(translate_js("[]]"), r"[^\x00-\x{10FFFF}]]");
    }

    #[test]
    fn line_terminators_and_dot() {
        assert_eq!(translate_js("a.b"), r"a[^\n\r\x{2028}\x{2029}]b");
        assert_eq!(translate("a.b", true), "a.b");
        assert_eq!(translate_js("[.]"), "[.]");
    }

    #[test]
    fn standard_escapes() {
        assert_eq!(translate_js(r"\0"), r"\x00");
        assert_eq!(translate_js(r"\cA\cj"), r"\x01\x0A");
        assert_eq!(translate_js(r"\01"), r"\01");
    }

    #[test]
    fn annex_b_braces() {
        assert_eq!(translate_js("a{2}b{1,}c{0,3}"), "a{2}b{1,}c{0,3}");
        assert_eq!(translate_js("x{"), r"x\{");
        assert_eq!(translate_js("{}"), r"\{\}");
        assert_eq!(translate_js("a{,3}"), r"a\{,3\}");
        assert_eq!(translate_js("a{x}"), r"a\{x\}");
    }

    #[test]
    fn translations_compile() {
        for p in [
            r"\d+",
            r"[\w.-]+@[\w-]+\.\w+",
            r"(?<year>\d{4})-(?<m>\d\d)",
            "[a[&&~]",
            "[^]",
            r"\0\cA.x{{}a{,3}",
        ] {
            assert!(regex::bytes::Regex::new(&translate_js(p)).is_ok(), "{p}");
        }
    }
}

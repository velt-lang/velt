//! Layout-insensitivity: inserting random whitespace and newlines between the tokens of every
//! corpus file must not change the formatted output.
//!
//! Whitespace is only added where it cannot change what the formatter keeps from the layout:
//! never inside literals, comments, JSX text or JSX attribute strings, never on a line that has a
//! comment after the insertion point (a comment's line decides whether it trails the code before
//! it), and no newline into whitespace that already holds one (that would create blank lines,
//! which are preserved).

mod common;

use common::{corpus, first_difference, parses};
use velt_fmt::format_source;

/// Deterministic xorshift generator (no external dependency needed).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Byte offsets where whitespace may be inserted, with whether a newline is allowed there.
fn insertion_points(src: &str) -> Vec<(usize, bool)> {
    let bytes = src.as_bytes();
    let mut code = vec![false; bytes.len() + 1];
    let mut comment_start = vec![false; bytes.len() + 1];
    mark_code(bytes, &mut code, &mut comment_start);
    let mut out = vec![];
    for i in 1..bytes.len() {
        let after_punct = code[i - 1] && matches!(bytes[i - 1], b'(' | b'[' | b'{' | b',' | b';');
        let in_space = code[i] && code[i - 1] && bytes[i].is_ascii_whitespace();
        if !(after_punct || in_space) {
            continue;
        }
        let line_end = src[i..].find('\n').map_or(bytes.len(), |n| i + n);
        if (i..line_end).any(|k| comment_start[k]) {
            continue;
        }
        let run_lo = src[..i].trim_end_matches(char::is_whitespace).len();
        let run_hi = i + (src[i..].len() - src[i..].trim_start_matches(char::is_whitespace).len());
        let newline_ok = !src[run_lo..run_hi].contains('\n');
        out.push((i, newline_ok));
    }
    out
}

/// Where the scanner is: inside braces in code, a JSX tag or JSX children.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Brace,
    JsxTag { closing: bool },
    JsxChildren,
}

/// Marks bytes that are plain code (not inside strings, templates, regular expressions,
/// comments, JSX text or JSX attribute strings).
fn mark_code(bytes: &[u8], code: &mut [bool], comment_start: &mut [bool]) {
    let mut i = 0;
    let mut template_depth = 0usize;
    let mut stack: Vec<Ctx> = vec![];
    while i < bytes.len() {
        let c = bytes[i];
        if template_depth > 0 {
            // Templates (with their substitutions) are opaque.
            match c {
                b'\\' => i += 1,
                b'`' => template_depth -= 1,
                _ => {}
            }
            i += 1;
            continue;
        }
        match stack.last() {
            Some(Ctx::JsxChildren) => {
                i = jsx_children_byte(bytes, i, code, &mut stack);
                continue;
            }
            Some(&Ctx::JsxTag { closing }) if !is_comment_start(bytes, i) => {
                i = jsx_tag_byte(bytes, i, closing, code, &mut stack);
                continue;
            }
            _ => {}
        }
        match c {
            b'"' | b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != c && bytes[i] != b'\n' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'`' => {
                template_depth += 1;
                i += 1;
            }
            b'/' if is_comment_start(bytes, i) => {
                comment_start[i] = true;
                let (close, extra): (&[u8], usize) = if bytes[i + 1] == b'*' {
                    (b"*/", 2)
                } else {
                    (b"\n", 0)
                };
                i = (i + 2..bytes.len())
                    .find(|&k| bytes[k..].starts_with(close))
                    .map_or(bytes.len(), |k| k + extra);
            }
            b'/' if regex_may_start(bytes, i) => i = regex_end(bytes, i),
            _ => {
                code[i] = true;
                match c {
                    b'{' => stack.push(Ctx::Brace),
                    b'}' if stack.last() == Some(&Ctx::Brace) => {
                        stack.pop();
                    }
                    b'<' if jsx_may_start(bytes, i) => stack.push(Ctx::JsxTag { closing: false }),
                    _ => {}
                }
                i += 1;
            }
        }
    }
}

fn is_comment_start(bytes: &[u8], i: usize) -> bool {
    bytes[i] == b'/' && matches!(bytes.get(i + 1), Some(b'/' | b'*'))
}

/// One step between JSX tags: text is not code; `{`, `<` and `</` are.
fn jsx_children_byte(bytes: &[u8], i: usize, code: &mut [bool], stack: &mut Vec<Ctx>) -> usize {
    match bytes[i] {
        b'{' => {
            code[i] = true;
            stack.push(Ctx::Brace);
            i + 1
        }
        b'<' => {
            code[i] = true;
            let closing = bytes.get(i + 1) == Some(&b'/');
            stack.push(Ctx::JsxTag { closing });
            i + 1 + usize::from(closing)
        }
        _ => i + 1,
    }
}

/// One step inside a JSX tag: attribute strings are not code; `{` opens an expression, `<` an
/// element as an attribute value, and `>` / `/>` end the tag.
fn jsx_tag_byte(
    bytes: &[u8],
    i: usize,
    closing: bool,
    code: &mut [bool],
    stack: &mut Vec<Ctx>,
) -> usize {
    let c = bytes[i];
    match c {
        b'"' | b'\'' => (i + 1..bytes.len())
            .find(|&k| bytes[k] == c)
            .map_or(bytes.len(), |k| k + 1),
        b'/' | b'>' => {
            let self_closing = c == b'/' && bytes.get(i + 1) == Some(&b'>');
            if c == b'>' || self_closing {
                stack.pop();
                if closing && stack.last() == Some(&Ctx::JsxChildren) {
                    stack.pop();
                } else if !closing && !self_closing {
                    stack.push(Ctx::JsxChildren);
                }
            }
            code[i] = true;
            i + 1 + usize::from(self_closing)
        }
        _ => {
            code[i] = true;
            match c {
                b'{' => stack.push(Ctx::Brace),
                b'<' => stack.push(Ctx::JsxTag { closing: false }),
                _ => {}
            }
            i + 1
        }
    }
}

/// Does a `<` at `i` start a JSX element (the lexer's rule: an operand is expected and a name
/// or `>` follows, but not the type parameters `<T,` / `<T extends` of a generic arrow)?
fn jsx_may_start(bytes: &[u8], i: usize) -> bool {
    let next = bytes.get(i + 1).copied().unwrap_or(0);
    if !(next.is_ascii_alphabetic() || matches!(next, b'_' | b'$' | b'>')) {
        return false;
    }
    let before = std::str::from_utf8(&bytes[..i]).unwrap_or("").trim_end();
    let operand_expected = before.ends_with("return")
        || matches!(
            before.bytes().last(),
            None | Some(
                b'(' | b',' | b'=' | b':' | b'[' | b'!' | b'&' | b'|' | b'?' | b'{' | b';' | b'>'
            )
        );
    let rest = std::str::from_utf8(&bytes[i + 1..]).unwrap_or("");
    let after_name = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
    let after_name = after_name.trim_start();
    let generic = after_name.len() < rest.len()
        && (after_name.starts_with(',') || after_name.starts_with("extends "));
    operand_expected && !generic
}

/// Does a `/` at `i` start a regular expression literal (no operand before it)?
fn regex_may_start(bytes: &[u8], i: usize) -> bool {
    let prev = bytes[..i].iter().rev().find(|c| !c.is_ascii_whitespace());
    matches!(
        prev,
        None | Some(b'(' | b',' | b'=' | b':' | b'[' | b'!' | b'&' | b'|' | b'?')
    )
}

/// The byte after the regular expression literal starting at `i` (body and flags).
fn regex_end(bytes: &[u8], i: usize) -> usize {
    let (mut k, mut class) = (i + 1, false);
    while k < bytes.len() && bytes[k] != b'\n' {
        match bytes[k] {
            b'\\' => k += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => break,
            _ => {}
        }
        k += 1;
    }
    k += 1;
    while k < bytes.len() && bytes[k].is_ascii_alphabetic() {
        k += 1;
    }
    k
}

fn perturb(src: &str, rng: &mut Rng) -> String {
    let points = insertion_points(src);
    let mut out = String::with_capacity(src.len() * 2);
    let mut last = 0;
    for (at, newline_ok) in points {
        if rng.below(3) != 0 {
            continue;
        }
        out.push_str(&src[last..at]);
        last = at;
        for _ in 0..=rng.below(3) {
            let choices: &[char] = if newline_ok {
                &[' ', '\t', '\n']
            } else {
                &[' ', '\t']
            };
            out.push(choices[rng.below(choices.len() as u64) as usize]);
        }
    }
    out.push_str(&src[last..]);
    out
}

#[test]
fn extra_whitespace_does_not_change_the_output() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut failures = vec![];
    for path in corpus() {
        let src = std::fs::read_to_string(&path).unwrap();
        if !parses(&src) {
            continue;
        }
        let expected = format_source(&src).unwrap();
        for round in 0..3 {
            let noisy = perturb(&src, &mut rng);
            match format_source(&noisy) {
                Ok(got) if got == expected => {}
                Ok(got) => failures.push(format!(
                    "{} (round {round}): {}",
                    path.display(),
                    first_difference(&expected, &got)
                )),
                Err(d) => failures.push(format!(
                    "{}: perturbed source refused: {d:?}",
                    path.display()
                )),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n"));
}

#[test]
fn jsx_text_and_attribute_strings_are_not_perturbed() {
    let src = "const a = <p t='x, y'>Hello, world ( {f(b, c)} </p>;\nconst g = <T,>(x: T) => x;\n";
    let points: Vec<usize> = insertion_points(src)
        .into_iter()
        .map(|(at, _)| at)
        .collect();
    let after = |needle: &str| src.find(needle).unwrap() + needle.len();
    assert!(!points.contains(&after("x,")), "{points:?}");
    assert!(!points.contains(&after("Hello,")), "{points:?}");
    assert!(!points.contains(&after("world (")), "{points:?}");
    assert!(points.contains(&after("{f(")), "{points:?}");
    assert!(points.contains(&after("(b,")), "{points:?}");
    assert!(points.contains(&after("<T,")), "{points:?}");
}

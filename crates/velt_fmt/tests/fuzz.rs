//! Layout-insensitivity: inserting random whitespace and newlines between the tokens of every
//! corpus file must not change the formatted output.
//!
//! Whitespace is only added where it cannot change what the formatter keeps from the layout:
//! never inside literals or comments, never on a line that has a comment after the insertion
//! point (a comment's line decides whether it trails the code before it), and no newline into
//! whitespace that already holds one (that would create blank lines, which are preserved).

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

/// Marks bytes that are plain code (not inside strings, templates, regular expressions or
/// comments).
fn mark_code(bytes: &[u8], code: &mut [bool], comment_start: &mut [bool]) {
    let mut i = 0;
    let mut template_depth = 0usize;
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
            b'/' if matches!(bytes.get(i + 1), Some(b'/') | Some(b'*')) => {
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
                i += 1;
            }
        }
    }
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

// JS string methods on Rust's `str`, for the `string_methods*` rows: positions and lengths in
// UTF-16 code units as JS's (an ASCII string's are its byte offsets; another string's are
// translated by counting), absent results -1 as Velt's. Text is well-formed and the positions
// these programs pass fall on code point boundaries, so the lone-surrogate cases are left out.
#![allow(dead_code)]

/// `s.length`.
pub fn len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

/// The byte offset of the code-unit position `u` (at most `len(s)`, on a boundary).
fn byte_at(s: &str, u: usize) -> usize {
    if s.is_ascii() {
        return u.min(s.len());
    }
    let mut units = 0;
    for (b, c) in s.char_indices() {
        if units >= u {
            return b;
        }
        units += c.len_utf16();
    }
    s.len()
}

/// The code-unit position of the byte offset `b`.
fn unit_at(s: &str, b: usize) -> usize {
    if s.is_ascii() {
        b
    } else {
        s[..b].encode_utf16().count()
    }
}

fn clamp(i: i64, len: usize) -> usize {
    i.clamp(0, len as i64) as usize
}

fn relative(i: i64, len: usize) -> usize {
    if i < 0 {
        (len as i64 + i).max(0) as usize
    } else {
        (i as usize).min(len)
    }
}

pub fn index_of(s: &str, needle: &str, from: i64) -> i64 {
    let start = byte_at(s, clamp(from, len(s)));
    s[start..]
        .find(needle)
        .map_or(-1, |i| unit_at(s, start + i) as i64)
}

pub fn last_index_of(s: &str, needle: &str) -> i64 {
    s.rfind(needle).map_or(-1, |i| unit_at(s, i) as i64)
}

pub fn slice(s: &str, start: i64, end: i64) -> &str {
    let n = len(s);
    let (a, b) = (relative(start, n), relative(end, n));
    if a >= b {
        return "";
    }
    &s[byte_at(s, a)..byte_at(s, b)]
}

pub fn substring(s: &str, start: i64, end: i64) -> &str {
    let n = len(s);
    let (a, b) = (clamp(start, n), clamp(end, n));
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    &s[byte_at(s, a)..byte_at(s, b)]
}

pub fn char_code_at(s: &str, i: i64) -> i64 {
    if i < 0 {
        return -1;
    }
    if s.is_ascii() {
        return s.as_bytes().get(i as usize).map_or(-1, |&b| b as i64);
    }
    s.encode_utf16().nth(i as usize).map_or(-1, |u| u as i64)
}

pub fn char_at(s: &str, i: i64) -> &str {
    if i < 0 || i as usize >= len(s) {
        return "";
    }
    slice(s, i, i + 1)
}

/// JS `WhiteSpace` + `LineTerminator`.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

pub fn trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

pub fn trim_start(s: &str) -> &str {
    s.trim_start_matches(is_js_space)
}

pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_space)
}

/// `s.padStart(target, fill)` / `padEnd` (a fill of code points outside the BMP is not used).
pub fn pad(s: &str, target: usize, fill: &str, at_start: bool) -> String {
    let n = len(s);
    if target <= n || fill.is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + (target - n) * fill.len());
    if !at_start {
        out.push_str(s);
    }
    out.extend(fill.chars().cycle().take(target - n));
    if at_start {
        out.push_str(s);
    }
    out
}

pub fn flag(b: bool) -> i64 {
    b as i64
}

pub fn mix(sum: i64, x: i64) -> i64 {
    (sum * 31 + x) % 1000000007
}

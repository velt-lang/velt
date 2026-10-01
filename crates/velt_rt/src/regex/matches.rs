//! Global-match iteration with JavaScript semantics (`matchAll`, `replaceAll`, `split`).
//!
//! JS searches again from `lastIndex` after every match. After an empty match it advances one
//! character first, so it can find an empty match right where a non-empty one ended ("axxb" with
//! `x*` gives "", "xx", "", ""). The `regex` crate's iterators skip that match, so this module
//! searches with `find_at` / `captures_read_at` itself.
//!
//! Callers that don't need groups use [`each_find`], which runs the DFA without capture
//! tracking. That is the fast path for group-free patterns and `$`-free replacements.

use regex::bytes::{CaptureLocations, Regex};

/// The search start after a match `start..end`: `end`, or one character past it for an empty
/// match. `None` once the search has passed the end of `s`.
fn next_pos(s: &[u8], start: usize, end: usize) -> Option<usize> {
    if end > start {
        return Some(end);
    }
    if end >= s.len() {
        return None;
    }
    // Skip one UTF-8 character; continuation bytes are 0b10xx_xxxx.
    let step = 1 + s[end + 1..]
        .iter()
        .take_while(|&&b| b & 0xC0 == 0x80)
        .count();
    Some(end + step)
}

/// Calls `f(start, end)` for every match in JS order; `f` returns `false` to stop.
pub fn each_find(re: &Regex, s: &[u8], mut f: impl FnMut(usize, usize) -> bool) {
    let mut pos = 0;
    while let Some(m) = re.find_at(s, pos) {
        if !f(m.start(), m.end()) {
            return;
        }
        match next_pos(s, m.start(), m.end()) {
            Some(p) => pos = p,
            None => return,
        }
    }
}

/// Calls `f(locs)` with the group locations of every match in JS order; `f` returns `false` to
/// stop. One `CaptureLocations` is reused for all matches.
pub fn each_captures(re: &Regex, s: &[u8], mut f: impl FnMut(&CaptureLocations) -> bool) {
    let mut locs = re.capture_locations();
    let mut pos = 0;
    while let Some(m) = re.captures_read_at(&mut locs, s, pos) {
        if !f(&locs) {
            return;
        }
        match next_pos(s, m.start(), m.end()) {
            Some(p) => pos = p,
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finds(p: &str, s: &str) -> Vec<(usize, usize)> {
        let re = Regex::new(p).expect("valid pattern");
        let mut v = Vec::new();
        each_find(&re, s.as_bytes(), |a, b| {
            v.push((a, b));
            true
        });
        v
    }

    #[test]
    fn empty_match_right_after_a_match() {
        assert_eq!(finds("x*", "axxb"), [(0, 0), (1, 3), (3, 3), (4, 4)]);
        assert_eq!(finds("x*", "xa"), [(0, 1), (1, 1), (2, 2)]);
        assert_eq!(finds("x*", ""), [(0, 0)]);
    }

    #[test]
    fn empty_match_steps_over_a_whole_character() {
        assert_eq!(finds("", "é"), [(0, 0), (2, 2)]);
    }

    #[test]
    fn captures_agree_with_finds() {
        let re = Regex::new("(x)*").expect("valid pattern");
        let mut v = Vec::new();
        each_captures(&re, b"axxb", |l| {
            v.push(l.get(0).expect("group 0"));
            true
        });
        assert_eq!(v, finds("(x)*", "axxb"));
    }
}

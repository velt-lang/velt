//! JS `String.prototype.replace` / `split` semantics on top of the `regex` crate.
//!
//! Rust's replacement syntax differs from JS (`$1a` is group "1a" in Rust, group 1 then `a` in
//! JS; Rust has no `` $` `` / `$'`), so replacements are expanded here, following
//! GetSubstitution in the ECMAScript spec. A replacement without `$` is inserted as is, and
//! the search then skips capture tracking.

use super::matches::{each_captures, each_find};
use crate::str::wtf8::push_joining;
use regex::bytes::{CaptureLocations, Regex};

/// Replace the first match (or every match with `all`) of `re` in `s`. Every piece is pushed
/// with [`push_joining`], so a high surrogate meeting a low one at a seam becomes the pair.
pub fn replace(re: &Regex, s: &[u8], replacement: &[u8], all: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut last = 0;
    if !replacement.contains(&b'$') {
        each_find(re, s, |start, end| {
            push_joining(&mut out, &s[last..start]);
            push_joining(&mut out, replacement);
            last = end;
            all
        });
    } else {
        each_captures(re, s, |locs| {
            let (start, end) = locs.get(0).expect("ICE: group 0 always matches");
            push_joining(&mut out, &s[last..start]);
            expand(re, locs, s, replacement, &mut out);
            last = end;
            all
        });
    }
    push_joining(&mut out, &s[last..]);
    out
}

/// The text of group `n`, if it took part in the match.
fn group<'s>(locs: &CaptureLocations, s: &'s [u8], n: usize) -> Option<&'s [u8]> {
    locs.get(n).map(|(a, b)| &s[a..b])
}

/// Append `replacement` for one match, expanding `$` patterns like JS GetSubstitution.
fn expand(re: &Regex, locs: &CaptureLocations, s: &[u8], replacement: &[u8], out: &mut Vec<u8>) {
    let (start, end) = locs.get(0).expect("ICE: group 0 always matches");
    let groups = locs.len();
    let mut i = 0;
    while i < replacement.len() {
        let c = replacement[i];
        let next = replacement.get(i + 1).copied();
        if c != b'$' || next.is_none() {
            out.push(c);
            i += 1;
            continue;
        }
        let consumed = match next.unwrap_or(0) {
            b'$' => {
                out.push(b'$');
                2
            }
            b'&' => {
                push_joining(out, &s[start..end]);
                2
            }
            b'`' => {
                push_joining(out, &s[..start]);
                2
            }
            b'\'' => {
                push_joining(out, &s[end..]);
                2
            }
            b'0'..=b'9' => numbered(locs, s, groups, &replacement[i + 1..], out),
            b'<' => named(re, locs, s, &replacement[i + 1..], out),
            _ => 0,
        };
        if consumed == 0 {
            out.push(b'$');
            i += 1;
        } else {
            i += consumed;
        }
    }
}

/// `$n` / `$nn` (two digits win when that group exists); returns the bytes consumed including
/// the `$`, or 0 when the text is not a valid group reference (then it is literal).
fn numbered(
    locs: &CaptureLocations,
    s: &[u8],
    groups: usize,
    rest: &[u8],
    out: &mut Vec<u8>,
) -> usize {
    let d1 = (rest[0] - b'0') as usize;
    let two = rest
        .get(1)
        .filter(|b| b.is_ascii_digit())
        .map(|b| d1 * 10 + (b - b'0') as usize);
    let (n, len) = match two {
        Some(n) if n >= 1 && n < groups => (n, 2),
        _ if d1 >= 1 && d1 < groups => (d1, 1),
        _ => return 0,
    };
    if let Some(g) = group(locs, s, n) {
        push_joining(out, g);
    }
    len + 1
}

/// `$<name>`: the named group's text (empty if it did not take part). Without named groups the
/// text is literal, as in JS.
fn named(re: &Regex, locs: &CaptureLocations, s: &[u8], rest: &[u8], out: &mut Vec<u8>) -> usize {
    if re.capture_names().all(|n| n.is_none()) {
        return 0;
    }
    let Some(close) = rest.iter().position(|&b| b == b'>') else {
        return 0;
    };
    let name = &rest[1..close];
    let index = re
        .capture_names()
        .position(|n| n.is_some_and(|n| n.as_bytes() == name));
    if let Some(g) = index.and_then(|i| group(locs, s, i)) {
        push_joining(out, g);
    }
    close + 2
}

/// JS `split` with a regex separator: captured groups are spliced into the result; an empty
/// match at the very start or end of the input does not produce an empty piece.
pub fn split(re: &Regex, s: &[u8], limit: usize) -> Vec<Vec<u8>> {
    let mut parts = Vec::new();
    if s.is_empty() {
        if re.find(s).is_none() {
            parts.push(Vec::new());
        }
        return parts;
    }
    let mut last = 0;
    let mut full = false;
    each_captures(re, s, |locs| {
        let (start, end) = locs.get(0).expect("ICE: group 0 always matches");
        if end == 0 || start == s.len() || end == last {
            return true;
        }
        parts.push(s[last..start].to_vec());
        for n in 1..locs.len() {
            parts.push(group(locs, s, n).map_or_else(Vec::new, <[u8]>::to_vec));
        }
        last = end;
        full = parts.len() >= limit;
        !full
    });
    if !full {
        parts.push(s[last..].to_vec());
    }
    parts.truncate(limit);
    parts
}

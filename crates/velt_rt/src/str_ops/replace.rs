//! `s.replace(from, to)` and `s.replaceAll(from, to)` with string patterns.
//!
//! The replacement follows JS `GetSubstitution` for string patterns: `$$` → `$`, `$&` → the
//! match, `` $` `` → the text before it, `$'` → the text after it; any other `$` sequence
//! (`$1`, `$<name>`) is copied literally because a string pattern has no captures.

use super::{sub_string, text};
use crate::str::VeltStr;

/// Append the replacement for the match at `pos..pos + len` of `s`.
fn push_substitution(out: &mut Vec<u8>, to: &str, s: &str, pos: usize, len: usize) {
    if !to.contains('$') {
        out.extend_from_slice(to.as_bytes());
        return;
    }
    let bytes = to.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let expansion = match (bytes[i], bytes.get(i + 1)) {
            (b'$', Some(b'$')) => Some("$"),
            (b'$', Some(b'&')) => Some(&s[pos..pos + len]),
            (b'$', Some(b'`')) => Some(&s[..pos]),
            (b'$', Some(b'\'')) => Some(&s[pos + len..]),
            _ => None,
        };
        match expansion {
            Some(e) => {
                out.extend_from_slice(e.as_bytes());
                i += 2;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
}

/// Positions where `from` matches: every non-overlapping match, or (empty `from`) every
/// character boundary including both ends; only the first one unless `all`.
fn match_positions(s: &str, from: &str, all: bool) -> Vec<usize> {
    let limit = if all { usize::MAX } else { 1 };
    if from.is_empty() {
        s.char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(s.len()))
            .take(limit)
            .collect()
    } else {
        s.match_indices(from).map(|(i, _)| i).take(limit).collect()
    }
}

unsafe fn replace(
    s: *const VeltStr,
    from: *const VeltStr,
    to: *const VeltStr,
    all: bool,
) -> VeltStr {
    let (t, from, to) = (text(s), text(from), text(to));
    let positions = match_positions(t, from, all);
    if positions.is_empty() {
        return sub_string(s, 0, t.len());
    }
    let mut out = Vec::with_capacity(t.len() + positions.len() * to.len());
    let mut copied = 0;
    for pos in positions {
        out.extend_from_slice(&t.as_bytes()[copied..pos]);
        push_substitution(&mut out, to, t, pos, from.len());
        copied = pos + from.len();
    }
    out.extend_from_slice(&t.as_bytes()[copied..]);
    VeltStr::from_vec(out)
}

/// `s.replace(from, to)`: the first occurrence only.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_replace(
    s: *const VeltStr,
    from: *const VeltStr,
    to: *const VeltStr,
    out: *mut VeltStr,
) {
    out.write(replace(s, from, to, false));
}

/// `s.replaceAll(from, to)`: every non-overlapping occurrence, left to right.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_replace_all(
    s: *const VeltStr,
    from: *const VeltStr,
    to: *const VeltStr,
    out: *mut VeltStr,
) {
    out.write(replace(s, from, to, true));
}

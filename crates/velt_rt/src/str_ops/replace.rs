//! `s.replace(from, to)` and `s.replaceAll(from, to)` with string patterns.
//!
//! The replacement follows JS `GetSubstitution` for string patterns: `$$` → `$`, `$&` → the
//! match, `` $` `` → the text before it, `$'` → the text after it; any other `$` sequence
//! (`$1`, `$<name>`) is copied literally because a string pattern has no captures.

use super::{bytes, sub_string, text};
use crate::str::wtf8::{self, push_joining};
use crate::str::VeltStr;

/// Append the replacement for the match at `pos..pos + len` of `s` (all WTF-8). Every piece is
/// pushed with [`push_joining`], so a high surrogate meeting a low one at a seam becomes the
/// pair (canonical WTF-8).
fn push_substitution(out: &mut Vec<u8>, to: &[u8], s: &[u8], pos: usize, len: usize) {
    let Some(first) = memchr::memchr(b'$', to) else {
        push_joining(out, to);
        return;
    };
    push_joining(out, &to[..first]);
    let mut i = first;
    let mut run = first;
    while i < to.len() {
        let expansion: Option<&[u8]> = match (to[i], to.get(i + 1)) {
            (b'$', Some(b'$')) => Some(b"$"),
            (b'$', Some(b'&')) => Some(&s[pos..pos + len]),
            (b'$', Some(b'`')) => Some(&s[..pos]),
            (b'$', Some(b'\'')) => Some(&s[pos + len..]),
            _ => None,
        };
        match expansion {
            Some(e) => {
                push_joining(out, &to[run..i]);
                push_joining(out, e);
                i += 2;
                run = i;
            }
            None => i += 1,
        }
    }
    push_joining(out, &to[run..]);
}

/// Positions where `from` matches: every non-overlapping match, or (empty `from`) every code
/// point boundary including both ends; only the first one unless `all`.
fn match_positions(s: &[u8], from: &[u8], all: bool) -> Vec<usize> {
    let limit = if all { usize::MAX } else { 1 };
    if from.is_empty() {
        wtf8::boundaries(s)
            .chain(std::iter::once(s.len()))
            .take(limit)
            .collect()
    } else {
        memchr::memmem::find_iter(s, from).take(limit).collect()
    }
}

/// [`match_positions`] for well-formed text (the `str` search, as before lone surrogates).
fn match_positions_str(s: &str, from: &str, all: bool) -> Vec<usize> {
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
    let positions = match (text(s), text(from)) {
        (Ok(t), Ok(f)) => match_positions_str(t, f, all),
        _ => match_positions(bytes(s), bytes(from), all),
    };
    let (t, from, to) = (bytes(s), bytes(from), bytes(to));
    if positions.is_empty() {
        return sub_string(s, 0, t.len());
    }
    let mut out = Vec::with_capacity(t.len() + positions.len() * to.len());
    let mut copied = 0;
    for pos in positions {
        push_joining(&mut out, &t[copied..pos]);
        push_substitution(&mut out, to, t, pos, from.len());
        copied = pos + from.len();
    }
    push_joining(&mut out, &t[copied..]);
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

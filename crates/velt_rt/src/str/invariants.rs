//! Checks of the string invariants (canonical WTF-8, the unit and lone-surrogate counts, the
//! form), compiled in only where they are affordable:
//! - debug builds of the runtime (every golden run) check each appended piece and the seam it
//!   makes: O(piece), so debug-built programs never turn quadratic;
//! - the runtime's own tests also recount the whole string after every operation that produces
//!   one, up to [`RECOUNT_MAX`] bytes (the performance tests build megabytes one small push at a
//!   time, which a recount per push would make quadratic).
//!
//! A violation is a runtime bug, reported as an internal error.

#[cfg(any(debug_assertions, test))]
use super::wtf8;
use super::{Summary, VeltStr};

/// Debug builds: `piece` is canonical WTF-8 and `summary`, if given, is its own.
#[cfg(debug_assertions)]
pub(super) fn check_piece(piece: &[u8], summary: Option<Summary>) {
    if let Err(e) = wtf8::check_canonical(piece) {
        panic!("ICE: a string piece is not canonical WTF-8: {e}");
    }
    if let Some(sum) = summary {
        let counted = wtf8::summarize(piece);
        assert_eq!(
            sum.units, counted.units,
            "ICE: wrong unit count of a string piece"
        );
        if sum.lone != wtf8::LONE_UNKNOWN {
            assert_eq!(
                sum.lone, counted.lone,
                "ICE: wrong lone count of a string piece"
            );
        }
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub(super) fn check_piece(_piece: &[u8], _summary: Option<Summary>) {}

/// Debug builds: the bytes around `at` (where a piece was appended) are canonical, i.e. a high
/// surrogate ending the string was joined with a low one starting the piece.
#[cfg(debug_assertions)]
pub(super) fn check_seam(s: &VeltStr, at: usize) {
    // SAFETY: called on strings the runtime just built.
    let bytes = unsafe { s.as_bytes() };
    let is_lead = |i: usize| i >= bytes.len() || bytes[i] & 0xC0 != 0x80;
    let mut start = at.saturating_sub(6).min(bytes.len());
    while !is_lead(start) {
        start -= 1;
    }
    let mut end = (at + 6).min(bytes.len());
    while !is_lead(end) {
        end += 1;
    }
    if let Err(e) = wtf8::check_canonical(&bytes[start..end]) {
        panic!("ICE: a string seam is not canonical WTF-8: {e}");
    }
}

/// Longest string [`check_whole`] recounts.
#[cfg(test)]
const RECOUNT_MAX: usize = 4096;

/// Runtime tests: everything about `s` is consistent with its bytes.
#[cfg(test)]
pub(super) fn check_whole(s: &VeltStr) {
    // SAFETY: called on strings the runtime just built.
    let bytes = unsafe { s.as_bytes() };
    if bytes.len() > RECOUNT_MAX {
        return;
    }
    if let Err(e) = wtf8::check_canonical(bytes) {
        panic!("ICE: {s:?} is not canonical WTF-8: {e}");
    }
    let counted = wtf8::summarize(bytes);
    assert_eq!(s.units(), counted.units, "ICE: unit count of {s:?}");
    assert_eq!(s.is_ascii(), counted.units == bytes.len(), "{s:?}");
    if s.is_inline() {
        assert!(super::fits_inline(s.len(), s.units()), "ICE: inline {s:?}");
        let flag = s.tag() & super::INLINE_LONE != 0;
        assert!(
            flag || counted.lone == 0,
            "ICE: {s:?} has lone surrogates but no flag"
        );
    } else if s.is_heap() {
        assert!(
            s.len() <= s.w2 as usize,
            "ICE: {s:?} is longer than its buffer"
        );
        if !s.is_ascii() {
            // SAFETY: a heap string with a header.
            let lone = unsafe { super::heap::lone(s.ptr()) };
            if lone != wtf8::LONE_UNKNOWN {
                assert_eq!(lone, counted.lone, "ICE: lone surrogates of {s:?}");
            }
        }
    }
}

#[cfg(not(test))]
#[inline(always)]
pub(super) fn check_whole(_s: &VeltStr) {}

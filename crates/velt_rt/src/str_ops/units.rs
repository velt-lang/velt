//! Code-unit work for the string methods (#377 phase 2b, docs/internals/design/strings.md "How
//! the operations compile"): slicing at a code-unit position that may fall between the two halves
//! of a pair, and the searches whose needle can match half of a pair.
//!
//! The common paths stay on the WTF-8 bytes: a byte search finds every match of a needle that
//! neither starts with a lone low surrogate nor ends with a lone high one (WTF-8 is
//! self-synchronizing, and canonical form stores a pair one way only). Such a needle ("half
//! needle") can also match the low or high half of a pair stored as one 4-byte sequence, so those
//! searches decode both sides to code units here (rare: lone surrogates come only from slicing
//! between halves, `String.fromCharCode` and JSON escapes).

use crate::str::{wtf8, BytePos, VeltStr};

/// The high surrogate of a supplementary code point.
#[inline]
pub(super) fn high_of(cp: u32) -> u16 {
    (0xD800 + ((cp - 0x10000) >> 10)) as u16
}

/// The low surrogate of a supplementary code point.
#[inline]
pub(super) fn low_of(cp: u32) -> u16 {
    (0xDC00 + ((cp - 0x10000) & 0x3FF)) as u16
}

/// The 3-byte WTF-8 of a lone surrogate.
fn lone(unit: u16) -> [u8; 3] {
    let mut buf = [0u8; 4];
    let mut out = [0u8; 3];
    out.copy_from_slice(wtf8::encode(unit as u32, &mut buf));
    out
}

/// The code unit at `pos` of canonical WTF-8.
#[inline]
pub(super) fn unit_at(bytes: &[u8], pos: BytePos) -> u16 {
    let (cp, _) = wtf8::decode_at(bytes, pos.byte);
    match (cp >= 0x10000, pos.low_half) {
        (false, _) => cp as u16,
        (true, false) => high_of(cp),
        (true, true) => low_of(cp),
    }
}

/// Code units `a..b` of `s` (`a < b <= s.units()`, `s` not ASCII): a sub-range of the bytes
/// (borrowed, shared or copied as `VeltStr::substring` does) unless an end falls between the
/// halves of a pair, which is then re-encoded as a lone surrogate (a copy).
///
/// # Safety
/// `s` must be valid.
pub(super) unsafe fn slice(s: &VeltStr, a: usize, b: usize) -> VeltStr {
    let (pa, pb) = s.unit_range_to_bytes(a, b);
    if !pa.low_half && !pb.low_half {
        return s.substring_units(pa.byte, pb.byte, b - a);
    }
    let bytes = s.as_bytes();
    let mut v = Vec::with_capacity(pb.byte - pa.byte + 6);
    let mut from = pa.byte;
    if pa.low_half {
        v.extend_from_slice(&lone(unit_at(bytes, pa)));
        from += 4;
    }
    if from <= pb.byte {
        v.extend_from_slice(&bytes[from..pb.byte]);
        if pb.low_half {
            v.extend_from_slice(&lone(unit_at(
                bytes,
                BytePos {
                    low_half: false,
                    ..pb
                },
            )));
        }
    }
    VeltStr::from_vec(v)
}

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

/// Can `needle` (canonical WTF-8) match half of a surrogate pair: does it start with a lone low
/// surrogate or end with a lone high one?
#[inline]
pub(super) fn is_half_needle(needle: &[u8]) -> bool {
    wtf8::starts_with_low(needle) || wtf8::ends_with_high(needle)
}

/// The code units of canonical WTF-8.
pub(super) fn decode(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let (cp, n) = wtf8::decode_at(bytes, i);
        if cp >= 0x10000 {
            out.push(high_of(cp));
            out.push(low_of(cp));
        } else {
            out.push(cp as u16);
        }
        i += n;
    }
    out
}

/// Canonical WTF-8 of code units: a high surrogate followed by a low one is the pair's 4-byte
/// sequence, any other surrogate its 3-byte form.
pub(super) fn encode(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len() * 3);
    let mut buf = [0u8; 4];
    let mut i = 0;
    while i < units.len() {
        let u = units[i] as u32;
        let cp = match units.get(i + 1) {
            Some(&lo) if is_high(u) && (0xDC00..0xE000).contains(&lo) => {
                i += 1;
                0x10000 + ((u - 0xD800) << 10) + (lo as u32 - 0xDC00)
            }
            _ => u,
        };
        out.extend_from_slice(wtf8::encode(cp, &mut buf));
        i += 1;
    }
    out
}

fn is_high(u: u32) -> bool {
    (0xD800..0xDC00).contains(&u)
}

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

/// `bytes[..pos]` as text: the bytes before the code point at `pos`, plus the pair's high half
/// re-encoded as a lone surrogate when `pos` is its low half.
pub(super) fn prefix_to(bytes: &[u8], pos: BytePos) -> Vec<u8> {
    let mut v = bytes[..pos.byte].to_vec();
    if pos.low_half {
        v.extend_from_slice(&lone(unit_at(
            bytes,
            BytePos {
                low_half: false,
                ..pos
            },
        )));
    }
    v
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

/// The first `needle.len()`-unit window of `hay` at or after `from` equal to `needle`
/// (non-empty), by a byte search over the units' little-endian bytes kept to even offsets (an odd
/// offset straddles two units, so the search resumes one byte later: matches may overlap).
pub(super) fn find(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    let from = from.min(hay.len());
    let (h, n) = (as_le_bytes(&hay[from..]), as_le_bytes(needle));
    let finder = memchr::memmem::Finder::new(&n);
    let mut at = 0;
    while let Some(i) = finder.find(&h[at..]) {
        let i = at + i;
        if i % 2 == 0 {
            return Some(from + i / 2);
        }
        at = i + 1;
    }
    None
}

/// The last window of `hay` starting at or before `last` equal to `needle` (non-empty).
pub(super) fn rfind(hay: &[u16], needle: &[u16], last: usize) -> Option<usize> {
    let end = last.saturating_add(needle.len()).min(hay.len());
    let (h, n) = (as_le_bytes(&hay[..end]), as_le_bytes(needle));
    let finder = memchr::memmem::FinderRev::new(&n);
    let mut end = h.len();
    while let Some(i) = finder.rfind(&h[..end]) {
        if i % 2 == 0 {
            return Some(i / 2);
        }
        end = i + n.len() - 1;
    }
    None
}

/// Every non-overlapping match of `needle` (non-empty) in `hay`, left to right (at most `limit`).
pub(super) fn find_all(hay: &[u16], needle: &[u16], limit: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while out.len() < limit {
        let Some(k) = find(hay, needle, from) else {
            break;
        };
        out.push(k);
        from = k + needle.len();
    }
    out
}

fn as_le_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

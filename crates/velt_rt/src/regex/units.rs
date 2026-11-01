//! The "units" form of a subject, for a pattern without the `u` flag that names surrogates
//! (surrogates.rs): JS matches such a pattern against UTF-16 code units, so each half of a
//! surrogate pair must be matchable on its own. The units form writes every 4-byte code point
//! (a pair) as its two surrogates' 3-byte sequences, so every code unit is a whole sequence of
//! 1 to 3 bytes: `count_units` of a prefix is still its length in code units, and a match can
//! start or end between the halves of a pair. Text taken out of it goes back to canonical WTF-8
//! with [`to_wtf8`].
//!
//! A regex keeps the [`Form`] of the last subject it searched (mod.rs), so a loop of `exec` or
//! `test` calls over one string converts it once: each call then finds its start in the form
//! from the string's own unit index and the list of the pairs before it.

use crate::str::{wtf8, VeltStr};

/// The units form of a subject, with what maps positions of the subject into it.
pub struct Form {
    /// A share of the subject (static, or heap: its bytes stay where they are while it is held),
    /// or the empty string for a form nobody keeps.
    held: VeltStr,
    /// Address and length of the subject's bytes.
    key: (usize, usize),
    /// The units form; empty when the subject holds no pair (its bytes are their own form).
    text: Vec<u8>,
    /// Byte offsets of the subject's pairs, in order.
    pairs: Vec<usize>,
}

// SAFETY: `held` is a counted share (atomic counts) or a static string, only released by drop.
unsafe impl Send for Form {}
// SAFETY: as above; nothing in a form changes after it is made.
unsafe impl Sync for Form {}

impl Form {
    /// The form of `s`; `keep` holds a share of `s` so that [`Self::is_of`] can recognize it
    /// (only for a static or heap string, whose bytes have a fixed address).
    ///
    /// # Safety
    /// `s` must be valid.
    pub unsafe fn new(s: &VeltStr, keep: bool) -> Form {
        let bytes = s.as_bytes();
        let mut pairs = Vec::new();
        let mut text = Vec::new();
        if !s.is_ascii() {
            let mut i = 0;
            while let Some(at) = bytes[i..].iter().position(|&b| b >= 0xF0) {
                if text.is_empty() {
                    text.reserve(bytes.len() + bytes.len() / 2);
                }
                text.extend_from_slice(&bytes[i..i + at]);
                i += at;
                pairs.push(i);
                let (cp, len) = wtf8::decode_at(bytes, i);
                i += len;
                let v = cp - 0x10000;
                let (hi, lo) = (0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF));
                let cont = |u: u32, shift: u32| 0x80 | ((u >> shift) & 0x3F) as u8;
                text.extend_from_slice(&[
                    0xED,
                    cont(hi, 6),
                    cont(hi, 0),
                    0xED,
                    cont(lo, 6),
                    cont(lo, 0),
                ]);
            }
            if !pairs.is_empty() {
                text.extend_from_slice(&bytes[i..]);
            }
        }
        Form {
            held: if keep {
                s.share()
            } else {
                VeltStr::from_static(b"")
            },
            key: (bytes.as_ptr() as usize, bytes.len()),
            text,
            pairs,
        }
    }

    /// Is this the (kept) form of `s`?
    ///
    /// # Safety
    /// `s` must be valid.
    pub unsafe fn is_of(&self, s: &VeltStr) -> bool {
        let b = s.as_bytes();
        !self.held.is_empty() && self.key == (b.as_ptr() as usize, b.len())
    }

    /// Does the subject hold a pair? Without one, its bytes are searched as they are.
    pub fn has_pairs(&self) -> bool {
        !self.pairs.is_empty()
    }

    /// The units form (of a subject with a pair).
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    /// Where byte `b` of the subject (a code point boundary) is in the units form: each pair
    /// before it takes 2 bytes more.
    pub fn byte_of(&self, b: usize) -> usize {
        b + 2 * self.pairs.partition_point(|&p| p < b)
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        // SAFETY: `held` is this form's own share (or the empty string).
        unsafe { self.held.release() }
    }
}

/// The units form of canonical WTF-8 `bytes` (tests).
#[cfg(test)]
pub fn from_wtf8(bytes: &[u8]) -> Vec<u8> {
    let mut s = VeltStr::from_bytes(bytes);
    // SAFETY: a valid string, released after its form is taken.
    unsafe {
        let f = Form::new(&s, false);
        s.release();
        match f.has_pairs() {
            true => f.text.clone(),
            false => bytes.to_vec(),
        }
    }
}

/// Text in units form (or canonical WTF-8, or a mix) as canonical WTF-8: each high surrogate
/// followed by a low one becomes the pair's 4-byte code point.
pub fn to_wtf8(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(at) = memchr::memchr(0xED, &bytes[i..]) {
        out.extend_from_slice(&bytes[i..i + at]);
        i += at;
        if matches!(bytes[i..], [0xED, 0xA0..=0xAF, _, 0xED, 0xB0..=0xBF, _, ..]) {
            out.extend_from_slice(&wtf8::join_pair(&bytes[i..i + 3], &bytes[i + 3..i + 6]));
            i += 6;
        } else {
            out.push(0xED);
            i += 1;
        }
    }
    out.extend_from_slice(&bytes[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_round_trip() {
        let s = "a😀é\u{10000}z".as_bytes();
        let u = from_wtf8(s);
        assert_eq!(u.len(), s.len() + 4);
        assert_eq!(wtf8::count_units(&u), wtf8::count_units(s));
        assert_eq!(to_wtf8(&u), s);
        let v = VeltStr::from_static("a😀é\u{10000}z".as_bytes());
        // SAFETY: a valid string.
        let f = unsafe { Form::new(&v, false) };
        assert_eq!(f.pairs, [1, 7]);
        let at = (f.byte_of(1), f.byte_of(5), f.byte_of(7), f.byte_of(11));
        assert_eq!(at, (1, 7, 9, 15));
        let plain = VeltStr::from_static("abc é".as_bytes());
        // SAFETY: as above.
        assert!(!unsafe { Form::new(&plain, false) }.has_pairs());
    }

    #[test]
    fn halves_stay_lone() {
        let u = from_wtf8("😀".as_bytes());
        assert_eq!(to_wtf8(&u[..3]), [0xED, 0xA0, 0xBD]);
        assert_eq!(to_wtf8(&u[3..]), [0xED, 0xB8, 0x80]);
    }
}

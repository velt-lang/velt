//! WTF-8 facts the string layout needs (#377, docs/internals/design/strings.md): the UTF-16
//! length of a byte string, its lone surrogates, the seam where a high and a low half join, and
//! (debug builds) the canonical-form check.
//!
//! A string's bytes are canonical WTF-8: UTF-8 that may also hold a lone surrogate as a 3-byte
//! sequence (`ED A0..BF xx`), where a surrogate *pair* is always stored as its 4-byte code point.

/// The UTF-16 length and the lone surrogates of a piece of text, when a caller already knows
/// them (another string's), so appending it needs no scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// UTF-16 code units.
    pub units: usize,
    /// Lone surrogates (3-byte sequences `ED A0..BF xx`).
    pub lone: usize,
}

impl Summary {
    /// The summary of `n` ASCII bytes.
    #[inline]
    pub const fn ascii(n: usize) -> Summary {
        Summary { units: n, lone: 0 }
    }
}

/// UTF-16 code units of `bytes`: one per byte that starts a code point, plus one more per
/// 4-byte sequence (a surrogate pair). Counting lead bytes makes the sum split-safe: the pieces
/// of a code point pushed separately still add up to its length.
#[inline]
pub fn count_units(bytes: &[u8]) -> usize {
    if bytes.is_ascii() {
        return bytes.len();
    }
    count_units_scan(bytes)
}

/// [`count_units`] without the ASCII shortcut: a plain loop over the bytes, which the compiler
/// vectorizes.
fn count_units_scan(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .map(|&b| (b & 0xC0 != 0x80) as usize + (b >= 0xF0) as usize)
        .sum()
}

/// Lone surrogates in `bytes`: a surrogate code point is the only sequence that starts with
/// `ED` followed by `A0..BF`. Text without an `ED` byte (most of it) is ruled out by a fast
/// search.
pub fn count_lone(bytes: &[u8]) -> usize {
    if !bytes.contains(&0xED) {
        return 0;
    }
    bytes
        .windows(2)
        .filter(|w| w[0] == 0xED && w[1] >= 0xA0)
        .count()
}

/// The summary of `bytes`, counted: inline for the ASCII check, out of line for the rest.
#[inline(always)]
pub fn summarize(bytes: &[u8]) -> Summary {
    if bytes.is_ascii() {
        Summary::ascii(bytes.len())
    } else {
        summarize_non_ascii(bytes)
    }
}

#[inline(never)]
fn summarize_non_ascii(bytes: &[u8]) -> Summary {
    Summary {
        units: count_units_scan(bytes),
        lone: count_lone(bytes),
    }
}

/// Does `bytes` end with a high surrogate (`ED A0..AF xx`)?
#[inline]
pub fn ends_with_high(bytes: &[u8]) -> bool {
    matches!(bytes, [.., 0xED, 0xA0..=0xAF, _])
}

/// Does `bytes` start with a low surrogate (`ED B0..BF xx`)?
#[inline]
pub fn starts_with_low(bytes: &[u8]) -> bool {
    matches!(bytes, [0xED, 0xB0..=0xBF, ..])
}

/// The 4-byte code point of a high surrogate `hi` and a low surrogate `lo` (3 bytes each).
pub fn join_pair(hi: &[u8], lo: &[u8]) -> [u8; 4] {
    let surrogate = |b: &[u8]| 0xD000 | ((b[1] as u32 & 0x3F) << 6) | (b[2] as u32 & 0x3F);
    let cp = 0x10000 + ((surrogate(hi) - 0xD800) << 10) + (surrogate(lo) - 0xDC00);
    [
        0xF0 | (cp >> 18) as u8,
        0x80 | ((cp >> 12) & 0x3F) as u8,
        0x80 | ((cp >> 6) & 0x3F) as u8,
        0x80 | (cp & 0x3F) as u8,
    ]
}

/// Why `bytes` is not canonical WTF-8, if it isn't: an invalid sequence, or a high surrogate
/// directly followed by a low one (a pair must be stored as its 4-byte code point). Only the
/// debug checks use it.
#[cfg(any(debug_assertions, test))]
pub fn check_canonical(bytes: &[u8]) -> Result<(), String> {
    let mut i = 0;
    let mut after_high = false;
    while i < bytes.len() {
        let b = bytes[i];
        let (n, mask, min) = match b {
            0x00..=0x7F => (1, 0x7F, 0),
            0xC2..=0xDF => (2, 0x1F, 0x80),
            0xE0..=0xEF => (3, 0x0F, 0x800),
            0xF0..=0xF4 => (4, 0x07, 0x10000),
            _ => return Err(format!("invalid byte {b:#04x} at {i}")),
        };
        let seq = bytes
            .get(i..i + n)
            .filter(|s| s[1..].iter().all(|&c| c & 0xC0 == 0x80))
            .ok_or_else(|| format!("truncated sequence at {i}"))?;
        let cp = seq[1..]
            .iter()
            .fold(b as u32 & mask, |cp, &c| (cp << 6) | (c as u32 & 0x3F));
        if cp < min || cp > 0x10FFFF {
            return Err(format!("overlong or out-of-range sequence at {i}"));
        }
        if after_high && (0xDC00..0xE000).contains(&cp) {
            return Err(format!("surrogate pair stored as two halves at {}", i - 3));
        }
        after_high = (0xD800..0xDC00).contains(&cp);
        i += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WTF-8 of one code point or surrogate.
    fn enc(cp: u32) -> Vec<u8> {
        match cp {
            0..=0x7F => vec![cp as u8],
            0x80..=0x7FF => vec![0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8],
            0x800..=0xFFFF => vec![
                0xE0 | (cp >> 12) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ],
            _ => char::from_u32(cp).unwrap().to_string().into_bytes(),
        }
    }

    #[test]
    fn units_per_alphabet() {
        assert_eq!(count_units(b"hello"), 5);
        assert_eq!(count_units("héllo".as_bytes()), 5);
        assert_eq!(count_units("日本".as_bytes()), 2);
        assert_eq!(count_units("😀".as_bytes()), 2);
        assert_eq!(count_units("a😀é€".as_bytes()), 5);
        assert_eq!(count_units(&enc(0xD83D)), 1);
        assert_eq!(count_units(&[]), 0);
    }

    #[test]
    fn units_are_split_safe() {
        let text = "a😀é€日\u{10FFFF}z".as_bytes();
        for cut in 0..=text.len() {
            let (a, b) = text.split_at(cut);
            assert_eq!(
                count_units(a) + count_units(b),
                count_units(text),
                "cut {cut}"
            );
        }
    }

    #[test]
    fn lone_surrogates() {
        assert_eq!(count_lone("😀 é \u{FFFD} \u{D7FF}".as_bytes()), 0);
        let mut s = enc(0xD83D);
        s.extend(b"x");
        s.extend(enc(0xDFFF));
        assert_eq!(count_lone(&s), 2);
        assert_eq!(summarize(&s), Summary { units: 3, lone: 2 });
        assert_eq!(count_lone(&[0xED]), 0);
    }

    #[test]
    fn seams() {
        let (hi, lo) = (enc(0xD83D), enc(0xDE00));
        assert!(ends_with_high(&hi) && !ends_with_high(&lo));
        assert!(starts_with_low(&lo) && !starts_with_low(&hi));
        assert_eq!(join_pair(&hi, &lo), *b"\xF0\x9F\x98\x80");
        assert_eq!(
            &join_pair(&enc(0xDBFF), &enc(0xDFFF)),
            "\u{10FFFF}".as_bytes()
        );
        assert_eq!(
            &join_pair(&enc(0xD800), &enc(0xDC00)),
            "\u{10000}".as_bytes()
        );
    }

    #[test]
    fn canonical_form() {
        assert!(check_canonical("a😀é€\u{10FFFF}".as_bytes()).is_ok());
        let lone = [
            enc(0xD83D),
            enc(0x41),
            enc(0xDE00),
            enc(0xDE00),
            enc(0xD800),
        ]
        .concat();
        assert!(check_canonical(&lone).is_ok());
        let halves = [enc(0xD83D), enc(0xDE00)].concat();
        assert!(check_canonical(&halves).is_err());
        for bad in [
            &b"\xff"[..],
            b"\xc3",
            b"\xc0\x80",
            b"\xe0\x80\x80",
            b"\xf4\x90\x80\x80",
        ] {
            assert!(check_canonical(bad).is_err(), "{bad:?}");
        }
    }
}

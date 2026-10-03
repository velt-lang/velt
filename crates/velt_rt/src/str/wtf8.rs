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
    /// Lone surrogates (3-byte sequences `ED A0..BF xx`), or [`LONE_UNKNOWN`].
    pub lone: usize,
}

/// A lone-surrogate count nobody took: text from a static string, which has no room to record
/// it. Counts add with saturation, so a string that absorbs such text keeps it unknown until
/// somebody needs the number and counts. Only the unit count must be exact; a seam join tests
/// the bytes, so it does not depend on this count.
pub const LONE_UNKNOWN: usize = usize::MAX;

/// `lone` less `n` lone surrogates (joined into a pair), unless it is unknown.
#[inline]
pub fn lone_less(lone: usize, n: usize) -> usize {
    if lone == LONE_UNKNOWN {
        lone
    } else {
        lone - n
    }
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
    if bytes.len() < SHORT {
        return count_units_short(bytes);
    }
    if bytes.is_ascii() {
        return bytes.len();
    }
    count_units_scan(bytes)
}

/// Below this length [`count_units`] and [`count_lone`] work a word at a time: the vectorized loops cost more to
/// set up than a short piece (a split field, a JSON key) takes to count, and an ASCII check first
/// would cost as much as the count.
const SHORT: usize = 32;

/// The top bit of every byte of a word.
const HIGH: u64 = 0x8080_8080_8080_8080;

/// The UTF-16 code units the eight bytes of `w` start (see [`unit_weight`]).
#[inline]
fn word_units(w: u64) -> usize {
    // Bytes whose top bit is set in `marks`: each becomes 0 or 1, and the multiplication sums
    // them into the top byte.
    let count = |marks: u64| ((marks >> 7).wrapping_mul(0x0101_0101_0101_0101) >> 56) as usize;
    let continuation = w & !(w << 1) & HIGH;
    let four_byte = w & (w << 1) & (w << 2) & (w << 3) & HIGH;
    8 - count(continuation) + count(four_byte)
}

/// [`count_units`] of fewer than [`SHORT`] bytes, a word at a time. The partial last word is
/// padded with zero bytes, which count as ASCII and are taken off again.
#[inline]
fn count_units_short(b: &[u8]) -> usize {
    let mut units = 0;
    let rest = for_words(b, |w| units += word_units(w));
    units - (8 - rest) % 8
}

/// Does a string of fewer than [`SHORT`] bytes contain an `ED` byte (the lead byte of every
/// surrogate)? A word at a time, without a call.
#[inline]
fn has_ed_short(b: &[u8]) -> bool {
    let mut found = false;
    for_words(b, |w| {
        // A zero byte of `w ^ EDED…` is an `ED` byte of `w` (padding bytes are zero, not ED).
        let x = w ^ 0xEDED_EDED_EDED_EDED;
        found |= x.wrapping_sub(0x0101_0101_0101_0101) & !x & HIGH != 0;
    });
    found
}

/// Call `f` on the little-endian words of `b` (fewer than [`SHORT`] bytes), the last one padded
/// with zero bytes; returns how many bytes that last word holds (0: none was partial). The partial
/// word is read without a copy: one overlapping read of the last 8 bytes, or for fewer than 8
/// bytes two overlapping reads whose shared bytes are equal (so OR-ing them is harmless).
#[inline(always)]
fn for_words(b: &[u8], mut f: impl FnMut(u64)) -> usize {
    let n = b.len();
    // SAFETY (all reads): every offset read plus its width is at most `n`.
    let read = |at: usize, width: usize| -> u64 {
        unsafe {
            match width {
                8 => b.as_ptr().add(at).cast::<u64>().read_unaligned(),
                4 => b.as_ptr().add(at).cast::<u32>().read_unaligned() as u64,
                2 => b.as_ptr().add(at).cast::<u16>().read_unaligned() as u64,
                _ => *b.as_ptr().add(at) as u64,
            }
        }
    };
    let mut i = 0;
    while i + 8 <= n {
        f(u64::from_le(read(i, 8)));
        i += 8;
    }
    let rest = n - i;
    if rest > 0 {
        let w = if n >= 8 {
            u64::from_le(read(n - 8, 8)) >> (8 * (8 - rest))
        } else {
            let (lo, hi, width) = match rest {
                4..=7 => (read(0, 4), read(rest - 4, 4), 4),
                2..=3 => (read(0, 2), read(rest - 2, 2), 2),
                _ => (read(0, 1), 0, 1),
            };
            u64::from_le(lo | (hi << (8 * (rest - width))))
        };
        f(w);
    }
    rest
}

/// UTF-16 code units a byte of UTF-8 starts: none for a continuation byte, two for the lead byte
/// of a 4-byte sequence (a surrogate pair), one otherwise.
#[inline]
fn unit_weight(b: u8) -> u8 {
    (b & 0xC0 != 0x80) as u8 + (b >= 0xF0) as u8
}

/// [`count_units`] without the ASCII shortcut. Blocks of 127 bytes are summed as bytes (at most 2
/// units each, so a block's sum fits a `u8`), which the compiler vectorizes 16 bytes at a time; a
/// sum in `usize` would widen every byte first.
fn count_units_scan(bytes: &[u8]) -> usize {
    bytes
        .chunks(127)
        .map(|block| {
            block
                .iter()
                .fold(0u8, |sum, &b| sum.wrapping_add(unit_weight(b))) as usize
        })
        .sum()
}

/// Lone surrogates in `bytes`: a surrogate code point is the only sequence that starts with
/// `ED` followed by `A0..BF`. Text without an `ED` byte (most of it) is ruled out by a fast
/// search, inline for a short piece (an append of one).
#[inline]
pub fn count_lone(bytes: &[u8]) -> usize {
    let has_ed = if bytes.len() < SHORT {
        has_ed_short(bytes)
    } else {
        bytes.contains(&0xED)
    };
    if has_ed {
        count_lone_scan(bytes)
    } else {
        0
    }
}

/// [`count_lone`] of text that has an `ED` byte.
#[inline(never)]
fn count_lone_scan(bytes: &[u8]) -> usize {
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
        units: count_units(bytes),
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
    fn short_counts_match_the_byte_rule() {
        // Lone surrogates, an `ED` lead byte that is not one, and every other sequence length.
        let alphabet: [&[u8]; 7] = [
            b"a",
            "é".as_bytes(),
            "日".as_bytes(),
            "😀".as_bytes(),
            "\u{D7FF}".as_bytes(),
            &[0xED, 0xA0, 0xBD],
            &[0xED, 0xB8, 0x80],
        ];
        for len in 0..40 {
            let text: Vec<u8> = (0..len)
                .flat_map(|i| alphabet[(i * 7 + len) % alphabet.len()].iter().copied())
                .collect();
            for cut in 0..text.len().min(SHORT) {
                let piece = &text[..cut];
                let want: usize = piece.iter().map(|&b| unit_weight(b) as usize).sum();
                assert_eq!(count_units_short(piece), want, "{piece:?}");
                let lone = piece
                    .windows(2)
                    .filter(|w| w[0] == 0xED && w[1] >= 0xA0)
                    .count();
                assert_eq!(count_lone(piece), lone, "{piece:?}");
            }
        }
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

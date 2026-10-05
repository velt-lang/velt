//! The order of strings by UTF-16 code units (JavaScript's `<` and default `sort`, #377 phase
//! 2b), computed from their canonical WTF-8 (docs/internals/design/strings.md "The ordering
//! rule").

use super::wtf8;

/// The order of two strings by UTF-16 code units (JavaScript's `<` and default `sort`), from
/// their canonical WTF-8 (design note "The ordering rule"). Byte order is code point order,
/// which differs from code-unit order between U+E000–U+FFFF and supplementary characters and
/// between lone surrogates and supplementary characters, so: the first differing byte is found
/// (a word at a time); unless it is a lead byte of a supplementary character against one of
/// ED..EF, the bytes decide (a difference in a continuation byte means equal leads). Otherwise
/// the first code unit of each side's code point decides; when those are equal (a
/// supplementary character against one with the same high surrogate, or against that lone high
/// surrogate) the second units do, where the end of a string is below every unit. In canonical
/// form a lone high surrogate is never followed by a low one, so that one extra comparison
/// decides. `velt_rt_str_cmp` (`<`, `sort()`) is this.
#[inline]
pub fn cmp_utf16(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let n = a.len().min(b.len());
    let Some(i) = mismatch(&a[..n], &b[..n]) else {
        return a.len().cmp(&b.len());
    };
    // SAFETY: `i < n`, within both.
    let (x, y) = unsafe { (*a.get_unchecked(i), *b.get_unchecked(i)) };
    // Byte order is code-unit order unless the first difference is between the lead byte of a
    // supplementary character (F0..F4) and that of a character at U+E000 or above or a surrogate
    // (ED..EF): a difference in a continuation byte means equal leads, so equal lengths.
    if x.max(y) < 0xF0 || x.min(y) < 0xED {
        return x.cmp(&y);
    }
    cmp_at_leads(a, b, i)
}

/// [`cmp_utf16`] when the strings first differ in lead bytes at `i` that may order differently
/// by code units (out of line: rare).
#[cold]
#[inline(never)]
fn cmp_at_leads(a: &[u8], b: &[u8], i: usize) -> std::cmp::Ordering {
    let (ca, la) = wtf8::decode_at(a, i);
    let (cb, lb) = wtf8::decode_at(b, i);
    first_unit(ca)
        .cmp(&first_unit(cb))
        .then_with(|| second_unit(a, i, ca, la).cmp(&second_unit(b, i, cb, lb)))
}

/// The first byte where `a` and `b` (of equal length) differ, a word at a time (the last word
/// overlapping the one before when the length is not a multiple of 8).
#[inline]
fn mismatch(a: &[u8], b: &[u8]) -> Option<usize> {
    let n = a.len();
    // SAFETY (both reads): every word read lies within `0..n`, the length of both slices.
    let word = |at: usize| unsafe {
        (
            a.as_ptr().add(at).cast::<u64>().read_unaligned(),
            b.as_ptr().add(at).cast::<u64>().read_unaligned(),
        )
    };
    let differ = |at: usize, (x, y): (u64, u64)| {
        (x != y).then(|| at + (u64::from_le(x ^ y).trailing_zeros() / 8) as usize)
    };
    if n < 8 {
        return (0..n).find(|&j| a[j] != b[j]);
    }
    let mut i = 0;
    while i + 8 <= n {
        if let Some(k) = differ(i, word(i)) {
            return Some(k);
        }
        i += 8;
    }
    if i < n {
        return differ(n - 8, word(n - 8));
    }
    None
}

/// The first UTF-16 unit of a code point (or lone surrogate).
#[inline]
fn first_unit(cp: u32) -> u32 {
    if cp < 0x10000 {
        cp
    } else {
        0xD800 + ((cp - 0x10000) >> 10)
    }
}

/// The unit after the first one of the code point `cp` (`len` bytes at `k` of `s`): its low
/// surrogate, else the first unit of the next code point, else 0 for the end of the string
/// (below every unit, so the shorter string is less).
#[inline]
fn second_unit(s: &[u8], k: usize, cp: u32, len: usize) -> u32 {
    if cp >= 0x10000 {
        0xDC00 + ((cp - 0x10000) & 0x3FF) + 1
    } else if k + len < s.len() {
        first_unit(wtf8::decode_at(s, k + len).0) + 1
    } else {
        0
    }
}

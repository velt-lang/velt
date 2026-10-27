//! JSON string output: quoting and escaping exactly like `JSON.stringify` (well-formed variant).
//!
//! Escaped: `"` `\\`, the short forms `\b \f \n \r \t`, and other control characters below
//! U+0020 as lowercase `\u00xx`, and lone surrogates (#377) as lowercase `\udxxx` (ES2019
//! well-formed `JSON.stringify`), so the output is always well-formed. Everything else
//! (including DEL, U+2028/U+2029 and other non-ASCII) is copied verbatim.
//!
//! The runs between escapes are found a block at a time: 16 bytes with SSE2 or NEON, 8 bytes
//! in a 64-bit word on other targets (wasm32 among them). The rest of a string after its last
//! whole block is read as the string's last block in the first run and after U+D000–U+D7FF
//! (Hangul among it, whose `ED` lead byte is that of a surrogate too). After an escape, and in
//! strings shorter than a block, it goes a byte at a time, which is cheaper for the short runs
//! between escapes.

use crate::Out;

/// For each byte: 0 = copy as is, otherwise the character after the backslash (`u` = `\u00xx`),
/// or [`MAYBE_LONE`] for the lead byte of a lone surrogate (also of U+D000–U+D7FF).
static ESCAPE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = b'u';
        i += 1;
    }
    t[0x08] = b'b';
    t[0x09] = b't';
    t[0x0a] = b'n';
    t[0x0c] = b'f';
    t[0x0d] = b'r';
    t[b'"' as usize] = b'"';
    t[b'\\' as usize] = b'\\';
    t[0xED] = MAYBE_LONE;
    t
};

/// [`ESCAPE`] mark of the `ED` byte.
const MAYBE_LONE: u8 = 1;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// A way to test a block of bytes at once for the bytes that end a run needing no escape.
trait Scan {
    /// Bytes per block.
    const WIDTH: usize;
    /// Bits of a mask per byte.
    const STRIDE: u32;
    /// Whether [`Scan::mask`] marks bytes where they are (bit `k * STRIDE` for byte `k`), so
    /// [`blocks`] can tell the first one.
    const EXACT: bool;
    /// A first, cheap test of `s[at..at + WIDTH]`: zero if no byte in it needs an escape or is
    /// `ED`, otherwise a mask for [`Scan::refine`].
    ///
    /// # Safety
    /// `at + WIDTH <= s.len()`.
    unsafe fn mask(s: &[u8], at: usize) -> u64;
    /// The stops in the block of `mask` (not zero): bit `k * STRIDE` is set for the bytes `k`
    /// below 0x20, `"` and `\\`, and for `ED` (the lead byte of U+D000–U+DFFF) at least where it
    /// starts a lone surrogate. Exact up to the first stop, which is all [`run_end`] reads.
    ///
    /// # Safety
    /// As [`Scan::mask`].
    unsafe fn refine(s: &[u8], at: usize, mask: u64) -> u64;
    /// The stops in `s[i..]`, shorter than a block, in a string of at least one block: bit
    /// `k * STRIDE` for byte `i + k`, as [`Scan::refine`]. Read from the last block of `s`.
    #[inline(always)]
    fn last(s: &[u8], i: usize) -> u64 {
        let at = s.len() - Self::WIDTH;
        debug_assert!(at < i);
        // SAFETY: `at + WIDTH == s.len()`.
        let m = unsafe { Self::mask(s, at) };
        if m == 0 {
            return 0;
        }
        // SAFETY: as above. The masks of these blocks are exact at every byte, so the bytes
        // before `i` (known to need no escape) can be shifted out.
        unsafe { Self::refine(s, at, m) >> ((i - at) as u32 * Self::STRIDE) }
    }
}

/// Where, from `i`, the whole blocks of `s` that need no escaping end: at the first byte that
/// [`Scan::mask`] marks (every `ED` among them), or at the block that holds it, or after the last
/// whole block. The byte loop after it finds the exact byte.
#[inline(always)]
fn blocks<S: Scan>(s: &[u8], mut i: usize) -> usize {
    while i + S::WIDTH <= s.len() {
        // SAFETY: the block is in bounds (the loop condition).
        let m = unsafe { S::mask(s, i) };
        if m != 0 {
            if S::EXACT {
                return i + (m.trailing_zeros() / S::STRIDE) as usize;
            }
            break;
        }
        i += S::WIDTH;
    }
    i
}

/// Where, from `i`, the run of bytes of `s` that need no escaping ends: at the first stop of
/// [`Scan::refine`] (a byte [`ESCAPE`] marks), or at the end of `s`. A rest shorter than a block
/// is read as the last block of `s`. `s` is at least one block long.
#[inline(always)]
fn run_end<S: Scan>(s: &[u8], mut i: usize) -> usize {
    let len = s.len();
    debug_assert!(len >= S::WIDTH);
    while i + S::WIDTH <= len {
        // SAFETY: the block is in bounds (the loop condition).
        let m = unsafe { S::mask(s, i) };
        if m != 0 {
            // SAFETY: as above.
            let m = unsafe { S::refine(s, i, m) };
            if m != 0 {
                return i + (m.trailing_zeros() / S::STRIDE) as usize;
            }
        }
        i += S::WIDTH;
    }
    if i < len {
        let m = S::last(s, i);
        if m != 0 {
            return i + (m.trailing_zeros() / S::STRIDE) as usize;
        }
    }
    len
}

/// 16 bytes at a time with the target's vector instructions.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
struct Simd;

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use core::arch::x86_64::*;

    /// The block at `at` and its bytes that need an escape (below 0x20, `"`, `\\`) and `ED`s.
    ///
    /// # Safety
    /// `at + 16 <= s.len()`.
    #[inline(always)]
    pub(super) unsafe fn classify(s: &[u8], at: usize) -> (__m128i, __m128i, __m128i) {
        debug_assert!(at + 16 <= s.len());
        // SAFETY: SSE2 is part of every x86-64 target; the load reads 16 bytes in bounds (the
        // caller's contract).
        unsafe {
            let v = _mm_loadu_si128(s.as_ptr().add(at) as *const __m128i);
            // Below 0x20: min(v, 0x1F) == v.
            let special = _mm_or_si128(
                _mm_cmpeq_epi8(_mm_min_epu8(v, _mm_set1_epi8(0x1F)), v),
                _mm_or_si128(
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(b'"' as i8)),
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(b'\\' as i8)),
                ),
            );
            let lead = _mm_cmpeq_epi8(v, _mm_set1_epi8(0xEDu8 as i8));
            (v, special, lead)
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl Scan for Simd {
    const WIDTH: usize = 16;
    const STRIDE: u32 = 1;
    const EXACT: bool = true;
    #[inline(always)]
    unsafe fn mask(s: &[u8], at: usize) -> u64 {
        use core::arch::x86_64::*;
        // SAFETY: the caller's contract; SSE2 is part of every x86-64 target.
        unsafe {
            let (_, special, lead) = sse2::classify(s, at);
            u64::from(_mm_movemask_epi8(_mm_or_si128(special, lead)) as u32)
        }
    }
    #[inline(always)]
    unsafe fn refine(s: &[u8], at: usize, mask: u64) -> u64 {
        use core::arch::x86_64::*;
        // SAFETY: as `mask` (the same block, read again: the compiler reuses the first read).
        unsafe {
            let (v, _, lead) = sse2::classify(s, at);
            let leads = u64::from(_mm_movemask_epi8(lead) as u32);
            if leads == 0 {
                return mask;
            }
            // An `ED` is a stop only before a byte from 0xA0 up (a surrogate, U+D800–U+DFFF);
            // the one in the last lane is kept, as its next byte is not in the block.
            let next = _mm_srli_si128(v, 1);
            let high = _mm_cmpeq_epi8(_mm_max_epu8(next, _mm_set1_epi8(0xA0u8 as i8)), next);
            let lone = u64::from(_mm_movemask_epi8(_mm_and_si128(lead, high)) as u32);
            (mask & !leads) | lone | (leads & 0x8000)
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use core::arch::aarch64::*;

    /// The block at `at` and its bytes that need an escape (below 0x20, `"`, `\\`) and `ED`s.
    ///
    /// # Safety
    /// `at + 16 <= s.len()`.
    #[inline(always)]
    pub(super) unsafe fn classify(s: &[u8], at: usize) -> (uint8x16_t, uint8x16_t, uint8x16_t) {
        debug_assert!(at + 16 <= s.len());
        // SAFETY: NEON is part of every aarch64 target; the load reads 16 bytes in bounds (the
        // caller's contract).
        unsafe {
            let v = vld1q_u8(s.as_ptr().add(at));
            let special = vorrq_u8(
                vcltq_u8(v, vdupq_n_u8(0x20)),
                vorrq_u8(
                    vceqq_u8(v, vdupq_n_u8(b'"')),
                    vceqq_u8(v, vdupq_n_u8(b'\\')),
                ),
            );
            let lead = vceqq_u8(v, vdupq_n_u8(0xED));
            (v, special, lead)
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Scan for Simd {
    const WIDTH: usize = 16;
    const STRIDE: u32 = 4;
    // `mask` is only whether the block has such a byte.
    const EXACT: bool = false;
    #[inline(always)]
    unsafe fn mask(s: &[u8], at: usize) -> u64 {
        use core::arch::aarch64::*;
        // SAFETY: the caller's contract; NEON is part of every aarch64 target.
        unsafe {
            let (_, special, lead) = neon::classify(s, at);
            u64::from(vmaxvq_u8(vorrq_u8(special, lead)))
        }
    }
    #[inline(always)]
    unsafe fn refine(s: &[u8], at: usize, _mask: u64) -> u64 {
        use core::arch::aarch64::*;
        // SAFETY: as `mask` (the same block, read again: the compiler reuses the first read).
        unsafe {
            let (v, special, lead) = neon::classify(s, at);
            // As on x86-64: an `ED` stops only before a byte from 0xA0 up; the last lane's
            // next byte is taken as 0xFF, so an `ED` there is kept.
            let next = vextq_u8::<1>(v, vdupq_n_u8(0xFF));
            let lone = vandq_u8(lead, vcgeq_u8(next, vdupq_n_u8(0xA0)));
            let m = vorrq_u8(special, lone);
            // Four bits per byte.
            let nibbles = vshrn_n_u16::<4>(vreinterpretq_u16_u8(m));
            vget_lane_u64::<0>(vreinterpret_u64_u8(nibbles))
        }
    }
}

/// Eight bytes at a time in a 64-bit word, on targets without [`Simd`] (and in tests on all).
#[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
struct Words;

#[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
impl Scan for Words {
    const WIDTH: usize = 8;
    const STRIDE: u32 = 8;
    const EXACT: bool = true;
    #[inline(always)]
    unsafe fn mask(s: &[u8], at: usize) -> u64 {
        Words::stops(Words::load(s, at))
    }
    #[inline(always)]
    unsafe fn refine(_: &[u8], _: usize, mask: u64) -> u64 {
        mask
    }
    #[inline(always)]
    fn last(s: &[u8], i: usize) -> u64 {
        // A wrong mark above a stop would come out when the stop is shifted out of the mask:
        // shift the word instead, with plain bytes coming in on top.
        let at = s.len() - 8;
        let skip = (i - at) as u32 * 8; // 8..=56
        Words::stops(Words::load(s, at) >> skip | (Words::ONES * u64::from(b'a')) << (64 - skip))
    }
}

#[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
impl Words {
    const ONES: u64 = 0x0101_0101_0101_0101;

    /// `s[at..at + 8]` as a word, byte `k` of it `s[at + k]` on every target (little-endian
    /// order).
    #[inline(always)]
    fn load(s: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(s[at..at + 8].try_into().unwrap())
    }

    /// Bit 7 of the lowest byte of `w` that is below 0x20, `"`, `\\` or `ED`, and maybe of
    /// some bytes above it: per byte, `(x - 0x01) & !x` sets bit 7 when x is 0, and
    /// `(w - 0x20) & !w` when w is below 0x20. A borrow goes up only from such a byte, so the
    /// lowest byte marked is exact; the marks above it may be wrong, and are not read.
    #[inline(always)]
    fn stops(w: u64) -> u64 {
        const ONES: u64 = Words::ONES;
        let zero = |x: u64| x.wrapping_sub(ONES) & !x;
        let control = w.wrapping_sub(ONES * 0x20) & !w;
        let quote = zero(w ^ (ONES * b'"' as u64));
        let backslash = zero(w ^ (ONES * b'\\' as u64));
        let lead = zero(w ^ (ONES * 0xED));
        (control | quote | backslash | lead) & (ONES * 0x80)
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
type Native = Simd;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
type Native = Words;

/// Append `s` (canonical WTF-8) as a quoted, escaped JSON string literal.
#[inline]
pub fn push_json_string(out: &mut (impl Out + ?Sized), s: &[u8]) {
    push_json_string_counted(out, s);
}

/// [`push_json_string`], returning the number of lone surrogates written as `\udxxx` (each 3
/// bytes and 1 unit in, 6 ASCII bytes out).
#[inline(never)]
pub fn push_json_string_counted(out: &mut (impl Out + ?Sized), s: &[u8]) -> usize {
    push_with::<Native>(out, s)
}

/// The first byte from `i` on that [`ESCAPE`] marks, or the end of `s`: [`blocks`], then a byte
/// at a time.
#[inline(always)]
fn scan<S: Scan>(s: &[u8], i: usize) -> usize {
    let mut i = blocks::<S>(s, i);
    while i < s.len() && ESCAPE[s[i] as usize] == 0 {
        i += 1;
    }
    i
}

/// [`push_json_string_counted`] reading the runs that need no escape with `S`.
#[inline(always)]
fn push_with<S: Scan>(out: &mut (impl Out + ?Sized), s: &[u8]) -> usize {
    out.reserve(s.len() + 2);
    out.push(b'"');
    let mut run_start = 0;
    let mut lone = 0;
    // The first run, to the end of a string with no escape. Every run ends at a byte that
    // `ESCAPE` marks, or at the end.
    let mut i = if s.len() >= S::WIDTH {
        run_end::<S>(s, 0)
    } else {
        scan::<S>(s, 0)
    };
    while let Some(&byte) = s.get(i) {
        let esc = ESCAPE[byte as usize];
        if esc == MAYBE_LONE {
            if s.get(i + 1).is_some_and(|&b| b >= 0xA0) {
                if run_start < i {
                    out.extend(&s[run_start..i]);
                }
                // ED A0..BF xx is U+D800..U+DFFF: `\ud` and the unit's other three hex digits.
                let unit = ((s[i + 1] as usize & 0x3F) << 6) | (s[i + 2] as usize & 0x3F);
                out.extend(&[
                    b'\\',
                    b'u',
                    b'd',
                    HEX[unit >> 8],
                    HEX[(unit >> 4) & 0xf],
                    HEX[unit & 0xf],
                ]);
                lone += 1;
                i += 3;
                run_start = i;
                i = scan::<S>(s, i);
            } else if s.len() >= S::WIDTH {
                // U+D000–U+D7FF (Hangul among it): on to the next byte that needs an escape.
                i = run_end::<S>(s, i + 1);
            } else {
                i = scan::<S>(s, i + 1);
            }
            continue;
        }
        if run_start < i {
            out.extend(&s[run_start..i]);
        }
        i += 1;
        run_start = i;
        // One write per escape.
        if esc == b'u' {
            out.extend(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(byte >> 4) as usize],
                HEX[(byte & 0xf) as usize],
            ]);
        } else {
            out.extend(&[b'\\', esc]);
        }
        i = scan::<S>(s, i);
    }
    if run_start < s.len() {
        out.extend(&s[run_start..]);
    }
    out.push(b'"');
    lone
}

#[cfg(test)]
mod tests;

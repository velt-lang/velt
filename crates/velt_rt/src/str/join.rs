//! `Array<string>.join(sep)` (`velt_rt_str_join`): every piece is known up front, so the result's
//! byte length, unit count and lone surrogates are summed from the pieces' values first, and the
//! text is written once into a string of the right form (inline, or a heap buffer with a header
//! exactly when the result is not ASCII). A builder fed piece by piece would start an ASCII
//! buffer and move it at the first non-ASCII piece.

use super::{fits_inline, heap, invariants, pack, wtf8, Summary, VeltStr, INLINE_MAX};

impl VeltStr {
    /// `parts.join(sep)`: a new string (one part: that string, shared).
    ///
    /// # Safety
    /// Every string must be valid.
    pub(super) unsafe fn join(parts: &[VeltStr], sep: &VeltStr) -> VeltStr {
        let Some((first, rest)) = parts.split_first() else {
            return VeltStr::empty();
        };
        if rest.is_empty() {
            return first.share();
        }
        let gaps = rest.len();
        let sep_sum = sep.summary();
        let mut len = sep.len() * gaps;
        let mut total = Summary {
            units: sep_sum.units * gaps,
            lone: sep_sum.lone.saturating_mul(gaps),
        };
        // A seam can join two halves of a pair only where a low surrogate starts a piece.
        let mut low_start = wtf8::starts_with_low(sep.as_bytes());
        for p in parts {
            let sum = p.summary();
            invariants::check_piece(p.as_bytes(), Some(sum));
            len += p.len();
            total.units += sum.units;
            total.lone = total.lone.saturating_add(sum.lone);
            low_start |= wtf8::starts_with_low(p.as_bytes());
        }
        if low_start {
            // A seam may join two halves of a pair: append piece by piece.
            return VeltStr::join_pushing(first, rest, sep);
        }
        let write = |dst: *mut u8| {
            let mut at = dst;
            let mut put = |s: &VeltStr| {
                let b = s.as_bytes();
                std::ptr::copy_nonoverlapping(b.as_ptr(), at, b.len());
                at = at.add(b.len());
            };
            put(first);
            for p in rest {
                put(sep);
                put(p);
            }
        };
        let s = if fits_inline(len, total.units) {
            let mut text = [0u8; INLINE_MAX];
            write(text.as_mut_ptr());
            VeltStr::inline(&text[..len], total.units, false)
        } else {
            let header = total.units != len;
            let data = heap::alloc(len, header);
            write(data);
            if header {
                heap::set_lone(data, total.lone);
            }
            VeltStr {
                w0: data as usize as u64,
                w1: pack(total.units, len),
                w2: len as u64,
            }
        };
        invariants::check_whole(&s);
        s
    }

    /// [`Self::join`] through the builder, whose pushes join halves at the seams.
    #[cold]
    unsafe fn join_pushing(first: &VeltStr, rest: &[VeltStr], sep: &VeltStr) -> VeltStr {
        let mut s = VeltStr::empty();
        s.push_str(first);
        for p in rest {
            s.push_str(sep);
            s.push_str(p);
        }
        s
    }
}

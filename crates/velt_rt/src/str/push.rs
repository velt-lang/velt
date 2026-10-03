//! The one way bytes enter a string: [`VeltStr::push_wtf8`], and the layout constructors it
//! shares with `VeltStr::from_bytes` and friends. Pushing keeps the byte count, the UTF-16 unit
//! count, the lone-surrogate count and the form (inline ASCII, inline non-ASCII, heap with or
//! without a header) in step, and joins a high surrogate at the end of the string with a low one
//! at the start of the piece into the pair's 4-byte code point (canonical WTF-8).

use std::mem::MaybeUninit;

use super::{
    fits_inline, heap, invariants, pack, wtf8, Summary, VeltStr, INLINE, INLINE_LEN, INLINE_MAX,
    INLINE_MAX_NON_ASCII, INLINE_UNITS, MAX_LEN, NON_ASCII,
};

impl VeltStr {
    /// Append `bytes`, keeping the string owned by `self`: in place when `self` is inline with
    /// room or the only reference to a heap buffer of its kind, else by moving to a new buffer.
    /// `summary`, when the caller knows it (another string's), saves counting the piece.
    ///
    /// # Safety
    /// `self` must be valid; `bytes` must be canonical WTF-8, and `summary`, if given, must be
    /// its own. `bytes` may lie in `self`'s own heap buffer (a share or an uncounted view of it):
    /// they are copied out before the buffer grows, moves or is rewritten.
    #[inline(always)]
    pub unsafe fn push_wtf8(&mut self, bytes: &[u8], summary: Option<Summary>) {
        invariants::check_piece(bytes, summary);
        let sum = match summary {
            Some(sum) => sum,
            None => wtf8::summarize(bytes),
        };
        #[cfg(debug_assertions)]
        let seam = self.len();
        if sum.lone == 0 {
            self.append(bytes, sum);
        } else {
            self.push_lone(bytes, sum);
        }
        #[cfg(debug_assertions)]
        invariants::check_seam(self, seam);
        invariants::check_whole(self);
    }

    /// [`Self::push_wtf8`] of a piece with lone surrogates, which may join the end of `self`.
    #[cold]
    #[inline(never)]
    unsafe fn push_lone(&mut self, bytes: &[u8], sum: Summary) {
        if self.joins(bytes) {
            self.push_joined(bytes, sum);
        } else {
            self.append(bytes, sum);
        }
    }

    /// Append another string (`s` must not be `self` itself; a share or view of it is fine).
    ///
    /// # Safety
    /// Both strings must be valid.
    #[inline(always)]
    pub unsafe fn push_str(&mut self, s: &VeltStr) {
        // One test of the form gives the text and whether it is ASCII.
        let tag = s.tag();
        let (data, len, ascii) = if tag & INLINE != 0 {
            let len = (tag & INLINE_LEN) as usize;
            (s as *const VeltStr as *const u8, len, tag & NON_ASCII == 0)
        } else {
            let len = s.w1 as u32 as usize;
            (
                s.ptr() as *const u8,
                len,
                (s.w1 >> 32) as u32 as usize == len,
            )
        };
        if !ascii {
            return self.push_non_ascii_str(s);
        }
        let bytes = if len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(data, len)
        };
        self.push_wtf8(bytes, Some(Summary::ascii(len)));
    }

    /// [`Self::push_str`] of a non-ASCII string, whose lone surrogates may need counting.
    #[inline(never)]
    unsafe fn push_non_ascii_str(&mut self, s: &VeltStr) {
        self.push_wtf8(s.as_bytes(), Some(s.summary()));
    }

    /// `a + b` as a new string (`velt_rt_str_concat`), allocated once at its exact size.
    ///
    /// # Safety
    /// Both strings must be valid.
    pub(super) unsafe fn concat(a: &VeltStr, b: &VeltStr) -> VeltStr {
        let (sa, sb) = (a.summary(), b.summary());
        let len = a.len() + b.len();
        let units = sa.units + sb.units;
        if fits_inline(len, units) || (sb.lone > 0 && a.joins(b.as_bytes())) {
            let mut s = VeltStr::owned(a.as_bytes(), Some(sa));
            s.push_wtf8(b.as_bytes(), Some(sb));
            return s;
        }
        invariants::check_piece(a.as_bytes(), Some(sa));
        invariants::check_piece(b.as_bytes(), Some(sb));
        let total = Summary {
            units,
            lone: sa.lone + sb.lone,
        };
        let s = VeltStr::heap_of(&[a.as_bytes(), b.as_bytes()], total, len);
        #[cfg(debug_assertions)]
        invariants::check_seam(&s, a.len());
        invariants::check_whole(&s);
        s
    }

    /// An owned copy of `bytes` (`known`: their summary, if the caller has it).
    pub(super) fn owned(bytes: &[u8], known: Option<Summary>) -> VeltStr {
        invariants::check_piece(bytes, known);
        let sum = known.unwrap_or_else(|| wtf8::summarize(bytes));
        let s = if fits_inline(bytes.len(), sum.units) {
            VeltStr::inline(bytes, sum.units)
        } else {
            // SAFETY: a fresh buffer of exactly the text's size.
            unsafe { VeltStr::heap_of(&[bytes], sum, bytes.len()) }
        };
        invariants::check_whole(&s);
        s
    }

    /// The inline string of `bytes`, which has `units` code units and must fit.
    pub(super) fn inline(bytes: &[u8], units: usize) -> VeltStr {
        VeltStr::inline_of(&[bytes], units)
    }

    /// The inline string of `pieces` concatenated (`units` in all; must fit).
    fn inline_of(pieces: &[&[u8]], units: usize) -> VeltStr {
        let len: usize = pieces.iter().map(|p| p.len()).sum();
        debug_assert!(fits_inline(len, units));
        let mut s = MaybeUninit::<VeltStr>::zeroed();
        let p = s.as_mut_ptr() as *mut u8;
        // SAFETY: 24 writable bytes; the text fits before byte 22 (non-ASCII) or 23 (ASCII).
        unsafe {
            let mut at = 0;
            for piece in pieces {
                std::ptr::copy_nonoverlapping(piece.as_ptr(), p.add(at), piece.len());
                at += piece.len();
            }
            let mut tag = INLINE | len as u8;
            if units != len {
                tag |= NON_ASCII;
                *p.add(INLINE_UNITS) = units as u8;
            }
            *p.add(INLINE_MAX) = tag;
            s.assume_init()
        }
    }

    /// A new heap string of capacity `cap` holding `pieces` concatenated (`sum` in all), in a
    /// buffer with a header when the text is not ASCII.
    ///
    /// # Safety
    /// The pieces must not join, and `cap` must hold them.
    unsafe fn heap_of(pieces: &[&[u8]], sum: Summary, cap: usize) -> VeltStr {
        let len: usize = pieces.iter().map(|p| p.len()).sum();
        debug_assert!(len <= cap);
        let header = sum.units != len;
        let data = heap::alloc(cap, header);
        let mut at = 0;
        for piece in pieces {
            std::ptr::copy_nonoverlapping(piece.as_ptr(), data.add(at), piece.len());
            at += piece.len();
        }
        if header {
            heap::set_lone(data, sum.lone);
        }
        VeltStr {
            w0: data as usize as u64,
            w1: pack(sum.units, len),
            w2: cap as u64,
        }
    }

    /// Append without a seam join, in place when there is room.
    #[inline(always)]
    unsafe fn append(&mut self, bytes: &[u8], sum: Summary) {
        let n = bytes.len();
        let tag = self.tag();
        if tag & INLINE == 0 {
            if self.w2 != 0 {
                let len = self.w1 as u32 as usize;
                let ascii = (self.w1 >> 32) as u32 as usize == len;
                // An ASCII buffer has no header: its first non-ASCII byte moves the text.
                if len + n <= self.w2 as usize
                    && (!ascii || sum.units == n)
                    && heap::is_unique(self.ptr())
                {
                    self.append_unique(len, bytes, sum);
                    return;
                }
            }
        } else {
            let len = (tag & INLINE_LEN) as usize;
            let need = len + n;
            let p = self as *mut VeltStr as *mut u8;
            if tag & NON_ASCII == 0 && sum.units == n {
                if need <= INLINE_MAX {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(len), n);
                    *p.add(INLINE_MAX) = INLINE | need as u8;
                    return;
                }
            } else if need <= INLINE_MAX_NON_ASCII {
                let units = self.units() + sum.units;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(len), n);
                *p.add(INLINE_UNITS) = units as u8;
                *p.add(INLINE_MAX) = INLINE | NON_ASCII | need as u8;
                return;
            }
        }
        self.append_slow(bytes, sum);
    }

    /// Copy into a unique heap buffer of `len` bytes that has room and the right kind (a header
    /// if the result is not ASCII).
    #[inline(always)]
    unsafe fn append_unique(&mut self, len: usize, bytes: &[u8], sum: Summary) {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr().add(len), bytes.len());
        self.w1 += pack(sum.units, bytes.len());
        if sum.lone != 0 {
            heap::set_lone(self.ptr(), heap::lone(self.ptr()) + sum.lone);
        }
    }

    /// [`Self::append`] when the text must move: a unique heap buffer of the right kind that is
    /// full grows; anything else (static, full inline, shared heap, an ASCII buffer getting its
    /// first non-ASCII text) moves to a new string.
    #[cold]
    #[inline(never)]
    unsafe fn append_slow(&mut self, bytes: &[u8], sum: Summary) {
        if bytes.is_empty() {
            return;
        }
        let need = self.len() + bytes.len();
        let units = self.units() + sum.units;
        let header = units != need;
        let unique = self.is_heap() && heap::is_unique(self.ptr());
        if unique && self.buffer_holds(bytes) {
            // The text lies in the buffer that is about to move: copy it out first.
            let copy = bytes.to_vec();
            return self.append_slow(&copy, sum);
        }
        if unique && header != self.is_ascii() {
            let cap = grown(self.w2 as usize, need);
            self.w0 = heap::grow(self.ptr(), self.w2 as usize, cap, header) as usize as u64;
            self.w2 = cap as u64;
            self.append_unique(need - bytes.len(), bytes, sum);
            return;
        }
        let pieces = [self.as_bytes(), bytes];
        // A unique buffer moving to a header keeps its size (a builder's hint) and stays a heap
        // string, as it would have for ASCII text.
        let s = if !unique && fits_inline(need, units) {
            VeltStr::inline_of(&pieces, units)
        } else {
            let cap = match self.w2 as usize {
                cap if unique && need <= cap => cap,
                cap if unique => grown(cap, need),
                _ => grown(need, need),
            };
            let mine = self.summary();
            let total = Summary {
                units,
                lone: mine.lone + sum.lone,
            };
            VeltStr::heap_of(&pieces, total, cap)
        };
        self.release();
        *self = s;
    }

    /// Does appending `bytes` join a high surrogate at the end of `self` with a low one at the
    /// start of `bytes`? Only when `self` has lone surrogates too (the caller checked `bytes`).
    unsafe fn joins(&self, bytes: &[u8]) -> bool {
        wtf8::starts_with_low(bytes)
            && !self.is_ascii()
            && (!self.is_heap() || heap::lone(self.ptr()) > 0)
            && wtf8::ends_with_high(self.as_bytes())
    }

    /// Append `bytes`, whose leading low surrogate joins the high surrogate ending `self`: the
    /// pair takes one byte more than the high half (3 bytes, 1 unit) and stands for 2 units, so
    /// units add as without a join, bytes shrink by 2 and lone surrogates by 2.
    #[cold]
    unsafe fn push_joined(&mut self, bytes: &[u8], sum: Summary) {
        if self.buffer_holds(bytes) {
            // The text lies in the buffer this join rewrites (and may move): copy it out first.
            let copy = bytes.to_vec();
            return self.push_joined(&copy, sum);
        }
        let len = self.len();
        let pair = wtf8::join_pair(&self.as_bytes()[len - 3..], &bytes[..3]);
        self.append(&pair[3..], Summary { units: 1, lone: 0 });
        // `self` is owned now (inline or a unique heap buffer), so the high half can become the
        // start of the pair in place.
        std::ptr::copy_nonoverlapping(pair.as_ptr(), (self.data() as *mut u8).add(len - 3), 3);
        if self.is_heap() {
            heap::set_lone(self.ptr(), heap::lone(self.ptr()) - 1);
        }
        let rest = Summary {
            units: sum.units - 1,
            lone: sum.lone - 1,
        };
        self.append(&bytes[3..], rest);
    }
}

/// Capacity for a buffer that must hold `need` bytes: amortized doubling from `cap`, at least 32,
/// at most the length limit (a larger `need` is refused by the allocation).
fn grown(cap: usize, need: usize) -> usize {
    need.max(cap.saturating_mul(2))
        .max(32)
        .min(MAX_LEN.max(need))
}

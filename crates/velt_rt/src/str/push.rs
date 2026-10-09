//! The one way bytes enter a string: [`VeltStr::push_wtf8`], and the layout constructors it
//! shares with `VeltStr::from_bytes` and friends. Pushing keeps the byte count, the UTF-16 unit
//! count, the lone-surrogate count and the form (inline ASCII, inline non-ASCII, heap with or
//! without a header) in step, and joins a high surrogate at the end of the string with a low one
//! at the start of the piece into the pair's 4-byte code point (canonical WTF-8).

use std::mem::MaybeUninit;
use std::sync::atomic::Ordering;

use super::slice::SLICE_TAG;
use super::{
    fits_inline, heap, invariants, pack, wtf8, Summary, VeltStr, INLINE, INLINE_LEN, INLINE_LONE,
    INLINE_MAX, INLINE_MAX_NON_ASCII, INLINE_UNITS, MAX_LEN, NON_ASCII,
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
        // A join needs a low surrogate at the start of the piece (and lone surrogates on both
        // sides, which `push_lone` checks).
        if sum.lone == 0 || !wtf8::starts_with_low(bytes) {
            self.append(bytes, sum);
        } else {
            self.push_lone(bytes, sum);
        }
        #[cfg(debug_assertions)]
        invariants::check_seam(self, seam);
        invariants::check_whole(self);
    }

    /// [`Self::push_wtf8`] of ASCII text (literal chunks, numbers, keywords, escaped JSON): the
    /// common builder path. Its summary is known, nothing can join, and the counts are updated
    /// before the copy, so nothing stays live across it.
    ///
    /// # Safety
    /// `self` must be valid and `bytes` ASCII; `bytes` may lie in `self`'s own heap buffer as for
    /// [`Self::push_wtf8`].
    #[inline(always)]
    pub unsafe fn push_ascii(&mut self, bytes: &[u8]) {
        let n = bytes.len();
        debug_assert!(bytes.is_ascii());
        let tag = self.tag();
        // A static or plain heap string (a slice is never appended to in place).
        if tag & (INLINE | SLICE_TAG) == 0 {
            let len = self.w1 as u32 as usize;
            if self.w2 != 0 && len + n <= self.w2 as usize && heap::is_unique(self.ptr()) {
                let dst = self.ptr().add(len);
                // One byte and one unit per character.
                self.w1 += n as u64 * 0x1_0000_0001;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
                return;
            }
        } else if tag & (INLINE | NON_ASCII) == INLINE {
            let len = (tag & INLINE_LEN) as usize;
            if len + n <= INLINE_MAX {
                let p = self as *mut VeltStr as *mut u8;
                *p.add(INLINE_MAX) = INLINE | (len + n) as u8;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(len), n);
                return;
            }
        }
        self.push_ascii_slow(bytes);
    }

    /// [`Self::push_ascii`] when the text doesn't fit in place (out of line, so the fast path
    /// saves no registers).
    #[cold]
    #[inline(never)]
    unsafe fn push_ascii_slow(&mut self, bytes: &[u8]) {
        self.push_wtf8(bytes, Some(Summary::ascii(bytes.len())));
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
            if self.is_inline() {
                return self.push_non_ascii_str(s);
            }
            // Onto a heap or static string: the append itself is inlined (the hot path of
            // `s += piece`).
            let bytes = if len == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(data, len)
            };
            return self.push_wtf8(bytes, Some(s.summary()));
        }
        let bytes = if len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(data, len)
        };
        self.push_wtf8(bytes, Some(Summary::ascii(len)));
    }

    /// [`Self::push_str`] of a non-ASCII string. Into an inline string with room nothing needs
    /// its lone surrogates (an inline string keeps no count), so they are counted only for a
    /// heap result.
    #[inline(never)]
    unsafe fn push_non_ascii_str(&mut self, s: &VeltStr) {
        let bytes = s.as_bytes();
        let tag = self.tag();
        let len = (tag & INLINE_LEN) as usize;
        if tag & INLINE != 0 && len + bytes.len() <= INLINE_MAX_NON_ASCII && !self.joins(bytes) {
            invariants::check_piece(bytes, None);
            let units = self.units() + s.units();
            let p = self as *mut VeltStr as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(len), bytes.len());
            let lone = (tag & INLINE_LONE) | if s.may_have_lone() { INLINE_LONE } else { 0 };
            *p.add(INLINE_UNITS) = units as u8;
            *p.add(INLINE_MAX) = INLINE | NON_ASCII | lone | (len + bytes.len()) as u8;
            #[cfg(debug_assertions)]
            invariants::check_seam(self, len);
            invariants::check_whole(self);
            return;
        }
        self.push_wtf8(bytes, Some(s.summary()));
    }

    /// Make `self` the string `head + self` with room for `cap` bytes, in its own buffer: when
    /// `self` holds the only reference to a heap buffer of its own (not a slice, a literal or an
    /// inline string), the buffer grows to `cap` (`realloc`), the text moves up behind `head`
    /// and `head` is copied in front. A template literal reuses its longest part this way
    /// instead of copying it into a new buffer (`velt_rt_strbuf_adopt`). Returns false, changing
    /// nothing, when the buffer can't be reused: it is shared, it lacks the header a non-ASCII
    /// result needs, it has breadcrumbs or remembered positions (they locate the text where it
    /// is), or a lone surrogate in `head` could join the text.
    ///
    /// # Safety
    /// Both strings must be valid; `head` must not lie in `self`'s buffer.
    pub unsafe fn prepend_in_place(&mut self, head: &VeltStr, cap: usize) -> bool {
        if !self.is_plain_heap() || !heap::is_unique(self.ptr()) {
            return false;
        }
        debug_assert!(
            !self.buffer_holds(head.as_bytes()),
            "the head lies in the part's buffer"
        );
        let header = !self.is_ascii();
        if !head.is_ascii() && !header {
            return false;
        }
        if header && !heap::crumbs(self.ptr()).load(Ordering::Relaxed).is_null() {
            return false;
        }
        if head.may_have_lone() && head.lone() > 0 {
            return false;
        }
        let (len, hlen) = (self.len(), head.len());
        let total = len + hlen;
        let old_cap = self.w2 as usize;
        let mut data = self.ptr();
        let new_cap = cap.max(total);
        if new_cap > old_cap {
            data = heap::grow(data, old_cap, new_cap, header);
            self.w2 = new_cap as u64;
        }
        if hlen > 0 {
            std::ptr::copy(data, data.add(hlen), len);
            std::ptr::copy_nonoverlapping(head.data(), data, hlen);
        }
        self.w0 = data as usize as u64;
        self.w1 = pack(self.units() + head.units(), total);
        #[cfg(debug_assertions)]
        invariants::check_seam(self, hlen);
        invariants::check_whole(self);
        true
    }

    /// `a + b` as a new string (`velt_rt_str_concat`), allocated once at its exact size.
    ///
    /// # Safety
    /// Both strings must be valid.
    pub(super) unsafe fn concat(a: &VeltStr, b: &VeltStr) -> VeltStr {
        let (ta, tb) = (a.as_bytes(), b.as_bytes());
        if a.joins(tb) {
            let mut s = VeltStr::owned_counted(ta, a.summary());
            s.push_wtf8(tb, Some(b.summary()));
            return s;
        }
        invariants::check_piece(ta, None);
        invariants::check_piece(tb, None);
        let len = ta.len() + tb.len();
        let units = a.units() + b.units();
        // An inline string keeps no lone count, so only a heap result needs the operands'.
        let s = if fits_inline(len, units) {
            VeltStr::inline_of(&[ta, tb], units, a.may_have_lone() || b.may_have_lone())
        } else {
            let total = Summary {
                units,
                lone: a.lone().saturating_add(b.lone()),
            };
            VeltStr::heap_of(&[ta, tb], total, len)
        };
        #[cfg(debug_assertions)]
        invariants::check_seam(&s, a.len());
        invariants::check_whole(&s);
        s
    }

    /// An owned copy of `bytes` (`known`: their summary, if the caller has it).
    pub(super) fn owned(bytes: &[u8], known: Option<Summary>) -> VeltStr {
        match known {
            Some(sum) => VeltStr::owned_counted(bytes, sum),
            None => VeltStr::owned_counted(bytes, wtf8::summarize(bytes)),
        }
    }

    /// An owned copy of `bytes`, whose summary is `sum`: inline when short, else a heap buffer
    /// of exactly its size.
    #[inline]
    pub(super) fn owned_counted(bytes: &[u8], sum: Summary) -> VeltStr {
        invariants::check_piece(bytes, Some(sum));
        let s = if fits_inline(bytes.len(), sum.units) {
            VeltStr::inline(bytes, sum.units, sum.lone > 0)
        } else {
            // SAFETY: a fresh buffer of exactly the text's size.
            unsafe { VeltStr::heap_of(&[bytes], sum, bytes.len()) }
        };
        invariants::check_whole(&s);
        s
    }

    /// The inline string of `bytes`, which has `units` code units and must fit; `lone`: it may
    /// hold lone surrogates.
    #[inline]
    pub(super) fn inline(bytes: &[u8], units: usize, lone: bool) -> VeltStr {
        let len = bytes.len();
        debug_assert!(fits_inline(len, units));
        let mut s = MaybeUninit::<VeltStr>::zeroed();
        let p = s.as_mut_ptr() as *mut u8;
        // SAFETY: 24 writable bytes; the text fits before byte 22 (non-ASCII) or 23 (ASCII).
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, len);
            *p.add(INLINE_MAX) = if units == len {
                INLINE | len as u8
            } else {
                *p.add(INLINE_UNITS) = units as u8;
                INLINE | NON_ASCII | if lone { INLINE_LONE } else { 0 } | len as u8
            };
            s.assume_init()
        }
    }

    /// The inline string of `pieces` concatenated (`units` in all; must fit; `lone` as in
    /// [`Self::inline`]).
    fn inline_of(pieces: &[&[u8]], units: usize, lone: bool) -> VeltStr {
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
                if lone {
                    tag |= INLINE_LONE;
                }
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
        // A static or plain heap string (a slice is never appended to in place).
        if tag & (INLINE | SLICE_TAG) == 0 {
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
        } else if tag & INLINE != 0 {
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
                let lone = (tag & INLINE_LONE) | if sum.lone > 0 { INLINE_LONE } else { 0 };
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(len), n);
                *p.add(INLINE_UNITS) = units as u8;
                *p.add(INLINE_MAX) = INLINE | NON_ASCII | lone | need as u8;
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
        if sum.lone == wtf8::LONE_UNKNOWN {
            // A plain store: reading the count first would chain one append to the next.
            heap::set_lone(self.ptr(), wtf8::LONE_UNKNOWN);
        } else if sum.lone != 0 {
            heap::set_lone(self.ptr(), heap::lone(self.ptr()).saturating_add(sum.lone));
        }
    }

    /// [`Self::append`] when the text must move: a unique heap buffer of the right kind that is
    /// full grows; anything else (static, full inline, shared heap, a slice, an ASCII buffer
    /// getting its first non-ASCII text) moves to a new string.
    #[cold]
    #[inline(never)]
    unsafe fn append_slow(&mut self, bytes: &[u8], sum: Summary) {
        if bytes.is_empty() {
            return;
        }
        let need = self.len() + bytes.len();
        let units = self.units() + sum.units;
        let header = units != need;
        let unique = self.own_cap() != 0 && heap::is_unique(self.ptr());
        if unique && self.buffer_holds(bytes) {
            // The text lies in the buffer that is about to grow or move: copy it out first.
            let copy = bytes.to_vec();
            return self.append_slow(&copy, sum);
        }
        let len = need - bytes.len();
        if unique {
            let cap = self.w2 as usize;
            if header != self.is_ascii() {
                // Full, and of the right kind: grow.
                let new_cap = grown(cap, need);
                self.w0 = heap::grow(self.ptr(), cap, new_cap, header) as usize as u64;
                self.w2 = new_cap as u64;
            } else {
                // An ASCII buffer getting its first non-ASCII text keeps its size (a builder's
                // hint) and moves in place behind a header (its lone count is 0).
                let new_cap = if need <= cap { cap } else { grown(cap, need) };
                self.w0 = heap::add_header(self.ptr(), cap, len, new_cap) as usize as u64;
                self.w2 = new_cap as u64;
            }
            self.append_unique(len, bytes, sum);
            return;
        }
        let pieces = [self.as_bytes(), bytes];
        let s = if fits_inline(need, units) {
            VeltStr::inline_of(&pieces, units, sum.lone > 0 || self.may_have_lone())
        } else {
            let cap = grown(need, need);
            let total = Summary {
                units,
                lone: self.lone().saturating_add(sum.lone),
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
            && (!self.is_heap() || self.heap_lone() > 0)
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
            heap::set_lone(self.ptr(), wtf8::lone_less(heap::lone(self.ptr()), 1));
        }
        let rest = Summary {
            units: sum.units - 1,
            lone: wtf8::lone_less(sum.lone, 1),
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

//! Shared slices (#402): a heap string whose bytes are a sub-range of another heap string's
//! buffer, sharing it through its count, so `rest = rest.slice(n)` is O(1) instead of a copy of
//! the rest (front-consuming parsers were O(n²); V8 has sliced strings for the same reason).
//!
//! A slice is a heap string (`w2 > 0` as `i64`, so generated code drops and reads it as one) whose
//! `w0` points at its first byte inside the buffer and whose `w1` is its own unit and byte count.
//! Its `w2` tells it apart and locates the buffer:
//!
//! | bits | plain heap string | slice |
//! |---|---|---|
//! | 63 | 0 | 0 |
//! | 62 | 0 | 1 ([`SLICE`]) |
//! | 61 | 0 | the buffer has a header ([`SLICE_HEADER`]: its text is not ASCII) |
//! | 31..60 | 0 | byte offset of `w0` from the buffer's first byte |
//! | 30 | capacity (bits 0..30, below 2³¹) | 0 |
//! | 0..29 | capacity | the buffer's capacity (below [`SLICE_MAX_CAP`]) |
//!
//! The offset of a plain heap string reads as 0 (its capacity is below 2³¹), so finding the
//! buffer, and through it the count, is the same few instructions for both. A slice is never
//! appended to in place (it is not "unique": a builder copies it first), never has breadcrumbs of
//! its own (the buffer's describe the whole text, see `crumbs.rs`), and knows its lone surrogates
//! only as "none" when the buffer has none.
//!
//! **Retention.** A slice keeps the whole buffer alive. A piece is shared only when it is at
//! least a quarter of the buffer's capacity ([`SHARE_DIVISOR`]), so live slices pin at most four
//! times their own size; a smaller piece is copied. A parser that consumes its input from the
//! front shares while the rest is large and copies once the rest falls below a quarter, so it
//! copies at most a third of its input in all (n/4 + n/16 + …): linear. Short pieces are inline
//! as before, and buffers of [`SLICE_MAX_CAP`] or more are never sliced (the offset and capacity
//! have 30 bits each).

use super::{fits_inline, heap, invariants, pack, wtf8, Summary, VeltStr};

/// Bit 62 of `w2`: a slice (bit 63 clear: not inline).
pub(super) const SLICE: u64 = 1 << 62;
/// [`SLICE`] in byte 23 (the tag): the append fast paths test it with the inline bit, in one
/// test, so a slice costs them nothing.
pub(super) const SLICE_TAG: u8 = 0x40;
/// Bit 61 of `w2` of a slice: its buffer has a header.
const SLICE_HEADER: u64 = 1 << 61;
/// Where a slice keeps its offset into the buffer.
const OFFSET_SHIFT: u32 = 31;
/// Width of the offset and capacity fields.
const FIELD: u64 = (1 << 30) - 1;
/// Buffers this large or larger are not sliced.
pub(super) const SLICE_MAX_CAP: usize = 1 << 30;
/// A piece is shared when it is at least 1/`SHARE_DIVISOR` of its buffer's capacity.
pub(super) const SHARE_DIVISOR: usize = 4;

impl VeltStr {
    /// Is this a slice (the form test of the table above; false for every other form)?
    #[inline]
    pub(super) fn is_slice(&self) -> bool {
        self.w2 >> 62 == 1
    }

    /// Is this a heap string that is not a slice? One compare: its `w2` is its capacity, from 1
    /// to `MAX_LEN`, while a static string has 0, an inline one bit 63 and a slice bit 62 set.
    #[inline]
    pub(super) fn is_plain_heap(&self) -> bool {
        self.w2.wrapping_sub(1) < super::MAX_LEN as u64
    }

    /// The first byte of the buffer of a heap string: `w0` minus a slice's offset (0 for a plain
    /// heap string). The count is the 8 bytes before it, as for every buffer.
    #[inline]
    pub(super) fn buffer(&self) -> *mut u8 {
        let offset = (self.w2 >> OFFSET_SHIFT) & FIELD;
        (self.w0 - offset) as usize as *mut u8
    }

    /// The capacity of a heap string's buffer and whether it has a header.
    #[inline]
    pub(super) fn buffer_kind(&self) -> (usize, bool) {
        if self.is_slice() {
            ((self.w2 & FIELD) as usize, self.w2 & SLICE_HEADER != 0)
        } else {
            (self.w2 as usize, !self.is_ascii())
        }
    }

    /// The capacity of a plain heap string, which may be appended to in place when unique; 0 for
    /// every other form (static, slice; inline strings test their form first).
    #[inline]
    pub(super) fn own_cap(&self) -> usize {
        if self.w2 < SLICE {
            self.w2 as usize
        } else {
            0
        }
    }

    /// A non-ASCII heap string's lone surrogates as its buffer's header says: exact for a plain
    /// heap string; for a slice 0 when the buffer has none, else unknown (the slice's share is
    /// not recorded anywhere).
    ///
    /// # Safety
    /// `self` must be a valid non-ASCII heap string.
    #[inline(always)]
    pub(super) unsafe fn heap_lone(&self) -> usize {
        if self.w2 < SLICE {
            heap::lone(self.ptr())
        } else {
            self.slice_lone()
        }
    }

    /// [`Self::heap_lone`] of a slice.
    #[inline(never)]
    unsafe fn slice_lone(&self) -> usize {
        match heap::lone(self.buffer()) {
            0 => 0,
            _ => wtf8::LONE_UNKNOWN,
        }
    }

    /// Before a slice's position is first remembered (`recent.rs`): mark its buffer, so freeing
    /// it bumps the epoch that expires the entry. Nothing for other strings.
    ///
    /// # Safety
    /// `self` must be valid and not ASCII (a non-ASCII slice's buffer has a header).
    #[inline]
    pub(super) unsafe fn mark_remembered(&self) {
        if self.is_slice() {
            heap::mark_remembered(self.buffer());
        }
    }

    /// [`Self::release`] of a slice: out of line, so dropping a plain string keeps its short path.
    ///
    /// # Safety
    /// `self` must be a valid slice, not used afterwards.
    #[inline(never)]
    pub(super) unsafe fn release_slice(&self) {
        let (cap, header) = self.buffer_kind();
        heap::release(self.buffer(), cap, header);
    }

    /// Bytes `start..end` as a string: a borrowed sub-range of a static string, the whole string
    /// shared (count +1), a slice sharing a heap buffer when the piece is a large enough part of
    /// it (module docs), else a copy (inline when short). A piece of an ASCII string is ASCII, so
    /// only a piece of a non-ASCII one is counted.
    ///
    /// # Safety
    /// `self` must be valid and `start <= end <= len`, both at code point boundaries.
    pub unsafe fn substring(&self, start: usize, end: usize) -> VeltStr {
        if start >= end {
            return VeltStr::empty();
        }
        let piece = &self.as_bytes()[start..end];
        if self.is_static() {
            let units = if self.is_ascii() {
                piece.len()
            } else {
                wtf8::count_units(piece)
            };
            return VeltStr::borrowed_counted(piece.as_ptr(), piece.len(), units);
        }
        if start == 0 && end == self.len() {
            return self.share();
        }
        let sum = self.piece_summary(piece);
        if !fits_inline(piece.len(), sum.units) && self.shares_piece(piece.len()) {
            return self.slice_of(start, piece.len(), sum.units);
        }
        VeltStr::owned_counted(piece, sum)
    }

    /// The summary of `piece`, a sub-range of `self`: ASCII if `self` is, and without lone
    /// surrogates if `self` is known to have none (its heap header or inline flag says so);
    /// otherwise the piece is counted.
    ///
    /// # Safety
    /// `self` must be valid.
    unsafe fn piece_summary(&self, piece: &[u8]) -> Summary {
        if self.is_ascii() {
            return Summary::ascii(piece.len());
        }
        let lone_free = !self.may_have_lone();
        Summary {
            units: wtf8::count_units(piece),
            lone: if lone_free {
                0
            } else {
                wtf8::count_lone(piece)
            },
        }
    }

    /// [`Self::substring`] of a non-ASCII string whose piece has `units` code units (the caller
    /// translated its ends): nothing is counted before deciding to share, so a slice costs O(1).
    ///
    /// # Safety
    /// As for [`Self::substring`]; `units` must be the piece's UTF-16 length.
    pub(crate) unsafe fn substring_units(&self, start: usize, end: usize, units: usize) -> VeltStr {
        if start >= end {
            return VeltStr::empty();
        }
        let piece = &self.as_bytes()[start..end];
        if self.is_static() {
            return VeltStr::borrowed_counted(piece.as_ptr(), piece.len(), units);
        }
        if start == 0 && end == self.len() {
            return self.share();
        }
        if !fits_inline(piece.len(), units) && self.shares_piece(piece.len()) {
            return self.slice_of(start, piece.len(), units);
        }
        let lone = if self.is_ascii() || !self.may_have_lone() {
            0
        } else {
            wtf8::count_lone(piece)
        };
        VeltStr::owned_counted(piece, Summary { units, lone })
    }

    /// Is a piece of `len` bytes of this string shared rather than copied (module docs)? Only a
    /// heap string's pieces too long to be inline get here.
    #[inline]
    fn shares_piece(&self, len: usize) -> bool {
        debug_assert!(self.is_heap());
        let (cap, _) = self.buffer_kind();
        cap < SLICE_MAX_CAP && len * SHARE_DIVISOR >= cap
    }

    /// The slice of `len` bytes from byte `start` of this heap string (`units` code units),
    /// sharing its buffer.
    ///
    /// # Safety
    /// `self` must be a valid heap string whose buffer is below [`SLICE_MAX_CAP`], and the range
    /// a piece of it at code point boundaries.
    unsafe fn slice_of(&self, start: usize, len: usize, units: usize) -> VeltStr {
        let buffer = self.buffer();
        heap::retain(buffer);
        let (cap, header) = self.buffer_kind();
        let at = self.ptr().add(start);
        let offset = at as usize - buffer as usize;
        let s = VeltStr {
            w0: at as usize as u64,
            w1: pack(units, len),
            w2: SLICE
                | if header { SLICE_HEADER } else { 0 }
                | (offset as u64) << OFFSET_SHIFT
                | cap as u64,
        };
        invariants::check_whole(&s);
        s
    }
}

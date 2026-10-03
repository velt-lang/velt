//! Strings: the `VeltStr` value (rt_abi.md "Strings") and the `velt_rt_str_*` core functions.
//!
//! A string is an immutable value in 24 bytes with three forms, told apart by the last word. The
//! bytes are canonical WTF-8 (`wtf8`), and every form also knows the string's UTF-16 length
//! (#377): a string is ASCII exactly when its unit count equals its byte count.
//! - **static / borrowed** (`w2 == 0`): `{ptr, units << 32 | len, 0}` — literals, and sub-ranges
//!   of them; never freed. The all-zero value is the empty string.
//! - **inline** (top bit of `w2` set): up to [`INLINE_MAX`] ASCII bytes or
//!   [`INLINE_MAX_NON_ASCII`] other bytes stored in the value itself; byte 23 is
//!   `0x80 | len`, plus `0x40` for a non-ASCII string, whose unit count is then byte 22, plus
//!   `0x20` when it may hold lone surrogates. No heap, copying is a 24-byte copy.
//! - **heap** (`w2 > 0` as `i64`): `{ptr, units << 32 | len, cap}` where `ptr` points into a
//!   reference-counted buffer (`heap`) of `cap` bytes; copying bumps the count, dropping the last
//!   copy frees it. Non-ASCII buffers carry a header with the lone-surrogate count.
//!
//! Bytes enter a string through one function, [`VeltStr::push_wtf8`] (`push`), or through the
//! constructors here, which use the same counting and layout helpers.
//!
//! Strings are immutable, so a shared heap buffer is never written; only a builder that holds the
//! single reference (count 1) appends in place (`strbuf.rs`). Counts are atomic: any string may
//! reach another thread (spawned tasks, HTTP handlers, `shared`), see rt_abi.md for the cost.

mod abi;
mod heap;
mod invariants;
mod join;
mod push;
pub mod stats;
#[cfg(test)]
mod tests;
mod wtf8;

pub use abi::*;
pub use wtf8::Summary;

#[cfg(not(target_endian = "little"))]
compile_error!("the VeltStr inline form assumes a little-endian target");

/// Longest ASCII string stored inline.
pub const INLINE_MAX: usize = 23;

/// Longest non-ASCII string stored inline (byte 22 holds its unit count).
pub const INLINE_MAX_NON_ASCII: usize = 22;

/// Longest string, in bytes (below 2 GiB): `w1` packs the unit and byte counts in 32 bits each,
/// and compiled code reads the byte count as a signed 32-bit number.
pub const MAX_LEN: usize = heap::MAX_CAP;

/// Byte 23 of an inline string: this bit, plus [`NON_ASCII`] and the byte length.
const INLINE: u8 = 0x80;
/// Byte 23 of a non-ASCII inline string has this bit set.
const NON_ASCII: u8 = 0x40;
/// Byte 23 of an inline string that may hold lone surrogates has this bit set: clear, the string
/// has none, so its lone count costs no scan (an inline string has no room for the count).
const INLINE_LONE: u8 = 0x20;
/// The byte length in byte 23 of an inline string.
const INLINE_LEN: u8 = 0x1f;
/// Where a non-ASCII inline string keeps its unit count.
const INLINE_UNITS: usize = 22;

/// `w1` of the static and heap forms.
#[inline]
const fn pack(units: usize, len: usize) -> u64 {
    ((units as u64) << 32) | len as u64
}

/// A Velt string: 24 bytes, align 8 (vir::STR_AGG). See the module docs for the three forms.
#[repr(C)]
pub struct VeltStr {
    /// The pointer of the static / heap forms, as a full word so the struct has no padding: on
    /// 32-bit targets (wasm32) bytes 4..8 of an inline string would otherwise be padding, which
    /// field-wise copies do not preserve.
    w0: u64,
    w1: u64,
    w2: u64,
}

// SAFETY: heap buffers are immutable while shared and use atomic counts; static bytes are
// read-only. So a string may be sent to and shared with other threads.
unsafe impl Send for VeltStr {}

const _: () = assert!(std::mem::size_of::<VeltStr>() == 24 && std::mem::align_of::<VeltStr>() == 8);

impl std::fmt::Debug for VeltStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: a VeltStr handed to Rust code is valid by the ABI contract.
        let text = String::from_utf8_lossy(unsafe { self.as_bytes() });
        let form = if self.is_inline() {
            "inline"
        } else if self.is_heap() {
            "heap"
        } else {
            "static"
        };
        write!(f, "VeltStr({form} {text:?})")
    }
}

impl VeltStr {
    /// The empty string (all zero).
    pub const fn empty() -> VeltStr {
        VeltStr {
            w0: 0,
            w1: 0,
            w2: 0,
        }
    }

    /// Static bytes (never freed).
    pub fn from_static(bytes: &'static [u8]) -> VeltStr {
        // SAFETY: 'static bytes outlive every copy.
        unsafe { VeltStr::borrowed(bytes.as_ptr(), bytes.len()) }
    }

    /// A string that borrows `len` bytes at `ptr` (static form: dropping it frees nothing). The
    /// bytes are scanned for their unit count.
    ///
    /// # Safety
    /// The bytes must be canonical WTF-8 and stay valid and unchanged for as long as the string
    /// (and its copies) live.
    pub unsafe fn borrowed(ptr: *const u8, len: usize) -> VeltStr {
        let bytes = if len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(ptr, len)
        };
        invariants::check_piece(bytes, None);
        VeltStr::borrowed_counted(ptr, len, wtf8::count_units(bytes))
    }

    /// [`Self::borrowed`] with a known unit count.
    unsafe fn borrowed_counted(ptr: *const u8, len: usize, units: usize) -> VeltStr {
        if len > MAX_LEN {
            crate::panic::fatal("string too long");
        }
        let s = VeltStr {
            w0: ptr as usize as u64,
            w1: pack(units, len),
            w2: 0,
        };
        invariants::check_whole(&s);
        s
    }

    /// An owned copy of `bytes` (canonical WTF-8): inline when short, else a fresh heap buffer
    /// of exactly its size.
    pub fn from_bytes(bytes: &[u8]) -> VeltStr {
        VeltStr::owned(bytes, None)
    }

    /// An owned copy of UTF-8 text, like [`Self::from_bytes`]. UTF-8 holds no lone surrogates,
    /// so only the units are counted.
    pub fn from_text(text: &str) -> VeltStr {
        VeltStr::from_text_counted(text, wtf8::count_units(text.as_bytes()))
    }

    /// The UTF-16 length of WTF-8 `bytes`.
    pub fn units_of(bytes: &[u8]) -> usize {
        wtf8::count_units(bytes)
    }

    /// [`Self::from_text`] when the caller already counted the text's UTF-16 length (`units`).
    #[inline]
    pub fn from_text_counted(text: &str, units: usize) -> VeltStr {
        VeltStr::owned_counted(text.as_bytes(), Summary { units, lone: 0 })
    }

    /// [`Self::borrowed`] for UTF-8 text whose UTF-16 length (`units`) the caller counted.
    ///
    /// # Safety
    /// As for [`Self::borrowed`]; the bytes must be UTF-8 and `units` their UTF-16 length.
    pub unsafe fn borrowed_text(ptr: *const u8, len: usize, units: usize) -> VeltStr {
        if len > 0 {
            let bytes = std::slice::from_raw_parts(ptr, len);
            invariants::check_piece(bytes, Some(Summary { units, lone: 0 }));
        }
        VeltStr::borrowed_counted(ptr, len, units)
    }

    /// An owned copy of a Vec's bytes (heap strings carry a count header, so this copies).
    pub fn from_vec(v: Vec<u8>) -> VeltStr {
        VeltStr::from_bytes(&v)
    }

    /// An empty string with room for `cap` bytes before it must allocate (inline up to 23). The
    /// buffer is an ASCII one: a non-ASCII push moves the text to a buffer with a header.
    pub fn with_capacity(cap: usize) -> VeltStr {
        if cap <= INLINE_MAX {
            return VeltStr::inline(&[], 0, false);
        }
        VeltStr {
            w0: heap::alloc(cap, false) as usize as u64,
            w1: 0,
            w2: cap as u64,
        }
    }

    /// Byte 23: `0x80 | len` (and `0x40` when non-ASCII) in the inline form, 0 otherwise (the
    /// top byte of `cap` / 0).
    ///
    /// The form is tested through this byte, not the whole of `w2`: a builder appending inline
    /// writes single bytes into `w2`, and reading them back as one 8-byte word right after would
    /// stall on store forwarding.
    #[inline]
    fn tag(&self) -> u8 {
        // SAFETY: byte 23 of a 24-byte value.
        unsafe { *(self as *const VeltStr as *const u8).add(INLINE_MAX) }
    }

    #[inline]
    pub fn is_static(&self) -> bool {
        !self.is_inline() && self.w2 == 0
    }

    #[inline]
    pub fn is_inline(&self) -> bool {
        self.tag() & INLINE != 0
    }

    #[inline]
    pub fn is_heap(&self) -> bool {
        !self.is_inline() && self.w2 != 0
    }

    /// Byte length.
    #[inline]
    pub fn len(&self) -> usize {
        let tag = self.tag();
        if tag & INLINE != 0 {
            (tag & INLINE_LEN) as usize
        } else {
            self.w1 as u32 as usize
        }
    }

    /// UTF-16 length (code units).
    #[inline]
    pub fn units(&self) -> usize {
        let tag = self.tag();
        if tag & INLINE == 0 {
            (self.w1 >> 32) as usize
        } else if tag & NON_ASCII != 0 {
            // SAFETY: byte 22 of a 24-byte value.
            unsafe { *(self as *const VeltStr as *const u8).add(INLINE_UNITS) as usize }
        } else {
            (tag & INLINE_LEN) as usize
        }
    }

    /// Is every byte ASCII (units == bytes)? Decides whether a heap buffer has a header.
    #[inline]
    pub fn is_ascii(&self) -> bool {
        let tag = self.tag();
        if tag & INLINE != 0 {
            tag & NON_ASCII == 0
        } else {
            (self.w1 >> 32) as u32 == self.w1 as u32
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Lone surrogates: stored in a heap buffer's header, counted for the other forms.
    ///
    /// # Safety
    /// `self` must be valid.
    #[inline]
    unsafe fn lone(&self) -> usize {
        let tag = self.tag();
        if (tag & INLINE != 0 && tag & INLINE_LONE == 0) || self.is_ascii() {
            0
        } else if self.is_heap() {
            heap::lone(self.ptr())
        } else {
            wtf8::count_lone(self.as_bytes())
        }
    }

    /// Might `self` hold lone surrogates? No for ASCII; for a heap string its header's count; for
    /// an inline string its flag (conservative after a join); and yes for a non-ASCII static
    /// string, which has no room to say (finding out would take a scan).
    ///
    /// # Safety
    /// `self` must be valid.
    #[inline]
    unsafe fn may_have_lone(&self) -> bool {
        let tag = self.tag();
        if tag & INLINE != 0 {
            tag & INLINE_LONE != 0
        } else if self.is_ascii() {
            false
        } else if self.w2 != 0 {
            heap::lone(self.ptr()) > 0
        } else {
            true
        }
    }

    /// The unit and lone-surrogate counts, to append `self` to another string without a scan.
    ///
    /// # Safety
    /// `self` must be valid.
    #[inline]
    pub unsafe fn summary(&self) -> Summary {
        Summary {
            units: self.units(),
            lone: self.lone(),
        }
    }

    /// Address of the first byte (inside `self` for the inline form).
    fn data(&self) -> *const u8 {
        if self.is_inline() {
            self as *const VeltStr as *const u8
        } else {
            self.ptr()
        }
    }

    /// The buffer pointer of the static / heap forms.
    fn ptr(&self) -> *mut u8 {
        self.w0 as usize as *mut u8
    }

    /// # Safety
    /// `self` must be a valid string per the layout contract.
    #[inline]
    pub unsafe fn as_bytes(&self) -> &[u8] {
        // One test of the form gives both the address and the length.
        let tag = self.tag();
        let (data, n) = if tag & INLINE != 0 {
            (
                self as *const VeltStr as *const u8,
                (tag & INLINE_LEN) as usize,
            )
        } else {
            (self.ptr() as *const u8, self.w1 as u32 as usize)
        };
        if n == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(data, n)
        }
    }

    /// Bytes `start..end` as a string: a borrowed sub-range of a static string, the whole string
    /// shared (count +1), else a fresh copy. A piece of an ASCII string is ASCII, so only a piece
    /// of a non-ASCII one is counted.
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
        VeltStr::owned_counted(piece, self.piece_summary(piece))
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

    /// Another reference to the same string (count +1 for heap strings).
    ///
    /// # Safety
    /// `self` must be valid.
    pub unsafe fn share(&self) -> VeltStr {
        if self.is_heap() {
            heap::retain(self.ptr());
        }
        VeltStr {
            w0: self.w0,
            w1: self.w1,
            w2: self.w2,
        }
    }

    /// Do `bytes` start in `self`'s heap buffer (text `self` is about to grow, move or rewrite)?
    fn buffer_holds(&self, bytes: &[u8]) -> bool {
        let start = self.w0 as usize;
        self.is_heap() && (start..start + self.w2 as usize).contains(&(bytes.as_ptr() as usize))
    }

    /// Give up this reference (frees the buffer with the last one) and leave `self` empty.
    ///
    /// # Safety
    /// `self` must be valid and not used afterwards except as the empty string.
    pub unsafe fn release(&mut self) {
        if self.is_heap() {
            heap::release(self.ptr(), self.w2 as usize, !self.is_ascii());
        }
        *self = VeltStr::empty();
    }

    /// The same string, moved inline if it is a short heap string (freeing its buffer).
    ///
    /// # Safety
    /// `self` must be valid.
    pub unsafe fn compact(mut self) -> VeltStr {
        if !self.is_heap() || !fits_inline(self.len(), self.units()) {
            return self;
        }
        let s = VeltStr::inline(self.as_bytes(), self.units(), self.lone() > 0);
        self.release();
        invariants::check_whole(&s);
        s
    }

    /// Insert `bytes` at byte offset `at` (rare: `console.log`'s `<ref *N>` prefix), moving
    /// the text to a new string.
    ///
    /// # Safety
    /// `self` must be valid, `at` at most its length and on a character boundary; `bytes` must
    /// not point into `self`.
    pub unsafe fn insert_bytes(&mut self, at: usize, bytes: &[u8]) {
        let old = self.as_bytes();
        let mut text = Vec::with_capacity(old.len() + bytes.len());
        text.extend_from_slice(&old[..at]);
        text.extend_from_slice(bytes);
        text.extend_from_slice(&old[at..]);
        let s = VeltStr::from_bytes(&text);
        self.release();
        *self = s;
    }

    /// Run `f` on a byte vector that is appended to `self` (formatting helpers write into a Vec).
    ///
    /// # Safety
    /// `self` must be valid; what `f` writes must be canonical WTF-8.
    pub unsafe fn push_with(&mut self, f: impl FnOnce(&mut Vec<u8>)) {
        self.push_with_summary(f, |_| None);
    }

    /// [`Self::push_with`] for a writer whose output's summary follows from what it wrote (its
    /// length): `summary` gives it, or `None` to have it counted.
    ///
    /// # Safety
    /// As for [`Self::push_with`]; a summary must be the output's own.
    pub unsafe fn push_with_summary(
        &mut self,
        f: impl FnOnce(&mut Vec<u8>),
        summary: impl FnOnce(usize) -> Option<Summary>,
    ) {
        let mut scratch = SCRATCH.with(|s| std::mem::take(&mut *s.borrow_mut()));
        scratch.clear();
        f(&mut scratch);
        self.push_wtf8(&scratch, summary(scratch.len()));
        SCRATCH.with(|s| *s.borrow_mut() = scratch);
    }
}

/// Does a string of `len` bytes and `units` code units fit in the inline form?
#[inline]
fn fits_inline(len: usize, units: usize) -> bool {
    if units == len {
        len <= INLINE_MAX
    } else {
        len <= INLINE_MAX_NON_ASCII
    }
}

thread_local! {
    /// Reused formatting buffer for [`VeltStr::push_with`] (numbers, JSON pieces).
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Write a string result to an out-parameter.
///
/// # Safety
/// `out` must be writable.
#[inline]
pub(crate) unsafe fn write_out(out: *mut VeltStr, s: VeltStr) {
    out.write(s);
}

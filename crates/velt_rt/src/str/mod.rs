//! Strings: the `VeltStr` value (rt_abi.md "Strings") and the `velt_rt_str_*` core functions.
//!
//! A string is an immutable value in 24 bytes with three forms, told apart by the last word:
//! - **static / borrowed** (`w2 == 0`): `{ptr, len, 0}` — literals, and sub-ranges of them; never
//!   freed. The all-zero value is the empty string.
//! - **inline** (top bit of `w2` set): up to [`INLINE_MAX`] bytes stored in the value itself;
//!   byte 23 is `0x80 | len`. No heap, copying is a 24-byte copy.
//! - **heap** (`w2 > 0` as `i64`): `{ptr, len, cap}` where `ptr` points into a reference-counted
//!   buffer (`heap`) of `cap` bytes; copying bumps the count, dropping the last copy frees it.
//!
//! Strings are immutable, so a shared heap buffer is never written; only a builder that holds the
//! single reference (count 1) appends in place (`strbuf.rs`). Counts are atomic: any string may
//! reach another thread (spawned tasks, HTTP handlers, `shared`), see rt_abi.md for the cost.

mod abi;
mod heap;
pub mod stats;

pub use abi::*;

use std::mem::MaybeUninit;

#[cfg(not(target_endian = "little"))]
compile_error!("the VeltStr inline form assumes a little-endian target");

/// Longest string stored inline.
pub const INLINE_MAX: usize = 23;

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

    /// A string that borrows `len` bytes at `ptr` (static form: dropping it frees nothing).
    ///
    /// # Safety
    /// The bytes must stay valid and unchanged for as long as the string (and its copies) live.
    pub unsafe fn borrowed(ptr: *const u8, len: usize) -> VeltStr {
        VeltStr {
            w0: ptr as usize as u64,
            w1: len as u64,
            w2: 0,
        }
    }

    /// An owned copy of `bytes`: inline when short, else a fresh heap buffer.
    pub fn from_bytes(bytes: &[u8]) -> VeltStr {
        if bytes.len() <= INLINE_MAX {
            return VeltStr::inline(bytes);
        }
        let mut s = VeltStr::with_capacity(bytes.len());
        // SAFETY: a fresh unique heap buffer with room for `bytes`.
        unsafe { s.append_unique(bytes) };
        s
    }

    /// An owned copy of a Vec's bytes (heap strings carry a count header, so this copies).
    pub fn from_vec(v: Vec<u8>) -> VeltStr {
        VeltStr::from_bytes(&v)
    }

    /// Inline string of `bytes` (at most [`INLINE_MAX`]).
    fn inline(bytes: &[u8]) -> VeltStr {
        debug_assert!(bytes.len() <= INLINE_MAX);
        let mut s = MaybeUninit::<VeltStr>::zeroed();
        let p = s.as_mut_ptr() as *mut u8;
        // SAFETY: 24 writable bytes; the data fits before byte 23.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            *p.add(INLINE_MAX) = 0x80 | bytes.len() as u8;
            s.assume_init()
        }
    }

    /// An empty string with room for `cap` bytes before it must allocate (inline up to 23).
    pub fn with_capacity(cap: usize) -> VeltStr {
        if cap <= INLINE_MAX {
            return VeltStr::inline(&[]);
        }
        VeltStr {
            w0: heap::alloc(cap) as usize as u64,
            w1: 0,
            w2: cap as u64,
        }
    }

    /// Byte 23: `0x80 | len` in the inline form, 0 otherwise (the top byte of `cap` / 0).
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
        self.tag() & 0x80 != 0
    }

    #[inline]
    pub fn is_heap(&self) -> bool {
        !self.is_inline() && self.w2 != 0
    }

    /// Byte length.
    #[inline]
    pub fn len(&self) -> usize {
        let tag = self.tag();
        if tag & 0x80 != 0 {
            (tag & 0x7f) as usize
        } else {
            self.w1 as usize
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
    pub unsafe fn as_bytes(&self) -> &[u8] {
        let n = self.len();
        if n == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(self.data(), n)
        }
    }

    /// Bytes `start..end` as a string: a borrowed sub-range of a static string, the whole string
    /// shared (count +1), else a fresh copy.
    ///
    /// # Safety
    /// `self` must be valid and `start <= end <= len`.
    pub unsafe fn substring(&self, start: usize, end: usize) -> VeltStr {
        if start >= end {
            return VeltStr::empty();
        }
        if self.is_static() {
            return VeltStr::borrowed(self.ptr().add(start), end - start);
        }
        if start == 0 && end == self.len() {
            return self.share();
        }
        VeltStr::from_bytes(&self.as_bytes()[start..end])
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

    /// Give up this reference (frees the buffer with the last one) and leave `self` empty.
    ///
    /// # Safety
    /// `self` must be valid and not used afterwards except as the empty string.
    pub unsafe fn release(&mut self) {
        if self.is_heap() {
            heap::release(self.ptr(), self.w2 as usize);
        }
        *self = VeltStr::empty();
    }

    /// The same string, moved inline if it is a short heap string (freeing its buffer).
    ///
    /// # Safety
    /// `self` must be valid.
    pub unsafe fn compact(mut self) -> VeltStr {
        if !self.is_heap() || self.len() > INLINE_MAX {
            return self;
        }
        let s = VeltStr::inline(self.as_bytes());
        self.release();
        s
    }

    /// Append `bytes`, keeping the string owned by `self`: in place when `self` is inline with
    /// room or the only reference to a heap buffer, else by moving to a new buffer.
    ///
    /// # Safety
    /// `self` must be valid; `bytes` must not point into `self`'s own buffer.
    #[inline]
    pub unsafe fn push_bytes(&mut self, bytes: &[u8]) {
        if self.is_heap() {
            let len = self.w1 as usize;
            let need = len + bytes.len();
            if need <= self.w2 as usize && heap::is_unique(self.ptr()) {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr().add(len), bytes.len());
                self.w1 = need as u64;
                return;
            }
        } else if self.is_inline() {
            let len = self.len();
            let need = len + bytes.len();
            if need <= INLINE_MAX {
                let p = self as *mut VeltStr as *mut u8;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(len), bytes.len());
                *p.add(INLINE_MAX) = 0x80 | need as u8;
                return;
            }
        }
        self.push_slow(bytes);
    }

    /// [`Self::push_bytes`] when the text must move: a unique heap buffer that is full grows,
    /// anything else (static, full inline, shared heap) moves to a new buffer.
    #[cold]
    unsafe fn push_slow(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let need = self.len() + bytes.len();
        if self.is_heap() && heap::is_unique(self.ptr()) {
            let cap = grown(self.w2 as usize, need);
            self.w0 = heap::grow(self.ptr(), self.w2 as usize, cap) as usize as u64;
            self.w2 = cap as u64;
            self.append_unique(bytes);
            return;
        }
        let mut s = if need <= INLINE_MAX {
            VeltStr::inline(self.as_bytes())
        } else {
            let mut h = VeltStr::with_capacity(grown(need, need));
            h.append_unique(self.as_bytes());
            h
        };
        self.release();
        s.push_bytes(bytes);
        *self = s;
    }

    /// Append to a unique heap buffer that has room.
    unsafe fn append_unique(&mut self, bytes: &[u8]) {
        let len = self.w1 as usize;
        debug_assert!(self.is_heap() && len + bytes.len() <= self.w2 as usize);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr().add(len), bytes.len());
        self.w1 = (len + bytes.len()) as u64;
    }

    /// Run `f` on a byte vector that is appended to `self` (formatting helpers write into a Vec).
    ///
    /// # Safety
    /// `self` must be valid.
    pub unsafe fn push_with(&mut self, f: impl FnOnce(&mut Vec<u8>)) {
        let mut scratch = SCRATCH.with(|s| std::mem::take(&mut *s.borrow_mut()));
        scratch.clear();
        f(&mut scratch);
        self.push_bytes(&scratch);
        SCRATCH.with(|s| *s.borrow_mut() = scratch);
    }
}

thread_local! {
    /// Reused formatting buffer for [`VeltStr::push_with`] (numbers, JSON pieces).
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Capacity for a buffer that must hold `need` bytes: amortized doubling from `cap`, at least 32.
fn grown(cap: usize, need: usize) -> usize {
    need.max(cap.saturating_mul(2)).max(32)
}

/// Write a string result to an out-parameter.
///
/// # Safety
/// `out` must be writable.
#[inline]
pub(crate) unsafe fn write_out(out: *mut VeltStr, s: VeltStr) {
    out.write(s);
}

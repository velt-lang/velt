//! [`StrBuf`]: a Velt `string` built in place, in the runtime's memory: what a library writes
//! becomes the string without a copy (native_abi.md "Strings").
//!
//! A Velt string knows its UTF-16 length and whether it is ASCII, so making one always reads its
//! text once; a `String` result also copies it (the bytes are in the library's allocator). A
//! `StrBuf` writes into the runtime's buffer directly: a write is a bounds check and a copy of
//! the written bytes into place, with no call into the runtime; the buffer grows (amortized
//! doubling) only when full, and [`StrBuf::finish`] counts the text once. Up to 23 bytes stay in
//! the builder until it finishes, so a short string is an inline one (no allocation), as Velt
//! makes them.

use std::mem::ManuallyDrop;

use crate::value::Str;
use crate::{api, VeltStr};

/// Text a [`StrBuf`] keeps before it moves to the runtime's memory (Velt's inline strings).
const SMALL: usize = 23;

/// A Velt `string` under construction (see the module docs). Write with [`push_str`]
/// (`Self::push_str`), `write!` ([`std::fmt::Write`]), or [`write_utf8`](Self::write_utf8).
pub struct StrBuf {
    /// The runtime's builder once the text outgrew [`SMALL`]: its committed text is empty and
    /// everything written is in its spare room, committed by `finish`.
    rt: VeltStr,
    /// The spare room of `rt` (null while the text is small).
    base: *mut u8,
    cap: usize,
    /// Bytes written (at `base`, or in `small`).
    len: usize,
    small: [u8; SMALL],
}

// SAFETY: owns its buffer (a unique runtime string) like a `String`.
unsafe impl Send for StrBuf {}
// SAFETY: `&self` only reads.
unsafe impl Sync for StrBuf {}

impl Default for StrBuf {
    fn default() -> StrBuf {
        StrBuf::new()
    }
}

impl StrBuf {
    /// An empty builder (no allocation until it outgrows an inline string).
    pub fn new() -> StrBuf {
        StrBuf {
            rt: VeltStr::empty(),
            base: std::ptr::null_mut(),
            cap: 0,
            len: 0,
            small: [0; SMALL],
        }
    }

    /// An empty builder with room for `n` bytes.
    pub fn with_capacity(n: usize) -> StrBuf {
        let mut b = StrBuf::new();
        b.reserve(n);
        b
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing was written.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Room for `additional` more bytes without growing again.
    pub fn reserve(&mut self, additional: usize) {
        let end = self.len + additional;
        if self.base.is_null() {
            if end > SMALL {
                self.grow(end);
            }
        } else if end > self.cap {
            self.grow(end.max(self.cap * 2));
        }
    }

    /// Room for at least `need` bytes in the runtime's buffer (moving the small text there).
    #[cold]
    fn grow(&mut self, need: usize) {
        let mut avail = 0u64;
        // SAFETY: `rt` is this builder's own string; its committed text is empty, so the spare
        // room starts at the text this builder wrote (kept by the runtime when it grows).
        let p = unsafe { (api().strbuf_spare)(&mut self.rt, need as u64, &mut avail) };
        if self.base.is_null() && self.len > 0 {
            // SAFETY: room for `need >= len` bytes.
            unsafe { std::ptr::copy_nonoverlapping(self.small.as_ptr(), p, self.len) };
        }
        self.base = p;
        self.cap = avail as usize;
    }

    /// Appends `bytes`, which must be UTF-8 made of whole characters.
    #[inline]
    fn put(&mut self, bytes: &[u8]) {
        let end = self.len + bytes.len();
        if self.base.is_null() {
            if end <= SMALL {
                self.small[self.len..end].copy_from_slice(bytes);
                self.len = end;
                return;
            }
            self.grow(end.max(64));
        } else if end > self.cap {
            self.grow(end.max(self.cap * 2));
        }
        // SAFETY: room for `end` bytes at `base`.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(self.len), bytes.len())
        };
        self.len = end;
    }

    /// Appends one byte (no copy call: punctuation and digits are written a byte at a time).
    #[inline]
    fn put_byte(&mut self, b: u8) {
        if self.base.is_null() {
            if self.len < SMALL {
                self.small[self.len] = b;
                self.len += 1;
                return;
            }
            self.grow(64);
        } else if self.len == self.cap {
            self.grow(self.cap * 2);
        }
        // SAFETY: room for `len + 1` bytes at `base`.
        unsafe { self.base.add(self.len).write(b) };
        self.len += 1;
    }

    /// Appends `s`.
    #[inline]
    pub fn push_str(&mut self, s: &str) {
        self.put(s.as_bytes());
    }

    /// Appends `c`.
    #[inline]
    pub fn push(&mut self, c: char) {
        if c.is_ascii() {
            self.put_byte(c as u8);
        } else {
            self.put(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }

    /// Appends `bytes` that are known to be UTF-8 (encoders that write UTF-8 a piece at a time:
    /// JSON, numbers).
    ///
    /// # Safety
    /// The text written as a whole, when [`finish`](Self::finish) is called, must be UTF-8.
    #[inline]
    pub unsafe fn write_utf8(&mut self, bytes: &[u8]) {
        self.put(bytes);
    }

    /// Appends the byte `b` (as [`write_utf8`](Self::write_utf8) with one byte).
    ///
    /// # Safety
    /// As for [`write_utf8`](Self::write_utf8).
    #[inline]
    pub unsafe fn write_byte(&mut self, b: u8) {
        self.put_byte(b);
    }

    /// The finished string (no copy: the runtime's buffer becomes it; a short one is inline).
    pub fn finish(self) -> Str {
        let mut me = ManuallyDrop::new(self);
        if me.base.is_null() {
            let mut out = VeltStr::empty();
            // SAFETY: UTF-8 (whole characters were written).
            unsafe { (api().str_from_utf8)(me.small.as_ptr(), me.len, &mut out) };
            return Str::from_raw(out);
        }
        // SAFETY: the bytes written at the spare room become the text (UTF-8, whole characters).
        unsafe { (api().strbuf_commit)(&mut me.rt, me.len as u64, 0) };
        Str::from_raw(std::mem::replace(&mut me.rt, VeltStr::empty()))
    }
}

impl Drop for StrBuf {
    fn drop(&mut self) {
        // Unfinished: the runtime's buffer (if any) is released; uncommitted bytes are not text.
        // SAFETY: owned, dropped once.
        unsafe { (api().str_drop)(&mut self.rt) };
    }
}

impl std::fmt::Write for StrBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.push_str(s);
        Ok(())
    }
}

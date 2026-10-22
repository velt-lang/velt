//! Text and JSON as Velt writes them, shared by the runtime (`velt_rt`) and the native libraries
//! of std's drivers (built on the `velt_native` SDK, std/*/native): Velt's string encoding
//! ([`wtf8`]), JavaScript's number formatting ([`fmt`]), JSON scanning and escaping, and the
//! database drivers' parameter and row JSON ([`db_json`]). One copy, so a driver in a native
//! library formats numbers and rows exactly as the runtime does.

pub mod db_json;
pub mod fmt;
pub mod json_escape;
pub mod json_scan;
pub mod wtf8;

/// Where the encoders write: a `Vec<u8>`, or (feature `sdk`) a native library's
/// `velt_native::StrBuf`, which is the Velt string itself, so encoded text is never copied.
pub trait Out {
    /// Appends `bytes`.
    fn extend(&mut self, bytes: &[u8]);
    /// Appends `b`.
    #[inline]
    fn push(&mut self, b: u8) {
        self.extend(&[b]);
    }
    /// Appends `n` copies of `b`.
    #[inline]
    fn fill(&mut self, b: u8, n: usize) {
        for _ in 0..n {
            self.push(b);
        }
    }
    /// A hint: about `additional` more bytes follow.
    #[inline]
    fn reserve(&mut self, _additional: usize) {}
}

impl Out for Vec<u8> {
    #[inline]
    fn extend(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
    #[inline]
    fn push(&mut self, b: u8) {
        Vec::push(self, b);
    }
    #[inline]
    fn fill(&mut self, b: u8, n: usize) {
        self.resize(self.len() + n, b);
    }
    #[inline]
    fn reserve(&mut self, additional: usize) {
        Vec::reserve(self, additional);
    }
}

/// The encoders write UTF-8 text, which is what a `StrBuf` must finish with.
#[cfg(feature = "sdk")]
impl Out for velt_native::StrBuf {
    #[inline]
    fn extend(&mut self, bytes: &[u8]) {
        // SAFETY: every encoder here writes UTF-8 text as a whole.
        unsafe { self.write_utf8(bytes) }
    }
    #[inline]
    fn push(&mut self, b: u8) {
        // SAFETY: as `extend`.
        unsafe { self.write_byte(b) }
    }
    #[inline]
    fn reserve(&mut self, additional: usize) {
        velt_native::StrBuf::reserve(self, additional);
    }
}

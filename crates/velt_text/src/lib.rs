//! Text and JSON as Velt writes them, for the runtime (`velt_rt`): Velt's string encoding
//! ([`wtf8`]), JavaScript's number formatting ([`fmt`]), JSON scanning and escaping, the
//! database drivers' parameter and row JSON ([`db_json`]).

pub mod db_json;
pub mod fmt;
pub mod json_escape;
pub mod json_scan;
pub mod wtf8;

/// Where the encoders write.
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

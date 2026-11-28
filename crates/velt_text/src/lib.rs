//! Text as Velt writes it, for the runtime (`velt_rt`) and the compiler: Velt's string encoding
//! ([`wtf8`]) and JavaScript's number formatting ([`fmt`]).

pub mod fmt;
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

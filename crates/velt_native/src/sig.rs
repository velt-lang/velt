//! Signature-record helpers used by [`crate::export`]: a type's name in a signature is a [`Buf`]
//! built at compile time, so generic types compose theirs (`T[]`, `IoResult<T>`, a struct's
//! `{name:T,...}`). The record is the NUL-terminated concatenation of the parts.

/// The longest name of one type in a signature.
pub const CAP: usize = 1024;

/// A type's name in a signature, built in constant evaluation.
#[derive(Clone, Copy)]
pub struct Buf {
    bytes: [u8; CAP],
    len: usize,
}

impl Buf {
    /// `s`.
    pub const fn lit(s: &str) -> Buf {
        Buf::EMPTY.push(s)
    }

    /// Nothing.
    pub const EMPTY: Buf = Buf {
        bytes: [0; CAP],
        len: 0,
    };

    /// `self` followed by `s`.
    pub const fn push(mut self, s: &str) -> Buf {
        let b = s.as_bytes();
        if self.len + b.len() > CAP {
            panic!("a type's name in a native signature is too long");
        }
        let mut i = 0;
        while i < b.len() {
            self.bytes[self.len + i] = b[i];
            i += 1;
        }
        self.len += b.len();
        self
    }

    /// `self` followed by `other`.
    pub const fn cat(mut self, other: Buf) -> Buf {
        if self.len + other.len > CAP {
            panic!("a type's name in a native signature is too long");
        }
        let mut i = 0;
        while i < other.len {
            self.bytes[self.len + i] = other.bytes[i];
            i += 1;
        }
        self.len += other.len;
        self
    }

    /// The bytes.
    pub const fn as_bytes(&self) -> &[u8] {
        self.bytes.split_at(self.len).0
    }
}

/// Total length of the NUL-terminated concatenation of `parts`.
pub const fn len(parts: &[Buf]) -> usize {
    let mut n = 1;
    let mut i = 0;
    while i < parts.len() {
        n += parts[i].len;
        i += 1;
    }
    n
}

/// The NUL-terminated concatenation of `parts` (`N` = [`len`]).
pub const fn build<const N: usize>(parts: &[Buf]) -> [u8; N] {
    let mut out = [0u8; N];
    let (mut i, mut k) = (0, 0);
    while i < parts.len() {
        let mut j = 0;
        while j < parts[i].len {
            out[k] = parts[i].bytes[j];
            k += 1;
            j += 1;
        }
        i += 1;
    }
    out
}

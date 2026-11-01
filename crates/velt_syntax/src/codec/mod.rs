//! A compact binary form of a parsed [`ast::Module`], for caches that keep parsed modules across
//! builds (the loader's cache of std's parsed files): [`encode_module`] and [`decode_module`].
//!
//! - The encoding is the tree in field order: integers as LEB128 varints, strings as their
//!   length and bytes, an enum as a tag byte and its fields, `Option` as a byte, `Vec` as its
//!   length and elements. Decoding allocates the nodes and copies the strings, with no lexing,
//!   no lookahead and no speculation, so it is much cheaper than parsing the source again.
//! - A [`Span`] is stored without its file: the module is decoded for the file id its source
//!   has in the decoding session's source map, so spans (and the diagnostics, debug info and
//!   editor positions made from them) come out exactly as a parse there would give them. A span
//!   of another file (none in a parsed module so far) keeps its file id, and [`Span::DUMMY`]
//!   has a tag of its own, so it stays `DUMMY` whatever file the module is decoded for. (A
//!   module parsed as file 0 cannot tell `DUMMY` from its own empty span at 0: encode such a
//!   module parsed under another id.)
//! - Every type's encoding is written out field by field ([`nodes`]): a field or variant added
//!   to the AST fails to compile until it is encoded too, so the form cannot fall behind it.
//! - [`FORMAT`] starts every encoding; a value of another format, or one that is cut short or
//!   damaged, decodes to `None` (never a panic), and the caller parses the source instead. A
//!   cache keys its entries by the compiler that wrote them as well, since the parser decides
//!   what a source parses to.
//! - Integrity is the store's job, not the encoding's: damaged bytes can still decode to a
//!   well-formed module (a flipped bit in a string or a span), so a cache must check its entries
//!   (the build cache's CRC-32 does) before they reach [`decode_module`].
//! - Decoding returns values, not `Result`s (each layer of `Result` around a large node costs a
//!   copy of it): the first problem marks the [`Reader`] failed and empties it, so everything
//!   after decodes as zeros (the first variant, empty lists) and the whole value is dropped.

mod nodes;
#[cfg(test)]
mod tests;

use velt_common::{FileId, Span};

use crate::ast;

/// The version of the encoding: the first bytes of every encoded module.
pub const FORMAT: &[u8; 8] = b"vast\x00\x00\x00\x02";

/// How deeply decoded nodes may nest: far more than a parse produces (the parser stops at 256
/// syntactic levels, each a handful of nodes), so only a damaged value reaches it.
const MAX_DEPTH: u32 = 1 << 14;

/// `module`, parsed from file `file`, in the compact form.
pub fn encode_module(module: &ast::Module, file: FileId) -> Vec<u8> {
    let mut w = Writer {
        out: Vec::with_capacity(4096),
        file,
    };
    w.out.extend_from_slice(FORMAT);
    module.enc(&mut w);
    w.out
}

/// The module [`encode_module`] encoded, with its spans in file `file`; `None` when `bytes` is
/// not a whole encoding of this [`FORMAT`].
pub fn decode_module(bytes: &[u8], file: FileId) -> Option<ast::Module> {
    let rest = bytes.strip_prefix(FORMAT.as_slice())?;
    crate::on_parser_thread(|| {
        let mut r = Reader {
            bytes: rest,
            pos: 0,
            file,
            depth: 0,
            failed: false,
        };
        let module = ast::Module::dec(&mut r);
        (!r.failed && r.pos == r.bytes.len()).then_some(module)
    })
}

struct Writer {
    out: Vec<u8>,
    /// The file the module was parsed from: its spans are stored without it.
    file: FileId,
}

impl Writer {
    fn byte(&mut self, b: u8) {
        self.out.push(b);
    }

    fn uint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.out.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.out.push(v as u8);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    file: FileId,
    depth: u32,
    /// The value is damaged: everything read from here on is zeros.
    failed: bool,
}

impl Reader<'_> {
    /// Mark the value damaged; reads return zeros from now on.
    #[cold]
    fn fail(&mut self) {
        self.failed = true;
        self.pos = self.bytes.len();
    }

    #[inline(always)]
    fn byte(&mut self) -> u8 {
        match self.bytes.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                b
            }
            None => {
                self.fail();
                0
            }
        }
    }

    /// A varint of at most 64 bits.
    #[inline(always)]
    fn uint(&mut self) -> u64 {
        let b = self.byte();
        if b < 0x80 {
            return u64::from(b);
        }
        self.uint_long(b)
    }

    fn uint_long(&mut self, first: u8) -> u64 {
        let mut v = u64::from(first & 0x7f);
        let mut shift = 7;
        loop {
            let b = self.byte();
            if shift == 63 && b > 1 {
                self.fail();
                return 0;
            }
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }

    #[inline(always)]
    fn u32(&mut self) -> u32 {
        let v = self.uint();
        u32::try_from(v).unwrap_or_else(|_| {
            self.fail();
            0
        })
    }

    /// A count of elements or bytes: never more than the bytes left (every element takes at
    /// least one), so a damaged length reserves no memory.
    #[inline(always)]
    fn len(&mut self) -> usize {
        let n = self.uint();
        if n > (self.bytes.len() - self.pos) as u64 {
            self.fail();
            return 0;
        }
        n as usize
    }

    fn take(&mut self, n: usize) -> &[u8] {
        if n > self.bytes.len() - self.pos {
            self.fail();
            return &[];
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        s
    }

    /// One level deeper into the tree (undone by [`Reader::leave`]). Every cycle of AST types
    /// goes through a `Box` or a `Vec`, the only callers.
    #[inline(always)]
    fn enter(&mut self) {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.fail();
        }
    }

    #[inline(always)]
    fn leave(&mut self) {
        self.depth -= 1;
    }
}

/// A type with a compact encoding.
trait Codec: Sized {
    fn enc(&self, w: &mut Writer);
    /// The value, or (once the reader failed) any value.
    fn dec(r: &mut Reader) -> Self;
}

impl Codec for bool {
    fn enc(&self, w: &mut Writer) {
        w.byte(u8::from(*self));
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        match r.byte() {
            0 => false,
            1 => true,
            _ => {
                r.fail();
                false
            }
        }
    }
}

impl Codec for u32 {
    fn enc(&self, w: &mut Writer) {
        w.uint(u64::from(*self));
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        r.u32()
    }
}

/// Two varints: the low and the high 64 bits.
impl Codec for u128 {
    fn enc(&self, w: &mut Writer) {
        w.uint(*self as u64);
        w.uint((*self >> 64) as u64);
    }
    fn dec(r: &mut Reader) -> Self {
        let lo = r.uint();
        u128::from(lo) | (u128::from(r.uint()) << 64)
    }
}

impl Codec for f64 {
    fn enc(&self, w: &mut Writer) {
        w.out.extend_from_slice(&self.to_bits().to_le_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        let mut b = [0u8; 8];
        let s = r.take(8);
        if s.len() == 8 {
            b.copy_from_slice(s);
        }
        f64::from_bits(u64::from_le_bytes(b))
    }
}

impl Codec for String {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        w.out.extend_from_slice(self.as_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        let n = r.len();
        match std::str::from_utf8(r.take(n)) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                r.fail();
                String::new()
            }
        }
    }
}

impl<T: Codec> Codec for Option<T> {
    fn enc(&self, w: &mut Writer) {
        match self {
            None => w.byte(0),
            Some(v) => {
                w.byte(1);
                v.enc(w);
            }
        }
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        match r.byte() {
            0 => None,
            1 => Some(T::dec(r)),
            _ => {
                r.fail();
                None
            }
        }
    }
}

impl<T: Codec> Codec for Vec<T> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for v in self {
            v.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        let n = r.len();
        if n == 0 {
            return Vec::new();
        }
        r.enter();
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(T::dec(r));
        }
        r.leave();
        out
    }
}

impl<T: Codec> Codec for Box<T> {
    fn enc(&self, w: &mut Writer) {
        (**self).enc(w);
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let v = Box::new(T::dec(r));
        r.leave();
        v
    }
}

impl<A: Codec, B: Codec> Codec for (A, B) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        (a, B::dec(r))
    }
}

/// How a span is stored: the low two bits of its first varint.
const SPAN_OWN: u64 = 0;
const SPAN_FOREIGN: u64 = 1;
const SPAN_DUMMY: u64 = 2;

/// [`Span::DUMMY`]: the tag alone. Otherwise `lo << 2 | tag`, then `hi - lo` (wrapping), then
/// the file for a span of another file.
impl Codec for Span {
    fn enc(&self, w: &mut Writer) {
        if *self == Span::DUMMY {
            w.uint(SPAN_DUMMY);
            return;
        }
        let tag = if self.file == w.file {
            SPAN_OWN
        } else {
            SPAN_FOREIGN
        };
        w.uint((u64::from(self.lo) << 2) | tag);
        w.uint(u64::from(self.hi.wrapping_sub(self.lo)));
        if tag == SPAN_FOREIGN {
            w.uint(u64::from(self.file.0));
        }
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        let first = r.uint();
        let tag = first & 3;
        if first >> 34 != 0 || tag == 3 || (tag == SPAN_DUMMY && first != SPAN_DUMMY) {
            r.fail();
        }
        if tag == SPAN_DUMMY {
            return Span::DUMMY;
        }
        let lo = (first >> 2) as u32;
        let hi = lo.wrapping_add(r.u32());
        let file = if tag == SPAN_FOREIGN {
            FileId(r.u32())
        } else {
            r.file
        };
        Span { file, lo, hi }
    }
}

impl Codec for ast::NodeId {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
    }
    #[inline(always)]
    fn dec(r: &mut Reader) -> Self {
        ast::NodeId(r.u32())
    }
}

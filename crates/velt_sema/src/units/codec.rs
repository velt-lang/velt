//! A compact binary form of a finished unit (#2943 M2, `docs/internals/design/build-architecture.md`):
//! std's frozen checking context, saved to disk so that a later process checks a program
//! against it without checking std again.
//!
//! - The encoding follows the AST's (`velt_syntax::codec`): integers as LEB128 varints, strings
//!   as their length and bytes, an enum as a tag byte and its fields, `Option` as a byte, `Vec`
//!   as its length and elements. Every type's encoding is written out field by field (the
//!   `structs!` / `enums!` macros destructure each value), so a field added to a saved type fails
//!   to compile until it is encoded too.
//! - A [`Span`] is stored with its file as an index into the unit's file table: a unit refers to
//!   every std file, whose ids depend on the order a session's source map added them. Decoding
//!   maps each index to the session's id for that file ([`Reader::files`]).
//! - [`DefId`], [`TyId`] and [`LocalId`] are stored as they are: a unit is restored whole, and a
//!   later unit's definitions and types are added after its own.
//! - A damaged or cut value decodes to zeros and the whole decode to `None` (never a panic); a
//!   store must check its entries' integrity (the build cache's checksums do).

#![allow(dead_code)] // Until the unit is saved (#2943 M2).

#[cfg(test)]
mod tests;

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{FileId, Span};

use crate::hir::{DefId, LocalId, TyId};

/// How deeply decoded values may nest: far more than a checked program's (an expression nests
/// as its source does, and the parser stops at 256 levels), so only a damaged value reaches it.
const MAX_DEPTH: u32 = 1 << 14;

pub(crate) struct Writer {
    out: Vec<u8>,
    /// The unit's file table: each file's index, in the order first met.
    files: FxHashMap<FileId, u32>,
    file_list: Vec<FileId>,
}

impl Writer {
    pub(crate) fn new() -> Writer {
        Writer {
            out: Vec::with_capacity(1 << 16),
            files: FxHashMap::default(),
            file_list: vec![],
        }
    }

    /// The bytes written, and the file table their spans index (the files in index order).
    pub(crate) fn finish(self) -> (Vec<u8>, Vec<FileId>) {
        (self.out, self.file_list)
    }

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

    fn file(&mut self, f: FileId) {
        let next = self.file_list.len() as u32;
        let i = *self.files.entry(f).or_insert(next);
        if i == next {
            self.file_list.push(f);
        }
        self.uint(u64::from(i));
    }
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// The session's id of each file of the unit's table.
    files: &'a [FileId],
    depth: u32,
    /// The value is damaged: everything read from here on is zeros.
    failed: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8], files: &'a [FileId]) -> Reader<'a> {
        Reader {
            bytes,
            pos: 0,
            files,
            depth: 0,
            failed: false,
        }
    }

    /// Whether everything was read, undamaged.
    pub(crate) fn done(&self) -> bool {
        !self.failed && self.pos == self.bytes.len()
    }

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

    fn file(&mut self) -> FileId {
        let i = self.uint();
        match self.files.get(i as usize) {
            Some(&f) => f,
            None => {
                self.fail();
                FileId(0)
            }
        }
    }

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
pub(crate) trait Codec: Sized {
    fn enc(&self, w: &mut Writer);
    /// The value, or (once the reader failed) any value.
    fn dec(r: &mut Reader) -> Self;
}

impl Codec for bool {
    fn enc(&self, w: &mut Writer) {
        w.byte(u8::from(*self));
    }
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

impl Codec for u8 {
    fn enc(&self, w: &mut Writer) {
        w.byte(*self);
    }
    fn dec(r: &mut Reader) -> Self {
        r.byte()
    }
}

impl Codec for u32 {
    fn enc(&self, w: &mut Writer) {
        w.uint(u64::from(*self));
    }
    fn dec(r: &mut Reader) -> Self {
        r.u32()
    }
}

impl Codec for u64 {
    fn enc(&self, w: &mut Writer) {
        w.uint(*self);
    }
    fn dec(r: &mut Reader) -> Self {
        r.uint()
    }
}

impl Codec for usize {
    fn enc(&self, w: &mut Writer) {
        w.uint(*self as u64);
    }
    fn dec(r: &mut Reader) -> Self {
        let v = r.uint();
        usize::try_from(v).unwrap_or_else(|_| {
            r.fail();
            0
        })
    }
}

impl Codec for i64 {
    fn enc(&self, w: &mut Writer) {
        // Zigzag: small negative numbers stay short.
        w.uint(((*self << 1) ^ (*self >> 63)) as u64);
    }
    fn dec(r: &mut Reader) -> Self {
        let v = r.uint();
        ((v >> 1) as i64) ^ -((v & 1) as i64)
    }
}

impl Codec for f64 {
    fn enc(&self, w: &mut Writer) {
        w.out.extend_from_slice(&self.to_bits().to_le_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        let b = r.take(8);
        let mut a = [0u8; 8];
        if b.len() == 8 {
            a.copy_from_slice(b);
        }
        f64::from_bits(u64::from_le_bytes(a))
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
            Ok(s) => s.to_string(),
            Err(_) => {
                r.fail();
                String::new()
            }
        }
    }
}

impl Codec for std::rc::Rc<str> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        w.out.extend_from_slice(self.as_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        String::dec(r).into()
    }
}

impl Codec for Span {
    fn enc(&self, w: &mut Writer) {
        if *self == Span::DUMMY {
            w.byte(0);
            return;
        }
        w.byte(1);
        w.file(self.file);
        w.uint(u64::from(self.lo));
        w.uint(u64::from(self.hi.saturating_sub(self.lo)));
    }
    fn dec(r: &mut Reader) -> Self {
        match r.byte() {
            0 => Span::DUMMY,
            1 => {
                let file = r.file();
                let lo = r.u32();
                let len = r.u32();
                Span::new(file, lo, lo.saturating_add(len))
            }
            _ => {
                r.fail();
                Span::DUMMY
            }
        }
    }
}

/// Ids of a unit's own tables, stored as they are.
macro_rules! ids {
    ($($t:ident),*) => {$(
        impl Codec for $t {
            fn enc(&self, w: &mut Writer) {
                w.uint(u64::from(self.0));
            }
            fn dec(r: &mut Reader) -> Self {
                $t(r.u32())
            }
        }
    )*};
}
ids!(DefId, TyId, LocalId);

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

impl<T: Codec> Codec for Box<T> {
    fn enc(&self, w: &mut Writer) {
        (**self).enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let v = Box::new(T::dec(r));
        r.leave();
        v
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
        r.enter();
        let n = r.len();
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(T::dec(r));
        }
        r.leave();
        out
    }
}

impl<A: Codec, B: Codec> Codec for (A, B) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        (a, B::dec(r))
    }
}

impl<A: Codec, B: Codec, C: Codec> Codec for (A, B, C) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
        self.2.enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        let b = B::dec(r);
        (a, b, C::dec(r))
    }
}

/// Maps and sets are stored sorted by key, so that the same unit always encodes to the same
/// bytes (a hash map's order changes from run to run).
impl<K: Codec + Ord + std::hash::Hash + Eq + Clone, V: Codec> Codec for FxHashMap<K, V> {
    fn enc(&self, w: &mut Writer) {
        let mut v: Vec<(&K, &V)> = self.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        w.uint(v.len() as u64);
        for (k, val) in v {
            k.enc(w);
            val.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = FxHashMap::with_capacity_and_hasher(n, Default::default());
        for _ in 0..n {
            let k = K::dec(r);
            out.insert(k, V::dec(r));
        }
        r.leave();
        out
    }
}

impl<K: Codec + Ord + std::hash::Hash + Eq + Clone> Codec for FxHashSet<K> {
    fn enc(&self, w: &mut Writer) {
        let mut v: Vec<&K> = self.iter().collect();
        v.sort();
        w.uint(v.len() as u64);
        for k in v {
            k.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = FxHashSet::with_capacity_and_hasher(n, Default::default());
        for _ in 0..n {
            out.insert(K::dec(r));
        }
        r.leave();
        out
    }
}

/// `Codec` for structs: `Name { field, … }`, in declaration order.
#[allow(unused_macros)]
macro_rules! structs {
    ($($ty:ident { $($f:ident),* $(,)? })*) => {$(
        impl $crate::units::codec::Codec for $ty {
            fn enc(&self, w: &mut $crate::units::codec::Writer) {
                let $ty { $($f),* } = self;
                $($crate::units::codec::Codec::enc($f, w);)*
            }
            fn dec(r: &mut $crate::units::codec::Reader) -> Self {
                // Struct expression fields are evaluated in the order written.
                $ty { $($f: $crate::units::codec::Codec::dec(r)),* }
            }
        }
    )*};
}

/// `Codec` for enums: `Name { tag => Unit, tag => Tuple(a, b), tag => Fields { x, y } }`. Tag 0
/// is a variant whose fields decode from zeros without recursing (a damaged value reads on as
/// zeros).
#[allow(unused_macros)]
macro_rules! enums {
    ($($ty:ident {
        $($tag:literal => $v:ident $(( $($t:ident),* ))? $({ $($f:ident),* })?),* $(,)?
    })*) => {$(
        impl $crate::units::codec::Codec for $ty {
            fn enc(&self, w: &mut $crate::units::codec::Writer) {
                match self {
                    $($ty::$v $(( $($t),* ))? $({ $($f),* })? => {
                        $crate::units::codec::Codec::enc(&($tag as u8), w);
                        $($($crate::units::codec::Codec::enc($t, w);)*)?
                        $($($crate::units::codec::Codec::enc($f, w);)*)?
                    })*
                }
            }
            fn dec(r: &mut $crate::units::codec::Reader) -> Self {
                let tag: u8 = $crate::units::codec::Codec::dec(r);
                match tag {
                    $($tag => $ty::$v
                        $(( $({ let $t = $crate::units::codec::Codec::dec(r); $t }),* ))?
                        $({ $($f: $crate::units::codec::Codec::dec(r)),* })?,)*
                    _ => {
                        r.fail_damaged();
                        Self::dec(r)
                    }
                }
            }
        }
    )*};
}

#[allow(unused_imports)]
pub(crate) use {enums, structs};

impl Reader<'_> {
    /// Mark the value damaged (the macros' unknown tag).
    pub(crate) fn fail_damaged(&mut self) {
        self.fail();
    }
}

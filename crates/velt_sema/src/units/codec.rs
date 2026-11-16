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
//! - The AST a unit's definition records borrow (`'m`) is stored as fragments
//!   ([`velt_syntax::encode_fragment`]), each once, and decoded into an [`Arena`] the caller
//!   keeps ([`ast_refs`]).

#![allow(dead_code)] // Until the unit is saved (#2943 M2).

mod ast_refs;
mod defs;
#[cfg(test)]
mod defs_tests;
mod hir;
#[cfg(test)]
mod hir_tests;
#[cfg(test)]
pub(crate) mod hook;
mod tables;
#[cfg(test)]
mod tests;
mod unit;
#[cfg(test)]
mod unit_tests;

use std::rc::Rc;

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
    /// The AST nodes written so far, by address and type: each is written once, into
    /// [`Writer::frags`], and referred to by its number ([`ast_refs`]).
    nodes: FxHashMap<(usize, std::any::TypeId), u32>,
    /// The AST fragments: each one's file (an index into the file table), length and bytes.
    frags: Vec<u8>,
    /// Where each fragment starts in `frags`.
    frag_starts: Vec<u32>,
    /// Write files as their ids, not through the table (a map key's sort key).
    raw_files: bool,
    /// How many of the bytes written are AST fragments ([`ast_refs`]).
    pub(crate) ast_bytes: usize,
}

impl Writer {
    pub(crate) fn new() -> Writer {
        Writer {
            out: Vec::with_capacity(1 << 16),
            files: FxHashMap::default(),
            file_list: vec![],
            nodes: FxHashMap::default(),
            frags: vec![],
            frag_starts: vec![],
            raw_files: false,
            ast_bytes: 0,
        }
    }

    /// The bytes written, after the table of AST fragments they refer to ([`Frags`]), and the
    /// file table their spans index (the files in index order).
    pub(crate) fn finish(self) -> (Vec<u8>, Vec<FileId>) {
        let mut out = Vec::with_capacity(8 + 4 * self.frag_starts.len());
        push_uint(&mut out, self.frag_starts.len() as u64);
        for &s in &self.frag_starts {
            push_uint(&mut out, u64::from(s));
        }
        push_uint(&mut out, self.frags.len() as u64);
        out.reserve(self.frags.len() + self.out.len());
        out.extend_from_slice(&self.frags);
        out.extend_from_slice(&self.out);
        (out, self.file_list)
    }

    fn byte(&mut self, b: u8) {
        self.out.push(b);
    }

    fn uint(&mut self, v: u64) {
        push_uint(&mut self.out, v);
    }

    fn file(&mut self, f: FileId) {
        if self.raw_files {
            self.uint(u64::from(f.0));
            return;
        }
        let i = self.file_index(f);
        self.uint(u64::from(i));
    }

    /// `f`'s index in the file table (added when new).
    fn file_index(&mut self, f: FileId) -> u32 {
        let next = self.file_list.len() as u32;
        let i = *self.files.entry(f).or_insert(next);
        if i == next {
            self.file_list.push(f);
        }
        i
    }
}

/// `v` as a LEB128 varint at the end of `out`.
fn push_uint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// The session's id of each file of the unit's table.
    files: &'a [FileId],
    depth: u32,
    /// The value is damaged: everything read from here on is zeros.
    failed: bool,
    /// The AST fragments the values refer to, decoded into their arena on first use
    /// (`None`: a value that borrows AST fails to decode).
    frags: Option<Rc<Frags>>,
    /// Where tables decoded on demand read their entries from (`DefTable`'s codec; `None`:
    /// tables are decoded whole).
    pub(crate) src: Option<Rc<Src>>,
}

/// The AST fragments of an encoding ([`Writer::finish`]), each decoded into an [`Arena`] when a
/// value first refers to it.
pub(crate) struct Frags {
    bytes: Rc<Vec<u8>>,
    /// Where the fragments' bytes start in `bytes`.
    base: usize,
    starts: Vec<u32>,
    files: Rc<Vec<FileId>>,
    /// Where decoded nodes live (`Arena`'s contract: it outlives everything decoded).
    arena: Option<*const Arena>,
    /// Each fragment decoded so far, with its type.
    nodes: std::cell::RefCell<Vec<Option<(std::any::TypeId, *const ())>>>,
}

impl Frags {
    /// The fragment table at `pos` in `bytes`, and where what follows it starts; `None` when
    /// it is damaged.
    fn read(
        bytes: &Rc<Vec<u8>>,
        pos: usize,
        files: Rc<Vec<FileId>>,
        arena: Option<*const Arena>,
    ) -> Option<(Frags, usize)> {
        let mut r = Reader::bare(&bytes[..], &[]);
        r.pos = pos;
        let n = r.len();
        let starts: Vec<u32> = (0..n).map(|_| r.u32()).collect();
        let size = r.len();
        let base = r.pos;
        r.take(size);
        if r.failed {
            return None;
        }
        let frags = Frags {
            bytes: bytes.clone(),
            base,
            nodes: std::cell::RefCell::new(vec![None; starts.len()]),
            starts,
            files,
            arena,
        };
        Some((frags, r.pos))
    }

    /// Fragment `i` as a `T`, decoded into the arena if it wasn't; `None` when there is no such
    /// fragment, it is of another type or doesn't decode, or there is no arena.
    pub(crate) fn node<T: AstNode>(&self, i: usize) -> Option<*const T> {
        let id = std::any::TypeId::of::<T>();
        if let Some(&Some((t, p))) = self.nodes.borrow().get(i) {
            return (t == id).then_some(p.cast());
        }
        let arena = self.arena?;
        let start = self.base + *self.starts.get(i)? as usize;
        let mut r = Reader::bare(&self.bytes[..], &self.files);
        r.pos = start;
        let file = r.file();
        let n = r.len();
        let mut bytes = Vec::with_capacity(velt_syntax::AST_FORMAT.len() + n);
        bytes.extend_from_slice(velt_syntax::AST_FORMAT);
        bytes.extend_from_slice(r.take(n));
        if r.failed {
            return None;
        }
        // On sema's thread, whose stack is sema's (`SEMA_STACK_BYTES`): the fragment decodes
        // right here, not on a parser thread of its own.
        let node = velt_syntax::with_stack_of(crate::SEMA_STACK_BYTES, || {
            velt_syntax::decode_fragment::<T>(&bytes, file)
        })?;
        // SAFETY: the arena outlives everything decoded with it (`Arena`'s contract).
        let p = unsafe { (*arena).keep(node) };
        self.nodes.borrow_mut()[i] = Some((id, p.cast()));
        Some(p)
    }
}

/// What a unit's tables decoded on demand read from: the whole encoding, its file table and
/// fragments (`DefTable`'s codec).
pub(crate) struct Src {
    pub(crate) bytes: Rc<Vec<u8>>,
    pub(crate) files: Rc<Vec<FileId>>,
    pub(crate) frags: Rc<Frags>,
}

impl Src {
    /// A reader of `bytes[range]` (an entry of a table), with the encoding's files and fragments.
    pub(crate) fn reader(&self, range: std::ops::Range<usize>) -> Reader<'_> {
        Reader {
            frags: Some(self.frags.clone()),
            ..Reader::bare(&self.bytes[range], &self.files)
        }
    }
}

impl<'a> Reader<'a> {
    /// A reader of `bytes` as they are (no fragment table in front).
    pub(crate) fn bare(bytes: &'a [u8], files: &'a [FileId]) -> Reader<'a> {
        Reader {
            bytes,
            pos: 0,
            files,
            depth: 0,
            failed: false,
            frags: None,
            src: None,
        }
    }

    /// A reader of what [`Writer::finish`] wrote: its values may not borrow AST nodes.
    pub(crate) fn new(bytes: &'a [u8], files: &'a [FileId]) -> Reader<'a> {
        let mut r = Reader::bare(bytes, files);
        let n = r.len();
        (0..n).for_each(|_| {
            r.uint();
        });
        let size = r.len();
        r.take(size);
        r
    }

    /// A reader whose values may borrow AST nodes, decoded into `arena`.
    ///
    /// # Safety
    ///
    /// The AST references in the values it decodes are given any lifetime (`'m`) and point into
    /// `arena`: every such value (a decoded `Ctx<'m>`, a `DefInfo<'m>`, ...) must be dropped
    /// before `arena` is ([`Arena`]).
    pub(crate) unsafe fn with_arena(
        bytes: &'a [u8],
        files: &'a [FileId],
        arena: &'a Arena,
    ) -> Reader<'a> {
        let owned = Rc::new(bytes.to_vec());
        let files_rc = Rc::new(files.to_vec());
        match Frags::read(&owned, 0, files_rc, Some(arena as *const Arena)) {
            Some((frags, after)) => {
                let mut r = Reader::bare(bytes, files);
                r.pos = after;
                r.frags = Some(Rc::new(frags));
                r
            }
            None => {
                let mut r = Reader::bare(bytes, files);
                r.fail();
                r
            }
        }
    }

    /// A reader of a unit's encoding `bytes` from `pos` (where [`Writer::finish`]'s output
    /// starts), whose tables are decoded on demand.
    ///
    /// # Safety
    ///
    /// As [`Reader::with_arena`]'s, for every value decoded from it, then or later.
    pub(crate) unsafe fn over_unit(
        bytes: &'a Rc<Vec<u8>>,
        pos: usize,
        files: &'a Rc<Vec<FileId>>,
        arena: &'a Arena,
    ) -> Reader<'a> {
        let mut r = Reader::bare(&bytes[..], files);
        match Frags::read(bytes, pos, files.clone(), Some(arena as *const Arena)) {
            Some((frags, after)) => {
                let frags = Rc::new(frags);
                r.pos = after;
                r.src = Some(Rc::new(Src {
                    bytes: bytes.clone(),
                    files: files.clone(),
                    frags: frags.clone(),
                }));
                r.frags = Some(frags);
            }
            None => r.fail(),
        }
        r
    }

    /// The fragment table, for [`ast_refs`].
    fn frags(&self) -> Option<&Frags> {
        self.frags.as_deref()
    }

    /// Where the reader is.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Whether it failed.
    pub(crate) fn failed(&self) -> bool {
        self.failed
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

    /// A varint that is an offset, not checked against the bytes left.
    pub(crate) fn len_unchecked(&mut self) -> usize {
        usize::try_from(self.uint()).unwrap_or_else(|_| {
            self.fail();
            0
        })
    }

    pub(crate) fn take(&mut self, n: usize) -> &[u8] {
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

/// 128-bit integers (`hir::Lit::Int`, literal types' and C constants' values) as LEB128 too:
/// almost all are small.
impl Codec for u128 {
    fn enc(&self, w: &mut Writer) {
        let mut v = *self;
        while v >= 0x80 {
            w.out.push(v as u8 | 0x80);
            v >>= 7;
        }
        w.out.push(v as u8);
    }
    fn dec(r: &mut Reader) -> Self {
        let mut v = 0u128;
        let mut shift = 0;
        loop {
            let b = r.byte();
            if shift == 126 && b > 3 {
                r.fail();
                return 0;
            }
            v |= u128::from(b & 0x7f) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }
}

impl Codec for i128 {
    fn enc(&self, w: &mut Writer) {
        (((*self << 1) ^ (*self >> 127)) as u128).enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let v = u128::dec(r);
        ((v >> 1) as i128) ^ -((v & 1) as i128)
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

/// A file (a map key: `throws::FnSites::by_file`), through the unit's file table.
impl Codec for FileId {
    fn enc(&self, w: &mut Writer) {
        w.file(*self);
    }
    fn dec(r: &mut Reader) -> Self {
        r.file()
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

/// Byte arrays (`hir::Intrinsic::SimdShuffle`'s lanes) as their bytes.
impl<const N: usize> Codec for [u8; N] {
    fn enc(&self, w: &mut Writer) {
        w.out.extend_from_slice(self);
    }
    fn dec(r: &mut Reader) -> Self {
        let mut a = [0u8; N];
        let b = r.take(N);
        if b.len() == N {
            a.copy_from_slice(b);
        }
        a
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

/// A cell's contents (the caches of `ownership::local_async::targets::Literals`): no identity
/// is shared through one.
impl<T: Codec> Codec for std::cell::RefCell<T> {
    fn enc(&self, w: &mut Writer) {
        self.borrow().enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        std::cell::RefCell::new(T::dec(r))
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
/// bytes (a hash map's order changes from run to run): by the bytes each key encodes to, with its
/// spans' files as their ids (keys need no `Ord`: spans have none).
fn sorted_by_key<'v, K: Codec, V>(entries: impl Iterator<Item = (&'v K, V)>) -> Vec<(&'v K, V)> {
    let mut entries: Vec<(&K, V)> = entries.collect();
    let mut w = Writer {
        out: vec![],
        files: FxHashMap::default(),
        file_list: vec![],
        nodes: FxHashMap::default(),
        frags: vec![],
        frag_starts: vec![],
        raw_files: true,
        ast_bytes: 0,
    };
    let mut at = vec![0];
    for (k, _) in &entries {
        k.enc(&mut w);
        at.push(w.out.len());
    }
    let key = |i: usize| &w.out[at[i]..at[i + 1]];
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&i, &j| key(i).cmp(key(j)));
    let mut rank = vec![0; entries.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let mut ranked: Vec<(usize, (&K, V))> = rank.into_iter().zip(entries.drain(..)).collect();
    ranked.sort_unstable_by_key(|e| e.0);
    ranked.into_iter().map(|(_, e)| e).collect()
}

impl<K: Codec + std::hash::Hash + Eq, V: Codec> Codec for FxHashMap<K, V> {
    fn enc(&self, w: &mut Writer) {
        let v = sorted_by_key(self.iter());
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

impl<K: Codec + std::hash::Hash + Eq> Codec for FxHashSet<K> {
    fn enc(&self, w: &mut Writer) {
        let v = sorted_by_key(self.iter().map(|k| (k, ())));
        w.uint(v.len() as u64);
        for (k, ()) in v {
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

impl<K: Codec + Ord, V: Codec> Codec for std::collections::BTreeMap<K, V> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for (k, v) in self {
            k.enc(w);
            v.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = std::collections::BTreeMap::new();
        for _ in 0..n {
            let k = K::dec(r);
            out.insert(k, V::dec(r));
        }
        r.leave();
        out
    }
}

impl<K: Codec + Ord> Codec for std::collections::BTreeSet<K> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for k in self {
            k.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = std::collections::BTreeSet::new();
        for _ in 0..n {
            out.insert(K::dec(r));
        }
        r.leave();
        out
    }
}

impl<A: Codec, B: Codec, C: Codec, D: Codec> Codec for (A, B, C, D) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
        self.2.enc(w);
        self.3.enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        let b = B::dec(r);
        let c = C::dec(r);
        (a, b, c, D::dec(r))
    }
}

/// `Codec` for structs: `Name { field, … }`, in declaration order (`Name<'_> { … }` for one
/// that borrows AST).
#[allow(unused_macros)]
macro_rules! structs {
    ($($ty:ident $(<$lt:lifetime>)? { $($f:ident),* $(,)? })*) => {$(
        impl $crate::units::codec::Codec for $ty $(<$lt>)? {
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

/// `Codec` for tuple structs: `Name(a, b)`, the fields named in order.
#[allow(unused_macros)]
macro_rules! tuples {
    ($($ty:ident $(<$lt:lifetime>)? ( $($f:ident),* $(,)? ))*) => {$(
        impl $crate::units::codec::Codec for $ty $(<$lt>)? {
            fn enc(&self, w: &mut $crate::units::codec::Writer) {
                let $ty ( $($f),* ) = self;
                $($crate::units::codec::Codec::enc($f, w);)*
            }
            fn dec(r: &mut $crate::units::codec::Reader) -> Self {
                $ty ( $({ let $f = $crate::units::codec::Codec::dec(r); $f }),* )
            }
        }
    )*};
}

/// `Codec` for enums: `Name { tag => Unit, tag => Tuple(a, b), tag => Fields { x, y } }`. Tag 0
/// is a variant whose fields decode from zeros without recursing (a damaged value reads on as
/// zeros).
#[allow(unused_macros)]
macro_rules! enums {
    ($($ty:ident $(<$lt:lifetime>)? {
        $($tag:literal => $v:ident $(( $($t:ident),* ))? $({ $($f:ident),* })?),* $(,)?
    })*) => {$(
        impl $crate::units::codec::Codec for $ty $(<$lt>)? {
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
pub(crate) use {enums, structs, tuples};

pub(crate) use ast_refs::{Arena, AstNode};
#[allow(unused_imports)] // Until the unit is saved (#2943 M2).
pub(crate) use unit::{decode_unit, encode_unit, encode_unit_sized, unit_module_paths, FORMAT};

impl Reader<'_> {
    /// Mark the value damaged (the macros' unknown tag).
    pub(crate) fn fail_damaged(&mut self) {
        self.fail();
    }
}

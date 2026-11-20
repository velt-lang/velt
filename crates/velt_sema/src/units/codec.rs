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
//!
//! [`Span`]: velt_common::Span
//! [`DefId`]: crate::hir::DefId
//! [`TyId`]: crate::hir::TyId
//! [`LocalId`]: crate::hir::LocalId

#![allow(dead_code)] // Until the unit is saved (#2943 M2).

pub(crate) mod ast_refs;
mod def_table;
mod defs;
#[cfg(test)]
mod defs_tests;
mod hir;
#[cfg(test)]
mod hir_tests;
#[cfg(test)]
pub(crate) mod hook;
#[cfg(test)]
mod lazy_tests;
mod tables;
#[cfg(test)]
mod tests;
mod values;

use std::rc::Rc;

use rustc_hash::FxHashMap;
use velt_common::FileId;

/// How deeply decoded values may nest (boxes and containers), whatever stack there is: far more
/// than a checked program's. A chain of a thousand operands (`a + b + ...`) nests a thousand
/// levels, so the stack is what stops a decode first ([`with_decode_stack`]).
const MAX_DEPTH: u32 = 1 << 14;

/// The stack a decode may use when its thread has not said how much there is
/// ([`with_decode_stack`]): what is left of an ordinary thread's.
const DECODE_STACK: usize = 1 << 20;

thread_local! {
    /// The address this thread's decoders stop at ([`with_decode_stack`]; 0: not said).
    static STACK_FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Runs `f` with `bytes` of stack, from here down, for the decoders this thread starts in it:
/// a value nested deeper than that holds is a failed decode, not a stack overflow. The check
/// of a program says so for its thread (sema's stack is large); without, a reader allows
/// itself [`DECODE_STACK`] from where it is made.
pub(crate) fn with_decode_stack<T>(bytes: usize, f: impl FnOnce() -> T) -> T {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            STACK_FLOOR.set(self.0);
        }
    }
    let floor = crate::stack_address().saturating_sub(bytes);
    let _restore = Restore(STACK_FLOOR.replace(floor));
    f()
}

/// Where a reader made here stops ([`with_decode_stack`]).
fn stack_floor() -> usize {
    match STACK_FLOOR.get() {
        0 => crate::stack_address().saturating_sub(DECODE_STACK),
        floor => floor,
    }
}

/// How many bytes of capacity a reader's containers may reserve before their elements are
/// read, per byte of input (and a little for a short input): past it they grow as they fill.
const RESERVE_PER_BYTE: usize = 16;

/// The first bytes of every encoding ([`Writer::finish`]): what it is, and the version of this
/// encoding (raised with every change to how a value is written). After them comes the format of
/// the AST fragments inside (`velt_syntax::AST_FORMAT`). A reader fails on any other header:
/// bytes written by another version of either encoding are not read as this one's.
const MAGIC: &[u8; 7] = b"velt-u\x01";

/// [`MAGIC`] and the AST format, as an encoding starts.
fn header() -> Vec<u8> {
    let mut h = MAGIC.to_vec();
    h.extend_from_slice(velt_syntax::AST_FORMAT);
    h
}

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
        let mut out = header();
        out.reserve(8 + 4 * self.frag_starts.len());
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
    /// The stack address nesting may not pass ([`with_decode_stack`]).
    stack_floor: usize,
    /// The bytes of capacity containers may still reserve up front ([`Reader::capacity`]).
    reserve_left: usize,
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
        r.header();
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
            velt_syntax::decode_fragment::<T>(&bytes, file, &self.files)
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
    /// The stack address the unit's readers stop at: the decode stack the unit was opened with
    /// ([`with_decode_stack`]), so that an entry decoded later, wherever the check reads it,
    /// has that stack too (a `Src` stays on its thread: it holds `Rc`s).
    stack_floor: usize,
    /// An entry decoded on demand did not decode: the unit is damaged ([`Src::damaged`]).
    damaged: std::cell::Cell<bool>,
}

impl Src {
    /// Did an entry decoded on demand fail to decode? Its value is then the decoder's stand-in,
    /// and the check that read it must be thrown away as one of a damaged unit (a cache miss).
    pub(crate) fn damaged(&self) -> bool {
        self.damaged.get()
    }

    /// Mark the unit damaged ([`Src::damaged`]).
    pub(crate) fn mark_damaged(&self) {
        self.damaged.set(true);
    }

    /// A reader of `bytes[range]` (an entry of a table), with the encoding's files and
    /// fragments; a failed one when the range is not inside the bytes (a damaged offset).
    pub(crate) fn reader(&self, range: std::ops::Range<usize>) -> Reader<'_> {
        let (bytes, inside) = match self.bytes.get(range) {
            Some(b) => (b, true),
            None => (&self.bytes[..0], false),
        };
        let mut r = Reader {
            frags: Some(self.frags.clone()),
            stack_floor: self.stack_floor,
            ..Reader::bare(bytes, &self.files)
        };
        if !inside {
            r.fail();
        }
        r
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
            stack_floor: stack_floor(),
            reserve_left: bytes.len().saturating_mul(RESERVE_PER_BYTE) + 4096,
            failed: false,
            frags: None,
            src: None,
        }
    }

    /// The capacity a container of `n` elements of `T` reserves before reading them: `n`
    /// while the reader's allowance lasts, then 0 (it grows as it fills). A length is only
    /// known to be at most the bytes left, and nested containers each state one, so
    /// reserving every stated length would take memory growing with the square of a damaged
    /// input; the allowance keeps it in proportion to the input.
    pub(crate) fn capacity<T>(&mut self, n: usize) -> usize {
        let bytes = n.saturating_mul(std::mem::size_of::<T>().max(1));
        if bytes > self.reserve_left {
            return 0;
        }
        self.reserve_left -= bytes;
        n
    }

    /// A reader of what [`Writer::finish`] wrote: its values may not borrow AST nodes.
    pub(crate) fn new(bytes: &'a [u8], files: &'a [FileId]) -> Reader<'a> {
        let mut r = Reader::bare(bytes, files);
        r.header();
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
                    stack_floor: r.stack_floor,
                    damaged: std::cell::Cell::new(false),
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

    /// Reads the header [`Writer::finish`] writes; any other bytes fail the reader.
    fn header(&mut self) {
        let want = header();
        if self.take(want.len()) != want.as_slice() {
            self.fail();
        }
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
        if self.depth > MAX_DEPTH || crate::stack_address() < self.stack_floor {
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

/// `Codec` for enums: `Name { tag => Unit, tag => Tuple(a, b), tag => Fields { x, y } }`.
///
/// A damaged value reads on as zeros, so every enum in it decodes as its tag 0 from then on:
/// tag 0 is a variant without fields, where that ends (the macro refuses a list that has
/// another). An enum with no such variant is listed `@fielded Name { .. }`: its tag-0 variant's
/// fields must then end from zeros themselves, which is so unless one holds the enum again in a
/// box (lists, maps and options are empty from zeros). [`ends_from_zeros`] checks one in a test.
#[allow(unused_macros)]
macro_rules! enums {
    ($($(@$fielded:ident)? $ty:ident $(<$lt:lifetime>)? {
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
                // An unknown tag decodes as tag 0, which a failed reader reads from then on:
                // that must end.
                const _: () = assert!(
                    $crate::units::codec::tag_zero_ends(
                        &[$((
                            $tag,
                            0usize
                                $(+ 1 $(+ $crate::units::codec::no_more!($t))*)?
                                $(+ 1 $(+ $crate::units::codec::no_more!($f))*)?,
                        )),*],
                        false $(|| $crate::units::codec::is_fielded!($fielded))?,
                    ),
                    "an encoded enum needs a variant without fields at tag 0 (or `@fielded`)"
                );
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

/// Does an encoded enum end from zeros (`enums!` checks it when the enum's encoding is built):
/// is there a tag 0 among `variants` (each tag with whether the variant has fields), and has it
/// no fields, unless the enum is listed `@fielded`?
pub(crate) const fn tag_zero_ends(variants: &[(u8, usize)], fielded: bool) -> bool {
    let mut i = 0;
    while i < variants.len() {
        if variants[i].0 == 0 {
            return fielded || variants[i].1 == 0;
        }
        i += 1;
    }
    false
}

/// Nothing to add for one more field (`enums!` counts whether a variant has any).
#[allow(unused_macros)]
macro_rules! no_more {
    ($field:ident) => {
        0usize
    };
}

/// The `@fielded` mark of `enums!`, and no other word.
#[allow(unused_macros)]
macro_rules! is_fielded {
    (fielded) => {
        true
    };
}

/// Does a `T` decode from a failed reader, which reads zeros, without nesting on and on: the
/// property `enums!` cannot see for an enum listed `@fielded`. Run on a thread of its own, with
/// a small stack: a type that does not end aborts the test there.
#[cfg(test)]
pub(crate) fn ends_from_zeros<T: Codec>() -> bool {
    let decode = || {
        let mut r = Reader::bare(&[], &[]);
        r.fail();
        let v = T::dec(&mut r);
        // Taken apart by the process, not on this stack.
        std::mem::forget(v);
    };
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(decode)
        .is_ok_and(|t| t.join().is_ok())
}

#[allow(unused_imports)]
pub(crate) use {enums, is_fielded, no_more, structs, tuples};

#[allow(unused_imports)] // Until the unit is saved (#2943 M2).
pub(crate) use ast_refs::{Arena, AstNode};

impl Reader<'_> {
    /// Mark the value damaged (the macros' unknown tag).
    pub(crate) fn fail_damaged(&mut self) {
        self.fail();
    }
}

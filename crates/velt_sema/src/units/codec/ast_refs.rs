//! The AST a unit's definition records borrow (#2943 M2): `FnSource`, `FnInfo::extern_sig`, the
//! `decl` of types, interfaces, enums and aliases, and a constant's annotation, initializer and C
//! library. Each node is stored once as a fragment ([`velt_syntax::encode_fragment`]) with its
//! spans in its own file (an index into the unit's file table, like a [`velt_common::Span`]'s),
//! and every later reference to the same node (the same address and type: a static method's
//! copies share their method's declaration) as its number. A function's body is stored empty: no
//! later unit's stage reads a finished unit's bodies from source (their checked HIR is kept), so
//! a unit keeps only what its declarations' signatures need ([`AstNode::kept`]).
//!
//! Decoded nodes live in an [`Arena`] the caller owns, under the contract on [`Arena`].

use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::cell::RefCell;

use velt_common::{FileId, Span};
use velt_syntax::ast;

use super::{Codec, Reader, Writer};

/// Owns the AST nodes that values decoded with it borrow.
///
/// The lifetime contract (the same erasure as `crate::kept`'s): a decoded
/// value's AST references (`FnSource`, `AdtInfo::decl`, `AliasInfo::decl`, ...) have whatever
/// lifetime `'m` the caller's context has, but point into this arena's boxes. A box's contents stay
/// at their heap address while the arena grows (only the `Vec` of boxes moves), and nothing is
/// removed before the arena is dropped, so they stay valid as long as the arena lives. The caller,
/// who made the reader with the `unsafe` [`Reader::with_arena`], keeps the arena alive longer
/// than every value decoded with it (for a decoded `Ctx`, longer than the context and every copy
/// of it).
#[derive(Default)]
pub(crate) struct Arena {
    nodes: RefCell<Vec<Box<dyn Any>>>,
}

impl Arena {
    /// How many nodes it holds.
    pub(crate) fn len(&self) -> usize {
        self.nodes.borrow().len()
    }

    /// Keep `v` here.
    pub(crate) fn alloc<T: 'static>(&self, v: T) -> &T {
        // SAFETY: the box stays, at its address, as long as the arena (above).
        unsafe { &*self.keep(v) }
    }

    pub(super) fn keep<T: 'static>(&self, v: T) -> *const T {
        let b = Box::new(v);
        let p: *const T = &*b;
        self.nodes.borrow_mut().push(b);
        p
    }
}

/// An AST node a definition record may borrow.
pub(crate) trait AstNode: velt_syntax::Fragment + Clone + Send + 'static {
    /// The file its spans are in.
    fn file(&self) -> FileId;
    /// What a unit keeps of it: without its functions' bodies.
    fn kept(&self) -> Cow<'_, Self> {
        Cow::Borrowed(self)
    }
    /// A node of this type, for a damaged value (which is dropped whole).
    fn placeholder() -> Self;
}

impl<T: AstNode> Codec for &T {
    fn enc(&self, w: &mut Writer) {
        let key = (*self as *const T as usize, TypeId::of::<T>());
        if let Some(&i) = w.nodes.get(&key) {
            w.uint(u64::from(i));
            return;
        }
        let next = w.nodes.len() as u32;
        w.nodes.insert(key, next);
        w.uint(u64::from(next));
        // The fragment itself goes to the table ([`super::Frags`]).
        let file = self.file();
        let index = w.file_index(file);
        let bytes = velt_syntax::encode_fragment(&*self.kept(), file, &mut |f| w.file_index(f));
        let body = &bytes[velt_syntax::AST_FORMAT.len()..];
        w.frag_starts.push(w.frags.len() as u32);
        super::push_uint(&mut w.frags, u64::from(index));
        super::push_uint(&mut w.frags, body.len() as u64);
        w.frags.extend_from_slice(body);
        w.ast_bytes += body.len();
    }

    fn dec(r: &mut Reader) -> Self {
        let i = r.uint() as usize;
        let p = r.frags().and_then(|f| f.node::<T>(i));
        match p {
            // SAFETY: a node of the reader's arena (`Arena`'s contract, which
            // `Reader::with_arena`'s caller keeps).
            Some(p) => unsafe { &*p },
            None => {
                r.fail();
                placeholder(r)
            }
        }
    }
}

/// A placeholder node of type `T` for a damaged value (which is dropped whole): kept in the
/// reader's arena, with the nodes it decoded. A reader without an arena has nowhere to keep
/// it, and leaks one per failed reference.
fn placeholder<'x, T: AstNode>(r: &Reader) -> &'x T {
    match r.frags().and_then(|f| f.arena) {
        // SAFETY: a node of the reader's arena (`Arena`'s contract, which
        // `Reader::with_arena`'s caller keeps).
        Some(arena) => unsafe { &*(*arena).keep(T::placeholder()) },
        None => Box::leak(Box::new(T::placeholder())),
    }
}

thread_local! {
    /// The function declarations (by address) whose bodies an encoding keeps whole
    /// ([`keep_bodies`]).
    static KEEP_BODIES: RefCell<rustc_hash::FxHashSet<usize>> = RefCell::default();
}

/// Keep the bodies of the declarations at `addresses` in what is encoded until the next call.
pub(crate) fn keep_bodies(addresses: rustc_hash::FxHashSet<usize>) {
    KEEP_BODIES.with(|k| *k.borrow_mut() = addresses);
}

/// A function's body as a unit keeps it: empty, at its place.
pub(super) fn no_body(b: &ast::Block) -> ast::Block {
    ast::Block {
        stmts: vec![],
        span: b.span,
    }
}

fn kept_fn(d: &ast::FnDecl) -> ast::FnDecl {
    ast::FnDecl {
        sig: d.sig.clone(),
        body: no_body(&d.body),
        overloads: d.overloads.clone(),
    }
}

/// The items of a small module, to take placeholders from.
fn sample() -> Vec<ast::ItemKind> {
    const SRC: &str = "function f(): T {}\nclass C {}\ninterface I {}\nenum E {}\ntype A = T;\n\
                       const x = 0;\n";
    let (m, _) = velt_syntax::parse_file(FileId(0), SRC);
    m.items.into_iter().map(|i| i.kind).collect()
}

macro_rules! sampled {
    ($($pat:pat => $out:expr),* $(,)?) => {{
        let mut found = None;
        for item in sample() {
            match item {
                $($pat => found = Some($out),)*
                #[allow(unreachable_patterns)]
                _ => {}
            }
            if found.is_some() {
                break;
            }
        }
        found.expect("ICE: the placeholder sample parses")
    }};
}

impl AstNode for ast::FnDecl {
    fn file(&self) -> FileId {
        self.sig.name.span.file
    }
    fn kept(&self) -> Cow<'_, Self> {
        // A body the encoding was asked to keep ([`keep_bodies`]) stays whole.
        match KEEP_BODIES.with(|k| k.borrow().contains(&(self as *const _ as usize))) {
            true => Cow::Borrowed(self),
            false => Cow::Owned(kept_fn(self)),
        }
    }
    fn placeholder() -> Self {
        sampled!(ast::ItemKind::Function(d) => d)
    }
}

impl AstNode for ast::FnSig {
    fn file(&self) -> FileId {
        self.name.span.file
    }
    fn placeholder() -> Self {
        ast::FnDecl::placeholder().sig
    }
}

impl AstNode for ast::Block {
    fn file(&self) -> FileId {
        self.span.file
    }
    fn kept(&self) -> Cow<'_, Self> {
        Cow::Owned(no_body(self))
    }
    fn placeholder() -> Self {
        no_body(&ast::FnDecl::placeholder().body)
    }
}

impl AstNode for ast::TypeDecl {
    fn file(&self) -> FileId {
        self.name.span.file
    }
    fn kept(&self) -> Cow<'_, Self> {
        let mut d = self.clone();
        if let Some(c) = &mut d.constructor {
            c.body = no_body(&c.body);
        }
        for m in &mut d.methods {
            m.decl.body = no_body(&m.decl.body);
        }
        Cow::Owned(d)
    }
    fn placeholder() -> Self {
        sampled!(ast::ItemKind::Class(d) => d)
    }
}

impl AstNode for ast::InterfaceDecl {
    fn file(&self) -> FileId {
        self.name.span.file
    }
    fn kept(&self) -> Cow<'_, Self> {
        let mut d = self.clone();
        for m in &mut d.methods {
            if let Some(b) = &mut m.body {
                *b = no_body(b);
            }
        }
        Cow::Owned(d)
    }
    fn placeholder() -> Self {
        sampled!(ast::ItemKind::Interface(d) => d)
    }
}

impl AstNode for ast::EnumDecl {
    fn file(&self) -> FileId {
        self.name.span.file
    }
    fn placeholder() -> Self {
        sampled!(ast::ItemKind::Enum(d) => d)
    }
}

impl AstNode for ast::TypeAlias {
    fn file(&self) -> FileId {
        self.name.span.file
    }
    fn placeholder() -> Self {
        sampled!(ast::ItemKind::TypeAlias(d) => d)
    }
}

impl AstNode for ast::TypeExpr {
    fn file(&self) -> FileId {
        self.span.file
    }
    fn placeholder() -> Self {
        ast::TypeAlias::placeholder().ty
    }
}

impl AstNode for ast::Expr {
    fn file(&self) -> FileId {
        self.span.file
    }
    fn placeholder() -> Self {
        sampled!(ast::ItemKind::Var(ast::VarDecl { init: Some(e), .. }) => e)
    }
}

impl AstNode for ast::CLibrary {
    fn file(&self) -> FileId {
        self.span.file
    }
    fn placeholder() -> Self {
        ast::CLibrary {
            library: String::new(),
            errno: false,
            span: Span::DUMMY,
        }
    }
}

/// A type a context owns rather than borrows (a deferred form's, `type_level::inline::Form`): a
/// fragment of its own, whole.
impl Codec for ast::TypeExpr {
    fn enc(&self, w: &mut Writer) {
        let file = AstNode::file(self);
        w.file(file);
        let bytes = velt_syntax::encode_fragment(self, file, &mut |f| w.file_index(f));
        let body = &bytes[velt_syntax::AST_FORMAT.len()..];
        w.uint(body.len() as u64);
        w.out.extend_from_slice(body);
        w.ast_bytes += body.len();
    }

    fn dec(r: &mut Reader) -> Self {
        let file = r.file();
        let n = r.len();
        let mut bytes = Vec::with_capacity(velt_syntax::AST_FORMAT.len() + n);
        bytes.extend_from_slice(velt_syntax::AST_FORMAT);
        bytes.extend_from_slice(r.take(n));
        let node = (!r.failed)
            .then(|| velt_syntax::decode_fragment(&bytes, file, r.files))
            .flatten();
        node.unwrap_or_else(|| {
            r.fail();
            ast::TypeExpr::placeholder()
        })
    }
}

use ast::Ident;

super::structs! {
    Ident { name, span }
}

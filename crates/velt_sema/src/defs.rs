//! Sema-side information about every definition (signatures, members, source AST, analysis
//! state). The final `hir::Def`s are built from these at the end of `check`.

use std::collections::HashMap;

use velt_common::Span;
use velt_syntax::ast;

use crate::hir::{self, AdtKind, DefId, PassMode, TyId};

/// Generic parameters of a definition: `TyKind::Param(i)` is `names[i]`.
#[derive(Clone, Default)]
pub(crate) struct Generics {
    pub names: Vec<String>,
    /// Interface bounds per parameter (`T extends A & B`).
    pub bounds: Vec<Vec<Bound>>,
}

impl Generics {
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn push(&mut self, name: &str) {
        self.names.push(name.to_string());
        self.bounds.push(vec![]);
    }
}

/// `T extends Iface<args>`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bound {
    pub iface: DefId,
    pub args: Vec<TyId>,
}

#[derive(Clone)]
pub(crate) struct ParamSig {
    pub name: String,
    pub span: Span,
    pub ty: TyId,
    /// Current pass mode (non-Copy params start `Borrow` and may be inferred `BorrowMut` or
    /// `Owned`, see `crate::ownership`).
    pub mode: PassMode,
    /// Checked default value (inserted at call sites that omit the argument).
    pub default: Option<hir::Expr>,
}

/// The `this` parameter of a method.
#[derive(Clone)]
pub(crate) struct ThisSig {
    pub ty: TyId,
    pub mode: PassMode,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FnKind {
    Free,
    Method,
    Static,
    Ctor,
    /// Interface default method (`self_ty` is the implicit last generic param).
    IfaceDefault,
    Extern,
    Closure,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BodyState {
    Unchecked,
    InProgress,
    Done,
}

/// Where a function's body comes from.
#[derive(Clone, Copy)]
pub(crate) enum FnSource<'m> {
    Decl(&'m ast::FnDecl),
    Default(&'m ast::FnSig, &'m ast::Block),
}

/// Something inside a body that may throw (see `crate::throws`).
#[derive(Clone, Debug)]
pub(crate) enum ThrowSrc {
    /// `throw e`, or a call / `await` whose type says what it throws (a function value, a
    /// promise value).
    Direct(TyId, Span),
    /// A call of a function with these type args: its final `throws`, substituted.
    Call(DefId, Vec<TyId>, Span),
    /// A call of interface method `slot` of `iface<args>` (interface value or bounded generic).
    Slot {
        iface: DefId,
        slot: u32,
        args: Vec<TyId>,
        span: Span,
    },
    /// The field initializers of class `C<args>` run by a `new` or a constructor: what `C`'s
    /// own initializers throw (a base class's have a source of their own). Kept unexpanded
    /// until resolved ([`crate::throws`]), so classes whose initializers construct each other
    /// in a cycle get the whole cycle's errors whatever order they are checked in.
    Defaults(DefId, Vec<TyId>, Span),
}

impl ThrowSrc {
    pub fn span(&self) -> Span {
        match self {
            ThrowSrc::Direct(_, s)
            | ThrowSrc::Call(_, _, s)
            | ThrowSrc::Defaults(_, _, s)
            | ThrowSrc::Slot { span: s, .. } => *s,
        }
    }

    /// This source used at `span` (a field default evaluated by a `new` or a struct literal),
    /// with its types mapped by `f` (from the owner's generic context into the use).
    pub fn used_at(&self, span: Span, mut f: impl FnMut(TyId) -> TyId) -> ThrowSrc {
        match self {
            ThrowSrc::Direct(t, _) => ThrowSrc::Direct(f(*t), span),
            ThrowSrc::Call(d, args, _) => {
                ThrowSrc::Call(*d, args.iter().map(|&t| f(t)).collect(), span)
            }
            ThrowSrc::Defaults(d, args, _) => {
                ThrowSrc::Defaults(*d, args.iter().map(|&t| f(t)).collect(), span)
            }
            ThrowSrc::Slot {
                iface, slot, args, ..
            } => ThrowSrc::Slot {
                iface: *iface,
                slot: *slot,
                args: args.iter().map(|&t| f(t)).collect(),
                span,
            },
        }
    }
}

/// A written (or context-fixed) `throws` of a function: its bodies may throw only what `ty`
/// allows, and `ty` is the function's error type (ABI) even when the body throws less.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeclaredThrows {
    /// `None`: must not throw (a closure where a non-throwing function type is expected).
    pub ty: Option<TyId>,
    pub span: Span,
    /// A closure without a written or expected error type: `ty` is what its body was known to
    /// throw where it was created (its function type needs one error type right away).
    pub from_body: bool,
}

#[derive(Clone)]
pub(crate) struct FnInfo<'m> {
    /// Fully qualified name (see `hir::FnDef::name`).
    pub name: String,
    pub name_span: Span,
    pub span: Span,
    pub module: usize,
    pub kind: FnKind,
    pub generics: Generics,
    pub this: Option<ThisSig>,
    pub params: Vec<ParamSig>,
    pub ret: TyId,
    pub ret_span: Option<Span>,
    /// Pass modes are part of a dynamically dispatched ABI (vtable / interface / closure /
    /// extern): no ownership inference, moving out of params is an error.
    pub fixed_modes: bool,
    pub source: Option<FnSource<'m>>,
    pub state: BodyState,
    /// Uncaught throw sources of the body (filled by the body pass).
    pub throw_srcs: Vec<ThrowSrc>,
    /// `throws` clause (or, for closures, the function type they were created for).
    pub declared_throws: Option<DeclaredThrows>,
    /// Final thrown type (filled by the throws pass).
    pub throws: Option<TyId>,
    /// Per-local role in the checked body (for the ownership passes).
    pub local_kinds: Vec<crate::body::LocalKind>,
    /// Owner type def for methods / ctors / defaults (class, struct, interface).
    pub owner: Option<DefId>,
    /// `declare async function` (M3 rt futures); other async functions are rejected in M2.
    pub is_async: bool,
    /// `function*` / `*name()`: a generator. `ret` is the declared result (`Generator<T>`,
    /// `Iterator<T>` or `Iterable<T>`, its `E` moved into `declared_throws`); a call returns it
    /// with the generator's final error type as `E` (`generators.rs`).
    pub is_generator: bool,
    /// `async function*` / `async *name()`: an async generator (`is_generator` is set too, and
    /// `is_async` is not: a call creates an `AsyncGenerator<T>`, not a promise). Its body may
    /// `await`; HIR `FnDef::is_async` is set for it.
    pub is_async_gen: bool,
    /// Arguments (spans of their places) moved into async calls: if the place is used again
    /// (or cannot be moved from), the argument becomes a clone (`crate::ownership::soft`).
    pub soft_moves: Vec<Span>,
    /// `private` method: only usable inside the body of `owner`.
    pub is_private: bool,
    /// `get name()`: read as a property (`x.name`), never called with `()`.
    pub is_getter: bool,
    /// Closures: stored or returned (captures by move), as opposed to passed directly.
    pub escaping: bool,
    /// Closures: the function values passed as parameters are always heap closures, so the
    /// body may keep them (a share). Set for the executor of `new Promise`, whose `resolve`
    /// and `reject` the prelude creates (docs/reference/functions.md "Captures").
    pub keeps_fn_params: bool,
    /// Indices of params that are `Owned` only because the body reassigns them: a caller that
    /// uses the argument again passes a clone (`crate::ownership::mutation`).
    pub soft_params: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MethodRef {
    pub def: DefId,
    pub is_static: bool,
}

#[derive(Clone)]
pub(crate) struct FieldInfo {
    pub name: String,
    pub ty: TyId,
    pub span: Span,
    pub readonly: bool,
    pub optional: bool,
    /// Has an initializer (`= e`, or optional → `null`).
    pub has_default: bool,
    pub default: Option<hir::Expr>,
    /// What evaluating `default` may throw, in the owner's generic context: a `new` or a struct
    /// literal that uses the default throws it too.
    pub default_throws: Vec<ThrowSrc>,
    /// `private`: the declaring type (inherited copies keep the base class).
    pub private_to: Option<DefId>,
    /// Declared without a type from an integer literal (`count = 0;`): reads are JS numbers
    /// (`body::expr::numbers`).
    pub inferred_int: bool,
}

pub(crate) struct AdtInfo<'m> {
    pub name: String,
    pub qual_name: String,
    pub kind: AdtKind,
    pub module: usize,
    pub span: Span,
    pub generics: Generics,
    /// All fields; for classes the base class's fields come first.
    pub fields: Vec<FieldInfo>,
    pub own_fields_start: usize,
    pub base: Option<TyId>,
    pub methods: HashMap<String, MethodRef>,
    /// Constructor: own, or inherited from the nearest base class that has one.
    pub ctor: Option<DefId>,
    pub own_ctor: Option<DefId>,
    pub vtable: Vec<DefId>,
    pub vslots: HashMap<String, u32>,
    pub implements: Vec<Bound>,
    /// Declares a `[Symbol.dispose]()` drop hook (such types are never Copy).
    pub has_dispose: bool,
    /// `static readonly` fields: one `Def::Global` each.
    pub statics: HashMap<String, DefId>,
    pub decl: Option<&'m ast::TypeDecl>,
}

pub(crate) struct VariantInfo {
    pub name: String,
    pub payload: Vec<TyId>,
    pub discriminant: i64,
    /// String enum member: its value (see `hir::VariantDef::str_value`).
    pub str_value: Option<String>,
}

pub(crate) struct EnumInfo<'m> {
    pub name: String,
    pub qual_name: String,
    pub span: Span,
    pub generics: Generics,
    pub variants: Vec<VariantInfo>,
    /// Compiler-generated union type (`crate::unions`).
    pub is_union: bool,
    pub decl: Option<&'m ast::EnumDecl>,
}

/// Signature of an interface method (in terms of the interface's generics; the implementor
/// is `Param(generics.len())`).
#[derive(Clone)]
pub(crate) struct IfaceMethod {
    pub name: String,
    pub span: Span,
    pub params: Vec<ParamSig>,
    pub ret: TyId,
    /// A call may mutate the receiver: setters, else the join over the default and every
    /// implementation (inferred by `crate::ownership`).
    pub mut_this: bool,
    pub default: Option<DefId>,
    /// `get name()`: read as a property.
    pub is_getter: bool,
    /// The method's own type params (`m<U>(...)`): `Param(iface generics + 1 + k)`, after the
    /// implementor. Generic methods are dispatched statically only (`T extends I`).
    pub generics: Generics,
    /// `throws` clause: bounds what implementations may throw (else inferred from them).
    pub throws: Option<DeclaredThrows>,
}

pub(crate) struct IfaceInfo<'m> {
    pub name: String,
    pub qual_name: String,
    pub module: usize,
    pub span: Span,
    pub generics: Generics,
    /// Own fields followed by the inherited ones (`extends`, flattened).
    pub fields: Vec<FieldInfo>,
    /// Own methods followed by the inherited ones (`extends`, flattened); slot order.
    pub methods: Vec<IfaceMethod>,
    /// Direct `extends` interfaces (args in terms of this interface's generics).
    pub parents: Vec<Bound>,
    pub decl: Option<&'m ast::InterfaceDecl>,
}

pub(crate) struct GlobalInfo<'m> {
    pub name: String,
    pub qual_name: String,
    pub module: usize,
    pub span: Span,
    pub ty: TyId,
    pub init: Option<hir::Expr>,
    pub state: BodyState,
    pub src: GlobalSrc<'m>,
}

/// Source of a constant: a module-level `const`, or a `static readonly` field of a type.
pub(crate) struct GlobalSrc<'m> {
    pub ann: Option<&'m ast::TypeExpr>,
    pub init: Option<&'m ast::Expr>,
    pub span: Span,
    /// Declaring type of a static field (its body may read it even when `private`).
    pub owner: Option<DefId>,
    pub is_private: bool,
}

pub(crate) enum DefInfo<'m> {
    Fn(Box<FnInfo<'m>>),
    Adt(Box<AdtInfo<'m>>),
    Enum(Box<EnumInfo<'m>>),
    Iface(Box<IfaceInfo<'m>>),
    Global(Box<GlobalInfo<'m>>),
}

/// A type alias `type X<T> = ...` (expanded where it is used).
pub(crate) struct AliasInfo<'m> {
    pub module: usize,
    pub decl: &'m ast::TypeAlias,
    pub expanding: bool,
}

/// An `extend<G> Target { methods }` block.
pub(crate) struct Extension {
    pub generics: Generics,
    /// Target type pattern (uses `Param(0..generics)`).
    pub target: TyId,
    pub methods: HashMap<String, MethodRef>,
}

/// Method-table key of a class / interface / `extend` member: its name, or `set <name>` for a
/// setter (`set name(v)`), which may share its name with a getter. The key is also the last
/// segment of the setter's def name (`Box.set size`) and how diagnostics name it.
pub(crate) fn member_key(name: &str, is_setter: bool) -> String {
    if is_setter {
        format!("set {name}")
    } else {
        name.to_string()
    }
}

/// Is this method-table key a setter's (see [`member_key`])?
pub(crate) fn is_setter_key(key: &str) -> bool {
    key.starts_with("set ")
}

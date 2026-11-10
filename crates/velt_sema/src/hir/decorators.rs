//! Decorator uses (part of the HIR contract, hir_encodings.md "Decorators"): what sema recorded
//! for every compile-time decorator of a program, in `Program::decorators`. Nothing runs at run
//! time; lowering reads only `@inline` and `@noInline`, by their hook roles (`velt_vir`
//! `lower/decorators.rs`), and `@export("c_name")` through [`CExport`]s.

use velt_common::Span;

use super::{CTy, DefId};

/// What a decorator is written on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecoratorTarget {
    /// A function, method, struct, class or enum (a `static` field: its `Def::Global`; an abstract
    /// property, `abstract x: T;`: its getter).
    Def(DefId),
    /// Field `.1` of type `.0`, an index into `AdtDef::fields`.
    Field(DefId, u32),
    /// Written parameter `.1` of function `.0` (a method's `this` doesn't count).
    Param(DefId, u32),
}

/// One use of a `declare decorator function`.
#[derive(Clone, Debug, PartialEq)]
pub struct Decorator {
    /// The `declare decorator function` (a `Def::ExternFn` that nothing calls).
    pub def: DefId,
    /// One per parameter of the declaration, in order, defaults filled in, as the parameter's
    /// type has it (an enum member where a number or a string is expected: that number or string).
    pub args: Vec<ConstTree>,
    /// The `@…` as written: where a reader reports what it finds wrong with it.
    pub span: Span,
}

/// The value of a decorator argument, after the constant folder.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstTree {
    Null,
    Undefined,
    Bool(bool),
    Int(i128),
    /// `f64::to_bits` of the value.
    Float(u64),
    Str(String),
    Array(Vec<ConstTree>),
    /// An object literal's properties, as written.
    Object(Vec<(String, ConstTree)>),
    /// Variant `.1` of enum `.0`.
    EnumMember(DefId, u32),
}

/// A function C calls (`@export("c_name")`, #2203 step 5, docs/internals/design/c-ffi.md
/// "Exported functions"), in `Program::c_exports`: lowering builds a function with the C calling
/// convention and the unmangled symbol `name`, whose parameters and result cross as `params` and
/// `ret` say, that calls `def`.
#[derive(Clone, Debug, PartialEq)]
pub struct CExport {
    /// The decorated top-level function: not generic, async or a generator, throwing nothing.
    pub def: DefId,
    /// The symbol: a C identifier, unique among the program's exports.
    pub name: String,
    /// One per parameter of `def`, in order: integers, `f32`, `f64`, `bool` or a `Ptr`.
    pub params: Vec<CTy>,
    /// The result, `CTy::Void` for `void`.
    pub ret: CTy,
    /// The `@…` decorator.
    pub span: Span,
}

//! Declarations of nominal types (part of the `ast` contract): `struct` / `class` and their
//! members, `interface`, `extend` blocks, `enum` and type aliases.

use velt_common::Span;

use super::{Block, CallSig, Expr, FnDecl, FnSig, GenericParam, Ident, TypeExpr};

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub readonly: bool,
    /// `name?: T` — the field has type `T | null` and defaults to `null`.
    pub optional: bool,
    /// `private name: T` — only accessible inside the declaring type's body.
    pub is_private: bool,
    /// `static readonly name: T = e;` — a per-type constant, not an instance field.
    pub is_static: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub decl: FnDecl,
    pub is_static: bool,
    /// `private` keyword: only callable inside the declaring type's body.
    pub is_private: bool,
    /// `get name(): T { ... }` — read as a property (`x.name`), no parameters.
    pub is_getter: bool,
    /// `set name(v: T) { ... }` — runs on assignment `x.name = v`; one parameter, no return
    /// type; it always mutates `this`. May share its name with a getter.
    pub is_setter: bool,
    /// `override` keyword: redefines a base-class method.
    pub is_override: bool,
    /// `abstract name(): T;` in an `abstract class`: declared without a body, implemented by
    /// every concrete subclass. `decl.body` is a placeholder the parser writes (a `panic`
    /// call), which is never run: the class cannot be instantiated.
    pub is_abstract: bool,
}

/// Shared by `struct` and `class`.
#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    /// `extends Base<T>` — classes only, at most one.
    pub extends: Option<TypeExpr>,
    /// `implements A, B`
    pub implements: Vec<TypeExpr>,
    pub fields: Vec<Field>,
    pub constructor: Option<FnDecl>,
    /// `private constructor` / `protected constructor` (`Public` without a constructor).
    pub ctor_visibility: CtorVisibility,
    pub methods: Vec<Method>,
    /// `abstract class`: cannot be instantiated with `new`; may declare abstract methods.
    pub is_abstract: bool,
}

/// Who may call a class's constructor (TypeScript's rules, docs/reference/classes.md): anyone;
/// the class body and its subclasses (`protected`); only the class body, and the class cannot
/// be extended (`private`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CtorVisibility {
    #[default]
    Public,
    Protected,
    Private,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub extends: Vec<TypeExpr>,
    /// Fields every implementor must have (same name and type).
    pub fields: Vec<Field>,
    pub methods: Vec<InterfaceMethod>,
    /// Call and construct signatures, as for [`TypeExprKind::Callable`].
    pub call_sigs: Vec<CallSig>,
}

/// Interface method: required (`body: None`) or with a default implementation.
#[derive(Clone, Debug)]
pub struct InterfaceMethod {
    pub sig: FnSig,
    pub body: Option<Block>,
    /// `get name(): T` — read as a property, no parameters.
    pub is_getter: bool,
    /// `set name(v: T)` — called by `x.name = v`; one parameter; it always mutates `this`.
    pub is_setter: bool,
}

/// `extend<T> Target<T> [implements A, B] { methods }`
#[derive(Clone, Debug)]
pub struct ExtendDecl {
    pub generics: Vec<GenericParam>,
    pub target: TypeExpr,
    /// `implements A, B`: the interfaces the block's methods implement for the target.
    pub implements: Vec<TypeExpr>,
    pub methods: Vec<Method>,
}

/// `enum Color { Red, Green = 5 }` (numeric, TS-style) or `enum Dir { Up = "UP" }` (string enum).
/// Tagged unions are written as discriminated unions of object types instead.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: Ident,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: Ident,
    /// `= 5` / `= -1` / `= "UP"`.
    pub discriminant: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeAlias {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
}

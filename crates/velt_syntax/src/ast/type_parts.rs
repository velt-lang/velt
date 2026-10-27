//! Parts of type expressions (part of the `ast` contract): index signature keys, object type
//! fields and call signatures.

use velt_common::Span;

use super::{FnSig, Ident, TypeExpr};

/// A call signature `(x: T): R` / `<T>(x: T): T`, or with `construct` a construct signature
/// `new (x: T): C`, in an interface or object type. `sig.name` is empty (zero-width, at the
/// `(` or `<`); a missing return type is `sig.ret: None`.
#[derive(Clone, Debug)]
pub struct CallSig {
    pub sig: FnSig,
    pub construct: bool,
    /// From `new` (or the signature's start) to the end of its return type.
    pub span: Span,
}

/// The key of an index signature, `[name: ty]` (the name is documentation only).
#[derive(Clone, Debug)]
pub struct IndexKey {
    pub name: Ident,
    pub ty: TypeExpr,
}

/// `name: T` / `name?: T` / `readonly name: T` in an object type.
#[derive(Clone, Debug)]
pub struct ObjectTypeField {
    pub name: Ident,
    /// For an optional field, the written type plus `| null`.
    pub ty: TypeExpr,
    /// `name?: T` — parsed as `name: T | null`; the flag makes it an optional field of the object
    /// type (left out by `JSON.stringify` while `null`). An object literal may leave out any
    /// `T | null` field of an object type.
    pub optional: bool,
    /// `readonly name: T` — the field can't be assigned (docs/internals/design/shared-models.md).
    pub readonly: bool,
    pub span: Span,
    /// A field written as accessors (`get v(): T`, `set v(x: T)` or both): each as written, for
    /// tools that print the source (`velt fmt`). The field reads as the getter's type (else the
    /// setter's), `readonly` without a setter.
    pub accessors: Vec<TypeFieldAccessor>,
}

/// One accessor of an object type's field (`ObjectTypeField::accessors`).
#[derive(Clone, Debug)]
pub struct TypeFieldAccessor {
    /// `set v(x: T)`: the parameter's name; `None` for a getter.
    pub setter_param: Option<Ident>,
    /// The getter's result type or the setter's parameter type.
    pub ty: TypeExpr,
    pub span: Span,
}

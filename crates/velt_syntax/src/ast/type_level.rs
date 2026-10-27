//! The type-level forms of type expressions (part of the `ast` contract): conditional and mapped
//! types (issue #934).

use super::{Ident, TypeExpr};

/// `check extends extends_ ? then : else_`.
#[derive(Clone, Debug)]
pub struct ConditionalType {
    pub check: TypeExpr,
    pub extends: TypeExpr,
    pub then: TypeExpr,
    pub else_: TypeExpr,
}

/// `{ readonly [param in source as name]?: value }`, with `+`/`-` on either modifier.
#[derive(Clone, Debug)]
pub struct MappedType {
    pub param: Ident,
    pub source: TypeExpr,
    /// `as N`: the key remapping clause.
    pub name: Option<TypeExpr>,
    pub readonly: MappedModifier,
    pub optional: MappedModifier,
    pub value: TypeExpr,
}

/// A mapped type's `readonly` or `?` modifier: absent, `readonly`/`+readonly`, `-readonly`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappedModifier {
    /// Not written: a homomorphic mapped type (`[K in keyof T]`) keeps `T`'s flags.
    Keep,
    Add,
    Remove,
}

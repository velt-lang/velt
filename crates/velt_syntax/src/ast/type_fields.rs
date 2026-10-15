//! The members of object types: named fields and index signature keys.

use super::{Ident, TypeExpr};
use velt_common::Span;

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
}

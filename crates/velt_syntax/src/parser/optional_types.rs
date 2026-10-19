//! The written type of an optional parameter or field (`name?: T`): `T | undefined`, as in
//! TypeScript (issue #930). The parser adds a zero-width `undefined` at the end of the written
//! type; a written type that already includes `undefined` stays as it is.

use crate::ast::*;
use velt_common::Span;

/// Does the written type `ty` include `undefined` (`undefined`, `T | undefined`)?
pub(super) fn has_undefined(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeExprKind::Undefined => true,
        TypeExprKind::Union(ms) => ms.iter().any(has_undefined),
        _ => false,
    }
}

/// The type of an optional parameter written `p?: ty`: `ty | undefined` ([`or_undefined`]). A
/// call that leaves the argument out passes `undefined`; `p?: T | null` is
/// `T | null | undefined`.
pub(super) fn optional_param(ty: TypeExpr) -> TypeExpr {
    or_undefined(ty)
}

/// `T | undefined` for the written type of an optional parameter or field (`name?: T`), keeping
/// `T`'s span; the added `undefined` is zero-width, at the end of `T`. A type that already
/// admits `undefined` stays as it is.
pub(super) fn or_undefined(ty: TypeExpr) -> TypeExpr {
    if has_undefined(&ty) {
        return ty;
    }
    let undefined = TypeExpr {
        kind: TypeExprKind::Undefined,
        span: Span::new(ty.span.file, ty.span.hi, ty.span.hi),
    };
    let span = ty.span;
    match ty.kind {
        TypeExprKind::Union(mut ms) => {
            ms.push(undefined);
            TypeExpr {
                kind: TypeExprKind::Union(ms),
                span,
            }
        }
        _ => TypeExpr {
            kind: TypeExprKind::Union(vec![ty, undefined]),
            span,
        },
    }
}

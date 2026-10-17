//! The written type of an optional parameter or field (`name?: T`): `T | null`, or `T` itself
//! when it includes `undefined`.

use crate::ast::*;
use velt_common::Span;

/// Does the written type `ty` include `undefined` (`undefined`, `T | undefined`)? An optional
/// parameter of such a type (`p?: T | undefined`) keeps it, and a call that leaves the argument
/// out passes `undefined`.
pub(super) fn has_undefined(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeExprKind::Undefined => true,
        TypeExprKind::Union(ms) => ms.iter().any(has_undefined),
        _ => false,
    }
}

/// The type of an optional parameter written `p?: ty`: `ty | null` ([`or_null`]), or `ty`
/// itself when it includes `undefined` ([`has_undefined`]).
pub(super) fn optional_param(ty: TypeExpr) -> TypeExpr {
    match has_undefined(&ty) {
        true => ty,
        false => or_null(ty),
    }
}

/// `T | null` for the written type of an optional parameter or field (`name?: T`), keeping
/// `T`'s span; a type that already admits `null` stays as it is.
pub(super) fn or_null(ty: TypeExpr) -> TypeExpr {
    let null = TypeExpr {
        kind: TypeExprKind::Null,
        span: Span::new(ty.span.file, ty.span.hi, ty.span.hi),
    };
    let span = ty.span;
    match ty.kind {
        TypeExprKind::Null => ty,
        TypeExprKind::Union(ms) if ms.iter().any(|m| matches!(m.kind, TypeExprKind::Null)) => {
            TypeExpr {
                kind: TypeExprKind::Union(ms),
                span,
            }
        }
        TypeExprKind::Union(mut ms) => {
            ms.push(null);
            TypeExpr {
                kind: TypeExprKind::Union(ms),
                span,
            }
        }
        _ => TypeExpr {
            kind: TypeExprKind::Union(vec![ty, null]),
            span,
        },
    }
}

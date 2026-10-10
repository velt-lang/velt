//! Visiting written types: signatures, generic bounds and every type nested in a type.

use crate::ast::{self, TypeExprKind as T};

use super::Visit;

/// Visit `t` and every type inside it.
pub(super) fn walk_type<'a>(t: &'a ast::TypeExpr, v: &mut dyn Visit<'a>) {
    v.ty(t);
    match &t.kind {
        T::Named { args, .. } => args.iter().for_each(|a| walk_type(a, v)),
        T::Array(elem) => walk_type(elem, v),
        T::Tuple(items) | T::Union(items) | T::Intersection(items) => {
            items.iter().for_each(|i| walk_type(i, v))
        }
        T::Indexed { object, key } => {
            walk_type(object, v);
            walk_type(key, v);
        }
        T::Function {
            params,
            ret,
            throws,
        } => {
            params.iter().for_each(|p| walk_type(p, v));
            walk_type(ret, v);
            if let Some(throws) = throws {
                walk_type(throws, v);
            }
        }
        T::Object(fields) => fields.iter().for_each(|f| walk_type(&f.ty, v)),
        T::Predicate { ty, .. } => {
            if let Some(t) = ty {
                walk_type(t, v);
            }
        }
        T::Literal(_) | T::Null | T::Void => {}
    }
}

pub(super) fn opt_type<'a>(t: Option<&'a ast::TypeExpr>, v: &mut dyn Visit<'a>) {
    if let Some(t) = t {
        walk_type(t, v);
    }
}

/// The bounds of type parameters (`T extends A & B`).
pub(super) fn walk_generics<'a>(generics: &'a [ast::GenericParam], v: &mut dyn Visit<'a>) {
    for g in generics {
        g.bounds.iter().for_each(|b| walk_type(b, v));
    }
}

/// The types of a signature: type parameter bounds, parameters, return and `throws` types.
pub(super) fn walk_sig_types<'a>(sig: &'a ast::FnSig, v: &mut dyn Visit<'a>) {
    walk_generics(&sig.generics, v);
    sig.params.iter().for_each(|p| walk_type(&p.ty, v));
    opt_type(sig.ret.as_ref(), v);
    opt_type(sig.throws.as_ref(), v);
}

/// The types written inside expression `e` itself (not in its sub-expressions).
pub(super) fn expr_types<'a>(e: &'a ast::Expr, v: &mut dyn Visit<'a>) {
    use ast::ExprKind as E;
    match &e.kind {
        E::Arrow {
            type_params,
            params,
            ret,
            throws,
            ..
        } => {
            walk_generics(type_params, v);
            params.iter().for_each(|p| opt_type(p.ty.as_ref(), v));
            opt_type(ret.as_ref(), v);
            opt_type(throws.as_ref(), v);
        }
        E::Call { type_args, .. } => type_args.iter().for_each(|t| walk_type(t, v)),
        E::New { class: ty, .. }
        | E::Cast { ty, .. }
        | E::InstanceOf { ty, .. }
        | E::StructLit { name: ty, .. } => walk_type(ty, v),
        _ => {}
    }
}

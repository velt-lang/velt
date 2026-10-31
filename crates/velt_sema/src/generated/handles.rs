//! How generated code names a type: `__type<"<instance>", <index>>`, the type at position
//! `index` of [`Model::canonical_order`] from the type arguments of the instance whose
//! fingerprint is `instance` ([`Model::fingerprint`]). Generated code is checked in a later
//! check than the one that asked for it, where type ids differ; the instance is demanded there
//! again, with the same fingerprint, and the same walk from its type arguments reaches the same
//! types in the same order. So a handle names any type a request reaches, whether or not Velt
//! syntax can write it (an object type with a symbol key, an intersection, a type declared
//! inside a function, a union whose layout was merged after the check), as the very type the
//! instance was demanded with.

use rustc_hash::FxHashMap;

use super::model::Model;
use crate::ctx::Ctx;
use crate::hir::TyId;

/// The name of the handle form in generated code.
pub const TYPE: &str = "__type";

/// The handle of the type at `index` of the walk from instance `fingerprint`'s type arguments.
pub fn handle(fingerprint: &str, index: u32) -> String {
    format!("{TYPE}<{fingerprint:?}, {index}>")
}

/// `(instance, index)` of a handle written by [`handle`].
pub fn parse(text: &str) -> Option<(String, u32)> {
    let rest = text.strip_prefix(TYPE)?.strip_prefix("<\"")?;
    let (fp, rest) = rest.split_once('"')?;
    let index = rest.strip_prefix(", ")?.strip_suffix('>')?;
    Some((fp.to_string(), index.parse().ok()?))
}

/// The types handles name in this check: per instance fingerprint, its walk's types.
#[derive(Default)]
pub(crate) struct Handles {
    pub walks: FxHashMap<String, Vec<TyId>>,
}

impl Handles {
    pub fn get(&self, fingerprint: &str, index: u32) -> Option<TyId> {
        self.walks.get(fingerprint)?.get(index as usize).copied()
    }
}

/// Give every type of `model` that has none the handle of the first of `requests` (each its
/// fingerprint and type arguments) whose walk reaches it.
pub(crate) fn assign(model: &mut Model, requests: &[(String, Vec<u32>)]) {
    for (fp, roots) in requests {
        for (k, t) in model.canonical_order(roots).into_iter().enumerate() {
            let s = &mut model.types[t as usize].spelling;
            if s.is_empty() {
                *s = handle(fp, k as u32);
            }
        }
    }
}

/// The walk of instance `fingerprint` at type arguments `targs` in this check, for the handles
/// of the code generated for it. `model` holds the types (built with [`super::model::Builder`]).
pub(crate) fn walk(
    model: &mut super::model::Builder,
    cx: &mut Ctx,
    targs: &[TyId],
) -> (Vec<u32>, Vec<TyId>) {
    let roots: Vec<u32> = targs.iter().map(|t| model.add(cx, *t)).collect();
    let order = model.model.canonical_order(&roots);
    let tys = order.iter().map(|&i| model.ty_of(i)).collect();
    (roots, tys)
}

impl Ctx<'_> {
    /// `__type<"<instance>", <index>>` in generated code ([`TYPE`]).
    pub(crate) fn resolve_handle(
        &mut self,
        t: &velt_syntax::ast::TypeExpr,
        args: &[velt_syntax::ast::TypeExpr],
    ) -> TyId {
        use velt_syntax::ast::{Lit, TypeExprKind};
        let lit = |a: Option<&velt_syntax::ast::TypeExpr>| match a.map(|a| &a.kind) {
            Some(TypeExprKind::Literal(l)) => Some(l.lit.clone()),
            _ => None,
        };
        let found = match (lit(args.first()), lit(args.get(1)), args.len()) {
            (Some(Lit::Str(fp)), Some(Lit::Int { value, .. }), 2) => u32::try_from(value)
                .ok()
                .and_then(|i| self.gen.binding.handles.get(&fp, i)),
            _ => None,
        };
        match found {
            Some(ty) => ty,
            None => {
                self.err(
                    format!("`{TYPE}<…>` names no type of an instance this program needs (a bug in the generator, or code generated for types that changed)"),
                    t.span,
                );
                self.ty.error
            }
        }
    }
}

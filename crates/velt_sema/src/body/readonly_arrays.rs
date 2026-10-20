//! Read-only arrays (docs/reference/types.md "Read-only arrays"): `readonly T[]` and
//! `ReadonlyArray<T>` are `T[]` (the parser reads the first as the second), and a variable or
//! parameter declared with one of them may not be written through: a mutating method
//! (`push`, `sort`, …) is TypeScript's TS2339 and an element assignment TS2542. The check
//! follows the variable, not the value: a copy into a `T[]` variable is not checked (TypeScript
//! rejects that assignment, TS4104).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::FnCx;
use crate::hir::{self, ExprKind as H, LitValue, LocalId, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Records local `l` declared with the written type `ty` when that is a read-only array.
    pub(super) fn note_readonly_array(&mut self, l: LocalId, ty: Option<&ast::TypeExpr>) {
        if ty.is_some_and(is_readonly_array) {
            self.f.readonly_arrays.insert(l);
        }
    }

    /// The readonly array type local `l` (of type `ty`) is declared with, as TypeScript shows
    /// it (`readonly number[]`).
    fn readonly_shown(&mut self, h: &hir::Expr) -> Option<String> {
        let H::Local(l, _) = h.kind else {
            return None;
        };
        if !self.f.readonly_arrays.contains(&l) {
            return None;
        }
        let TyKind::Array(elem) = *self.cx.ty.kind(h.ty) else {
            return None;
        };
        let shown = self.cx.display(elem);
        match self.cx.union_members(elem) {
            Some(_) if shown.contains(" | ") => Some(format!("readonly ({shown})[]")),
            _ => Some(format!("readonly {shown}[]")),
        }
    }

    /// Place `e` is about to be assigned: an error when it is an element of a read-only array
    /// variable.
    pub(super) fn readonly_element_write(&mut self, e: &hir::Expr) {
        let H::Index { base, .. } = &e.kind else {
            return;
        };
        if let Some(shown) = self.readonly_shown(base) {
            self.cx.error(Diagnostic::error(
                format!(
                    "index signature in type '{shown}' only permits reading (TypeScript's TS2542)"
                ),
                e.span,
            ));
        }
    }

    /// A call of method `name` on `recv`: an error when it is a mutating method of an array and
    /// `recv` a read-only array variable.
    pub(crate) fn readonly_method_call(&mut self, recv: &hir::Expr, name: &str, span: Span) {
        if !MUTATING.contains(&name) {
            return;
        }
        if let Some(shown) = self.readonly_shown(recv) {
            self.cx.error(
                Diagnostic::error(
                    format!("property '{name}' does not exist on type '{shown}' (TypeScript's TS2339)"),
                    span,
                )
                .with_note("a read-only array has no methods that change it; copy it first (`[...xs]`) to get an array you may change"),
            );
        }
    }
}

impl FnCx<'_, '_> {
    /// `e as const`. An array literal of string literals with no expected type keeps their
    /// literal types (TypeScript's `readonly ["a", "b"]`): it is an array of their union (of
    /// the one literal type, `"a"[]`, for `["a"] as const`), so iterating or indexing it gives
    /// `"a" | "b"`. Anything else is typed as `e` alone: Velt arrays and literals need no
    /// annotation for it.
    pub(super) fn as_const(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        want: super::Want,
    ) -> hir::Expr {
        let exp = match exp {
            Some(t) => Some(t),
            None => self.as_const_array_ty(e),
        };
        self.expr(e, exp, want)
    }

    fn as_const_array_ty(&mut self, e: &ast::Expr) -> Option<TyId> {
        let elems = as_const_strings(e)?;
        let mut members = vec![];
        for s in elems {
            let t = self.cx.lit_type(LitValue::Str(s.clone()));
            if !members.contains(&t) {
                members.push(t);
            }
        }
        let u = match members[..] {
            [one] => one,
            _ => self.cx.union_of(&members, false, e.span),
        };
        Some(self.cx.ty.array(u))
    }

    /// Records local `l` initialized with `init` as a read-only array when that is an array
    /// literal of strings `as const`.
    pub(super) fn note_as_const_array(&mut self, l: LocalId, init: Option<&ast::Expr>) {
        let Some(ast::ExprKind::Cast { expr, ty }) = init.map(|e| &e.kind) else {
            return;
        };
        if super::expr::member::is_as_const(ty) {
            if let Some(names) = as_const_tuple(expr) {
                self.f.as_const_tuples.insert(l, names);
            }
        }
        if super::expr::member::is_as_const(ty) && as_const_strings(expr).is_some() {
            self.f.readonly_arrays.insert(l);
        }
    }
}

/// `[2, "y"]` (in parentheses or not): its elements when they are literals of more than one
/// type (TypeScript's `readonly [2, "y"]` as `as const` gives it).
pub(super) fn as_const_tuple(e: &ast::Expr) -> Option<Vec<ast::Expr>> {
    let elems = match &e.kind {
        ast::ExprKind::Paren(inner) => return as_const_tuple(inner),
        ast::ExprKind::Array(elems) if elems.len() > 1 => elems,
        _ => return None,
    };
    let names: Vec<&'static str> = elems
        .iter()
        .map(|x| match &x.kind {
            ast::ExprKind::Lit(ast::Lit::Str(_)) => Some("string"),
            ast::ExprKind::Lit(
                ast::Lit::Int { suffix: None, .. } | ast::Lit::Float { suffix: None, .. },
            ) => Some("number"),
            ast::ExprKind::Lit(ast::Lit::Bool(_)) => Some("boolean"),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (names.iter().any(|n| *n != names[0])).then(|| elems.clone())
}

/// The elements of `[...]` (in parentheses or not) when it has some and they are all string
/// literals.
fn as_const_strings(e: &ast::Expr) -> Option<Vec<&String>> {
    match &e.kind {
        ast::ExprKind::Paren(inner) => as_const_strings(inner),
        ast::ExprKind::Array(elems) if !elems.is_empty() => elems
            .iter()
            .map(|x| match &x.kind {
                ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// The methods of `T[]` that `readonly T[]` doesn't have.
const MUTATING: [&str; 9] = [
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "sort",
    "reverse",
    "fill",
    "copyWithin",
];

/// `readonly T[]` (parsed as `ReadonlyArray<T>`) or `ReadonlyArray<T>`, possibly `| null`.
pub(crate) fn is_readonly_array(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            path.len() == 1 && path[0].name == "ReadonlyArray" && args.len() == 1
        }
        ast::TypeExprKind::Union(ms) => {
            let mut non_null = ms.iter().filter(|m| {
                !matches!(
                    m.kind,
                    ast::TypeExprKind::Null | ast::TypeExprKind::Undefined
                )
            });
            matches!((non_null.next(), non_null.next()), (Some(m), None) if is_readonly_array(m))
        }
        _ => false,
    }
}

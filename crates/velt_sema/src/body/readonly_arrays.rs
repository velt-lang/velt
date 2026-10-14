//! Read-only arrays (docs/reference/types.md "Read-only arrays"): `readonly T[]` and
//! `ReadonlyArray<T>` are `T[]` (the parser reads the first as the second), and a variable or
//! parameter declared with one of them may not be written through: a mutating method
//! (`push`, `sort`, …) is TypeScript's TS2339 and an element assignment TS2542. The check
//! follows the variable, not the value: a copy into a `T[]` variable is not checked (TypeScript
//! rejects that assignment, TS4104).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::FnCx;
use crate::hir::{self, ExprKind as H, LocalId, TyKind};

impl FnCx<'_, '_> {
    /// Records local `l` declared with the written type `ty` when that is a read-only array.
    pub(super) fn note_readonly_array(&mut self, l: LocalId, ty: Option<&ast::TypeExpr>) {
        if ty.is_some_and(is_readonly_array) {
            self.f.readonly_arrays.insert(l);
        }
    }

    /// The readonly array type local `l` (of type `ty`) is declared with, as TypeScript shows
    /// it (`readonly number[]`).
    fn readonly_shown(&self, h: &hir::Expr) -> Option<String> {
        let H::Local(l, _) = h.kind else {
            return None;
        };
        if !self.f.readonly_arrays.contains(&l) {
            return None;
        }
        match *self.cx.ty.kind(h.ty) {
            TyKind::Array(elem) => Some(format!("readonly {}[]", self.cx.display(elem))),
            _ => None,
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
fn is_readonly_array(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            path.len() == 1 && path[0].name == "ReadonlyArray" && args.len() == 1
        }
        ast::TypeExprKind::Union(ms) => {
            let mut non_null = ms
                .iter()
                .filter(|m| !matches!(m.kind, ast::TypeExprKind::Null));
            matches!((non_null.next(), non_null.next()), (Some(m), None) if is_readonly_array(m))
        }
        _ => false,
    }
}

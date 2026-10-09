//! `let x;` without a type or initializer: as in TypeScript, its type comes from its
//! assignments. Bodies are checked in source order, so the first assignment checked is the first
//! one written (`let st; try { st = parse(s); } catch { … }`): it gives `x` its type, as
//! `let x = value` would, and later assignments convert to that type.
//!
//! Until then the local's type is unknown: a read of it, a compound assignment and a use from a
//! closure are errors asking for an annotation (TypeScript would read `undefined`, which Velt
//! has no counterpart for), and so is a `let x;` never assigned.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::hir::{self, ExprKind as H, LocalId};

const NOTE: &str = "a `let` without a type or initializer takes its type from its first assignment";

impl FnCx<'_, '_> {
    /// `x = value` where `place` is an untyped `let` not assigned yet: `value` checked on its
    /// own, giving `x` (and `place`) its type. `None` for any other place.
    pub(crate) fn first_assign(
        &mut self,
        place: &mut hir::Expr,
        value: &ast::Expr,
    ) -> Option<hir::Expr> {
        let H::Local(l, _) = place.kind else {
            return None;
        };
        let decl = self.f.untyped_lets.remove(&l)?;
        if self.reject_untyped_init(value) {
            // Reported once: the variable stays without a type.
            return Some(self.error_expr(value.span));
        }
        let v = self.expr(value, None, Want::Move);
        let v = self.inferred_local_init(v);
        if self.cx.ty.is_bottom(v.ty) || v.ty == self.cx.ty.unit {
            if v.ty == self.cx.ty.never {
                // `x = fail()` does not finish: the next assignment decides.
                self.f.untyped_lets.insert(l, decl);
            } else if v.ty == self.cx.ty.unit {
                let name = self.f.locals[l.0 as usize].name.clone();
                self.cx
                    .err(format!("variable `{name}` cannot have type `void`"), v.span);
            }
            return Some(v);
        }
        self.f.locals[l.0 as usize].ty = v.ty;
        place.ty = v.ty;
        self.note_inferred_local(l, &v);
        Some(v)
    }

    /// A use of `id` that is not its first assignment, while it is an untyped `let` not
    /// assigned yet (also from a closure): reported. `assign`: the use assigns it, which is
    /// fine in the declaring function itself.
    pub(crate) fn untyped_use(&mut self, id: &ast::Ident, assign: bool) -> bool {
        if self.f.untyped_lets.is_empty() && self.outer.iter().all(|f| f.untyped_lets.is_empty()) {
            return false;
        }
        let Some((f, l)) = self.peek_local(&id.name) else {
            return false;
        };
        let here = std::ptr::eq(f, &self.f);
        if !f.untyped_lets.contains_key(&l) || (assign && here) {
            return false;
        }
        // Reported once: later uses and the end of its scope say nothing more.
        let outer = self.outer.iter().position(|o| std::ptr::eq(o, f));
        match outer {
            Some(j) => self.outer[j].untyped_lets.remove(&l),
            None => self.f.untyped_lets.remove(&l),
        };
        let why = match here {
            true => format!("`{}` is read here before it is first assigned", id.name),
            false => format!(
                "a closure uses `{}` before the function first assigns it",
                id.name
            ),
        };
        self.untyped_error(&id.name, id.span, why);
        true
    }

    /// `x += v` / `x++` on an untyped `let` not assigned yet: reported.
    pub(crate) fn untyped_update(&mut self, place: &hir::Expr, span: Span) -> bool {
        let H::Local(l, _) = place.kind else {
            return false;
        };
        // Reported once: not again as never assigned.
        if self.f.untyped_lets.remove(&l).is_none() {
            return false;
        }
        let name = self.f.locals[l.0 as usize].name.clone();
        let why = format!("`{name}` is updated here before it is first assigned");
        self.untyped_error(&name, span, why);
        true
    }

    /// The untyped `let`s among `locals` that were never assigned: reported at their
    /// declarations.
    pub(crate) fn report_untyped_lets(&mut self, locals: impl Iterator<Item = LocalId>) {
        let mut left: Vec<(Span, String)> = locals
            .filter_map(|l| {
                let at = self.f.untyped_lets.remove(&l)?;
                Some((at, self.f.locals[l.0 as usize].name.clone()))
            })
            .collect();
        left.sort_by_key(|(at, _)| at.lo);
        for (at, name) in left {
            self.cx.error(
                Diagnostic::error(format!("type annotations needed for `{name}`"), at)
                    .with_note(format!("{NOTE}, and `{name}` is never assigned")),
            );
        }
    }

    fn untyped_error(&mut self, name: &str, span: Span, why: String) {
        self.cx.error(
            Diagnostic::error(format!("type annotations needed for `{name}`"), span)
                .with_note(format!("{NOTE}; {why}"))
                .with_note(format!(
                    "annotate the declaration, e.g. `let {name}: number;`, or assign it first"
                )),
        );
    }
}

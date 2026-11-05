//! The error of a type assertion that does not hold (`x as T`, `body/expr/member.rs`).

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// The error of `h as target` that does not assert (`Self::assert_value`).
    pub(super) fn cast_error(&mut self, h: hir::Expr, target: TyId, span: Span) -> hir::Expr {
        let nothings = (self.cx.ty.nothing_of(h.ty), self.cx.ty.nothing_of(target));
        if let (Some(from), Some(to)) = nothings {
            if !from.within(to) && !self.cx.ty.has_error(h.ty) {
                // TypeScript keeps the value (`undefined` stays `undefined`); the type
                // `T | null | undefined` that could hold it comes with phase 1b.
                let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
                self.cx.error(
                    Diagnostic::error(
                        format!("a cast from `{s}` to `{d}` is not supported yet (#930)"),
                        span,
                    )
                    .with_note(format!(
                        "the value would stay `{}`, which `{d}` cannot hold yet; convert it instead (`x ?? {}`)",
                        from.word(),
                        to.word()
                    )),
                );
                return self.error_expr(span);
            }
        }
        if !self.cx.ty.has_error(h.ty) {
            let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
            let diag = Diagnostic::error(format!("cannot cast `{s}` as `{d}`"), span);
            let mut diag = self.cx.with_layout_note(diag, target);
            if let Some(te) = self.array_narrows_to_unique(h.ty, target) {
                let name = self.cx.display(te);
                diag = diag.with_note(format!(
                    "`{name}` holds a `unique symbol`, which only its constant has: a copy checks each element (`xs.map(x => x as {name})`)"
                ));
            }
            if self.cx.has_literal_member(target) {
                diag = diag.with_note(
                    "a value takes a literal type after a test that narrows it (`if (k === \"a\")`)",
                );
            }
            self.cx.error(diag);
        }
        self.error_expr(span)
    }
}

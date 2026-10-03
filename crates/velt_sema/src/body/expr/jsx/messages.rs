//! JSX diagnostics worded like TypeScript's, so messages match what TSX users know.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// `Property 'x' does not exist on type 'T'.`, suggesting the closest of `known`.
    pub(super) fn no_property(
        &mut self,
        name: &str,
        on: &str,
        known: &[String],
        span: Span,
        note: Option<String>,
    ) {
        let mut msg = format!("Property '{name}' does not exist on type '{on}'.");
        if let Some(s) = crate::suggest::closest(name, known) {
            msg.push_str(&format!(" Did you mean '{s}'?"));
        }
        let d = Diagnostic::error(msg, span);
        self.cx.error(match note {
            Some(n) => d.with_note(n),
            None => d,
        });
    }

    /// `Type 'F' is not assignable to type 'E'.` for `found` checked against `expected`.
    pub(super) fn not_assignable(&mut self, expected: TyId, found: &hir::Expr, what: &str) {
        let (e, f) = (self.cx.display(expected), self.cx.display(found.ty));
        self.cx.error(
            Diagnostic::error(
                format!("Type '{f}' is not assignable to type '{e}'."),
                found.span,
            )
            .with_note(what.to_string()),
        );
    }

    /// Convert `h` to `expected`, reporting a mismatch the TypeScript way (`what` says where);
    /// a mismatched value becomes an error placeholder.
    pub(super) fn jsx_coerce(&mut self, h: hir::Expr, expected: TyId, what: &str) -> hir::Expr {
        match self.try_coerce(h, expected) {
            Ok(h) => h,
            Err(h) => {
                self.not_assignable(expected, &h, what);
                // Reported: the value must not cause a second mismatch where it is used.
                self.error_expr(h.span)
            }
        }
    }
}

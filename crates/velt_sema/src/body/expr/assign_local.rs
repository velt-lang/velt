//! `x = v` where `x` names a variable: the local as a writable place, or the error for a name
//! that can't be assigned (a `const`, a `using` declaration, a borrowed element, a pattern
//! binding, a non-variable item or an unknown name). A module's `let` is `module_let_target`'s.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, UseMode};

impl FnCx<'_, '_> {
    /// The local `id` names, as a place to write (`None`: reported).
    pub(super) fn assign_local(&mut self, id: &ast::Ident, span: Span) -> Option<hir::Expr> {
        if self.untyped_use(id, true) {
            return None;
        }
        let Some(l) = self.lookup_local(&id.name, id.span) else {
            let item = self.lookup_item(&id.name, id.span);
            if let Some(place) = self.module_let_target(item, id) {
                return place;
            } else if item.is_some() {
                self.cx.err(
                    format!("cannot assign to `{}`: it is not a variable", id.name),
                    span,
                );
            } else {
                self.unknown_name(id);
            }
            return None;
        };
        self.rec_local(id.span, l);
        let name = &id.name;
        let mutable = self.f.locals[l.0 as usize].mutable;
        let problem = match self.local_kind(l) {
            LocalKind::Const => Some((format!("cannot assign twice to const `{name}`"), None)),
            LocalKind::Using => Some((
                format!("cannot assign to `{name}`, a `using` declaration"),
                Some("it is disposed at the end of its block; declare another variable"),
            )),
            LocalKind::Elem if !mutable => Some((
                format!("cannot assign to `{name}`, which borrows an array element"),
                Some("index the array to replace an element: `xs[i] = v`"),
            )),
            LocalKind::Bind if !mutable => {
                Some((format!("cannot assign to pattern binding `{name}`"), None))
            }
            _ => None,
        };
        if let Some((msg, note)) = problem {
            let mut d = Diagnostic::error(msg, span);
            if let Some(n) = note {
                d = d.with_note(n);
            }
            self.cx.error(d);
        }
        self.mark_mutated(l, id.span);
        let ty = self.local_ty(l);
        Some(self.mk(H::Local(l, UseMode::BorrowMut), ty, id.span))
    }
}

//! The error for a member read of a field a type doesn't have, with notes on why.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(super) fn no_field(&mut self, t: TyId, obj: Span, prop: &ast::Ident) {
        if self.private_accessor_outside(t, prop, false) || self.shadowed_private_name(t, prop) {
            return;
        }
        if crate::collect::exports::member_maybe_dropped(self.cx, t, &prop.name) {
            return; // the parser may have dropped its declaration (`Ctx::recovered`)
        }
        if self.has_setter(t, &prop.name) {
            return self.cx.err(
                format!("cannot read `{}`: it has a setter but no getter", prop.name),
                prop.span,
            );
        }
        let tn = self.cx.display(t);
        let mut d = Diagnostic::error(
            format!(
                "no field `{}` on type `{tn}`",
                self.cx.shown_name(&prop.name)
            ),
            prop.span,
        );
        if self.adt_of(t).is_some() {
            d = d.with_note(
                "objects have a fixed shape: fields cannot be added at runtime; use a `Map<string, V>` for dynamic keys",
            );
        }
        if matches!(
            self.cx.ty.kind(t),
            TyKind::FnPtr { .. } | TyKind::Closure(_)
        ) {
            d = d.with_note(super::callables::FN_PROPERTY_NOTE);
        }
        if let Some(note) = self.narrowing_note(t) {
            d = d.with_note(note);
        }
        if matches!(self.cx.ty.kind(t), hir::TyKind::Op(hir::TyOp::Intersect, _)) {
            d = d.with_note(
                "in generic code, an intersection with a type parameter part has only the members of its object parts; callers see the type parameter's once the type arguments are known (#350)",
            );
        }
        if let Some(note) = self.unnarrowed_note(obj) {
            d = d.with_note(note);
        }
        if self.method_exists(t, &prop.name) {
            d = d.with_note(match self.cx.key_spelling(&prop.name) {
                Some(key) => format!("`{key}` is a method: call it with `o{key}()`"),
                None => format!(
                    "`{}` is a method: call it with `.{}()`",
                    prop.name, prop.name
                ),
            });
        }
        self.cx.error(d);
    }
}

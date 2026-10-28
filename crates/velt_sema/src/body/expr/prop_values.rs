//! The value of one property of an object literal, checked against the type its field, slot or
//! record value expects.

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// The value of property `name`: `value`, or the variable `name` for `{ name }`. A function
    /// named there is adapted to an expected function type as an initializer is
    /// (`adapted_prop_fn`).
    pub(super) fn prop_value(
        &mut self,
        name: &ast::Ident,
        value: Option<&ast::Expr>,
        exp: Option<TyId>,
    ) -> hir::Expr {
        if let Some(h) = self.adapted_prop_fn(name, value, exp) {
            return h;
        }
        match value {
            Some(v) => self.stored_value(v, |s| s.expr(v, exp, Want::Move)),
            None => self.ident_expr(name, exp, Want::Move),
        }
    }

    /// [`Self::prop_value`] for a field of an object type. A value that may be null written
    /// for an optional field without a presence flag (`watched`, `anon::absent_is_null`) is
    /// remembered: Node finds the key of `{ b: undefined }`, which such a field cannot hold
    /// (`crate::absent_fields`).
    pub(super) fn field_value(
        &mut self,
        name: &ast::Ident,
        value: Option<&ast::Expr>,
        exp: Option<TyId>,
        watched: bool,
    ) -> hir::Expr {
        let h = self.prop_value(name, value, exp);
        // A promise that rejects, stored into a field whose `Promise<T>` is written without an
        // error type: the field's site takes its errors (`throws::promise_sites`). The literal
        // converts its values to the field types after the field's site is no longer the
        // target, so it is seeded here.
        if let Some(exp) = exp {
            self.cx.seed_promise_site(h.ty, exp);
        }
        if !watched {
            return h;
        }
        let nullable = crate::absent_fields::literal_may_be_null(self.cx, &h);
        if nullable {
            let store = (name.name.clone(), h.span);
            self.cx.object_copies.null_literals.push(store);
        }
        h
    }
}

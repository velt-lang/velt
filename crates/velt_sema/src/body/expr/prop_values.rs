//! The value of one property of an object literal, checked against the type its field, slot or
//! record value expects.

use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// The value of field `name` of an object literal of its own type. Converted to an `any`
    /// or `unknown` (`dynamic`: `const u: unknown = { a: null }`), a `null` field (TypeScript's
    /// type `null`) is an `any` holding `null`, and so is one in a nested object or array
    /// literal (`{ b: { c: null } }`, `{ a: [null, 1] }`). A conditional, `&&`, `||` or `??`
    /// with an `any` value is an `any` there too (`{ q: c ? 1 : a.q }`, `any_join`).
    pub(super) fn anon_prop_value(
        &mut self,
        name: &ast::Ident,
        value: Option<&ast::Expr>,
        dynamic: bool,
    ) -> hir::Expr {
        let any = match value.is_some_and(|v| dynamic && super::lit::dynamic_hinted(v)) {
            true => self.any_ty(),
            false => None,
        };
        // `{ value: null }` with nothing expected: TypeScript types the property `null`, and a
        // later write of another value is its error. The same object converted to another type
        // (`init as unknown as Ctx`, sigx's component context) is written through that type,
        // which Node allows: so the field is an `any` holding `null`, which takes it.
        let null = value.is_some_and(|v| crate::body::narrow::nothing_word(v) == Some("null"));
        // `{ signal: signal }` of a generic function: function types have no type parameters, so
        // the field is an `any`, as for `typeof signal` (`Ctx::resolve_field_type`).
        let generic = match value.map(|v| &v.kind) {
            Some(ast::ExprKind::Ident(id)) => self.names_generic_fn(id),
            None => self.names_generic_fn(name),
            _ => false,
        };
        let any = match generic {
            true => self.any_ty(),
            false => any,
        };
        let any = match null && any.is_none() {
            true => self.any_ty(),
            false => any,
        };
        self.prop_value(name, value, any)
    }

    /// Does `id` name a generic function of this module's scope, not a local?
    fn names_generic_fn(&mut self, id: &ast::Ident) -> bool {
        if !self.is_fn_item(id) {
            return false;
        }
        match self.lookup_item(&id.name, id.span) {
            Some(crate::ctx::Item::Def(d)) => !self.cx.fn_info(d).generics.names.is_empty(),
            _ => false,
        }
    }

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
        let h = match value {
            Some(v) => self.stored_value(v, |s| s.expr(v, exp, Want::Move)),
            None => self.ident_expr(name, exp, Want::Move),
        };
        // A promise that rejects, stored into a field or record value whose `Promise<T>` is
        // written without an error type: that site takes its errors (`throws::promise_sites`).
        // The literal converts its values to the expected types after the site is no longer
        // the target, so the value seeds it here.
        if let Some(exp) = exp {
            self.cx.seed_promise_site(h.ty, exp);
        }
        h
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

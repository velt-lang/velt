//! Functions with properties (#921): an interface or object type with one call signature and
//! members (`interface Runner { (): T; stop(): void }`, `(() => T) & { stop(): void }`) is a
//! callable object type. It is the anonymous object type whose first field, the hidden
//! [`CALL_FIELD`], holds the function value, followed by the members: a closure plus a fixed
//! record of fields, an ordinary object to lowering. The field name starts with `#`, so
//! `Object.keys`, `JSON.stringify`, printing and spreads leave it out, as they leave out a
//! private name (`property_order::is_hidden_key`).
//!
//! Sema builds one from `Object.assign(fn, { … })` and from a function cast to such a type
//! followed by the property writes that complete it (`body::expr::callables`); calling one
//! calls its function, `typeof` is `"function"`, and it converts to its plain function type
//! by reading the function.

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, TyId, TyKind};

/// The hidden field of a callable object type that holds its function.
pub(crate) const CALL_FIELD: &str = "#call";

impl Ctx<'_> {
    /// The function type of callable object type `t` (its [`CALL_FIELD`]'s type, type
    /// arguments substituted), or `None` when `t` is not one.
    pub(crate) fn call_type(&mut self, t: TyId) -> Option<TyId> {
        if !self.is_callable_object(t) {
            return None;
        }
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let declared = self.adt(d)?.fields.first()?.ty;
        Some(self.subst(declared, &args))
    }

    /// Is `t` a callable object type (an anonymous object type whose first field is
    /// [`CALL_FIELD`])?
    pub(crate) fn is_callable_object(&self, t: TyId) -> bool {
        let TyKind::Adt(d, _) = self.ty.kind(t) else {
            return false;
        };
        self.adt(*d).is_some_and(|a| {
            a.kind == AdtKind::Anon && a.fields.first().is_some_and(|f| f.name == CALL_FIELD)
        })
    }

    /// The callable object type `{ #call: f }` of function type `f` alone (an operand of an
    /// intersection with object types), declared in `module`.
    pub(crate) fn callable_part(&mut self, f: TyId, module: usize) -> TyId {
        self.anon_type_with(&[ShapeField::plain(CALL_FIELD, f)], module)
    }

    /// `fields` with [`CALL_FIELD`] moved to the front (an intersection written with the
    /// function type after the object types), or `None` when it is already first or absent.
    pub(crate) fn call_field_first(fields: &[ShapeField]) -> Option<Vec<ShapeField>> {
        let i = fields.iter().position(|f| f.name == CALL_FIELD)?;
        if i == 0 {
            return None;
        }
        let mut out = fields.to_vec();
        let call = out.remove(i);
        out.insert(0, call);
        Some(out)
    }
}

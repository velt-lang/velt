//! Member access rules (docs/reference/classes.md):
//! - `private` fields, methods and static fields are usable only inside the body of the type
//!   declaring them (its methods, constructor, field initializers and the closures inside them;
//!   not subclasses, like TypeScript).
//! - `get name(): T` accessors are read as properties: `x.name` is a call of the getter (receiver
//!   borrowed); they cannot be called with `()`, nor assigned unless a setter of the same
//!   name exists (`setters`).
//! - `static readonly NAME` fields are module constants read as `Type.NAME`.
//! - A prelude function `F` whose prelude also exports a class `FConstructor` has that class's
//!   static members as its own (`Number(s)` and `Number.isInteger(x)`, like TypeScript's
//!   `NumberConstructor`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::method::Resolved;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, TyId};
use crate::ide::record::Target;

impl FnCx<'_, '_> {
    /// Report a use of a private member of `private_to` outside that type's body.
    pub(crate) fn check_private(&mut self, private_to: Option<DefId>, name: &str, span: Span) {
        let Some(owner) = private_to else { return };
        if self.owner == Some(owner) {
            return;
        }
        let tn = self
            .cx
            .adt(owner)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        self.cx.error(
            Diagnostic::error(format!("`{name}` is private"), span)
                .with_note(format!("it can only be used inside the body of `{tn}`")),
        );
    }

    /// `private` check for field `index` of struct/class values of type `t`.
    pub(crate) fn check_field_private(&mut self, t: TyId, index: u32, name: &ast::Ident) {
        let private_to = self
            .adt_of(t)
            .and_then(|(d, _)| self.cx.adt(d))
            .and_then(|a| a.fields.get(index as usize))
            .and_then(|f| f.private_to);
        self.check_private(private_to, &name.name, name.span);
    }

    /// The type a method is private to, if it is `private`.
    pub(crate) fn fn_private_to(&self, def: DefId) -> Option<DefId> {
        let f = self.cx.fn_info(def);
        f.owner.filter(|_| f.is_private)
    }

    /// The type a resolved method is private to, if it is `private`.
    pub(crate) fn method_private_to(&self, r: &Resolved) -> Option<DefId> {
        match r {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => self.fn_private_to(*def),
            _ => None,
        }
    }

    /// `span` names the resolved method `r` (for `crate::ide`).
    pub(crate) fn rec_method(&mut self, span: Span, r: &Resolved) {
        let t = match r {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => Target::Def(*def),
            Resolved::Iface { iface, slot, .. } => Target::IfaceMethod(*iface, *slot),
            Resolved::Builtin(_) => return,
        };
        self.cx.rec_ref(span, t);
    }

    pub(crate) fn is_getter(&self, r: &Resolved) -> bool {
        match r {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => {
                self.cx.fn_info(*def).is_getter
            }
            Resolved::Iface { method, .. } => method.is_getter,
            Resolved::Builtin(_) => false,
        }
    }

    /// Does values of type `t` have a getter `name` (and no field of that name)?
    pub(crate) fn has_getter(&mut self, t: TyId, name: &str) -> bool {
        if self.field_of(t, name).is_some() {
            return false;
        }
        self.resolve_method(t, name)
            .is_some_and(|r| self.is_getter(&r))
    }

    /// `obj.name` where `name` is a getter: the getter's call; `Err` gives `obj` back otherwise.
    pub(crate) fn getter_read(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.has_getter(obj.ty, &prop.name) {
            return Err(obj);
        }
        Ok(self.method_call_at(obj, prop, &[], &[], None, span, true))
    }

    /// Assigning to a getter without a setter is an error.
    pub(crate) fn reject_getter_assign(&mut self, t: TyId, prop: &ast::Ident) -> bool {
        if !self.has_getter(t, &prop.name) {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                format!("cannot assign to `{}`: it is a getter", prop.name),
                prop.span,
            )
            .with_note(format!(
                "define a setter `set {}(v)` to make it assignable",
                prop.name
            )),
        );
        true
    }

    /// The class holding the static members of prelude function `name` (`NumberConstructor`
    /// for `Number`), if `item` is that prelude function and not a user item shadowing it.
    pub(crate) fn companion_class(&self, name: &str, item: crate::ctx::Item) -> Option<DefId> {
        if self.cx.prelude.get(name) != Some(&item) {
            return None;
        }
        self.cx.prelude_adt(&format!("{name}Constructor"))
    }

    /// `Type.NAME` for a `static readonly` field of struct/class `d`.
    pub(crate) fn static_field(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let g = self.cx.adt(d)?.statics.get(&prop.name).copied()?;
        self.cx.rec_ref(prop.span, Target::Def(g));
        let private_to = self
            .cx
            .global(g)
            .filter(|g| g.src.is_private)
            .and_then(|g| g.src.owner);
        self.check_private(private_to, &prop.name, prop.span);
        Some(self.global_read(g, want, span))
    }
}

//! The polymorphic `this` type as a method's result (`register(): this { ...; return this; }`,
//! docs/reference/classes.md "Returning `this`"). The signature resolves `this` to the class
//! declaring the method (`resolve::TyEnv::this_ty`); a call on a receiver of a subclass gives
//! the receiver's own type, as in TypeScript, so chained calls keep the subclass's methods.
//!
//! That conversion is a `Downcast` of the result, which is sound because such a method can only
//! return `this` itself: `return this;`, or the result of another `this`-returning method called
//! on `this` (checked by `check_this_return`; anything else is TypeScript's TS2322).

use velt_common::{Diagnostic, Span};

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The receiver's type and whether it is `this` (`is_this_value`), for `this_result`.
    pub(super) fn this_recv(&self, recv: &hir::Expr) -> (TyId, bool) {
        (recv.ty, self.is_this_value(recv))
    }

    /// The call `call` of method `def` on a receiver of type `to` (before its conversion to the
    /// declaring class; `on_this`: the receiver is `this`): when `def` returns `this`, typed as
    /// the receiver's class.
    pub(super) fn this_result(
        &mut self,
        call: hir::Expr,
        def: DefId,
        (to, on_this): (TyId, bool),
    ) -> hir::Expr {
        if !self.cx.fn_info(def).returns_this() {
            return call;
        }
        if on_this {
            self.this_calls.insert(call.span);
        }
        if to == call.ty
            || self.cx.ty.is_bottom(call.ty)
            || !matches!(self.cx.ty.kind(to), TyKind::Adt(..))
        {
            return call;
        }
        let span = call.span;
        self.mk(H::Downcast(Box::new(call)), to, span)
    }

    /// Is `h` the method's `this` (or a `this`-returning method's result on it)?
    fn is_this_value(&self, h: &hir::Expr) -> bool {
        match &h.kind {
            H::Local(l, _) => self.outer.is_empty() && self.local_kind(*l) == LocalKind::This,
            H::Upcast(x) | H::Downcast(x) => self.is_this_value(x),
            H::Call { .. } => self.this_calls.contains(&h.span),
            _ => false,
        }
    }

    /// `return h;` in the body: in a method returning `this`, `h` must be `this`.
    pub(crate) fn check_this_return(&mut self, h: &hir::Expr, ret: TyId, span: Span) {
        let Some(d) = self.body_def else {
            return;
        };
        if !self.outer.is_empty() || !self.cx.fn_info(d).returns_this() {
            return;
        }
        if self.is_this_value(h) || self.cx.ty.is_bottom(h.ty) {
            return;
        }
        let found = self.cx.display(h.ty);
        let class = self.cx.display(ret);
        let note = match self.cx.fn_info(d).inherits_this {
            true => format!(
                "TypeScript's TS2416: `{}` overrides a method returning `this` (the class of the object it is called on, which may be a subclass of `{class}`), so it must return `this` too",
                self.cx.fn_info(d).name.rsplit('.').next().unwrap_or_default()
            ),
            false => format!(
                "TypeScript's TS2322: `this` is the class of the object the method is called on, which may be a subclass of `{class}`; return `this`"
            ),
        };
        self.cx.error(
            Diagnostic::error(
                format!("type `{found}` is not assignable to type `this`"),
                span,
            )
            .with_note(note),
        );
    }
}

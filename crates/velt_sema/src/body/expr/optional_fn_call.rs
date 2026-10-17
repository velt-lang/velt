//! Calling an optional member of function type after a check: `if (o.onError) o.onError(e)`,
//! `o.f ? o.f(x) : d`. The member reads as its function once a check narrowed the field path
//! (`field_narrow`; the read is checked again, so a call that cleared it in between panics
//! instead of calling `null`); without such a check the call is an error, as in TypeScript
//! ("cannot invoke an object which is possibly undefined").

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyKind};

impl FnCx<'_, '_> {
    /// `object.prop(args)` where `prop` is not a method of `recv` (the checked `object`) but an
    /// optional field holding a function: the call of the function it holds, or `Err(recv)`
    /// when that is not the case.
    pub(super) fn optional_fn_member_call(
        &mut self,
        object: &ast::Expr,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.optional_fn_field(recv.ty, &prop.name) {
            return Err(recv);
        }
        let r = self.in_place_receiver(object, recv);
        let h = self.member_of(r.recv, prop, Want::Borrow, span);
        let h = self.after_receiver(r.before, h);
        let f = self.narrowed_field(object, prop, h, Want::Borrow);
        if self.cx.ty.opt_payload(f.ty).is_some() {
            let what = self.cx.ty.nothing_of(f.ty).map_or("null", |n| n.word());
            let callee = Span {
                hi: prop.span.hi,
                ..object.span
            };
            self.cx.error(
                Diagnostic::error(
                    format!("cannot call `{}`: it may be {what}", prop.name),
                    callee,
                )
                .with_note(format!(
                    "the field is optional: check it first (`if (x.{0}) x.{0}(…)`) or call it with `?.()`",
                    prop.name
                )),
            );
            self.check_args_loose(args);
            return Ok(self.error_expr(span));
        }
        Ok(self.call_value(f, args, span))
    }

    /// Is field `name` of type `t` (and not a method of it) an optional function value?
    fn optional_fn_field(&mut self, t: hir::TyId, name: &str) -> bool {
        if self.cx.ty.opt_payload(t).is_some() || self.resolve_method(t, name).is_some() {
            return false;
        }
        let field = match self.field_of(t, name) {
            Some((_, fty)) => Some(fty),
            // A field of an interface value (read through its slot).
            None => self.iface_field_of(t, name).map(|f| f.ty),
        };
        let Some(fty) = field else {
            return false;
        };
        if self.cx.ty.opt_payload(fty).is_none() {
            return false;
        }
        let Some(p) = self.present_payload(fty) else {
            return false;
        };
        matches!(self.cx.ty.kind(p), TyKind::FnPtr { .. })
            || matches!(
                self.cx.dynamic(p),
                Some(crate::any::Dynamic::Function | crate::any::Dynamic::Any)
            )
    }
}

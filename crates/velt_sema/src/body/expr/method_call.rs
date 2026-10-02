//! Building method calls on a checked receiver: direct (`Callee::Def`), virtual, interface
//! (`Dyn` / `ParamMethod`) and builtin; the receiver is converted to the declaring type and
//! used per the method's `this` mode.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::iface_call::IfaceSlot;
use super::method::Resolved;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, DefId, ExprKind as H, PassMode, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// Method call on a checked receiver.
    pub(crate) fn method_call_on(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        self.method_call_at(recv, prop, type_args, args, exp, span, false)
    }

    /// Method call on a checked receiver; `getter` for a property read of a `get` accessor.
    #[allow(clippy::too_many_arguments)] // `method_call_on` plus how the member is used
    pub(crate) fn method_call_at(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
        getter: bool,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(recv.ty) {
            self.check_args_loose(args);
            // A receiver narrowed to `never` panics before the call: the call is unreachable.
            let never = recv.ty == self.cx.ty.never;
            return if never { recv } else { self.error_expr(span) };
        }
        // Methods of a literal type are its base type's (`kind.toUpperCase()`).
        let recv = self.widen_literal_receiver(recv, &prop.name);
        if prop.name == crate::collect::DISPOSE && !getter {
            return self.explicit_dispose(recv, args, span);
        }
        let Some(r) = self.resolve_method(recv.ty, &prop.name) else {
            return self.no_method(recv, prop, args, span);
        };
        self.check_private(self.method_private_to(&r), &prop.name, prop.span);
        self.rec_method(prop.span, &r);
        if self.is_getter(&r) && !getter {
            self.cx.error(
                Diagnostic::error(
                    format!("`{}` is a getter, not a method", prop.name),
                    prop.span,
                )
                .with_note(format!("read it as a property: `.{}`", prop.name)),
            );
        }
        match r {
            Resolved::Def {
                def,
                slots,
                recv_ty,
                is_static,
            } => {
                if is_static {
                    self.cx.err(
                        format!("`{}` is a static method; call it on the type", prop.name),
                        prop.span,
                    );
                }
                self.check_generic_override(recv.ty, prop);
                self.def_method_call(recv, def, slots, recv_ty, type_args, args, exp, span)
            }
            Resolved::Virtual { def, slot, slots } => {
                let c = self.fn_callable(def, format!("method `{}`", prop.name));
                let ck = self.check_call(&c, slots, args, exp, span);
                let recv = self.receiver(recv, None, self.this_mode(def));
                let mut all = vec![recv];
                all.extend(ck.args);
                self.call_throws(def, &ck.type_args, ck.ret, span);
                let kind = H::Call {
                    callee: Callee::Virtual { slot },
                    args: all,
                };
                self.mk(kind, ck.ret, span)
            }
            Resolved::Iface {
                on_param,
                iface,
                iface_args,
                slot,
                method,
            } => {
                let s = IfaceSlot {
                    on_param,
                    iface,
                    iface_args,
                    slot,
                    method,
                };
                self.iface_call(recv, s, type_args, args, exp, span)
            }
            Resolved::Builtin(b) => self.builtin_method_call(recv, b, prop, args, exp, span),
        }
    }

    #[allow(clippy::too_many_arguments)] // receiver + resolved target + call-site parts
    pub(super) fn def_method_call(
        &mut self,
        recv: hir::Expr,
        def: DefId,
        slots: Vec<Option<TyId>>,
        recv_ty: TyId,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let name = self
            .cx
            .fn_info(def)
            .name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_string();
        let c = self.fn_callable(def, format!("method `{name}`"));
        let mut slots = slots;
        let own = slots.iter().filter(|s| s.is_none()).count();
        self.explicit_type_args(&mut slots, own, type_args, span);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.note_async_args(def, &ck.args);
        let recv = self.receiver(recv, Some(recv_ty), self.this_mode(def));
        let mut all = vec![recv];
        all.extend(ck.args);
        self.call_throws(def, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(def, ck.type_args),
            args: all,
        };
        self.mk(kind, ck.ret, span)
    }

    /// The receiver argument: upcast to the declaring type, then used per the `this` mode.
    pub(super) fn receiver(
        &mut self,
        recv: hir::Expr,
        to: Option<TyId>,
        mode: PassMode,
    ) -> hir::Expr {
        let mut recv = match to {
            Some(t) if t != recv.ty => self.coerce(recv, t),
            _ => recv,
        };
        let target = match &mut recv.kind {
            H::Upcast(inner) => &mut **inner,
            _ => &mut recv,
        };
        match mode {
            PassMode::BorrowMut => self.use_mutably(target, "call a mutating method on"),
            PassMode::Owned => self.force_move(target),
            PassMode::Copy | PassMode::Borrow => set_place_mode(target, UseMode::Borrow),
        }
        recv
    }

    fn no_method(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        if let Some((_, fty)) = self.field_of(recv.ty, &prop.name) {
            if matches!(self.cx.ty.kind(fty), TyKind::FnPtr { .. }) {
                return match self.field_access(recv, prop, Want::Borrow, span) {
                    Some(f) => self.call_value(f, args, span),
                    None => self.error_expr(span),
                };
            }
        }
        let tn = self.cx.display(recv.ty);
        let mut d = Diagnostic::error(
            format!("no method named `{}` found for type `{tn}`", prop.name),
            prop.span,
        );
        if recv.ty == self.cx.ty.str_ && prop.name == "length" {
            d = Diagnostic::error(
                "`length` is a property, not a method; write `s.length`",
                prop.span,
            );
        } else if let Some(note) = self.narrowing_note(recv.ty) {
            d = d.with_note(note);
        } else if matches!(self.cx.ty.kind(recv.ty), TyKind::Promise(..))
            && matches!(prop.name.as_str(), "then" | "catch" | "finally")
        {
            d = d.with_note(
                "promises take no callbacks: `await` the promise, inside `try`/`catch`/`finally` to handle its error",
            );
        } else if self.cx.ty.opt_payload(recv.ty).is_some() {
            d = d.with_note(format!(
                "the value may be null: use `?.{}(...)` or check `!= null` first",
                prop.name
            ));
        }
        self.cx.error(d);
        self.check_args_loose(args);
        self.error_expr(span)
    }

    /// Call a zero-argument method on a checked receiver (desugarings such as `entries()`).
    pub(crate) fn method_call_hir(
        &mut self,
        recv: hir::Expr,
        name: &str,
        span: Span,
    ) -> Option<hir::Expr> {
        let prop = ast::Ident {
            name: name.to_string(),
            span,
        };
        let h = self.method_call_on(recv, &prop, &[], &[], None, span);
        (!self.cx.ty.is_bottom(h.ty)).then_some(h)
    }
}

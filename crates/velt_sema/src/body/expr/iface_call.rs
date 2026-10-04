//! Calls of interface methods: on interface values (`Callee::Dyn`, through the vtable) and on
//! bounded generics (`Callee::ParamMethod`, static after monomorphization).
//!
//! Generic interface methods (`m<U>(...)`) have no vtable slot (a table cannot hold every
//! instantiation), so they can only be called on a `T extends I` receiver, where the call is
//! resolved statically; calling one on an interface value is an error. The method's own type
//! arguments (inferred or explicit) go into `ParamMethod::method_type_args`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use crate::body::FnCx;
use crate::defs::{IfaceMethod, ThrowSrc};
use crate::hir::{self, Callee, DefId, ExprKind as H, PassMode, TyId};
use crate::throws::ThrowCheck;

/// A resolved interface method slot.
pub(crate) struct IfaceSlot {
    pub on_param: bool,
    pub iface: DefId,
    pub iface_args: Vec<TyId>,
    pub slot: u32,
    pub method: IfaceMethod,
}

impl FnCx<'_, '_> {
    /// A generic class method overridden in a subclass of the receiver's static class would
    /// need dynamic dispatch of a generic method (no vtable slot): reject the call.
    pub(super) fn check_generic_override(&mut self, recv: TyId, prop: &ast::Ident) {
        let Some((d, _)) = self.cx.class_of(recv) else {
            return;
        };
        let below = self
            .cx
            .generic_overrides
            .iter()
            .find(|(c, key)| *key == prop.name && *c != d && self.cx.class_extends(*c, d))
            .map(|(c, _)| *c);
        let Some(sub) = below else { return };
        let (cn, sn) = (
            self.cx.display(recv),
            self.cx.adt(sub).map(|a| a.name.clone()),
        );
        self.cx.error(
            Diagnostic::error(
                format!(
                    "generic method `{}` is overridden in `{}`, so calling it on a `{cn}` would need dynamic dispatch",
                    prop.name,
                    sn.unwrap_or_default()
                ),
                prop.span,
            )
            .with_note("generic methods are dispatched statically; call it on the concrete class, or make the method non-generic"),
        );
    }

    /// `recv.m<type_args>(args)` for an interface method.
    pub(super) fn iface_call(
        &mut self,
        recv: hir::Expr,
        s: IfaceSlot,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let own = s.method.generics.len();
        if own > 0 && !s.on_param {
            let tn = self.cx.display(recv.ty);
            let iface = self
                .cx
                .iface(s.iface)
                .map_or(tn.clone(), |i| i.name.clone());
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "generic method `{}` cannot be called on an interface value (`{tn}`)",
                        s.method.name
                    ),
                    span,
                )
                .with_note("an interface value calls its methods through a table with one entry per method, but a generic method is compiled once for each type argument")
                .with_note(format!(
                    "make the calling function generic over the receiver, `<T extends {iface}>(x: T)` instead of `(x: {iface})`, so the call is resolved for each concrete type; or call a method of `{iface}` that is not generic"
                )),
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let n = s.iface_args.len();
        let mut slots: Vec<Option<TyId>> = s.iface_args.iter().map(|t| Some(*t)).collect();
        slots.push(Some(recv.ty));
        slots.resize(n + 1 + own, None);
        self.explicit_type_args(&mut slots, own, type_args, span);
        let mut slot_names: Vec<String> = (0..=n).map(|k| format!("T{k}")).collect();
        slot_names.extend(s.method.generics.names.iter().cloned());
        let mut bounds = vec![vec![]; n + 1];
        bounds.extend(s.method.generics.bounds.iter().cloned());
        let c = Callable {
            what: format!("method `{}`", s.method.name),
            params: s.method.params.clone(),
            ret: s.method.ret,
            slot_names,
            bounds,
            js_numbers: false,
            rest: false,
            defaults: vec![],
        };
        let ck = self.check_call(&c, slots, args, exp.filter(|_| own > 0), span);
        let mode = if s.method.mut_this {
            PassMode::BorrowMut
        } else {
            PassMode::Borrow
        };
        let recv = self.receiver(recv, None, mode, false);
        let mut all = vec![recv];
        all.extend(ck.args);
        let src = ThrowSrc::Slot {
            iface: s.iface,
            slot: s.slot,
            args: ck.type_args.clone(),
            span,
        };
        let ret = self.slot_call_throws(&s, src, ck.ret, span);
        let callee = if s.on_param {
            Callee::ParamMethod {
                iface: s.iface,
                iface_args: s.iface_args,
                slot: s.slot,
                method_type_args: ck.type_args.get(n + 1..).unwrap_or_default().to_vec(),
            }
        } else {
            Callee::Dyn { slot: s.slot }
        };
        let kind = H::Call { callee, args: all };
        self.mk(kind, ret, span)
    }

    /// A call through interface slot `src` (result type `ret`) may throw here; the result type.
    /// A slot of a promise group (throws/groups.rs `Group::promise`, as lowering reads it)
    /// reports its errors through the promise: the result is `Promise<T, E>` with what the slot
    /// is known to throw (checked after inference, like an async call). Other slots throw, even
    /// when the instantiated result is a promise (`get(): T` with `T = Promise<i64>`).
    fn slot_call_throws(&mut self, s: &IfaceSlot, src: ThrowSrc, ret: TyId, span: Span) -> TyId {
        let (iface, slot) = (s.iface, s.slot);
        let groups = self.cx.throw_groups();
        let promise = groups
            .slot_group(iface, slot)
            .is_some_and(|g| groups.list[g].promise);
        let payload = self.cx.ty.promise_payload(ret);
        let Some(v) = payload.filter(|_| promise) else {
            self.throw_src(src);
            return ret;
        };
        let e = crate::throws::srcs_now(self.cx, std::slice::from_ref(&src));
        self.cx.throw_checks.push(ThrowCheck {
            srcs: vec![src],
            observed: e,
            exact: true,
            span,
        });
        match e {
            Some(e) => self.cx.ty.promise_rejecting(v, e),
            None => ret,
        }
    }
}

//! Calls of interface methods: on interface values (`Callee::Dyn`, through the vtable) and on
//! bounded generics (`Callee::ParamMethod`, static after monomorphization).
//!
//! A generic interface method (`m<U>(...)`) is called with its own type arguments (inferred or
//! explicit) in `method_type_args`: on a `T extends I` receiver the call is resolved statically;
//! on an interface value, the method's vtable slot holds a table with one entry per
//! instantiation that lowering reaches (#943).

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
    /// The method's last parameter is a rest parameter (a generic function type's,
    /// `crate::generic_fns`).
    pub rest: bool,
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

    /// A value of class type `ty` (not a `new` of it, which is exactly that class) converted to
    /// interface `iface` (at `span`): when a subclass of
    /// the class overrides one of the interface's generic methods, the value may hold that
    /// subclass, whose method JavaScript would run, and a generic method has no vtable slot to
    /// find it by (issue #1132): reported, as a direct call of the method is
    /// (`check_generic_override`). True when reported.
    pub(super) fn check_generic_override_dyn(
        &mut self,
        ty: TyId,
        iface: DefId,
        span: Span,
    ) -> bool {
        let Some((d, _)) = self.cx.class_of(ty) else {
            return false;
        };
        let Some(i) = self.cx.iface(iface) else {
            return false;
        };
        let generic: Vec<String> = (i.methods.iter())
            .filter(|m| !m.generics.names.is_empty())
            .map(|m| m.name.clone())
            .collect();
        let found = (self.cx.generic_overrides.iter())
            .find(|(c, key)| generic.contains(key) && *c != d && self.cx.class_extends(*c, d))
            .cloned();
        let Some((sub, key)) = found else {
            return false;
        };
        let (cn, iname) = (self.cx.display(ty), i.name.clone());
        let sn = self.cx.adt(sub).map(|a| a.name.clone()).unwrap_or_default();
        self.cx.error(
            Diagnostic::error(
                format!("generic method `{key}` is overridden in `{sn}`, so a `{cn}` converted to `{iname}` would need dynamic dispatch"),
                span,
            )
            .with_note(format!("a call through the interface value runs the method of the class it was converted from; convert the subclass itself (a `{sn}` as a `{iname}`), or make the method non-generic"))
            .with_note(format!("generic methods are dispatched statically: `{cn}` may hold a `{sn}`, whose `{key}` would not run")),
        );
        true
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
        if own > 0 {
            let calls = &mut self.cx.generic_called;
            match s.on_param {
                true => calls.on_params.insert(s.method.name.clone()),
                false => calls.on.insert((s.iface, s.method.name.clone())),
            };
        }
        let n = s.iface_args.len();
        let mut slots: Vec<Option<TyId>> = s.iface_args.iter().map(|t| Some(*t)).collect();
        slots.push(Some(recv.ty));
        slots.resize(n + 1 + own, None);
        self.explicit_type_args(&mut slots, own, type_args, &[], span);
        let mut slot_names: Vec<String> = (0..=n).map(|k| format!("T{k}")).collect();
        slot_names.extend(s.method.generics.names.iter().cloned());
        let mut bounds = vec![vec![]; n + 1];
        bounds.extend(s.method.generics.bounds.iter().cloned());
        let c = Callable {
            what: match s.method.name == crate::generic_fns::CALL {
                // A generic function value's call (`crate::generic_fns`).
                true => "this function".into(),
                false => format!("method `{}`", s.method.name),
            },
            params: s.method.params.clone(),
            ret: s.method.ret,
            slot_names,
            bounds,
            constraints: vec![],
            js_numbers: false,
            positions: false,
            js_api: false,
            rest: s.rest,
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
        let what = format!("`{}()` called", s.method.name);
        self.note_iface_array_read(s.iface, &s.iface_args, (s.slot, what), ret, span);
        let method_type_args = ck.type_args.get(n + 1..).unwrap_or_default().to_vec();
        let callee = if s.on_param {
            Callee::ParamMethod {
                iface: s.iface,
                iface_args: s.iface_args,
                slot: s.slot,
                method_type_args,
            }
        } else {
            Callee::Dyn {
                slot: s.slot,
                method_type_args,
            }
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
        let mut srcs = vec![src];
        // What its `Promise<T>` written without an error type is given (`throws::promise_sites`).
        let never = self.cx.ty.never;
        if let Some(w) = self.cx.ty.promise_error(ret).filter(|w| *w != never) {
            srcs.push(ThrowSrc::Direct(w, span));
        }
        let e = crate::throws::srcs_now(self.cx, &srcs);
        self.cx.throw_checks.push(ThrowCheck {
            srcs,
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

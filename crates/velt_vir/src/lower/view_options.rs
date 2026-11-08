//! A `T | null` (or `T | undefined`) never holds a view of a nothing (any_view.rs): a view on
//! its way into one becomes the option's own nothing (`WrapSome`), so testing and matching an
//! option is its own test alone. Sema may still read such an option as its payload where a
//! narrowing says it holds one (`const o: Foo | null = f` narrows `o` to `Foo` even when `f`
//! held `null!`): that read gives a view of `null` again (`unwrapped_view`), so a member read
//! is Node's `TypeError` and a test sees the nothing, never a null pointer read as a `Foo`.

use velt_sema::hir::{self, Nothing, TyId, TyKind};

use super::FnLower;
use crate::vir::{BinOp, Callee, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Is `t` a pointer type that may hold a view, whose options this file keeps view-free?
    fn view_option_payload(&mut self, t: TyId) -> bool {
        self.holds_view(t) && self.cx.ty(t) == Ty::Ptr
    }

    /// `WrapSome` of the owned pointer `v` of type `inner` into the pointer-sized option `ty`:
    /// a view of a nothing becomes the option's nothing (its reference released). `None` where
    /// `inner` holds no view (the plain `WrapSome`).
    pub(super) fn wrap_some_view(&mut self, v: Operand, inner: TyId, ty: TyId) -> Option<Operand> {
        if self.dead() || self.cx.ty(ty) != Ty::Ptr || !self.view_option_payload(inner) {
            return None;
        }
        let vp = Place::local(self.temp(Ty::Ptr));
        self.assign(vp.clone(), Rvalue::Use(v));
        let nil = self.view_nothing(Operand::Copy(vp.clone()), true);
        let out = Place::local(self.temp(Ty::Ptr));
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(nil, yes, no);
        self.switch_to(yes);
        self.drop_glue(vp.clone(), inner);
        let none = self.none_value(ty);
        self.assign(out.clone(), Rvalue::Use(none));
        self.goto(join);
        self.switch_to(no);
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(vp)));
        self.goto(join);
        self.switch_to(join);
        Some(self.own_value(Operand::Copy(out), ty))
    }

    /// The payload `p` (of type `pty`) an `UnwrapSome` of the option type `oty` reads: a null
    /// pointer (the option's nothing, where a narrowing assumed a value) becomes a view of
    /// `null`, owned by a slot of the frame's temporaries, so what follows sees `null`.
    pub(super) fn unwrapped_view(&mut self, p: Operand, pty: TyId, oty: TyId) -> Operand {
        if self.dead() || pty == oty || !self.view_option_payload(pty) {
            return p;
        }
        if self.cx.view_fn().is_none() {
            return p;
        }
        let pv = self.rvalue_temp(Ty::Ptr, Rvalue::Use(p));
        let zero = super::cint(0, Ty::Ptr);
        let zero = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, pv.clone(), zero));
        // The view's owner: a `T | null` slot, whose drop releases it and skips its nothing.
        let opt = self.cx.intern(TyKind::Option(pty, Nothing::Null));
        let slot = self.temp(Ty::Ptr);
        self.assign(Place::local(slot), Rvalue::Use(super::cint(0, Ty::Ptr)));
        self.own_temp(slot, opt);
        let out = Place::local(self.temp(Ty::Ptr));
        self.assign(out.clone(), Rvalue::Use(pv));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(zero, yes, join);
        self.switch_to(yes);
        let view = self.null_view();
        self.assign(Place::local(slot), Rvalue::Use(view.clone()));
        self.assign(out.clone(), Rvalue::Use(view));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// The place of the payload an `UnwrapSome` of `oty` (at the place `p`) names as a write's
    /// target (`ety`: its type there): a null pointer there stops with Node's `TypeError`, where
    /// a narrowing assumed a value.
    pub(super) fn unwrapped_place(&mut self, p: Place, oty: TyId, ety: TyId) -> Place {
        if ety == oty {
            return p;
        }
        let payload = self.some_payload(&p, oty);
        if self.dead() || !self.view_option_payload(ety) {
            return payload;
        }
        let pv = Operand::Copy(payload.clone());
        let zero = super::cint(0, Ty::Ptr);
        let zero = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, pv, zero));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(zero, bad, ok);
        self.switch_to(bad);
        self.panic_msg("TypeError: Cannot set properties of null");
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        payload
    }

    /// Is `cond` a nothing test of a variable (sema's view chain: `IsNullish(l)`) whose type
    /// has no nothing and holds no view here? Then it is `false` with nothing to evaluate, and
    /// an `if` on it is its other branch (expr.rs), so the chain is the code it would be
    /// without the test.
    pub(super) fn no_view_test(&mut self, cond: &hir::Expr) -> bool {
        let hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::IsNullish | hir::Intrinsic::IsNull),
            args,
        } = &cond.kind
        else {
            return false;
        };
        let [arg] = args.as_slice() else {
            return false;
        };
        if !matches!(arg.kind, hir::ExprKind::Local(..)) {
            return false;
        }
        let t = self.sub(arg.ty);
        let plain = matches!(self.cx.kind(t), TyKind::Adt(..)) && !self.cx.is_dynamic(t);
        plain && !self.holds_view(t)
    }

    /// A new view of `null`, tagged (any_view.rs `any_nothing_view`).
    fn null_view(&mut self) -> Operand {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let p = Place::local(self.temp(avt));
        self.any_nothing_into(Nothing::Null, &p);
        let Some(view) = self.cx.view_fn() else {
            super::ice("a view of null without `Proxy`")
        };
        let f = self.cx.func_for_direct(view, vec![], self.mode);
        let arg = self.addr(p);
        let r = self.temp(Ty::Ptr);
        self.call(Callee::Func(f), vec![arg], Some(Place::local(r)), false);
        self.proxy_tag(Operand::Copy(Place::local(r)))
    }
}

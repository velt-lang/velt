//! Options whose payload may be a view of a nothing (any_view.rs). A `T | null` keeps such a
//! view as its payload, and `option_present` tests the payload for one, so `??`, `?.` and
//! patterns see the nothing. A `T | null | undefined`'s `null` pattern takes a view of `null`
//! but not one of `undefined` (`three_state_null`). Sema may still read such an option as its payload where a
//! narrowing says it holds one (`const o: Foo | null = f` narrows `o` to `Foo` even when `f`
//! held `null!`): that read gives a view of `null` again (`unwrapped_view`), so a member read
//! is Node's `TypeError` and a test sees the nothing, never a null pointer read as a `Foo`.

use velt_sema::hir::{self, Nothing, TyId, TyKind};

use super::FnLower;
use crate::vir::{BinOp, Callee, Operand, Place, Rvalue, Terminator, Ty, UnOp};

impl FnLower<'_, '_> {
    /// Is `t` a pointer type that may hold a view, whose options this file keeps view-free?
    fn view_option_payload(&mut self, t: TyId) -> bool {
        self.holds_view(t) && self.cx.ty(t) == Ty::Ptr
    }

    /// The `null` of a `T | null | undefined` (`ty`, at `p`) as a pattern tests it (`Some(None)`,
    /// sema's `=== null` on it): its `T | null` holds `null` itself or a view of `null`, not a
    /// view of `undefined` (`undefined!` given to the `T | null`), which is its `undefined`.
    pub(super) fn three_state_null(&mut self, p: &Place, ty: TyId) -> Option<Operand> {
        let TyKind::Option(inner, Nothing::Undefined) = self.cx.kind(ty) else {
            return None;
        };
        let TyKind::Option(e, Nothing::Null) = self.cx.kind(inner) else {
            return None;
        };
        if self.cx.ty(inner) != Ty::Ptr || !self.view_option_payload(e) {
            return None;
        }
        let outer = self.option_is_some(p, ty);
        let ip = self.some_payload(p, ty);
        let inner_some = self.option_is_some(&ip, inner);
        let null = self.view_nothing(Operand::Copy(ip.clone()), false);
        let none = self.rvalue_temp(Ty::Bool, Rvalue::Unary(UnOp::Not, inner_some));
        let either = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitOr, none, null));
        Some(self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, outer, either)))
    }

    /// The payload `p` (of type `pty`) an `UnwrapSome` of the option type `oty` reads: a null
    /// pointer (the option's nothing, where a narrowing assumed a value) becomes a view of that
    /// nothing, owned by a slot of the frame's temporaries, so what follows sees it.
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
        let n = match self.cx.kind(oty) {
            TyKind::Option(_, Nothing::Undefined) => Nothing::Undefined,
            _ => Nothing::Null,
        };
        let view = self.nothing_view(n);
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
        // A class instance or an object type (sema's `may_hold_nothing`), never a union, whose
        // members may hold a nothing of their own.
        let plain = match self.cx.kind(t) {
            TyKind::Adt(d, _) => matches!(
                self.cx.hir.def(d),
                hir::Def::Adt(a) if matches!(a.kind, hir::AdtKind::Class | hir::AdtKind::Anon)
            ),
            _ => false,
        };
        plain && !self.cx.is_dynamic(t) && !self.holds_view(t)
    }

    /// A new view of the nothing `n`, tagged (any_view.rs `any_nothing_view`).
    fn nothing_view(&mut self, n: Nothing) -> Operand {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let p = Place::local(self.temp(avt));
        self.any_nothing_into(n, &p);
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

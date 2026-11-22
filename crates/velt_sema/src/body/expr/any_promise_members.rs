//! `then`, `catch` and `finally` of a promise held as `any` (#924 step 2). A member of that name
//! read or called through an `any` in the program's own code tests the value: a promise's goes to
//! std (`std/lang/hooks/any_promise.velt`, through the hooks `anyPromiseMethod` and
//! `anyPromiseCall`), anything else is read or called as before (`AnyGet`, `AnyCall`), so a
//! class's own `then` is still its method. Only these three names, written as such: a program
//! that writes none of them gets none of this. A computed key (`v[k]`) is not tested.

use velt_common::Span;

use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, TyId, TyKind, UseMode};
use crate::hook_calls::{self as hooks, ty, HookTy};

/// The members of a promise a program can use through `any`.
pub(super) const PROMISE_MEMBERS: [&str; 3] = ["then", "catch", "finally"];

impl FnCx<'_, '_> {
    /// Does a member `name` read or called here on `recv`, as the code gives it, go through the
    /// promise test? Only on an `any` or `unknown` value: not on a typed value read through
    /// `any` (a brand, a narrowed or generic receiver), nor on a cast of a typed one (`(r as
    /// any).then`, whose type sema knows: `thenable_test.rs`).
    pub(super) fn promise_member_named(&self, name: &str, recv: &hir::Expr) -> bool {
        PROMISE_MEMBERS.contains(&name)
            && !self.cx.scopes[self.module].is_std
            && matches!(
                self.cx.dynamic(recv.ty),
                Some(Dynamic::Any | Dynamic::Unknown)
            )
            && !matches!(recv.kind, H::ToDyn { .. })
    }

    /// `obj.name` (`name` one of [`PROMISE_MEMBERS`]) on the `any` `obj`, the read being `get`:
    /// `{ let t = obj; AnyIsPromise(t) ? anyPromiseMethod(name) : get(t) }`. The promise's is
    /// one shared function per name, whose `this` is the receiver (`p.then === p.then`, and
    /// `const t = v.then; t.call(v, f)` works, as in Node).
    pub(super) fn promise_member_get(
        &mut self,
        obj: hir::Expr,
        name: &str,
        any: TyId,
        span: Span,
        get: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        self.demand_promise_members();
        let obj = self.share_if_place(obj);
        let t = self.new_local("<held>", any, false, span, LocalKind::Temp);
        let read = |s: &mut Self| s.mk(H::Local(t, UseMode::Borrow), any, span);
        let held = read(self);
        let test = self.held_promise_test(held, span);
        let key = self.mk(H::Lit(Lit::Str(name.to_string())), self.cx.ty.str_, span);
        let promise = hooks::any_promise_method(ty::Str::of(key)).emit(self, any, span);
        let held = read(self);
        let other = get(self, held);
        let pick = self.mk(
            H::If {
                cond: Box::new(test),
                then: Box::new(promise),
                els: Box::new(other),
            },
            any,
            span,
        );
        self.block_with(vec![(t, obj)], pick, any, span)
    }

    /// `recv.name(args)` (`name` one of [`PROMISE_MEMBERS`]) on the `any` `recv`, `args` the
    /// arguments already boxed and `call` the call as before, given the receiver and the
    /// arguments: the arguments evaluated once, then `AnyIsPromise(t) ? anyPromiseCall(t, name,
    /// [args]) : call(t, args)`.
    pub(super) fn promise_member_call(
        &mut self,
        recv: hir::Expr,
        name: &str,
        args: Vec<hir::Expr>,
        any: TyId,
        span: Span,
        call: impl FnOnce(&mut Self, hir::Expr, Vec<hir::Expr>) -> hir::Expr,
    ) -> hir::Expr {
        self.demand_promise_members();
        let recv = self.share_if_place(recv);
        let t = self.new_local("<held>", any, false, span, LocalKind::Temp);
        let mut lets = vec![(t, recv)];
        let mut locals = vec![];
        for a in args {
            let (aty, aspan) = (a.ty, a.span);
            let l = self.new_local("<arg>", aty, false, aspan, LocalKind::Temp);
            locals.push((l, aty, aspan));
            lets.push((l, a));
        }
        let read = |s: &mut Self| s.mk(H::Local(t, UseMode::Borrow), any, span);
        let held = read(self);
        let test = self.held_promise_test(held, span);
        let unknown = match self.cx.any {
            Some(defs) => self.cx.ty.intern(TyKind::Dyn(defs.unknown, vec![])),
            None => any,
        };
        let held = read(self);
        let held = match self.coerce_dynamic(held, unknown, false) {
            Ok(h) | Err(h) => h,
        };
        let key = self.mk(H::Lit(Lit::Str(name.to_string())), self.cx.ty.str_, span);
        let moved = |s: &mut Self, (l, aty, aspan): (hir::LocalId, TyId, Span)| {
            s.mk(H::Local(l, UseMode::Move), aty, aspan)
        };
        let list_ty = self.cx.ty.intern(TyKind::Array(any));
        // A spread (`p.then(...fs)`): `dynamic_args` gives its `any[]` as the one argument.
        let spread = matches!(locals.as_slice(), [(_, t, _)] if matches!(self.cx.ty.kind(*t), TyKind::Array(_)));
        let list = match spread {
            true => {
                let (l, aty, aspan) = locals[0];
                let v = self.mk(H::Local(l, UseMode::Borrow), aty, aspan);
                self.intrinsic(Intrinsic::Share, vec![v], aty, aspan)
            }
            false => {
                let list: Vec<hir::Expr> = locals.iter().map(|&a| moved(self, a)).collect();
                self.mk(H::ArrayLit(list), list_ty, span)
            }
        };
        let promise =
            hooks::any_promise_call(ty::Unknown::of(held), ty::Str::of(key), ty::Array::of(list))
                .emit(self, any, span);
        let held = read(self);
        let args: Vec<hir::Expr> = locals.iter().map(|&a| moved(self, a)).collect();
        let other = call(self, held, args);
        let pick = self.mk(
            H::If {
                cond: Box::new(test),
                then: Box::new(promise),
                els: Box::new(other),
            },
            any,
            span,
        );
        self.block_with(lets, pick, any, span)
    }

    /// Loads std/lang/hooks/any_promise.velt, the hooks' module, which no program loads by default
    /// (`ON_DEMAND_MODULES`): only a program whose code uses one of these members gets it.
    fn demand_promise_members(&mut self) {
        self.cx
            .demand_std(velt_common::std_module!(lang_hooks_any_promise));
    }

    /// `AnyIsPromise(v)` of the borrowed dynamic `v`.
    fn held_promise_test(&mut self, v: hir::Expr, span: Span) -> hir::Expr {
        let b = self.cx.ty.bool_;
        debug_assert!(matches!(
            self.cx.dynamic(v.ty),
            Some(Dynamic::Any | Dynamic::Unknown)
        ));
        self.intrinsic(Intrinsic::AnyIsPromise, vec![v], b, span)
    }

    /// `{ let l1 = e1; …; value }`.
    fn block_with(
        &mut self,
        lets: Vec<(hir::LocalId, hir::Expr)>,
        value: hir::Expr,
        ty: TyId,
        span: Span,
    ) -> hir::Expr {
        let stmts = lets
            .into_iter()
            .map(|(local, init)| hir::Stmt {
                kind: hir::StmtKind::Let {
                    local,
                    init: Some(init),
                },
                span,
            })
            .collect();
        let b = super::accessor_rmw::block(stmts, Some(value), span);
        self.mk(H::Block(b), ty, span)
    }
}

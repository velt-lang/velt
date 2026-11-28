//! `then`, `catch` and `finally` of a promise held as `any` (#924 step 2). A member of that name
//! read or called through an `any` in the program's own code tests the value: a promise's goes to
//! std (`std/lang/hooks/any_promise.velt`, through the hooks `anyPromiseMethod` and
//! `anyPromiseCall`), anything else is read or called as before (`AnyGet`, `AnyCall`), so a
//! class's own `then` is still its method. Only these three names, written as such: a program
//! that writes none of them gets none of this. A computed key (`v[k]`) is not tested.

use velt_common::Span;

use velt_syntax::ast;

use super::setters;
use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, TyId, TyKind, UseMode};
use crate::hook_calls::{self as hooks, ty, HookTy};

/// The members of a promise a program can use through `any`.
pub(super) const PROMISE_MEMBERS: [&str; 3] = ["then", "catch", "finally"];

impl FnCx<'_, '_> {
    /// Does a member `name` read or called here on `recv`, as the code gives it, go through the
    /// promise test? Only on an `any` or `unknown` value, not on a typed value read through
    /// `any` (a brand, a narrowed or generic receiver). A cast to `any` (`(p as any).then`,
    /// `(x as any).then` of a `T`) is such a value, unless its type can't hold a promise.
    pub(super) fn promise_member_named(&self, name: &str, recv: &hir::Expr) -> bool {
        PROMISE_MEMBERS.contains(&name)
            && !self.cx.scopes[self.module].is_std
            && matches!(
                self.cx.dynamic(recv.ty),
                Some(Dynamic::Any | Dynamic::Unknown)
            )
            && !self.cast_of_no_promise(recv)
    }

    /// Is `recv` a cast to `any` of a value whose type can't be a promise (a primitive, an array,
    /// a map, a tuple, a function)? Its `AnyIsPromise` would be false.
    fn cast_of_no_promise(&self, recv: &hir::Expr) -> bool {
        let H::ToDyn { expr, .. } = &recv.kind else {
            return false;
        };
        matches!(
            self.cx.ty.kind(expr.ty),
            TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Str
                | TyKind::Unit
                | TyKind::Literal(_)
                | TyKind::Symbol
                | TyKind::Simd(_)
                | TyKind::Array(_)
                | TyKind::Map(..)
                | TyKind::Tuple(_)
                | TyKind::FnPtr { .. }
                | TyKind::Closure(_)
        )
    }

    /// `obj.name` (`name` one of [`PROMISE_MEMBERS`]) on the `any` `obj`, the read being `get`:
    /// `{ let t = obj; AnyIsPromise(t) ? anyPromiseMethod(name) : get(t) }`. The promise's is
    /// one shared function per name, so `p.then === p.then` as in Node. It is read without its
    /// receiver: called later (`const t = v.then; t.call(v, f)`), it stops with a TypeError.
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

    /// `recv.name?.(args)` and `recv?.name?.(args)` (`name` one of [`PROMISE_MEMBERS`]) where
    /// `recv`, any expression, is an `any` / `unknown`: `recv` held, `undefined` if it is
    /// `null` / `undefined` and the member read is optional; then on a promise the call
    /// `recv.name(args)` (its member is never missing), on anything else the member read,
    /// `undefined` if it is `null` / `undefined`, else called as before (`AnyApply`). `None` for
    /// any other optional call.
    pub(super) fn promise_member_optional_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        self.promise_member_optional_then(callee, args, span, |_, called| called)
    }

    /// Is `callee`, called as `callee?.(…)`, `recv.name` or `recv?.name` with `name` one of
    /// [`PROMISE_MEMBERS`] and `recv` an `any` / `unknown`, in the program's own code? `recv`
    /// is typed on trial (nothing kept) to choose, and checked for real where it is used.
    pub(super) fn promise_optional_callee(&mut self, callee: &ast::Expr) -> bool {
        let ast::ExprKind::Member { object, prop, .. } = &callee.kind else {
            return false;
        };
        if !PROMISE_MEMBERS.contains(&prop.name.as_str()) || self.cx.scopes[self.module].is_std {
            return false;
        }
        self.trial_type(object)
            .is_some_and(|t| matches!(self.cx.dynamic(t), Some(Dynamic::Any | Dynamic::Unknown)))
    }

    /// [`Self::promise_member_optional_call`] with `rest` applied to the call's value: the rest of
    /// an optional chain the call is a link of (`p.then?.(f).then(g)`, `chain.rs`), skipped
    /// with the call when the member is missing. `rest`'s value is the result's type.
    pub(super) fn promise_member_optional_then(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
        rest: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> Option<hir::Expr> {
        if !self.promise_optional_callee(callee) {
            return None;
        }
        let ast::ExprKind::Member {
            object,
            prop,
            optional: read_optional,
        } = &callee.kind
        else {
            return None;
        };
        // `a?.b.then?.(f)`: short-circuited at `a?.`, the rest (`<a>.b.then?.(f)`) back here.
        let call = setters::synth(
            ast::ExprKind::Call {
                callee: Box::new(callee.clone()),
                type_args: vec![],
                args: args.to_vec(),
                optional: true,
            },
            span,
        );
        if let Some(h) = self.short_circuit_call(&call, None) {
            return Some(h);
        }
        let recv = self.receiver_expr(object);
        let (recv, any) = match self.dynamic_receiver(recv, Some(Some(&prop.name))) {
            Ok((recv, Some(any))) => (recv, any),
            Ok((_, None)) => {
                // Reported by `dynamic_receiver` (an `unknown` not narrowed).
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
            Err(_) => panic!(
                "ICE: the receiver of `{}?.()` typed dynamic on trial but typed when checked",
                prop.name
            ),
        };
        // In Node's order: the receiver (and its test for `null` / `undefined` in `recv?.`), its
        // member, the member's test, and only then the arguments and the call.
        let str_ = self.cx.ty.str_;
        let text = format!("{}.{}", self.callee_text(&recv), prop.name);
        let recv = self.share_if_place(recv);
        let t = self.new_local("<held>", any, false, span, LocalKind::Temp);
        let held = |s: &mut Self| s.mk(H::Local(t, UseMode::Borrow), any, span);
        let get = move |s: &mut Self, h: hir::Expr| {
            let key = s.key_lit_any(&prop.name, span);
            s.any_op(Intrinsic::AnyGet, vec![h, key], any, span)
        };
        let h = held(self);
        let member = self.promise_member_get(h, &prop.name, any, span, get);
        let m = self.new_local("<member>", any, false, span, LocalKind::Temp);
        let read = |s: &mut Self| s.mk(H::Local(m, UseMode::Borrow), any, span);
        let r = read(self);
        let missing = self.dynamic_null_compare(r, "null", false, true, span);
        let Some(boxed) = self.dynamic_args(args, any, false) else {
            return Some(self.error_expr(span));
        };
        self.note_any_call();
        let apply = move |s: &mut Self, _held: hir::Expr, boxed: Vec<hir::Expr>| {
            let text = s.mk(H::Lit(Lit::Str(text)), str_, span);
            let mut all = vec![read(s), text];
            all.extend(boxed);
            s.any_call_op(Intrinsic::AnyApply, all, any, span)
        };
        let h = held(self);
        let called = self.promise_member_call(h, &prop.name, boxed, any, span, apply);
        let present = rest(self, called);
        let ty = present.ty;
        let pick = self.pick_undefined(missing, present, span);
        let mut value = self.block_with(vec![(m, member)], pick, ty, span);
        if *read_optional {
            let h = held(self);
            let nothing = self.dynamic_null_compare(h, "null", false, true, span);
            value = self.pick_undefined(nothing, value, span);
        }
        Some(self.block_with(vec![(t, recv)], value, ty, span))
    }

    /// `missing ? undefined : present`, of `present`'s type.
    fn pick_undefined(&mut self, missing: hir::Expr, present: hir::Expr, span: Span) -> hir::Expr {
        let any = present.ty;
        let undefined = setters::synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
        let undefined = self.expr(&undefined, Some(any), Want::Move);
        self.mk(
            H::If {
                cond: Box::new(missing),
                then: Box::new(undefined),
                els: Box::new(present),
            },
            any,
            span,
        )
    }

    /// The string literal `name`.
    fn key_lit_any(&mut self, name: &str, span: Span) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        self.mk(H::Lit(Lit::Str(name.to_string())), str_, span)
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
    pub(super) fn block_with(
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

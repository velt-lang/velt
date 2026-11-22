//! `await v` of a value that is no promise (`await null`, `await 5`): JavaScript takes one step
//! (the rest of the function runs as a microtask) and the result is `v` itself. It is
//! `{ const <awaited> = v; await Promise.resolve(); <awaited> }`; `await null;` and
//! `await undefined;` as statements are the step alone. An `any` or `unknown` may hold a
//! promise, so its `await` goes through std's `awaitAny` (#924, `await_dynamic`).

use velt_common::Span;
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind};
use crate::ctx::Item;
use crate::hir::{self, ExprKind as H, TyId, TyKind};
use crate::hook_calls::{self as hooks, ty, HookTy};

impl FnCx<'_, '_> {
    /// The step of an `await` of a value: `await Promise.resolve()` (also at the top level).
    pub(crate) fn await_step(&mut self, span: Span) -> hir::Expr {
        let Some(Item::Def(d)) = self.cx.prelude.get("promiseResolveVoid").cloned() else {
            panic!("ICE: the prelude has no `promiseResolveVoid`");
        };
        let mut report = |s: &mut Self, _: &str, expected: TyId, found: &hir::Expr| {
            s.report_mismatch(expected, found);
        };
        let call = self.call_checked(d, vec![], span, &mut report);
        let unit = self.cx.ty.unit;
        self.awaiting(call, unit, span)
    }

    /// `await x` of an `any` or `unknown` `h` (#924): std's `awaitAny(x)` awaited, which is the
    /// promise `x` holds (converted to `unknown` as it settles, no step more than awaiting it) or
    /// `x` itself after one step. The value is an `unknown`, an `any` again when `x` is one, as
    /// TypeScript types it; a rejection is thrown as an `unknown`. A thenable stops first
    /// (`thenable_stop`). `{ let <awaited> = x; thenable_stop; await awaitAny(<awaited>) }`.
    pub(super) fn await_dynamic(&mut self, h: hir::Expr, d: Dynamic, span: Span) -> hir::Expr {
        let Some(defs) = self.cx.any else {
            return h;
        };
        let unknown = self.cx.ty.intern(TyKind::Dyn(defs.unknown, vec![]));
        let x = match self.coerce_dynamic(h, unknown, false) {
            Ok(x) | Err(x) => x,
        };
        let l = self.new_local("<awaited>", unknown, false, span, LocalKind::Temp);
        let stop = self.thenable_stop(l, unknown, span);
        let held = self.mk(H::Local(l, hir::UseMode::Borrow), unknown, span);
        let promise = self.cx.ty.intern(TyKind::Promise(unknown, unknown));
        let call = hooks::await_any(ty::Unknown::of(held)).emit(self, promise, span);
        self.await_throws(&call);
        let v = self.awaiting(call, unknown, span);
        let v = match d {
            Dynamic::Any => {
                let any = self.cx.ty.intern(TyKind::Dyn(defs.any, vec![]));
                match self.coerce_dynamic(v, any, false) {
                    Ok(v) | Err(v) => v,
                }
            }
            _ => v,
        };
        let vty = v.ty;
        let stmts = vec![
            hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: l,
                    init: Some(x),
                },
                span,
            },
            hir::Stmt {
                kind: hir::StmtKind::Expr(stop),
                span,
            },
        ];
        let b = super::accessor_rmw::block(stmts, Some(v), span);
        self.mk(H::Block(b), vty, span)
    }

    /// `if (!AnyIsPromise(t) && anyHoldsObject(t) && "then" in t) panic(…)` on the `unknown`
    /// local `t`: JavaScript adopts a thenable, which Velt doesn't yet (#3412), so it stops with
    /// a TypeError rather than settling with the object itself. Made here, in the program's own
    /// function: std called by name doesn't give the program the member slots `in` reads.
    fn thenable_stop(&mut self, t: hir::LocalId, unknown: TyId, span: Span) -> hir::Expr {
        use hir::{Intrinsic as I, LogicOp, UseMode};
        let (b, unit, never) = (self.cx.ty.bool_, self.cx.ty.unit, self.cx.ty.never);
        let read = |s: &mut Self| s.mk(H::Local(t, UseMode::Borrow), unknown, span);
        let as_any = |s: &mut Self| {
            let v = read(s);
            match s.any_ty() {
                Some(any) => match s.coerce_dynamic(v, any, false) {
                    Ok(v) | Err(v) => v,
                },
                None => v,
            }
        };
        let held = read(self);
        let promise = self.intrinsic(I::AnyIsPromise, vec![held], b, span);
        let not_promise = self.negated(promise, true, span);
        let obj = as_any(self);
        let object = hooks::any_holds_object(ty::Any::of(obj)).emit(self, b, span);
        let (obj, key) = (as_any(self), self.str_lit("then", span));
        // `in` reads the tables' member slots, which the program then has (`note_dyn_members`).
        self.note_dyn_members();
        let has = self.any_op(I::AnyHas, vec![obj, key], b, span);
        let and = |s: &mut Self, lhs: hir::Expr, rhs: hir::Expr| {
            let kind = H::Logical {
                op: LogicOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
            s.mk(kind, b, span)
        };
        let test = and(self, not_promise, object);
        let test = and(self, test, has);
        let msg = "TypeError: adopting an object with a `then` member is not supported yet (#3412)";
        let msg = self.str_lit(msg, span);
        let stop = self.intrinsic(I::Panic, vec![msg], never, span);
        let then = super::accessor_rmw::block(vec![], Some(stop), span);
        let els = super::accessor_rmw::block(vec![], None, span);
        let kind = H::If {
            cond: Box::new(test),
            then: Box::new(self.mk(H::Block(then), unit, span)),
            els: Box::new(self.mk(H::Block(els), unit, span)),
        };
        self.mk(kind, unit, span)
    }

    /// Does `return u` of a dynamic `u` in an async function returning `ret` go through
    /// `adopt_dynamic` (#924)? A dynamic result adopts; a typed one, `void` too, tests for a held
    /// promise (`typed_dynamic_return`); not `never`.
    pub(crate) fn adopts_dynamic(&self, ret: TyId) -> bool {
        let t = &self.cx.ty;
        ret != t.never && !t.has_error(ret)
    }

    /// `return u` of the `any` or `unknown` `x` in an async function whose result `ret` is
    /// typed, `void` included (#924): a promise `u` holds is not adopted yet (#3475; adopting
    /// would add its `unknown` rejection to the function's error type), so it stops with a
    /// TypeError; else the value converts as a `return` of it in a plain function does (no
    /// `thenable_stop` here: its `in` would add a proxy's `has` trap to the error type).
    /// `{ let <returned> = x; if (AnyIsPromise(<returned>)) panic(…); <returned> }`.
    fn typed_dynamic_return(&mut self, x: hir::Expr, ret: TyId, span: Span) -> hir::Expr {
        let (b, unit, never) = (self.cx.ty.bool_, self.cx.ty.unit, self.cx.ty.never);
        let xty = x.ty;
        let l = self.new_local("<returned>", xty, false, span, LocalKind::Temp);
        let held = self.mk(H::Local(l, hir::UseMode::Borrow), xty, span);
        let promise = self.intrinsic(hir::Intrinsic::AnyIsPromise, vec![held], b, span);
        let msg = "TypeError: adopting a promise held in `any` into a typed async result is not supported yet (#3475)";
        let msg = self.str_lit(msg, span);
        let stop = self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span);
        let then = super::accessor_rmw::block(vec![], Some(stop), span);
        let els = super::accessor_rmw::block(vec![], None, span);
        let kind = H::If {
            cond: Box::new(promise),
            then: Box::new(self.mk(H::Block(then), unit, span)),
            els: Box::new(self.mk(H::Block(els), unit, span)),
        };
        let refused = self.mk(kind, unit, span);
        // `void`: the value is dropped (at the block's end), as `return u;` there drops it.
        let value = match ret == unit {
            true => self.mk(H::Lit(hir::Lit::Unit), unit, span),
            false => {
                let v = self.mk(H::Local(l, hir::UseMode::Move), xty, span);
                self.coerce(v, ret)
            }
        };
        let stmt = |kind| hir::Stmt { kind, span };
        let stmts = vec![
            stmt(hir::StmtKind::Let {
                local: l,
                init: Some(x),
            }),
            stmt(hir::StmtKind::Expr(refused)),
        ];
        let b = super::accessor_rmw::block(stmts, Some(value), span);
        self.mk(H::Block(b), ret, span)
    }

    /// `return u` in an async function returning `ret`, `u` an `any` or `unknown` (#924): the
    /// promise `u` holds is adopted as a returned promise is (`adopt`: once the scopes are
    /// left, Node's extra steps, its rejection thrown), else `u` is the value; either converts
    /// to a typed `ret` with a check, as `u as R` does. A thenable stops (#3412).
    /// `{ let <returned> = u; match AnyPromise(<returned>) { p => adopt(p), _ =>
    /// notThenable(<returned>) } }`.
    pub(crate) fn adopt_dynamic(&mut self, h: hir::Expr, ret: TyId, span: Span) -> hir::Expr {
        use hir::{PatKind as P, UseMode};
        let Some(defs) = self.cx.any else {
            return h;
        };
        if !matches!(self.cx.dynamic(ret), Some(Dynamic::Any | Dynamic::Unknown)) {
            return self.typed_dynamic_return(h, ret, span);
        }
        let unknown = self.cx.ty.intern(TyKind::Dyn(defs.unknown, vec![]));
        let x = match self.coerce_dynamic(h, unknown, false) {
            Ok(x) | Err(x) => x,
        };
        let l = self.new_local("<returned>", unknown, false, span, LocalKind::Temp);
        let promise = self.cx.ty.intern(TyKind::Promise(unknown, unknown));
        let opt = self.cx.ty.option(promise);
        let held = self.mk(H::Local(l, UseMode::Borrow), unknown, span);
        let test = self.intrinsic(hir::Intrinsic::AnyPromise, vec![held], opt, span);
        let b = self.new_local("<held>", promise, false, span, LocalKind::Bind);
        let bind = self.pat(P::Binding(b, UseMode::Move), promise, span);
        let some = self.pat(P::Some(Box::new(bind)), opt, span);
        let p = self.mk(H::Local(b, UseMode::Move), promise, span);
        self.await_throws(&p);
        // One arm adopts, the other returns a value: still a `return` (as a union holding a
        // promise is, `await_union_with`), not a statement that only adopts (`adopt_stmt`).
        let adopted = self.adopt(p, ret, span);
        // A value that is no promise: a thenable stops (#3412), else it is the result.
        let stop = self.thenable_stop(l, unknown, span);
        let value = self.mk(H::Local(l, UseMode::Move), unknown, span);
        let value = self.coerce(value, ret);
        let stop = hir::Stmt {
            kind: hir::StmtKind::Expr(stop),
            span,
        };
        let value = super::accessor_rmw::block(vec![stop], Some(value), span);
        let value = self.mk(H::Block(value), ret, span);
        let none = self.pat(P::None, opt, span);
        let arms = vec![
            hir::Arm {
                pat: some,
                guard: None,
                body: adopted,
            },
            hir::Arm {
                pat: none,
                guard: None,
                body: value,
            },
        ];
        let m = H::Match {
            scrutinee: Box::new(test),
            arms,
        };
        let m = self.mk(m, ret, span);
        let stmt = hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(x),
            },
            span,
        };
        let b = super::accessor_rmw::block(vec![stmt], Some(m), span);
        self.mk(H::Block(b), ret, span)
    }

    /// `await v` of the checked value `h`, which is no promise and holds none.
    pub(super) fn await_plain(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        let (ty, vspan) = (h.ty, h.span);
        let step = self.await_step(span);
        let l = self.new_local("<awaited>", ty, false, vspan, LocalKind::Temp);
        let stmt = |kind| hir::Stmt { kind, span };
        let stmts = vec![
            stmt(hir::StmtKind::Let {
                local: l,
                init: Some(h),
            }),
            stmt(hir::StmtKind::Expr(step)),
        ];
        let value = self.read_temp(l, ty, vspan);
        let b = super::accessor_rmw::block(stmts, Some(value), span);
        self.mk(H::Block(b), ty, span)
    }

    /// Can a value of type `ty` (no promise and no union with one) be awaited as itself: not
    /// dynamic (an `any` may hold a promise, and so may a type parameter's value) and without a
    /// `then` method (a thenable)?
    pub(super) fn awaits_as_value(&mut self, ty: TyId) -> bool {
        !self.cx.ty.has_error(ty)
            && !self.cx.ty.is_bottom(ty)
            && self.cx.dynamic(ty).is_none()
            && !matches!(self.cx.ty.kind(ty), TyKind::Param(_))
            && !self.method_exists(ty, "then")
    }
}

/// `null` or `undefined`, possibly parenthesized: the operand of `await null`.
pub(crate) fn nothing_lit(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined) => true,
        ast::ExprKind::Paren(inner) => nothing_lit(inner),
        _ => false,
    }
}

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
    /// TypeScript types it; a rejection is thrown as an `unknown`.
    pub(super) fn await_dynamic(&mut self, h: hir::Expr, d: Dynamic, span: Span) -> hir::Expr {
        let Some(defs) = self.cx.any else {
            return h;
        };
        let unknown = self.cx.ty.intern(TyKind::Dyn(defs.unknown, vec![]));
        let x = match self.coerce_dynamic(h, unknown, false) {
            Ok(x) | Err(x) => x,
        };
        let promise = self.cx.ty.intern(TyKind::Promise(unknown, unknown));
        let call = hooks::await_any(ty::Unknown::of(x)).emit(self, promise, span);
        self.await_throws(&call);
        let v = self.awaiting(call, unknown, span);
        match d {
            Dynamic::Any => {
                let any = self.cx.ty.intern(TyKind::Dyn(defs.any, vec![]));
                match self.coerce_dynamic(v, any, false) {
                    Ok(v) | Err(v) => v,
                }
            }
            _ => v,
        }
    }

    /// `return u` in an async function whose result is `any` or `unknown` (`ret`), `u` an
    /// `any` or `unknown` (#924): the promise `u` holds is adopted as a returned promise is
    /// (`adopt`: once the scopes are left, Node's extra steps, its rejection thrown), else `u`
    /// is the value. `{ let <returned> = u; match AnyPromise(<returned>) { p => adopt(p), _ =>
    /// <returned> } }`.
    pub(super) fn adopt_dynamic(&mut self, h: hir::Expr, ret: TyId, span: Span) -> hir::Expr {
        use hir::{PatKind as P, UseMode};
        let Some(defs) = self.cx.any else {
            return h;
        };
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
        let value = self.mk(H::Local(l, UseMode::Move), unknown, span);
        let value = self.coerce(value, ret);
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

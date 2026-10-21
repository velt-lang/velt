//! `p.then(f)`, `p.then(f, g)`, `p.catch(g)` and `p.finally(h)` on a promise: calls of the
//! prelude's `promiseThen` family (std/prelude/promise.vlt), which take the promise. A callback
//! that returns a promise selects the `…Async` form, which awaits it (flattening, as in JS).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, PassMode, TyId, TyKind};

/// Is `name` a method a promise has (`then`, `catch`, `finally`)?
pub(crate) fn is_promise_callback_method(name: &str) -> bool {
    matches!(name, "then" | "catch" | "finally")
}

/// Is `name` one of the prelude functions these methods compile to?
pub(crate) fn is_promise_callback_fn(name: &str) -> bool {
    matches!(
        name,
        "promiseThen"
            | "promiseThenThrows"
            | "promiseThenAsync"
            | "promiseThenAsyncThrows"
            | "promiseThenElse"
            | "promiseThenElseAsync"
            | "promiseCatch"
            | "promiseCatchAsync"
            | "promiseFinally"
            | "promiseFinallyThrows"
            | "promiseFinallyAsync"
            | "promiseFinallyAsyncThrows"
    )
}

impl FnCx<'_, '_> {
    /// What a callback of type `t` gives (its promise's value when it returns one) and what it
    /// throws (its promise's error type too; `None`: nothing).
    fn callback_parts(&mut self, t: TyId) -> (TyId, Option<TyId>) {
        let TyKind::FnPtr { ret, throws, .. } = *self.cx.ty.kind(t) else {
            return (t, None);
        };
        match *self.cx.ty.kind(ret) {
            TyKind::Promise(v, e) => (v, self.cx.error_union(&[e, throws])),
            _ => (ret, self.cx.error_union(&[throws])),
        }
    }

    /// Is `h` a `p.then(…)` / `catch` / `finally` call? Its promise may be dropped (`p.then(f);`
    /// as a statement): like JS, it runs to completion, and a rejection nobody handles is
    /// reported as uncaught.
    pub(crate) fn is_promise_callback_call(&self, e: &ast::Expr, h: &hir::Expr) -> bool {
        let ast::ExprKind::Call { callee, .. } = &e.kind else {
            return false;
        };
        let ast::ExprKind::Member { prop, .. } = &callee.kind else {
            return false;
        };
        is_promise_callback_method(&prop.name) && self.calls_promise_callback_fn(h)
    }

    /// Is `h` a call of one of the prelude's `promiseThen` functions?
    fn calls_promise_callback_fn(&self, h: &hir::Expr) -> bool {
        if let hir::ExprKind::Call {
            callee: hir::Callee::Def(d, _),
            ..
        } = &h.kind
        {
            let name = &self.cx.fn_info(*d).name;
            return name
                .strip_prefix("std/prelude/promise::")
                .is_some_and(is_promise_callback_fn);
        }
        false
    }

    /// `recv.<prop>(args)` where `recv` is a promise and `prop` a callback method.
    pub(crate) fn promise_callback_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let TyKind::Promise(t, e) = *self.cx.ty.kind(recv.ty) else {
            panic!("ICE: promise_callback_call on a non-promise receiver");
        };
        // `then(null, g)` / `then(undefined, g)` is `catch(g)`, and `then(f, null)` is `then(f)`,
        // as in JS (an absent callback passes the value or the reason through).
        let absent = |a: &ast::Expr| {
            matches!(
                a.kind,
                ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
            )
        };
        let (method, args) = match (prop.name.as_str(), args) {
            ("then", [f, g]) if absent(f) && !absent(g) => ("catch", std::slice::from_ref(g)),
            ("then", [f, g]) if absent(g) && !absent(f) => ("then", std::slice::from_ref(f)),
            (m, _) => (m, args),
        };
        let max = if method == "then" { 2 } else { 1 };
        if args.is_empty() || args.len() > max {
            self.arg_count_error(&format!("`{method}`"), 1, max, args.len(), span);
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let mut callbacks = vec![];
        for (i, a) in args.iter().enumerate() {
            // What the callback gets: the value (`then`'s first), the reason (`then`'s second,
            // `catch`), or nothing (`finally`).
            let params = match (method, i) {
                ("then", 0) => vec![t],
                ("finally", _) => vec![],
                _ => vec![e],
            };
            let error = self.cx.ty.error;
            let hint = self.cx.ty.intern(TyKind::FnPtr {
                params,
                ret: error,
                throws: error,
                rest: false,
            });
            let h = self.expr(a, Some(hint), Want::Move);
            if self.cx.ty.has_error(h.ty) {
                self.check_args_loose(&args[i + 1..]);
                return self.error_expr(span);
            }
            let ret = match self.cx.ty.kind(h.ty) {
                TyKind::FnPtr { ret, .. } => *ret,
                _ => {
                    let found = self.cx.display(h.ty);
                    self.cx.err(
                        format!("the callback of `{method}` must be a function, found `{found}`"),
                        h.span,
                    );
                    self.check_args_loose(&args[i + 1..]);
                    return self.error_expr(span);
                }
            };
            let returns_promise = self.cx.ty.promise_payload(ret).is_some();
            callbacks.push((h, returns_promise));
        }
        let all_async = callbacks.iter().all(|c| c.1);
        if callbacks.len() == 2 && callbacks[0].1 != callbacks[1].1 {
            self.cx.err(
                "the callbacks of `then(onFulfilled, onRejected)` must both return a promise or both return a value",
                span,
            );
            return self.error_expr(span);
        }
        // What each callback gives (its promise's value when it returns one) and throws (its
        // promise's error type too).
        let parts: Vec<(TyId, Option<TyId>)> = callbacks
            .iter()
            .map(|(h, _)| self.callback_parts(h.ty))
            .collect();
        let never = self.cx.ty.never;
        // `then(f)` and `finally(h)` reject with what the promise rejects with and what the
        // callback throws: one type (the error types are type parameters of their own).
        let joined = |s: &mut Self, x: Option<TyId>| -> Result<(TyId, bool), ()> {
            match x {
                None => Ok((e, false)),
                Some(x) if e == never || e == x => Ok((x, true)),
                Some(x) => {
                    let (a, b) = (s.cx.display(e), s.cx.display(x));
                    s.cx.err(
                        format!("the callback of `{method}` throws `{b}` but the promise rejects with `{a}`"),
                        span,
                    );
                    Err(())
                }
            }
        };
        // The prelude function, its type arguments, the receiver's type, and the new promise's
        // value and error types.
        let (name, targs, recv_ty, value, err) = match (method, parts.as_slice()) {
            ("then", [(u, x)]) => {
                let Ok((j, throws)) = joined(self, *x) else {
                    return self.error_expr(span);
                };
                let name = match (all_async, throws) {
                    (false, false) => "promiseThen",
                    (false, true) => "promiseThenThrows",
                    (true, false) => "promiseThenAsync",
                    (true, true) => "promiseThenAsyncThrows",
                };
                let recv_ty = self.cx.ty.promise_rejecting(t, j);
                (name, vec![t, j, *u], recv_ty, *u, j)
            }
            ("then", [(u, xf), (ug, xg)]) => {
                if u != ug || xf != xg {
                    let (a, b) = (
                        self.cx.display(callbacks[0].0.ty),
                        self.cx.display(callbacks[1].0.ty),
                    );
                    self.cx.err(
                        format!("the callbacks of `then(onFulfilled, onRejected)` must give the same type and throw the same errors; found `{a}` and `{b}`"),
                        span,
                    );
                    return self.error_expr(span);
                }
                let f = xf.unwrap_or(never);
                let name = if all_async {
                    "promiseThenElseAsync"
                } else {
                    "promiseThenElse"
                };
                (name, vec![t, e, *u, f], recv.ty, *u, f)
            }
            ("catch", [(u, xg)]) => {
                if *u != t {
                    let (a, b) = (self.cx.display(t), self.cx.display(*u));
                    self.cx.err(
                        format!("the callback of `catch` must give the promise's value type `{a}`, found `{b}`"),
                        span,
                    );
                    return self.error_expr(span);
                }
                let g = xg.unwrap_or(never);
                let name = if all_async {
                    "promiseCatchAsync"
                } else {
                    "promiseCatch"
                };
                (name, vec![t, e, g], recv.ty, t, g)
            }
            (_, [(r, x)]) => {
                let Ok((j, throws)) = joined(self, *x) else {
                    return self.error_expr(span);
                };
                let name = match (all_async, throws) {
                    (false, false) => "promiseFinally",
                    (false, true) => "promiseFinallyThrows",
                    (true, false) => "promiseFinallyAsync",
                    (true, true) => "promiseFinallyAsyncThrows",
                };
                let recv_ty = self.cx.ty.promise_rejecting(t, j);
                (name, vec![t, j, *r], recv_ty, t, j)
            }
            _ => return self.error_expr(span),
        };
        let Some(Item::Def(d)) = self.cx.prelude.get(name).cloned() else {
            self.cx
                .err(format!("`{method}` needs the prelude (std/prelude)"), span);
            return self.error_expr(span);
        };
        // The promise has one owner: the call takes it, converted to the error type the new
        // promise rejects with. The callbacks are passed as they are.
        let recv = self.receiver(recv, None, PassMode::Owned, true);
        let recv = match self.try_coerce(recv, recv_ty) {
            Ok(h) => h,
            Err(h) => {
                self.report_mismatch(recv_ty, &h);
                return self.error_expr(span);
            }
        };
        let mut all = vec![recv];
        all.extend(callbacks.into_iter().map(|c| c.0));
        let ret = self.cx.ty.promise_rejecting(value, err);
        self.note_async_args(d, &all);
        self.call_throws(d, &targs, ret, span);
        let kind = hir::ExprKind::Call {
            callee: hir::Callee::Def(d, targs),
            args: all,
        };
        self.mk(kind, ret, span)
    }
}

//! M3 async expressions (docs/reference/async.md): `await`, and `spawn` of a promise or
//! of an async task body (`spawn(async () => { ... })`). The other task builtins (`sleep`,
//! `yieldNow`, `Promise.all`, `performance.now`, `Date.now`) are plain intrinsic calls.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::places::is_place;
use crate::body::switch::cases::source_text;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::{FnKind, ThrowSrc};
use crate::hir::{self, Callee, DefId, ExprKind as H, Intrinsic, PassMode, TyId, TyKind};
use crate::throws::ThrowCheck;

impl FnCx<'_, '_> {
    /// `await p`: only in async bodies; `p: Promise<T>` is consumed, the value is `T`.
    pub(crate) fn await_expr(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if !self.f.is_async && self.f.yield_ty.is_some() {
            self.cx.error(
                Diagnostic::error("`await` is not allowed in a generator", span)
                    .with_note("a generator (`function*`) runs synchronously, one `next()` at a time: await the promise before calling the generator, or yield the promise for the caller to await"),
            );
        } else if !self.f.is_async {
            self.cx.error(
                Diagnostic::error("`await` is only allowed inside async functions", span)
                    .with_note("mark the enclosing function or arrow `async`"),
            );
        }
        let hint = self.hint(exp).map(|t| self.cx.ty.promise(t));
        self.direct_await = super::promise_new::awaited_new_promise(inner);
        let h = self.expr(inner, hint, Want::Move);
        self.direct_await = None;
        self.await_throws(&h);
        let ty = match self.cx.ty.kind(h.ty) {
            TyKind::Promise(t, _) => *t,
            TyKind::Error | TyKind::Never => self.cx.ty.error,
            _ => {
                let found = self.cx.display(h.ty);
                self.cx.error(
                    Diagnostic::error(format!("`await` needs a promise, found `{found}`"), h.span)
                        .with_note("only `Promise<T>` values (async calls, `spawn`, `sleep`, ...) can be awaited"),
                );
                self.cx.ty.error
            }
        };
        self.mk(H::Await(Box::new(h)), ty, span)
    }

    /// Awaiting rethrows the promise's rejection: a directly awaited async call throws what the
    /// function throws (inferred with it); any other promise what its type says.
    fn await_throws(&mut self, h: &hir::Expr) {
        if let H::Call {
            callee: Callee::Def(d, targs),
            ..
        } = &h.kind
        {
            if self.is_async_fn(*d) {
                self.throw_src(ThrowSrc::Call(*d, targs.clone(), h.span));
                return;
            }
        }
        if let Some(e) = self.cx.ty.promise_error(h.ty) {
            if e != self.cx.ty.never {
                self.throw_src(ThrowSrc::Direct(e, h.span));
            }
        }
    }

    /// `Promise.allSettled(ps)` / `Promise.any(ps)`: calls of the prelude's `promiseAllSettled` /
    /// `promiseAny` (std/prelude/promise.vlt), so a directly awaited `Promise.any` throws like
    /// any async call.
    pub(super) fn prelude_call(
        &mut self,
        name: &str,
        what: &str,
        type_args: &[TyId],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some(Item::Def(d)) = self.cx.prelude.get(name).cloned() else {
            self.cx
                .err(format!("`{what}` needs the prelude (std/prelude)"), span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let c = self.fn_callable(d, format!("`{what}`"));
        let mut slots = vec![None; c.slot_names.len()];
        for (slot, t) in slots.iter_mut().zip(type_args) {
            *slot = Some(*t);
        }
        let ck = self.check_call(&c, slots, args, exp, span);
        self.note_async_args(d, &ck.args);
        self.call_throws(d, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(d, ck.type_args),
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }

    /// `f();` where `f()` is a promise: an error (a dropped promise's result and errors would be
    /// lost), with the fixes `await f()` and `spawn(f())`. `spawn(...)` itself may be dropped: the
    /// task keeps running.
    pub(crate) fn check_floating(&mut self, e: &ast::Expr, h: &hir::Expr) {
        let (promise, array) = match self.cx.ty.kind(h.ty) {
            TyKind::Promise(..) => (true, false),
            TyKind::Array(t) => (matches!(self.cx.ty.kind(*t), TyKind::Promise(..)), true),
            _ => (false, false),
        };
        let spawned = matches!(
            h.kind,
            H::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn),
                ..
            }
        );
        if !promise || spawned {
            return;
        }
        let mut d = Diagnostic::error(
            "floating promise: this promise is neither awaited nor spawned",
            e.span,
        )
        .with_note("its result, and any error it throws, would be lost");
        let code = floating_code(e);
        let (wait, background) = match &code {
            _ if array => (
                "to wait for them: `await Promise.all(...)`".to_string(),
                "to run each in the background: `spawn(...)` it".to_string(),
            ),
            Some(c) => (
                format!("to wait for it: `await {c}`"),
                format!("to run it in the background: `spawn({c})`"),
            ),
            None => (
                "to wait for it: put `await` in front".to_string(),
                "to run it in the background: wrap it in `spawn(...)`".to_string(),
            ),
        };
        if self.f.is_async {
            d = d.with_note(wait);
        }
        self.cx.error(d.with_note(background));
    }

    fn is_async_fn(&self, d: DefId) -> bool {
        self.cx
            .try_fn(d)
            .is_some_and(|f| f.is_async && f.kind != FnKind::Extern)
    }

    /// A call of `d<targs>` (result type `ret`) may throw here. An async function's errors
    /// surface where its promise is awaited: the promise's type carries them (`ret` is
    /// `Promise<T, E>`, built from what `d` was known to throw; checked after inference).
    pub(super) fn call_throws(&mut self, d: DefId, targs: &[TyId], ret: TyId, span: Span) {
        let f = self.cx.fn_info(d);
        if f.kind == FnKind::Extern || f.is_generator {
            // A generator's errors come out of `next()`, not out of the call creating it.
            return;
        }
        if !self.rejects_through_promise(d) {
            self.throw_src(ThrowSrc::Call(d, targs.to_vec(), span));
            return;
        }
        let never = self.cx.ty.never;
        let observed = self.cx.ty.promise_error(ret).filter(|e| *e != never);
        self.cx.throw_checks.push(ThrowCheck {
            srcs: vec![ThrowSrc::Call(d, targs.to_vec(), span)],
            observed,
            exact: true,
            span,
        });
    }

    /// Does calling `d` report its errors through the promise it returns instead of throwing:
    /// an async function, or a member of a promise dispatch group (a forwarder synthesized for
    /// an async interface default, throws/groups.rs `Group::promise`)?
    pub(crate) fn rejects_through_promise(&mut self, d: DefId) -> bool {
        let f = self.cx.fn_info(d);
        if f.kind == FnKind::Extern {
            return false;
        }
        f.is_async || self.cx.throw_groups().in_promise_group(d)
    }

    /// The result type of calling async function `d` (declared `Promise<T>`): `Promise<T, E>`
    /// with what `d` is known to throw now (in `d`'s generic context).
    pub(crate) fn async_call_ret(&mut self, d: DefId, ret: TyId) -> TyId {
        let Some(v) = self.cx.ty.promise_payload(ret) else {
            return ret;
        };
        match crate::throws::throws_now(self.cx, d, &[]) {
            Some(e) => self.cx.ty.promise_rejecting(v, e),
            None => ret,
        }
    }

    /// Places passed to the owned params of async function `def` (`args` in param order): see
    /// `FnInfo::soft_moves` (an async call copies an argument that is still needed afterwards).
    pub(super) fn note_async_args(&mut self, def: DefId, args: &[hir::Expr]) {
        let f = self.cx.fn_info(def);
        if !(f.is_async || f.is_generator) || f.kind == FnKind::Extern {
            return;
        }
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        for (a, m) in args.iter().zip(modes) {
            let mut place = a;
            loop {
                place = match &place.kind {
                    H::WrapSome(x) | H::Upcast(x) => x,
                    // A narrowed union value passed as the union (the member re-wrapped).
                    H::Variant { args, .. } if args.len() == 1 && is_place(&args[0]) => &args[0],
                    _ => break,
                };
            }
            // A resource that is a shared value (an object with `[Symbol.dispose]`, a generator)
            // is shared like any object; one holding a promise cannot be.
            let soft = !self.cx.owns_resource(place.ty) || self.cx.is_shared_value(place.ty);
            if m == PassMode::Owned && is_place(place) && soft {
                self.f.soft_moves.push(place.span);
            }
        }
    }

    /// `spawn(p)` starts promise `p` now; `spawn(async () => { ... })` starts a task body (an
    /// escaping async closure, so it may not mutate what it captures).
    pub(super) fn spawn_call(
        &mut self,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let [arg] = args else {
            return self.simple_intrinsic(Intrinsic::Spawn, "`spawn`", args, exp, span);
        };
        if as_arrow(arg).is_none() {
            let mut e = self.simple_intrinsic(Intrinsic::Spawn, "`spawn`", args, exp, span);
            self.no_spawned_generators(&e);
            if let H::Call { args, .. } = &mut e.kind {
                if let [p] = args.as_mut_slice() {
                    self.hand_on_spawned_callee(p);
                }
            }
            return e;
        }
        let ret = self
            .hint(exp)
            .filter(|t| matches!(self.cx.ty.kind(*t), TyKind::Promise(..)))
            .unwrap_or(self.cx.ty.error);
        let expected = self.cx.ty.fn_ptr(vec![], ret);
        let f = self.expr(arg, Some(expected), Want::Move);
        match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr { params, ret, .. }
                if params.is_empty() && self.cx.ty.promise_payload(ret).is_some() =>
            {
                self.intrinsic(Intrinsic::Spawn, vec![f], ret, span)
            }
            TyKind::Error => self.error_expr(span),
            _ => {
                let found = self.cx.display(f.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("`spawn` needs a promise or an async task body, found `{found}`"),
                        f.span,
                    )
                    .with_note("write `spawn(async () => { ... })` or `spawn(asyncFn(args))`"),
                );
                self.error_expr(span)
            }
        }
    }
}

impl FnCx<'_, '_> {
    /// `spawn(g())` through a function value `g` (also in either branch of a conditional): the
    /// task needs `g`'s captures of its own. `g` is taken (a soft move): moved to the task when
    /// this is its last use, so a capture nothing else uses moves with it, and shared, then
    /// copied for the task, when `g` is used again (velt_vir callee.rs `call_indirect`).
    fn hand_on_spawned_callee(&mut self, p: &mut hir::Expr) {
        match &mut p.kind {
            H::If { then, els, .. } => {
                self.hand_on_spawned_callee(then);
                self.hand_on_spawned_callee(els);
            }
            H::Block(b) if b.stmts.is_empty() => {
                if let Some(v) = b.value.as_deref_mut() {
                    self.hand_on_spawned_callee(v);
                }
            }
            H::Call {
                callee: Callee::Indirect(f),
                ..
            } if is_place(f) => {
                crate::body::places::set_place_mode(f, hir::UseMode::Move);
                self.f.soft_moves.push(f.span);
            }
            _ => {}
        }
    }

    /// `spawn(f(args))` transfers the call's arguments (the receiver included) to the task's
    /// thread, copying what the caller still shares: none may hold a generator.
    fn no_spawned_generators(&mut self, h: &hir::Expr) {
        let H::Call { args, .. } = &h.kind else {
            return;
        };
        let Some(H::Call {
            args: call_args, ..
        }) = args.first().map(|a| &a.kind)
        else {
            return;
        };
        let tys: Vec<(TyId, Span)> = call_args.iter().map(|a| (a.ty, a.span)).collect();
        for (t, at) in tys {
            self.no_generator_copy(t, crate::body::GenCopy::Task, at);
        }
    }
}

/// `f(...)` / `obj.method(...)` for the floating-promise fixes, if the callee has a short name.
fn floating_code(e: &ast::Expr) -> Option<String> {
    let ast::ExprKind::Call { callee, .. } = &e.kind else {
        return None;
    };
    let name = source_text(callee);
    (!name.contains("the value")).then(|| format!("{name}(...)"))
}

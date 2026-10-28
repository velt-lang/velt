//! Inferred result types (docs/reference/functions.md "Return types"). A function, method or
//! arrow function without a return type returns what its `return` expressions give, as in
//! TypeScript: the one of their types that every other converts to (`i64` and `f64` give `f64`,
//! a class and its base class give the base), else their union; a `return null` makes it
//! nullable; no value gives `void`, only values that never complete give `never`.
//!
//! Callers ask for a function's result with [`ret_of`], which checks the body first when
//! needed (bodies are checked on demand, see `driver`). A use of a function while its own body
//! is being checked is handled by `body::recursion`: only uses that its `return` expressions
//! depend on need a written return type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{ensure_body, FnCx, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, RetSource, RetWant};
use crate::hir::{self, DefId, ExprKind as H, Nothing, StmtKind as S, TyId, TyKind};
use crate::visit::{self, VisitMut};

/// The `return`s of a body whose result type is being inferred.
/// Where a closure's returned values go when they reject or throw with errors its type doesn't
/// say (`throws::promise_sites`, `throws::fn_flows`).
#[derive(Default, Clone, Copy)]
pub(crate) struct ReturnSite {
    /// Where a closure's result type is written (not for an async one: its `Promise<T>`).
    pub ret_span: Option<Span>,
    /// A closure's span (its errors returned into no site go where it is stored).
    pub closure_at: Option<Span>,
}

#[derive(Default, Clone)]
pub(crate) struct Returns {
    values: Vec<Value>,
    /// `return null` / `return undefined` (typed once the result is known).
    nulls: Vec<Span>,
    /// Which nothings those `return`s give (`None` while there are none).
    nothing: Option<Nothing>,
    /// `return;`
    bare: Vec<Span>,
    /// An async body returns a promise: its value type counts (`promise_value`), and the
    /// promise converts to the result.
    promises: bool,
}

#[derive(Clone)]
struct Value {
    /// Widened type of the returned value (`"a"` → `string`).
    ty: TyId,
    span: Span,
}

/// Has inferring result types used up its stack budget (`Ctx::stack_budget`)? Each body checked
/// inside another to infer its result takes stack space, as much as its nesting needs; a deeper
/// chain of unannotated functions is reported instead of overflowing the stack.
fn stack_exhausted(cx: &Ctx) -> bool {
    cx.stack_base.saturating_sub(crate::stack_address()) > cx.stack_budget
}

/// The result type of function `d` (used at `at`), inferring it from the body if needed.
pub(crate) fn ret_of(cx: &mut Ctx, d: DefId, at: Span) -> TyId {
    if cx.bodies_ok && !(cx.sigq.deferred_sigs.is_empty() && cx.sigq.filling_sigs.is_empty()) {
        if let Some(t) = pending_sig(cx, d, at) {
            return t;
        }
    }
    // A deferred std function is checked before its first call is (`lazy_std`): what a caller's
    // check learns from the callee's body is there, as when every body was checked first.
    if cx.fn_info(d).state == BodyState::Unchecked
        && super::lazy_std::deferred(cx, d)
        && !stack_exhausted(cx)
    {
        ensure_body(cx, d);
    }
    let f = cx.fn_info(d);
    match f.ret_source.clone() {
        RetSource::Known => f.ret,
        RetSource::Body if f.state == BodyState::InProgress => {
            super::recursion::placeholder(cx, d, at);
            cx.ty.error
        }
        RetSource::Body if f.state == BodyState::Unchecked && stack_exhausted(cx) => {
            report_too_deep(cx, d, at);
            cx.ty.error
        }
        RetSource::Body => {
            ensure_body(cx, d);
            cx.fn_info(d).ret
        }
        RetSource::Base(base, args) => {
            let r = ret_of(cx, base, at);
            let r = cx.subst(r, &args);
            let f = cx.fn_info_mut(d);
            f.ret = r;
            f.ret_source = RetSource::Known;
            r
        }
    }
}

/// While type queries fill deferred signatures (`collect::sigs::fill_pending`): `d`'s result is
/// needed at `at`, so its signature is filled first; if it is being filled and its result comes
/// from its body, which needs its parameters, that is a cycle (TS7023), reported here.
fn pending_sig(cx: &mut Ctx, d: DefId, at: Span) -> Option<TyId> {
    let Some((_, param)) = cx.sigq.filling_sigs.iter().find(|f| f.0 == d).cloned() else {
        crate::collect::sigs::fill_pending(cx, d);
        return None;
    };
    if cx.fn_info(d).ret_source == RetSource::Known {
        return None;
    }
    let name = super::recursion::short_name(cx, d);
    cx.error(
        Diagnostic::error(
            format!("the result of `{name}` is needed to resolve its own signature"),
            at,
        )
        .with_note(format!(
            "the type of `{param}` needs it, and `{name}` takes its result from its body; write the return type of `{name}`"
        )),
    );
    Some(cx.ty.error)
}

/// `d`'s result is needed at `at`, inside a chain of bodies checked for their result types
/// that is too long.
fn report_too_deep(cx: &mut Ctx, d: DefId, at: Span) {
    let name = super::recursion::short_name(cx, d);
    cx.error(
        Diagnostic::error(
            format!("inferring the return type of `{name}` nests too many functions"),
            at,
        )
        .with_note(format!(
            "{} functions without a return type wait for each other here, too many to check \
             one inside another; write the return type of `{name}`",
            cx.checking.len()
        )),
    );
}

/// Signature comparisons that waited for inferred results (`collect::ret_infer`).
pub(crate) fn check_deferred(cx: &mut Ctx) {
    for c in std::mem::take(&mut cx.ret_checks) {
        let have = ret_of(cx, c.def, c.span);
        let have = cx.subst(have, &c.args);
        let want = match c.want {
            RetWant::Ty(t) => t,
            RetWant::Of(base, args) => {
                let r = ret_of(cx, base, c.span);
                cx.subst(r, &args)
            }
        };
        if !cx.same_ty(have, want) && !cx.ty.has_error(have) && !cx.ty.has_error(want) {
            cx.err(c.message, c.span);
        }
    }
}

impl FnCx<'_, '_> {
    /// The value `e` returned from a body whose result type is `ret`. A callback of the JS API
    /// returning a number (a comparator) may return an integer of any type: it converts. A
    /// predicate of the JS API may return any value: it is tested for truthiness.
    pub(crate) fn returned(&mut self, e: &ast::Expr, ret: TyId) -> hir::Expr {
        if self.f.truthy_returns {
            return self.cond(e);
        }
        if self.f.int_returns_number {
            // Its own type first: `x.n != y.n ? y.n - x.n : -1` with `n: i64` is an `i64`.
            let h = self.expr(e, None, Want::Move);
            if self.cx.ty.is_int(h.ty) {
                return self.int_to_float(h, ret);
            }
            return self.coerce(h, ret);
        }
        if self.f.is_async && self.f.yield_ty.is_none() && self.adopts_promise(ret) {
            return self.async_returned(e, ret);
        }
        // In an arrow asserted to a function type, TypeScript compares the result it infers
        // with the asserted one: an `any` / `unknown` returned converts, as `e as R` does.
        if self.f.asserted_ret && self.cx.dynamic(ret).is_none() {
            if let Some(t) = self.trial_type(e).filter(|t| self.cx.dynamic(*t).is_some()) {
                let h = self.expr(e, Some(t), Want::Move);
                return self.cast_dynamic(h, ret);
            }
        }
        // Stored into the site of the written result type (`throws::fn_flows`); a closure's
        // without one goes where the closure is stored (`throws::promise_sites`).
        let site = self.return_site(ret);
        let at = site
            .is_none()
            .then_some(self.f.return_site.closure_at)
            .flatten();
        let outer = std::mem::replace(&mut self.cx.fn_sites.closure_return, at);
        let h = self.stored_into(site, |s| s.expr_coerce(e, ret, Want::Move));
        self.cx.fn_sites.closure_return = outer;
        h
    }

    /// Can an async function returning `ret` adopt a returned promise? Not when `ret` is itself
    /// a promise (an async function's value is never one) or a function type (checked with the
    /// adapter of `expr_coerce`), nor where `ret` is not known.
    fn adopts_promise(&self, ret: TyId) -> bool {
        let t = self.cx.ty.core(ret).unwrap_or(ret);
        !matches!(
            self.cx.ty.kind(t),
            TyKind::Promise(..) | TyKind::FnPtr { .. } | TyKind::Error
        )
    }

    /// The body `block` of an async function that returns promises (`async_returned`): as in
    /// JS, the promise of the `return` that completes the body is awaited once every scope of
    /// the body is left. The body becomes `let <adopted_i> = null; let <adopt_which> = 0;
    /// <adopt>: while (true) { body; <adopt_which> = 0; break; }` (the last two only when the
    /// body may fall off its end), then `match (<adopt_which>) { i => return value_i, … }`.
    /// Each store sets `<adopt_which>`, so a `return p` that a `finally` cancels (`continue`,
    /// `break`, another `return`) is not adopted, and falling off the end adopts none.
    pub(crate) fn wrap_adopted(&mut self, block: &mut hir::Block) {
        let adopted = std::mem::take(&mut self.f.adopted);
        let which = self.f.adopt_which.take();
        let (Some(which), false) = (which, adopted.is_empty()) else {
            return;
        };
        let span = block.span;
        let (unit, never, bool_, i32_) = (
            self.cx.ty.unit,
            self.cx.ty.never,
            self.cx.ty.bool_,
            self.cx.ty.i32,
        );
        let label = super::ADOPT_LABEL.to_string();
        let stmt = |kind| hir::Stmt { kind, span };
        let falls_off = !crate::flow::block_diverges(block, &self.cx.ty);
        let mut body = std::mem::take(&mut block.stmts);
        if let Some(v) = block.value.take() {
            body.push(stmt(S::Expr(*v)));
        }
        if falls_off {
            let place = self.mk(H::Local(which, hir::UseMode::BorrowMut), i32_, span);
            let zero = self.mk(H::Lit(hir::Lit::Int(0)), i32_, span);
            let reset = self.mk(
                H::Assign {
                    place: Box::new(place),
                    value: Box::new(zero),
                },
                unit,
                span,
            );
            body.push(stmt(S::Expr(reset)));
            body.push(stmt(S::Break(Some(label.clone()))));
        }
        let mut stmts = vec![];
        for a in &adopted {
            let ty = self.f.locals[a.slot.0 as usize].ty;
            let null = self.mk(H::Lit(hir::Lit::Null), ty, span);
            stmts.push(stmt(S::Let {
                local: a.slot,
                init: Some(null),
            }));
        }
        let zero = self.mk(H::Lit(hir::Lit::Int(0)), i32_, span);
        stmts.push(stmt(S::Let {
            local: which,
            init: Some(zero),
        }));
        let cond = self.mk(H::Lit(hir::Lit::Bool(true)), bool_, span);
        stmts.push(stmt(S::While {
            label: Some(label),
            cond,
            body: hir::Block {
                stmts: body,
                value: None,
                span,
            },
            step: None,
        }));
        let last = adopted.len() - 1;
        let mut arms = vec![];
        for (i, a) in adopted.into_iter().enumerate() {
            let slot_ty = self.f.locals[a.slot.0 as usize].ty;
            let pty = self.f.locals[a.bind.0 as usize].ty;
            // Only a stored promise leads to its arm.
            let read = self.mk(H::Local(a.slot, hir::UseMode::Move), slot_ty, span);
            let p = self.mk(H::UnwrapSome(Box::new(read), hir::UseMode::Move), pty, span);
            let returns = hir::Block {
                stmts: vec![
                    stmt(S::Let {
                        local: a.bind,
                        init: Some(p),
                    }),
                    stmt(S::Return(Some(a.value))),
                ],
                value: None,
                span,
            };
            // When the body cannot fall off its end, the loop is only left through a store.
            let pat = if i == last && !falls_off {
                hir::PatKind::Wildcard
            } else {
                hir::PatKind::Lit(hir::Lit::Int(i as u128 + 1))
            };
            arms.push(hir::Arm {
                pat: self.pat(pat, i32_, span),
                guard: None,
                body: self.mk(H::Block(returns), never, span),
            });
        }
        if falls_off {
            let none = hir::Block {
                stmts: vec![],
                value: None,
                span,
            };
            arms.push(hir::Arm {
                pat: self.pat(hir::PatKind::Wildcard, i32_, span),
                guard: None,
                body: self.mk(H::Block(none), unit, span),
            });
        }
        let read = self.mk(H::Local(which, hir::UseMode::Copy), i32_, span);
        let ty = if falls_off { unit } else { never };
        let m = self.mk(
            H::Match {
                scrutinee: Box::new(read),
                arms,
            },
            ty,
            span,
        );
        stmts.push(stmt(S::Expr(m)));
        block.stmts = stmts;
    }

    /// `return e` / `return;` in a body whose result type is being inferred.
    pub(super) fn infer_return(&mut self, e: Option<&ast::Expr>, span: Span) -> Option<hir::Expr> {
        let Some(e) = e else {
            self.f.returns.bare.push(span);
            return None;
        };
        if let Some(n) = nothing_of_lit(e) {
            self.cx.writes_undefined |= n.has_undefined() && !self.cx.scopes[self.module].is_std;
            self.f.returns.nulls.push(e.span);
            let r = &mut self.f.returns;
            r.nothing = Some(r.nothing.map_or(n, |m| m.join(n)));
            return Some(self.mk(H::Lit(hir::Lit::Null), self.cx.ty.error, e.span));
        }
        // A value that takes its type from context (`[]`, a number, an arrow) gets the type of
        // an earlier `return`.
        let never = self.cx.ty.never;
        let hint = super::expr::deferred(e)
            .then(|| {
                self.f
                    .returns
                    .values
                    .iter()
                    .map(|v| v.ty)
                    .find(|t| *t != never)
            })
            .flatten();
        let outer = std::mem::replace(&mut self.cx.rec.in_return, true);
        let h = self.expr(e, hint, Want::Move);
        self.cx.rec.in_return = outer;
        let ty = match self.promise_value(h.ty) {
            Some(t) => {
                self.f.returns.promises = true;
                t
            }
            None => h.ty,
        };
        let ty = self.cx.widened(ty);
        self.f.returns.values.push(Value { ty, span: h.span });
        Some(h)
    }

    /// `T` when an async body (not a generator) returns a `Promise<T>`: like TypeScript, its
    /// result is `T` (the function's `Promise<T>`), not a promise of a promise (issue #1166).
    pub(crate) fn promise_value(&self, t: TyId) -> Option<TyId> {
        if !self.f.is_async || self.f.yield_ty.is_some() {
            return None;
        }
        self.cx.ty.promise_payload(t)
    }

    /// The promise `h` returned where an async body's inferred result is its value type `t`
    /// (`promise_value`): adopted like one returned with a written result type
    /// (`async_returned`), so the `return` becomes the statement that stores it.
    pub(crate) fn inferred_promise_return(&mut self, h: hir::Expr, t: TyId) -> hir::Expr {
        let span = h.span;
        self.await_throws(&h);
        self.f.adopt_stmt = true;
        self.adopt(h, t, span)
    }

    /// The inferred result type of the body `block` (whose `return`s were recorded); converts
    /// every returned value to it.
    pub(crate) fn finish_inferred_ret(&mut self, block: &mut hir::Block) -> TyId {
        let mut r = std::mem::take(&mut self.f.returns);
        self.undefined_returns(&mut r, block);
        let ty = self.common_ret(&r, block);
        self.f.ret = Some(ty);
        if r.promises || !r.nulls.is_empty() || r.values.iter().any(|v| v.ty != ty) {
            visit::block(block, &mut CoerceReturns { fcx: self, to: ty });
        }
        ty
    }

    /// A body that returns values and also `return;` or falls off its end returns `undefined`
    /// there, as in TypeScript: its result type allows `undefined` (issue #930). The bare
    /// returns become `return undefined;`; `check_returns` adds the one at the end.
    fn undefined_returns(&mut self, r: &mut Returns, block: &mut hir::Block) {
        let (never, unit) = (self.cx.ty.never, self.cx.ty.unit);
        let values = !r.nulls.is_empty() || r.values.iter().any(|v| v.ty != never && v.ty != unit);
        if !values || self.f.yield_ty.is_some() {
            return;
        }
        let falls_off = !crate::flow::block_diverges(block, &self.cx.ty);
        if r.bare.is_empty() && !falls_off {
            return;
        }
        self.cx.writes_undefined |= !self.cx.scopes[self.module].is_std;
        r.nothing = Some(
            r.nothing
                .map_or(Nothing::Undefined, |m| m.join(Nothing::Undefined)),
        );
        if r.bare.is_empty() {
            // Only the end: `nulls` gives the result its `undefined` (and a span for it).
            r.nulls
                .push(Span::new(block.span.file, block.span.hi, block.span.hi));
            return;
        }
        r.nulls.append(&mut r.bare);
        visit::block(block, &mut BareReturns { fcx: self });
    }

    fn common_ret(&mut self, r: &Returns, block: &hir::Block) -> TyId {
        let (never, unit, error) = (self.cx.ty.never, self.cx.ty.unit, self.cx.ty.error);
        let mut tys: Vec<TyId> = vec![];
        for v in &r.values {
            if v.ty != never && !tys.contains(&v.ty) {
                tys.push(v.ty);
            }
        }
        if tys.contains(&error) {
            return error;
        }
        if tys.is_empty() && r.nulls.is_empty() {
            let diverges = crate::flow::block_diverges(block, &self.cx.ty);
            // An arrow or function expression whose end is never reached and that returns
            // nothing (`(e) => { throw e; }`) gives `never`, as in TypeScript (a declared
            // function's is `void`).
            let closure = self.f.kind == crate::defs::FnKind::Closure
                && self.f.never_end
                && !self.f.is_async
                && self.f.yield_ty.is_none();
            let never_only = r.bare.is_empty() && diverges && (!r.values.is_empty() || closure);
            return if never_only { never } else { unit };
        }
        if tys.contains(&unit) {
            if tys.len() == 1 && r.nulls.is_empty() {
                return unit;
            }
            tys.retain(|t| *t != unit);
            // Reported once; the result is unknown, so nothing else is reported about it.
            self.void_returns(r, &tys);
            return error;
        }
        let span = r.nulls.first().or(r.values.first().map(|v| &v.span));
        let span = span.copied().unwrap_or(block.span);
        let core = self.common_type(&tys, span);
        if r.nulls.is_empty() || core == error {
            return core;
        }
        let members = if tys.is_empty() { vec![] } else { vec![core] };
        self.cx.union_with(&members, r.nothing, span)
    }

    /// A `void` value returned next to other values (`return log(x)` and `return 1`, or
    /// `return null` when `others` is empty).
    fn void_returns(&mut self, r: &Returns, others: &[TyId]) {
        let other = others
            .first()
            .map_or_else(|| "null".to_string(), |t| self.cx.display(*t));
        let unit = self.cx.ty.unit;
        for v in r.values.iter().filter(|v| v.ty == unit) {
            self.cx.error(
                Diagnostic::error("mismatched types", v.span)
                    .with_note(format!("expected {other}, found void")),
            );
        }
    }

    /// The type of `tys` that every other converts to, else their union.
    fn common_type(&mut self, tys: &[TyId], span: Span) -> TyId {
        for &c in tys {
            let all = tys.iter().all(|&t| self.converts(t, c));
            if all {
                return c;
            }
        }
        self.cx.union_of(tys, false, span)
    }

    /// Does a value of type `t` convert to `to` implicitly (as `coerce` would)?
    fn converts(&mut self, t: TyId, to: TyId) -> bool {
        if t == to {
            return true;
        }
        if let Some(p) = self.cx.ty.opt_payload(to) {
            return self.converts(t, p);
        }
        if to == self.cx.ty.f64 && self.exact_in_number(t) {
            return true;
        }
        if to == self.cx.ty.str_ && self.is_string_enum(t) {
            return true;
        }
        if let Some(ms) = self.cx.union_members(to) {
            return match self.cx.union_members(t) {
                Some(ts) => ts.iter().all(|m| ms.contains(m)),
                None => ms.contains(&t),
            };
        }
        let mut cur = t;
        for _ in 0..64 {
            match self.cx.base_of(cur) {
                Some(b) if b == to => return true,
                Some(b) => cur = b,
                None => return false,
            }
        }
        false
    }
}

/// Converts every `return` value of a body to the inferred result type.
struct CoerceReturns<'f, 'a, 'm> {
    fcx: &'f mut FnCx<'a, 'm>,
    to: TyId,
}

impl VisitMut for CoerceReturns<'_, '_, '_> {
    fn stmt(&mut self, s: &mut hir::Stmt) {
        let S::Return(Some(e)) = &mut s.kind else {
            return;
        };
        if matches!(e.kind, H::Lit(hir::Lit::Null)) && e.ty == self.fcx.cx.ty.error {
            e.ty = self.to;
            return;
        }
        if self.fcx.promise_value(e.ty).is_some() && self.fcx.promise_value(self.to).is_none() {
            let placeholder = self.fcx.mk(H::Lit(hir::Lit::Unit), self.to, e.span);
            let h = std::mem::replace(e, placeholder);
            let stored = self.fcx.inferred_promise_return(h, self.to);
            self.fcx.f.adopt_stmt = false;
            s.kind = S::Expr(stored);
            return;
        }
        if e.ty != self.to {
            let placeholder = self.fcx.mk(H::Lit(hir::Lit::Unit), self.to, e.span);
            let h = std::mem::replace(e, placeholder);
            *e = self.fcx.coerce(h, self.to);
        }
    }
}

/// Turns every `return;` of a body into `return undefined;` (typed by `CoerceReturns`).
struct BareReturns<'f, 'a, 'm> {
    fcx: &'f mut FnCx<'a, 'm>,
}

impl VisitMut for BareReturns<'_, '_, '_> {
    fn stmt(&mut self, s: &mut hir::Stmt) {
        if let S::Return(None) = s.kind {
            let error = self.fcx.cx.ty.error;
            let value = self.fcx.mk(H::Lit(hir::Lit::Null), error, s.span);
            s.kind = S::Return(Some(value));
        }
    }
}

/// `null` (possibly parenthesized).
/// `null` / `undefined` (possibly parenthesized): which nothing it is.
fn nothing_of_lit(e: &ast::Expr) -> Option<Nothing> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => Some(Nothing::Null),
        ast::ExprKind::Lit(ast::Lit::Undefined) => Some(Nothing::Undefined),
        ast::ExprKind::Paren(inner) => nothing_of_lit(inner),
        _ => None,
    }
}

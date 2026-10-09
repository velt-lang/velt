//! Functions passed as callbacks whose type differs from the expected function type the way TS
//! allows. Every case is one adaptation: the function is wrapped in an arrow typed by the
//! expected type, so the closure's own rules (captures, ownership, async) apply to the wrapper
//! as to any arrow.
//!
//! A **function value** is wrapped as `(p0, …) => f(p0, …)` ([`FnCx::callback_adapter`]): a
//! module-level function is called by its name, any other value is read once into a hidden
//! local where it is passed (a later assignment to its variable doesn't change the callback,
//! as in JS). It is wrapped when
//! - the expected type passes more parameters (`xs.map(double)`, `map` passes `(x, i)`): `f`
//!   gets the leading ones;
//! - it takes more parameters than the expected type passes, all of them optional or with
//!   defaults (`setTimeout(tick, 10)` with `function tick(n?: number)`): `f` gets none of them;
//! - the expected type returns `void` and `f` returns a value: it is dropped;
//! - the expected type returns a union with `f`'s result as a member (`(x) => Resp` for
//!   `(x) => Resp | Promise<Resp>`): it converts;
//! - a standard timer expects the promise to run (`() => Promise<void>`) and `f` is not `async`
//!   (`setTimeout(tick, 10)`): the wrapper is checked as an `async` arrow ([`void task`]);
//! - it is the handler of a server that runs it on several threads (`serve`): always, by an
//!   `async` wrapper, so each request copies what it captured.
//!
//! An **arrow** is checked against the expected type itself; the cases that need more are
//! - an `async` arrow where the function type returns `void` (`onClick: () => void`) or a union
//!   with one promise member (`(n) => View | Promise<View>`) ([`FnCx::async_arrow_callback`]):
//!   it is an async closure held in a hidden local `f`, wrapped as `(p…) => f(p…)` (the
//!   promise converts to the union) or `(p…) => { const p = f(p…); }` (the started promise
//!   runs to completion on its own, as in JS). Wrapping the closure value, rather than nesting
//!   its body in another arrow, keeps its capture rules: each call of an async closure that may
//!   run on another thread copies what it captured;
//! - an arrow that is not `async` whose declared result type is a member of the expected union
//!   (`(req): Response => …`): checked against that member, then wrapped;
//! - an arrow that is not `async` passed to `serve` ([`FnCx::thread_arrow`]): checked as an
//!   `async` arrow, its expression body awaited when it is a promise;
//! - an arrow that is not `async` passed to a standard timer, which takes the promise to run
//!   (#501): it is checked as an `async` arrow ([void task]), its expression body as a
//!   statement, awaited when it is a promise (`() => save(doc)` runs `save` to completion).
//!
//! [void task]: FnCx::void_task_body

use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::collect::ret_infer::stmt_returns_value;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyId, TyKind};

/// What a callee does with a callback argument (`FnCx::std_callback_arg`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallbackMode {
    /// Calls it.
    Plain,
    /// A standard timer: runs the promise it returns.
    Task,
    /// `serve`: runs it on several threads at once.
    Thread,
}

/// What a wrapper calls.
enum Callee {
    /// A module-level function, by name.
    Named(ast::Expr),
    /// A function value, read once (and the closure a closure `const` holds).
    Value(hir::Expr, Option<DefId>),
}

/// How a wrapper calls the function it wraps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wrap {
    /// `(p…) => f(p…)`.
    Call,
    /// `(p…) => { const p = f(p…); }`: the started promise runs on its own.
    Discard,
    /// `() => f()` checked as an async arrow (a timer's task).
    Task,
    /// `(p…) => f(p…)` checked as an async arrow, awaiting a promise (a server's handler).
    Thread,
}

/// The timer functions whose first parameter is the callback.
const TIMERS: [&str; 3] = ["setTimeout", "setInterval", "setImmediate"];

/// What a callback adapter needs to know about the function a name refers to.
struct FnSig {
    /// Its parameters, and how many of them must be passed (the rest are optional or have
    /// defaults).
    params: usize,
    required: usize,
    /// Its result type (`None` when not asked for).
    ret: Option<TyId>,
}

/// An expression with `span` and no node of its own.
fn synth(kind: ast::ExprKind, span: velt_common::Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

/// `#arg{k}`: a parameter name the wrapped code cannot name.
fn arg_name(k: usize, span: velt_common::Span) -> ast::Ident {
    ast::Ident {
        name: format!("#arg{k}"),
        span,
    }
}

/// `(#arg0, …, #arg{n-1}) => callee(#arg0, …, #arg{passed-1})`, or with `discard` a block body
/// that keeps the call's value in a hidden `const` (a started promise runs to completion).
fn wrapper(
    callee: ast::Expr,
    n: usize,
    passed: usize,
    discard: bool,
    span: velt_common::Span,
) -> ast::Expr {
    let call = synth(
        ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: (0..passed)
                .map(|k| synth(ast::ExprKind::Ident(arg_name(k, span)), span))
                .collect(),
            optional: false,
        },
        span,
    );
    let body = if discard {
        let pattern = ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(ast::Ident {
                name: "#started".into(),
                span,
            }),
            span,
        };
        ast::ArrowBody::Block(ast::Block {
            stmts: vec![ast::Stmt {
                kind: ast::StmtKind::Var(ast::VarDecl {
                    kind: ast::VarKind::Const,
                    pattern,
                    ty: None,
                    init: Some(call),
                    span,
                }),
                span,
            }],
            span,
        })
    } else {
        ast::ArrowBody::Expr(Box::new(call))
    };
    synth(
        ast::ExprKind::Arrow {
            type_params: vec![],
            params: (0..n)
                .map(|k| ast::ArrowParam {
                    name: arg_name(k, span),
                    ty: None,
                    default: None,
                    optional: false,
                })
                .collect(),
            ret: None,
            throws: None,
            body,
            is_async: false,
        },
        span,
    )
}

impl FnCx<'_, '_> {
    /// The function value `arg` adapted to the function type `expected`, when the two differ
    /// in a way TS allows (see the module docs): the value is evaluated once into a hidden local
    /// and wrapped. `mode` says what the callee does with it. `None` when no adapter is needed
    /// (or `arg` is an arrow, which `closure` adapts).
    pub(super) fn callback_adapter(
        &mut self,
        arg: &ast::Expr,
        expected: TyId,
        mode: CallbackMode,
    ) -> Option<hir::Expr> {
        if as_arrow(arg).is_some() {
            return None;
        }
        // `ns.f` (a function of a namespace import) is the function `f`, by name.
        let resolved = self
            .without_namespace(arg)
            .filter(|e| matches!(e.kind, ast::ExprKind::Ident(_)));
        let arg = resolved.as_ref().unwrap_or(arg);
        // `f?: (s: string) => void` is `((s: string) => void) | null`: adapt to the function type.
        let exp = expected;
        let expected = self.cx.ty.opt_payload(expected).unwrap_or(expected);
        let TyKind::FnPtr {
            params: want,
            ret: want_ret,
            ..
        } = self.cx.ty.kind(expected).clone()
        else {
            return None;
        };
        let n = want.len();
        if mode == CallbackMode::Thread {
            // Any function value for a handler that runs on several threads: an async wrapper,
            // so each call copies what it captured like any async closure.
            if let Some(sig) = self.named_fn_sig(arg, false) {
                if sig.required <= n {
                    let callee = self.callback_callee(arg);
                    return Some(self.wrap(callee, n, sig.params.min(n), Wrap::Thread, Some(exp)));
                }
            }
            let value = self.callback_value(arg);
            let passed = match self.cx.ty.kind(value.ty) {
                TyKind::FnPtr { params, .. } => params.len().min(n),
                _ => return Some(value),
            };
            return Some(self.wrap(
                Callee::Value(value, None),
                n,
                passed,
                Wrap::Thread,
                Some(exp),
            ));
        }
        let task = mode == CallbackMode::Task;
        let to_void = want_ret == self.cx.ty.unit;
        // The result matters only for `void` (dropped), union results (widened) and tasks.
        let widening = {
            let inner = self.cx.ty.opt_payload(want_ret).unwrap_or(want_ret);
            inner != want_ret || self.cx.union_def(inner).is_some()
        };
        let sig = self.named_fn_sig(arg, to_void || widening || task)?;
        let ret_known = sig
            .ret
            .filter(|r| !self.cx.ty.is_bottom(*r) && !self.cx.ty.has_error(*r));
        let returns_promise = ret_known.is_some_and(|r| self.cx.ty.promise_payload(r).is_some());
        let drops_result = to_void && ret_known.is_some_and(|r| r != self.cx.ty.unit);
        let widens_result = ret_known.is_some_and(|r| self.result_widens(r, want_ret));
        // A sync function where a timer wants the promise to run: checked as an async arrow.
        let becomes_task = task && !returns_promise;
        if sig.required > n {
            return None;
        }
        if sig.params == n && !drops_result && !widens_result && !becomes_task {
            return None;
        }
        let passed = sig.params.min(n);
        let wrap = if becomes_task {
            Wrap::Task
        } else if to_void && returns_promise {
            // An async function as a `void` callback: the promise it starts runs on its own.
            Wrap::Discard
        } else {
            Wrap::Call
        };
        let callee = self.callback_callee(arg);
        Some(self.wrap(callee, n, passed, wrap, Some(exp)))
    }

    /// What a wrapper of the function `arg` names calls: a module-level function by its name
    /// (it can't be reassigned, and a call by name fills in defaults), a local's value read
    /// once (a closure `const` keeps filling in its parameters' defaults).
    fn callback_callee(&mut self, arg: &ast::Expr) -> Callee {
        if let ast::ExprKind::Ident(id) = &arg.kind {
            if !self.is_local_name(&id.name) {
                return Callee::Named(arg.clone());
            }
            let closure = self.local_closure_const(&id.name);
            return Callee::Value(self.callback_value(arg), closure);
        }
        Callee::Value(self.callback_value(arg), None)
    }

    /// The function value `arg` names, read once for a wrapper: a variable still used
    /// afterwards passes a copy (JS shares the function).
    fn callback_value(&mut self, arg: &ast::Expr) -> hir::Expr {
        let v = self.expr(arg, None, Want::Move);
        if is_place(&v) {
            self.f.soft_moves.push(v.span);
        }
        v
    }

    /// The wrapper arrow calling `callee` with `passed` of its `n` parameters, checked against
    /// `exp`. A value is held in a hidden local first: `{ const f = value; (p0, …) => f(p0, …) }`;
    /// the wrapper owns `f` (it outlives the block).
    fn wrap(
        &mut self,
        callee: Callee,
        n: usize,
        passed: usize,
        wrap: Wrap,
        exp: Option<TyId>,
    ) -> hir::Expr {
        let (value, closure, call) = match callee {
            Callee::Named(e) => (None, None, e),
            Callee::Value(v, closure) => {
                let name = ast::Ident {
                    name: format!("#callback{}", self.f.locals.len()),
                    span: v.span,
                };
                let span = v.span;
                (
                    Some((v, name.clone())),
                    closure,
                    synth(ast::ExprKind::Ident(name), span),
                )
            }
        };
        let span = call.span;
        self.push_scope();
        let bound = value.map(|(v, name)| {
            let local = self.declare_local(&name, v.ty, LocalKind::Const);
            if let Some(c) = closure {
                self.f.closure_consts.insert(local, c);
            }
            (local, v)
        });
        let arrow = wrapper(call, n, passed, wrap == Wrap::Discard, span);
        match wrap {
            Wrap::Task => self.void_task = Some(arrow.span),
            Wrap::Thread => {
                self.thread_task = Some(arrow.span);
                self.thread_adapter = Some(arrow.span);
            }
            Wrap::Call | Wrap::Discard => {}
        }
        let w = self.closure(&arrow, exp, true);
        self.pop_scope();
        let Some((local, value)) = bound else {
            return w;
        };
        if let H::Closure(c) = &w.kind {
            if matches!(&value.kind, H::Closure(inner) if self.cx.fn_info(*inner).is_async) {
                self.cx.callback_wrappers.insert(*c, value.ty);
            }
        }
        let ty = w.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Let {
                    local,
                    init: Some(value),
                },
                span,
            }],
            value: Some(Box::new(w)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// Whether a function returning `ret` fits where one returning `want` is expected only
    /// through its result: `want` is a union (or `T | null`) with `ret` as a member.
    fn result_widens(&mut self, ret: TyId, want: TyId) -> bool {
        if ret == want {
            return false;
        }
        let inner = self.cx.ty.opt_payload(want).unwrap_or(want);
        inner == ret
            || self
                .cx
                .union_members(inner)
                .is_some_and(|ms| ms.contains(&ret))
    }

    /// The parameters (and with `with_ret` the result type, inferring it from the body if
    /// needed) of the function `arg` names: a local of function type (a closure `const` knows
    /// which of its parameters have defaults), or a non-generic module-level function.
    fn named_fn_sig(&mut self, arg: &ast::Expr, with_ret: bool) -> Option<FnSig> {
        let ast::ExprKind::Ident(id) = &arg.kind else {
            return None;
        };
        if let Some(t) = self.peek_local_ty(&id.name) {
            let TyKind::FnPtr { params, ret, .. } = self.cx.ty.kind(t).clone() else {
                return None;
            };
            let closure = self.local_closure_const(&id.name);
            let required = match closure {
                Some(c) => {
                    let ps = &self.cx.fn_info(c).params;
                    ps.iter().take_while(|p| p.default.is_none()).count()
                }
                None => params.len(),
            };
            return Some(FnSig {
                params: params.len(),
                required,
                ret: Some(ret),
            });
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            Item::Def(d) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)) => {
                let f = self.cx.fn_info(d);
                if f.generics.len() != 0 {
                    return None;
                }
                let params = f.params.len();
                let required = f.params.iter().take_while(|p| p.default.is_none()).count();
                let ret = with_ret.then(|| self.callee_ret(d, id.span));
                Some(FnSig {
                    params,
                    required,
                    ret,
                })
            }
            _ => None,
        }
    }

    /// What calling the named function `d` gives: its result type, a promise of it for an
    /// `async` function.
    fn callee_ret(&mut self, d: DefId, span: velt_common::Span) -> TyId {
        let r = crate::body::returns::ret_of(self.cx, d, span);
        if self.is_async_fn(d) {
            self.async_call_ret(d, r)
        } else {
            r
        }
    }

    /// An `async` arrow where the function type returns `void` or a union with one promise
    /// member (see the module docs); `None` for any other expected type.
    pub(super) fn async_arrow_callback(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
    ) -> Option<hir::Expr> {
        let fn_ty = self.hint(exp)?;
        let TyKind::FnPtr {
            params,
            ret,
            throws,
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return None;
        };
        let discard = ret == self.cx.ty.unit;
        let promise = if discard {
            self.cx.ty.promise(ret)
        } else {
            self.promise_member(ret)?
        };
        let span = e.span;
        let n = params.len();
        let async_ty = self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret: promise,
            throws,
        });
        // A `void` callback's promise rejects with what the arrow throws.
        let async_ty = if discard {
            self.cx.ty.without_error_types(async_ty)
        } else {
            async_ty
        };
        let f = self.closure(e, Some(async_ty), true);
        if self.thread_adapter == Some(e.span) {
            self.thread_adapter = None;
            if let H::Closure(c) = &f.kind {
                self.cx.thread_adapters.push(*c);
            }
        }
        // An error type still unknown (`E` of a generic callee's `T | Promise<T, E>`) is what the
        // async closure rejects with, now that it is checked: the wrapper is typed with it.
        let exp = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr { ret: found, .. } if !discard && self.cx.ty.has_error(ret) => {
                Some(self.with_promise_member(fn_ty, ret, promise, found, span))
            }
            _ => exp,
        };
        let wrap = if discard { Wrap::Discard } else { Wrap::Call };
        Some(self.wrap(Callee::Value(f, None), n, n, wrap, exp))
    }

    /// An arrow that is not `async` whose declared result type is a member of the union the
    /// function type returns (`(req): Response => …` for `(req) => Response | Promise<Response>`):
    /// checked as a function returning that member, then wrapped so its result converts.
    pub(super) fn declared_member_callback(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Arrow {
            ret: Some(ret),
            is_async: false,
            params,
            ..
        } = &e.kind
        else {
            return None;
        };
        let fn_ty = self.hint(exp)?;
        let TyKind::FnPtr {
            params: ptys,
            ret: want,
            throws,
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return None;
        };
        let inner = self.cx.ty.opt_payload(want).unwrap_or(want);
        if inner == want && self.cx.union_def(inner).is_none() {
            return None;
        }
        let declared = self.resolve_quiet(ret)?;
        if !self.result_widens(declared, want) {
            return None;
        }
        let n = ptys.len().max(params.len());
        let member_ty = self.cx.ty.intern(TyKind::FnPtr {
            params: ptys,
            ret: declared,
            throws,
        });
        let f = self.closure(e, Some(member_ty), true);
        let exp = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr { ret: found, .. }
                if self.cx.ty.has_error(want) && self.cx.ty.promise_payload(found).is_some() =>
            {
                let member = self.promise_member(want).unwrap_or(found);
                Some(self.with_promise_member(fn_ty, want, member, found, e.span))
            }
            _ => exp,
        };
        let passed = match self.cx.ty.kind(f.ty) {
            TyKind::FnPtr { params, .. } => params.len().min(n),
            _ => n,
        };
        Some(self.wrap(Callee::Value(f, None), n, passed, Wrap::Call, exp))
    }

    /// An arrow that is not `async`, passed as the handler of a server that runs it on several
    /// threads (`serve`): checked as an `async` arrow, so each call copies what it captured and
    /// modifying a capture is an error, as for any async handler. A declared result type that
    /// is not a promise is the promise's value type, and an expression body that is a promise
    /// is awaited (`(req) => respond(req)`).
    pub(super) fn thread_arrow(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        escaping: bool,
    ) -> hir::Expr {
        let ast::ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body,
            ..
        } = &e.kind
        else {
            unreachable!("ICE: thread_arrow of a non-arrow expression")
        };
        let ret = ret.as_ref().map(|t| {
            let is_promise = self
                .resolve_quiet(t)
                .is_some_and(|r| self.cx.ty.promise_payload(r).is_some());
            if is_promise {
                t.clone()
            } else {
                ast::TypeExpr {
                    kind: ast::TypeExprKind::Named {
                        path: vec![ast::Ident {
                            name: "Promise".into(),
                            span: t.span,
                        }],
                        args: vec![t.clone()],
                    },
                    span: t.span,
                }
            }
        });
        let as_async = synth(
            ast::ExprKind::Arrow {
                type_params: type_params.clone(),
                params: params.clone(),
                ret,
                throws: throws.clone(),
                body: body.clone(),
                is_async: true,
            },
            e.span,
        );
        self.await_body = Some(e.span);
        self.cx.sync_handlers.push(e.span);
        let h = self.closure(&as_async, exp, escaping);
        self.await_body = None;
        h
    }

    /// The type `t` names, without diagnostics or IDE records (the arrow's own check reports
    /// what is wrong with it).
    fn resolve_quiet(&mut self, t: &ast::TypeExpr) -> Option<TyId> {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let r = self.resolve(t);
        mark.rollback(self.cx);
        (!self.cx.ty.has_error(r)).then_some(r)
    }

    /// The body of an expression-bodied arrow checked as `async` by [`FnCx::thread_arrow`]: its
    /// value, awaited when it is a promise or may be one.
    pub(super) fn awaited_body(&mut self, e: &ast::Expr) -> hir::Block {
        self.direct_await = super::promise_new::awaited_new_promise(e);
        let h = self.expr(e, self.f.ret, Want::Move);
        self.direct_await = None;
        let h = match self.cx.ty.kind(h.ty).clone() {
            TyKind::Promise(v, _) => {
                self.awaited_using_shares(&h);
                self.await_throws(&h);
                let span = h.span;
                self.mk(H::Await(Box::new(h)), v, span)
            }
            // A value that may be a promise (a handler of `serve`'s own type, passed on).
            _ => {
                let span = h.span;
                self.await_union(h, span).unwrap_or_else(|h| h)
            }
        };
        let h = match self.f.ret {
            Some(r) => self.coerce(h, r),
            None => {
                self.f.ret = Some(h.ty);
                h
            }
        };
        let span = h.span;
        let kind = if h.ty == self.cx.ty.unit {
            S::Expr(h)
        } else {
            S::Return(Some(h))
        };
        hir::Block {
            stmts: vec![hir::Stmt { kind, span }],
            value: None,
            span: e.span,
        }
    }

    /// The function type `fn_ty` (returning `ret`) with `ret`'s promise member `old` replaced
    /// by `new`, and an unknown error type it throws by `new`'s.
    fn with_promise_member(
        &mut self,
        fn_ty: TyId,
        ret: TyId,
        old: TyId,
        new: TyId,
        span: velt_common::Span,
    ) -> TyId {
        let TyKind::FnPtr { params, throws, .. } = self.cx.ty.kind(fn_ty).clone() else {
            return fn_ty;
        };
        let inner = self.cx.ty.opt_payload(ret).unwrap_or(ret);
        let members: Vec<TyId> = match self.cx.union_members(inner) {
            Some(ms) => ms
                .into_iter()
                .map(|m| if m == old { new } else { m })
                .collect(),
            None => vec![new],
        };
        let ret = self.cx.union_of(&members, inner != ret, span);
        let throws = if self.cx.ty.has_error(throws) {
            self.cx.ty.promise_error(new).unwrap_or(self.cx.ty.never)
        } else {
            throws
        };
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    /// The promise member of `ret` when `ret` is not a promise but a union (or `T | null`) with
    /// exactly one promise member.
    fn promise_member(&mut self, ret: TyId) -> Option<TyId> {
        if self.cx.ty.promise_payload(ret).is_some() || ret == self.cx.ty.error {
            return None;
        }
        let inner = self.cx.ty.opt_payload(ret).unwrap_or(ret);
        if self.cx.ty.promise_payload(inner).is_some() {
            return Some(inner);
        }
        let members = self.cx.union_members(inner)?;
        let mut promises = members
            .into_iter()
            .filter(|m| self.cx.ty.promise_payload(*m).is_some());
        match (promises.next(), promises.next()) {
            (Some(p), None) => Some(p),
            _ => None,
        }
    }

    /// For a call of a standard function that takes a callback: a timer (`setTimeout`, …), whose
    /// callback is the promise to run, or `serve`, whose handler runs on several threads. Notes
    /// the callback argument (`void_task` / `thread_task` for an arrow, `task_callback` /
    /// `thread_callback` for any other value), for `closure` and `callback_adapter`.
    pub(super) fn std_callback_arg(&mut self, d: DefId, args: &[ast::Expr]) {
        let info = self.cx.fn_info(d);
        if !self.cx.scopes[info.module].is_std {
            return;
        }
        let name = info.name.rsplit("::").next().unwrap_or("");
        let (k, thread) = if TIMERS.contains(&name) {
            (0, false)
        } else if name == "serve" && self.cx.modules[info.module].path == "std/http" {
            (1, true)
        } else {
            return;
        };
        let Some(arg) = args.get(k) else {
            return;
        };
        match (as_arrow(arg), thread) {
            (Some(a), false) => self.void_task = Some(a.span),
            (Some(a), true) => {
                if !matches!(a.kind, ast::ExprKind::Arrow { is_async: true, .. }) {
                    self.thread_task = Some(a.span);
                }
            }
            (None, false) => self.task_callback = Some(arg.span),
            (None, true) => self.thread_callback = Some(arg.span),
        }
    }

    /// How the callee uses the callback argument `arg` (`std_callback_arg`).
    pub(super) fn callback_mode(&self, arg: &ast::Expr) -> CallbackMode {
        if self.thread_callback == Some(arg.span) {
            CallbackMode::Thread
        } else if self.task_callback == Some(arg.span) {
            CallbackMode::Task
        } else {
            CallbackMode::Plain
        }
    }

    /// Is the arrow (its parts) a callback to check as an `async` one: not `async` (checked by
    /// the caller), no parameters, no result type, and no `return` with a value?
    pub(super) fn is_void_task(
        &self,
        params: &[ast::ArrowParam],
        ret: &Option<ast::TypeExpr>,
        body: &ast::ArrowBody,
    ) -> bool {
        params.is_empty()
            && ret.is_none()
            && match body {
                ast::ArrowBody::Expr(_) => true,
                ast::ArrowBody::Block(b) => !b.stmts.iter().any(stmt_returns_value),
            }
    }

    /// The body of an expression-bodied callback checked as `async`: the expression as a
    /// statement, awaited when it is a promise.
    pub(super) fn void_task_body(&mut self, e: &ast::Expr) -> hir::Block {
        self.direct_await = super::promise_new::awaited_new_promise(e);
        let h = self.expr(e, None, Want::Move);
        self.direct_await = None;
        let h = match self.cx.ty.kind(h.ty).clone() {
            TyKind::Promise(v, _) => {
                self.awaited_using_shares(&h);
                self.await_throws(&h);
                let span = h.span;
                self.mk(H::Await(Box::new(h)), v, span)
            }
            _ => h,
        };
        let span = h.span;
        hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Expr(h),
                span,
            }],
            value: None,
            span: e.span,
        }
    }
}

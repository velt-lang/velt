//! Functions passed as callbacks whose type differs from the expected function type the way TS
//! allows. Every case is one adaptation: the function is wrapped in an arrow typed by the
//! expected type, so the closure's own rules (captures, ownership, async) apply to the wrapper
//! as to any arrow.
//!
//! A **function value** (a named function, or a local holding a function value) is wrapped as
//! `(p0, …) => f(p0, …)` ([`FnCx::callback_adapter`]) when
//! - the expected type passes more parameters (`xs.map(double)`, `map` passes `(x, i)`): `f`
//!   gets the leading ones;
//! - it takes more parameters than the expected type passes, all of them optional or with
//!   defaults (`setTimeout(tick, 10)` with `function tick(n?: number)`): `f` gets none of them;
//! - the expected type returns `void` and `f` returns a value: it is dropped;
//! - the expected type returns a union with `f`'s result as a member (`(x) => Resp` for
//!   `(x) => Resp | Promise<Resp>`): it converts;
//! - a standard timer expects the promise to run (`() => Promise<void>`) and `f` is not `async`
//!   (`setTimeout(tick, 10)`): the wrapper is checked as an `async` arrow ([`void task`]).
//!
//! An **arrow** is checked against the expected type itself; the cases that need more are
//! - an `async` arrow where the function type returns `void` (`onClick: () => void`) or a union
//!   with one promise member (`(n) => View | Promise<View>`) ([`FnCx::async_arrow_callback`]):
//!   it is an async closure held in a hidden local `f`, wrapped as `(p…) => f(p…)` (the
//!   promise converts to the union) or `(p…) => { const p = f(p…); }` (the started promise
//!   runs to completion on its own, as in JS). Wrapping the closure value, rather than nesting
//!   its body in another arrow, keeps its capture rules: each call of an async closure that may
//!   run on another thread copies what it captured;
//! - an arrow that is not `async` passed to a standard timer, which takes the promise to run
//!   (#501): it is checked as an `async` arrow ([void task]), its expression body as a
//!   statement, awaited when it is a promise (`() => save(doc)` runs `save` to completion).
//!
//! [void task]: FnCx::void_task_body

use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::{FnCx, LocalKind, Want};
use crate::collect::ret_infer::stmt_returns_value;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyId, TyKind};

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
    /// The arrow standing for the function value `arg` where a value of the function type
    /// `expected` is expected, when the two differ in a way TS allows (see the module docs);
    /// `task`: a standard timer, which takes the promise to run. Only plain names (a function,
    /// or a local holding a function value): the wrapper evaluates `arg` again on each call.
    pub(super) fn callback_adapter(
        &mut self,
        arg: &ast::Expr,
        expected: TyId,
        task: bool,
    ) -> Option<ast::Expr> {
        // `f?: (s: string) => void` is `((s: string) => void) | null`: adapt to the function type.
        let expected = self.cx.ty.opt_payload(expected).unwrap_or(expected);
        let TyKind::FnPtr {
            params: want,
            ret: want_ret,
            ..
        } = self.cx.ty.kind(expected).clone()
        else {
            return None;
        };
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
        let drops_result = to_void && ret_known.is_some_and(|r| r != self.cx.ty.unit);
        let widens_result = ret_known.is_some_and(|r| self.result_widens(r, want_ret));
        // A sync function where a timer wants the promise to run: checked as an async arrow.
        let becomes_task =
            task && ret_known.is_none_or(|r| self.cx.ty.promise_payload(r).is_none());
        let n = want.len();
        if sig.required > n {
            return None;
        }
        if sig.params == n && !drops_result && !widens_result && !becomes_task {
            return None;
        }
        let passed = sig.params.min(n);
        let w = wrapper(arg.clone(), n, passed, false, arg.span);
        if becomes_task {
            self.void_task = Some(w.span);
        }
        Some(w)
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
        // An error type still unknown (`E` of a generic callee's `T | Promise<T, E>`) is what the
        // async closure rejects with, now that it is checked: the wrapper is typed with it.
        let exp = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr { ret: found, .. } if !discard && self.cx.ty.has_error(ret) => {
                Some(self.with_promise_member(fn_ty, ret, promise, found, span))
            }
            _ => exp,
        };
        let name = ast::Ident {
            name: format!("#async{}", self.f.locals.len()),
            span,
        };
        self.push_scope();
        let local = self.declare_local(&name, f.ty, LocalKind::Const);
        let callee = synth(ast::ExprKind::Ident(name), span);
        // The wrapper owns the async closure (it escapes the hidden local's block): borrowing
        // it would leave a callee that keeps the wrapper with a dropped closure.
        let w = self.closure(&wrapper(callee, n, n, discard, span), exp, true);
        self.pop_scope();
        let ty = w.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Let {
                    local,
                    init: Some(f),
                },
                span,
            }],
            value: Some(Box::new(w)),
            span,
        };
        Some(self.mk(H::Block(block), ty, span))
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

    /// For a call of a standard timer function `d`: notes a callback arrow that is not `async`
    /// (`void_task`), and returns the arguments with a function value that is not `async`
    /// replaced by its adapter (`callback_adapter`).
    pub(super) fn timer_callback(
        &mut self,
        d: DefId,
        args: &[ast::Expr],
    ) -> Option<Vec<ast::Expr>> {
        let info = self.cx.fn_info(d);
        if !self.cx.scopes[info.module].is_std
            || !TIMERS.contains(&info.name.rsplit("::").next().unwrap_or(""))
        {
            return None;
        }
        let expected = info.params.first()?.ty;
        let first = args.first()?;
        if let Some(arrow) = as_arrow(first) {
            self.void_task = Some(arrow.span);
            return None;
        }
        let adapted = self.callback_adapter(first, expected, true)?;
        let mut out = args.to_vec();
        out[0] = adapted;
        Some(out)
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

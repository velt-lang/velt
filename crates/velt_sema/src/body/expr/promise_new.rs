//! `new Promise<T, E>((resolve, reject) => …)`: a call of the prelude's `promiseNew` (or
//! `promiseNewResolveOnly` for a one-parameter executor), unless a class named `Promise` is in
//! scope. `T` and `E` come from the type arguments, else from the expected type; without either,
//! `E` is `never` and `reject` cannot be called.
//!
//! The executor must be an arrow-function literal: its `resolve` and `reject` are heap closures
//! the prelude makes, so the literal may keep them (`FnInfo::keeps_fn_params`). The call also
//! gets the source location of the `new Promise` and whether it is awaited directly, for the
//! report when an abandoned promise is awaited (std/prelude/promise.vlt).
//!
//! `Promise.withResolvers<T, E>()` (ES2024) is the same slot without an executor: a call of the
//! prelude's `promiseWithResolvers`, whose `resolve` and `reject` are fields of the result.
//! `Promise.resolve(v)` and `Promise.reject(e)` are calls of `promiseResolve` / `promiseReject`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(super) fn promise_new(
        &mut self,
        class: &ast::TypeExpr,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let targs = promise_class(class)?;
        if self.lookup_item("Promise", class.span).is_some() {
            return None;
        }
        let direct = self.direct_await.take() == Some(span);
        let Some(params) = executor_params(args) else {
            self.cx.error(
                Diagnostic::error(
                    "the executor of `new Promise` must be an arrow function, as in `new Promise((resolve, reject) => …)`",
                    args.first().map_or(span, |a| a.span),
                )
                .with_note("`resolve` and `reject` are made by the compiler; only an arrow literal may keep them"),
            );
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        };
        let hint = self.hint(exp);
        let type_args = self.promise_type_args(class.span, targs, hint);
        let (name, type_args) = match params {
            1 => (
                "promiseNewResolveOnly",
                &type_args[..type_args.len().min(1)],
            ),
            _ => ("promiseNew", &type_args[..]),
        };
        let mut call = self.prelude_call(name, "new Promise", type_args, &args[..1], hint, span);
        self.finish_promise_new(&mut call, span, direct);
        Some(call)
    }

    /// `Promise.withResolvers<T, E>()`: a call of the prelude's `promiseWithResolvers`. `T` and
    /// `E` come from the type arguments (`E` defaults to `never`), else from the expected type.
    pub(super) fn promise_with_resolvers(
        &mut self,
        targs: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let hint = self.hint(exp);
        let expected = hint
            .and_then(|h| self.field_of(h, "promise"))
            .map(|(_, t)| t);
        let type_args = self.promise_type_args(span, targs, expected);
        if type_args.is_empty() {
            self.cx.err(
                "cannot infer the type of `Promise.withResolvers`: write `Promise.withResolvers<T>()` or `Promise.withResolvers<T, E>()`",
                span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let what = "Promise.withResolvers";
        self.prelude_call("promiseWithResolvers", what, &type_args, args, hint, span)
    }

    /// `Promise.resolve(value)` / `Promise.reject(reason)`: calls of the prelude's
    /// `promiseResolve` (`promiseResolveVoid` without an argument: a `Promise<void>`) /
    /// `promiseReject`. As in TS, the one type argument is the promise's value type: for
    /// `reject` it comes from the type argument or the expected type, else it is `never`.
    /// `Promise.resolve(p)` of a promise is `p`.
    pub(super) fn promise_settled(
        &mut self,
        which: &str,
        targs: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if targs.len() > 1 {
            self.cx.err(
                format!("`Promise.{which}` takes at most 1 type argument"),
                span,
            );
        }
        let mut type_args: Vec<TyId> = targs.iter().take(1).map(|t| self.resolve(t)).collect();
        let hint = self.hint(exp);
        let what = format!("Promise.{which}");
        if which == "resolve" {
            if args.is_empty() && type_args.is_empty() {
                return self.prelude_call("promiseResolveVoid", &what, &[], args, hint, span);
            }
            let [arg] = args else {
                return self.prelude_call("promiseResolve", &what, &type_args, args, hint, span);
            };
            return self.promise_resolve(arg, type_args.first().copied(), hint, span);
        }
        if type_args.is_empty() {
            type_args.push(match hint.map(|h| self.cx.ty.kind(h)) {
                Some(&TyKind::Promise(t, _)) => t,
                _ => self.cx.ty.never,
            });
        }
        self.prelude_call("promiseReject", &what, &type_args, args, hint, span)
    }

    /// `Promise.resolve(arg)` (`Promise.resolve<T>(arg)` when `targ` is given): the argument may
    /// be a value or a promise, so it is checked first and the call built from its type. A promise is
    /// returned itself, as in JS (converted to the `Promise<T>` the type argument or the expected
    /// type asks for); a value becomes a call of the prelude's `promiseResolve`.
    fn promise_resolve(
        &mut self,
        arg: &ast::Expr,
        targ: Option<TyId>,
        hint: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let value = targ.or_else(|| match hint.map(|h| self.cx.ty.kind(h)) {
            Some(&TyKind::Promise(t, _)) => Some(t),
            _ => None,
        });
        // The hint: `T` for an expression that is never a promise (a literal, an array, …),
        // else `Promise<T>`, so that `Promise.resolve(Promise.resolve(4))` checks its inner call
        // at the expected type.
        let arg_hint = value.map(|t| {
            if never_a_promise(arg) {
                t
            } else {
                self.cx.ty.promise(t)
            }
        });
        let h = self.expr(arg, arg_hint, Want::Move);
        if self.cx.ty.has_error(h.ty) {
            return self.error_expr(span);
        }
        let target = match (self.cx.ty.kind(h.ty), value) {
            (_, None) => None,
            (&TyKind::Promise(_, e), Some(t)) => Some(self.cx.ty.promise_rejecting(t, e)),
            (_, Some(t)) => Some(t),
        };
        let h = match target {
            Some(target) => match self.try_coerce(h, target) {
                Ok(h) => h,
                Err(h) => {
                    self.report_mismatch(target, &h);
                    return self.error_expr(span);
                }
            },
            None => h,
        };
        if matches!(self.cx.ty.kind(h.ty), TyKind::Promise(..)) {
            return h;
        }
        let Some(Item::Def(d)) = self.cx.prelude.get("promiseResolve").cloned() else {
            self.cx
                .err("`Promise.resolve` needs the prelude (std/prelude)", span);
            return self.error_expr(span);
        };
        let mut report = |s: &mut Self, _: &str, expected: TyId, found: &hir::Expr| {
            s.report_mismatch(expected, found);
        };
        self.call_checked(d, vec![h], span, &mut report)
    }

    /// `T` and `E` from the type arguments or the expected `Promise<T, E>` (`E` defaults to
    /// `never`).
    fn promise_type_args(
        &mut self,
        span: Span,
        targs: &[ast::TypeExpr],
        hint: Option<TyId>,
    ) -> Vec<TyId> {
        if targs.len() > 2 {
            self.cx
                .err("`Promise` takes at most 2 type arguments", span);
        }
        let mut type_args: Vec<TyId> = targs.iter().take(2).map(|t| self.resolve(t)).collect();
        if type_args.is_empty() {
            if let Some(&TyKind::Promise(t, e)) = hint.map(|h| self.cx.ty.kind(h)) {
                type_args = vec![t, e];
            }
        }
        if type_args.len() == 1 {
            type_args.push(self.cx.ty.never);
        }
        type_args
    }

    /// The executor literal may keep its parameters; the trailing (defaulted) parameters of the
    /// prelude function get the site and whether the promise is awaited directly.
    fn finish_promise_new(&mut self, call: &mut hir::Expr, span: Span, direct: bool) {
        let H::Call { args, .. } = &mut call.kind else {
            return;
        };
        if let Some(hir::Expr {
            kind: H::Closure(def),
            ..
        }) = args.first()
        {
            self.cx.fn_info_mut(*def).keeps_fn_params = true;
        }
        if let [_, site, awaited] = args.as_mut_slice() {
            let call = H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::SourceLocation),
                args: vec![],
            };
            *site = self.mk(call, self.cx.ty.str_, span);
            *awaited = self.mk(H::Lit(hir::Lit::Bool(direct)), self.cx.ty.bool_, span);
        }
    }
}

/// Whether `e` is a kind of expression whose value is never a promise (parentheses allowed): a
/// literal, an array, an arithmetic operation, a conditional of such, ….
fn never_a_promise(e: &ast::Expr) -> bool {
    use ast::{BinaryOp as B, ExprKind as E};
    match &e.kind {
        E::Paren(inner) => never_a_promise(inner),
        E::Cond { then, els, .. } => never_a_promise(then) && never_a_promise(els),
        E::Binary {
            op: B::And | B::Or | B::Nullish,
            lhs,
            rhs,
        } => never_a_promise(lhs) && never_a_promise(rhs),
        E::Lit(_)
        | E::Template { .. }
        | E::Unary { .. }
        | E::Binary { .. }
        | E::Update { .. }
        | E::Arrow { .. }
        | E::Function(_)
        | E::Array(_)
        | E::Object(_) => true,
        _ => false,
    }
}

/// The type arguments of `Promise<…>` if `class` names `Promise`.
fn promise_class(class: &ast::TypeExpr) -> Option<&[ast::TypeExpr]> {
    let ast::TypeExprKind::Named { path, args } = &class.kind else {
        return None;
    };
    match path.as_slice() {
        [name] if name.name == "Promise" => Some(args),
        _ => None,
    }
}

/// The parameter count (1 or 2) of the one argument if it is an arrow literal (parentheses
/// allowed).
fn executor_params(args: &[ast::Expr]) -> Option<usize> {
    let [arg] = args else { return None };
    let mut e = arg;
    while let ast::ExprKind::Paren(inner) = &e.kind {
        e = inner;
    }
    match &e.kind {
        ast::ExprKind::Arrow { params, .. } if (1..=2).contains(&params.len()) => {
            Some(params.len())
        }
        _ => None,
    }
}

/// The `new Promise` operand of an `await`, if it is one (parentheses allowed): its span.
pub(super) fn awaited_new_promise(inner: &ast::Expr) -> Option<Span> {
    let mut e = inner;
    while let ast::ExprKind::Paren(x) = &e.kind {
        e = x;
    }
    match &e.kind {
        ast::ExprKind::New { class, .. } if promise_class(class).is_some() => Some(e.span),
        _ => None,
    }
}

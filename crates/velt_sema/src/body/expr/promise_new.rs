//! `new Promise<T, E>((resolve, reject) => …)`: a call of the prelude's `promiseNew` (or
//! `promiseNewResolveOnly` for a one-parameter executor), unless a class named `Promise` is in
//! scope. `T` and `E` come from the type arguments, else from the expected type; without either,
//! `E` is `never` and `reject` cannot be called.
//!
//! The executor must be an arrow-function literal: its `resolve` and `reject` are heap closures
//! the prelude makes, so the literal may keep them (`FnInfo::keeps_fn_params`). The call also
//! gets the source location of the `new Promise` and whether it is awaited directly, for the
//! report when an abandoned promise is awaited (std/prelude/promise.vlt).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
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

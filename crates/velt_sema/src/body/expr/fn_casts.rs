//! `f as unknown as F` for a function type `F` (#943): the function converted directly, as
//! TypeScript keeps the value (`FnCx::fn_through_top`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `f as unknown as F` (or `as any as F`) for a function type `F` and a name `f` whose own
    /// function type converts to `F` as an assignment would (`Symbol as unknown as (d: string)
    /// => Token<T>`, #943): TypeScript keeps the value, so the function converts directly. Through
    /// `unknown` it would be boxed as its own type and then fail the run-time test for `F`, which
    /// only the exact type passes. Tried as a trial, rolled back when `f` doesn't convert (or is
    /// `F` already: then the boxed round trip is the same value). `None`: not this form.
    pub(super) fn fn_through_top(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        want: Want,
    ) -> Option<hir::Expr> {
        if !matches!(self.cx.ty.kind(target), TyKind::FnPtr { .. }) {
            return None;
        }
        let ast::ExprKind::Cast { expr: x, ty } = &strip_parens(expr).kind else {
            return None;
        };
        let top = matches!(&ty.kind, ast::TypeExprKind::Named { path, args }
            if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "unknown" | "any"));
        if !top || !is_path(x) {
            return None;
        }
        // A brand is its primitive at run time: a function returning (or taking) a `symbol` is
        // one returning a branded `symbol` as it is, as `x as Token` keeps the value.
        let plain = self.unbranded_fn(target);
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let h = self.expr(x, Some(plain), want);
        let fits = self.cx.diags.len() == diags
            && h.ty != target
            && matches!(self.cx.ty.kind(h.ty), TyKind::FnPtr { .. });
        if fits {
            if let Ok(mut w) = self.try_coerce(h, plain) {
                if self.cx.diags.len() == diags && w.ty == plain {
                    w.ty = target;
                    return Some(w);
                }
            }
        }
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        None
    }

    /// `f as unknown as G` (or `as any`) for a generic function type `G` (#943): a generic
    /// function `f` is its value of `G`; a function of fixed types can't be one (each call
    /// would need its type arguments), which is reported here rather than as a failing check at
    /// run time. `None`: not this shape.
    pub(super) fn fn_as_generic(
        &mut self,
        expr: &ast::Expr,
        (target, written): (TyId, &ast::TypeExpr),
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if !self.cx.is_generic_fn(target) {
            return None;
        }
        let ast::ExprKind::Cast { expr: x, ty } = &strip_parens(expr).kind else {
            return None;
        };
        let top = matches!(&ty.kind, ast::TypeExprKind::Named { path, args }
            if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "unknown" | "any"));
        if !top || !is_path(x) {
            return None;
        }
        let t = self.peek_ty(x)?;
        let t = self.cx.ty.core(t).unwrap_or(t);
        if self.cx.is_generic_fn(t) {
            let h = self.expr(x, Some(target), want);
            return Some(self.coerce(h, target));
        }
        if !matches!(
            self.cx.ty.kind(t),
            TyKind::FnPtr { .. } | TyKind::Closure(_)
        ) {
            return None;
        }
        // A function that takes the type's parameters at every type argument (`Symbol` as
        // `<T>(d: string) => Token<T>`, sigx's `createToken`): the generic arrow calling it,
        // its result asserted to the type's (`<T>(a0: string) => Symbol(a0) as Token<T>`).
        if let Some(h) = self.fixed_as_generic(x, written, target) {
            return Some(h);
        }
        let (f, g) = (self.cx.display(t), self.cx.display(target));
        self.cx.error(
            velt_common::Diagnostic::error(
                format!("a function of fixed types (`{f}`) cannot be a `{g}`"),
                span,
            )
            .with_note("a value of a generic function type takes any type arguments at each call; one of fixed types is checked against the type when it is converted back, and would fail there (#943)")
            .with_note("pass a generic function, or a generic arrow (`<T>(x: T) => …`)"),
        );
        Some(self.error_expr(span))
    }

    /// `x` (a function of fixed types) as generic function type `target`, written `written`:
    /// the generic arrow calling it, when that checks (`fn_as_generic`). `None`, with nothing
    /// reported, when it doesn't (or the type is not written out here).
    fn fixed_as_generic(
        &mut self,
        x: &ast::Expr,
        written: &ast::TypeExpr,
        target: TyId,
    ) -> Option<hir::Expr> {
        let ast::TypeExprKind::GenericFunction { type_params, ty } = &written.kind else {
            return None;
        };
        let ast::TypeExprKind::Function {
            params, ret, rest, ..
        } = &ty.kind
        else {
            return None;
        };
        let at = x.span;
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span: at,
        };
        let name = |k: usize| ast::Ident {
            name: format!("#a{k}"),
            span: at,
        };
        let n = params.len();
        let last = |k: usize| *rest && k + 1 == n;
        let ps = (params.iter().enumerate())
            .map(|(k, t)| ast::ArrowParam {
                name: name(k),
                pattern: None,
                ty: Some(t.clone()),
                default: None,
                optional: false,
                rest: last(k),
            })
            .collect();
        let args = (0..n)
            .map(|k| match last(k) {
                true => mk(ast::ExprKind::Spread(Box::new(mk(ast::ExprKind::Ident(
                    name(k),
                ))))),
                false => mk(ast::ExprKind::Ident(name(k))),
            })
            .collect();
        let call = mk(ast::ExprKind::Call {
            callee: Box::new(x.clone()),
            type_args: vec![],
            args,
            optional: false,
        });
        let body = mk(ast::ExprKind::Cast {
            expr: Box::new(call),
            ty: (**ret).clone(),
        });
        let arrow = mk(ast::ExprKind::Arrow {
            type_params: type_params.clone(),
            params: ps,
            ret: Some((**ret).clone()),
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(body)),
            is_async: false,
        });
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let h = self.expr(&arrow, Some(target), Want::Move);
        if self.cx.diags.len() == diags {
            return Some(self.coerce(h, target));
        }
        self.cx.diags.truncate(diags);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        None
    }

    /// Function type `t` with each branded parameter and result type its primitive.
    fn unbranded_fn(&mut self, t: TyId) -> TyId {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.cx.ty.kind(t).clone()
        else {
            return t;
        };
        let plain = |t: TyId| self.cx.brand_base(t).unwrap_or(t);
        let params: Vec<TyId> = params.iter().map(|p| plain(*p)).collect();
        let ret = plain(ret);
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        })
    }
}

/// `e` without parentheses.
fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}

/// Is `e` a name or a property path of one (`Symbol`, `ns.f`), in parentheses or not?
fn is_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) => true,
        ast::ExprKind::Paren(x) => is_path(x),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_path(object),
        _ => false,
    }
}

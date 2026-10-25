//! `f as unknown as F` for a function type `F` (#943): the function converted directly, as
//! TypeScript keeps the value (`FnCx::fn_through_top`).

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

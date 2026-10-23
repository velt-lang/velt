//! A function whose parameters are wider than those of the function type it is used as
//! (`(v: any) => …` where `(x: number) => void` is expected: a `then` callback, an array
//! callback, a function stored in a typed variable or passed to a generic function), as
//! TypeScript allows (it compares parameters bivariantly; Velt takes the half where each
//! expected parameter type converts to the function's own as an argument would, an `any` to a
//! `number` too, checked at run time). The function is wrapped,
//! `{ const f = value; (p…) => f(p…) }` (`adapt_fn`), the arrow typed by the expected type, so
//! each argument is converted where the wrapper calls the function (`ToDyn` for an `any`, to
//! the union, the interface, …) and its result converts to the expected one. A function value
//! is never used as a function of another type as it is: its parameters would read arguments
//! laid out differently (#1454).

use velt_syntax::ast;

use super::callback::Wrap;
use super::callback_wrap::Shape;
use crate::body::recheck::Mark;
use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h`, a function, as the function type `exp` whose parameters convert to its own (module
    /// docs); `Err` gives `h` back when they don't.
    pub(super) fn widened_params(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        // A module-level constant or a field initializer has no frame for the hidden local.
        if self.frameless() || !self.params_widen(h.ty, exp) {
            return Err(h);
        }
        // Checked as a trial: what doesn't convert is reported by the wrapper's check, rolled
        // back to the mismatch the caller reports.
        let mark = Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let keep = h.clone();
        match self.adapt_fn(h, exp) {
            Ok(w) if self.cx.diags.len() == diags && w.ty == exp => Ok(w),
            _ => {
                mark.rollback(self.cx);
                self.restore_trial_frames(frames);
                Err(keep)
            }
        }
    }

    /// An arrow with wider parameters (`(v: any) => …`) where no frame holds the hidden local
    /// `widened_params` keeps it in (a module-level constant, a field initializer): checked by
    /// its own parameter types and called in place by a wrapper typed by `exp`
    /// (`immediate_wrapper`). `None` when its parameters are the expected ones, or the
    /// wrapper doesn't check (the arrow's own check reports what is wrong).
    pub(super) fn frameless_widened_params(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Arrow { params, .. } = &e.kind else {
            return None;
        };
        if !self.frameless() || params.iter().any(|p| p.rest) {
            return None;
        }
        let fn_ty = self.hint(exp)?;
        let TyKind::FnPtr {
            params: want,
            ret,
            throws,
            rest: false,
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return None;
        };
        if params.len() != want.len() || self.cx.ty.has_error(fn_ty) {
            return None;
        }
        let mut own = Vec::with_capacity(want.len());
        for (p, &w) in params.iter().zip(&want) {
            own.push(match &p.ty {
                Some(t) => self.resolve_quiet(t)?,
                None => w,
            });
        }
        // Types that are one as they are (two forms of one object type, `readonly`) need no
        // wrapper: the arrow converts as it is.
        let differs = |s: &mut Self, o: TyId, w: TyId| {
            o != w && s.cx.canon(o) != s.cx.canon(w) && !s.cx.same_layout(o, w)
        };
        if !own.iter().zip(&want).any(|(&o, &w)| differs(self, o, w)) {
            return None;
        }
        let inner = self.cx.ty.intern(TyKind::FnPtr {
            params: own,
            ret,
            throws,
            rest: false,
        });
        let shape = Shape::of(want.len(), false);
        let mark = Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let w = self.immediate_wrapper(e, inner, (shape, &shape.same()), Wrap::Call, exp);
        if self.cx.diags.len() == diags && w.ty == fn_ty {
            return Some(w);
        }
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        None
    }

    /// Are `from` and `to` function types of the same shape whose parameter types differ?
    /// Whether each of `to`'s converts to `from`'s, and `from`'s result and errors to `to`'s,
    /// is what the wrapper's check finds (the conversions of arguments and results, object
    /// copies included).
    fn params_widen(&self, from: TyId, to: TyId) -> bool {
        let t = &self.cx.ty;
        match (t.kind(from), t.kind(to)) {
            (
                TyKind::FnPtr {
                    params: fp,
                    rest: frest,
                    ..
                },
                TyKind::FnPtr {
                    params: tp,
                    rest: trest,
                    ..
                },
            ) => {
                fp.len() == tp.len()
                    && frest == trest
                    && fp != tp
                    && !t.has_error(from)
                    && !t.has_error(to)
            }
            _ => false,
        }
    }
}

//! Trials whose answer is kept, so inference that checks an argument first costs a constant factor
//! however deep such arguments nest. An argument tried and then checked for real doubles the work
//! at each level (`fold((a: number | string) => fold((a: number | string) => …, 1), 1)` took 52 s
//! at 18 levels). So:
//! - a trial is answered once per expression, expected type and types of the locals it names;
//! - inside such a trial, nothing is kept and only types are read, so an arrow already checked
//!   there with the same inputs gives its type without its body being checked again, and so does
//!   one tried for what it returns (`arrow_tried`) when that fits what is now expected.
//!
//! The real check of an argument is not affected: it runs once, outside every trial.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The type `check` gives for `e` against `expected` as a trial, remembered.
    pub(super) fn remembered_trial(
        &mut self,
        e: &ast::Expr,
        expected: TyId,
        check: impl FnOnce(&mut Self) -> TyId,
    ) -> Option<TyId> {
        let key = (e.span, expected, self.named_local_types(e));
        if let Some(&answer) = self.trial_answers.get(&key) {
            return answer;
        }
        self.memo_trials += 1;
        let answer = self.trial_with(check);
        self.memo_trials -= 1;
        self.trial_answers.insert(key, answer);
        answer
    }

    /// [`Self::trial_type`], remembered.
    pub(crate) fn remembered_type(&mut self, e: &ast::Expr) -> Option<TyId> {
        let none = self.cx.ty.error;
        self.remembered_trial(e, none, |lw| lw.expr(e, None, Want::Borrow).ty)
    }

    /// Arrow `e` checked inside a remembered trial: its type when it was checked there with the
    /// same inputs and no errors. What is returned then is only a typed placeholder, which the
    /// trial's rollback drops with everything else.
    pub(super) fn closure_in_trial(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        escaping: bool,
    ) -> hir::Expr {
        let key: (Span, Option<TyId>, Vec<TyId>) = (e.span, exp, self.named_local_types(e));
        if let Some(&ty) = self.trial_closures.get(&key) {
            let mut h = self.error_expr(e.span);
            h.ty = ty;
            return h;
        }
        if let Some(ty) = self.fits_arrow_trial(e, exp, &key.2) {
            let mut h = self.error_expr(e.span);
            h.ty = ty;
            return h;
        }
        let diags = self.cx.diags.len();
        let h = self.closure_checked(e, exp, escaping);
        if self.cx.diags.len() == diags && !self.cx.ty.has_error(h.ty) {
            self.trial_closures.insert(key, h.ty);
        }
        h
    }

    /// Remembers `found`, the type arrow `e` gave as a trial with its result left open.
    pub(super) fn arrow_tried(&mut self, e: &ast::Expr, found: TyId) {
        let key = (e.span, self.named_local_types(e));
        self.arrow_trials.insert(key, found);
    }

    /// The type arrow `e` has against `exp`, from its trial with the result left open: its
    /// unannotated parameters have the same types there, so its body gives the same result, and
    /// that fits `exp`'s result. What it throws is what the trial found.
    fn fits_arrow_trial(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        locals: &[TyId],
    ) -> Option<TyId> {
        let exp = exp?;
        let found = *self.arrow_trials.get(&(e.span, locals.to_vec()))?;
        let (
            TyKind::FnPtr {
                params: wanted,
                ret: wanted_ret,
                rest,
                ..
            },
            TyKind::FnPtr {
                params: tried,
                ret: tried_ret,
                throws,
                ..
            },
        ) = (self.cx.ty.kind(exp).clone(), self.cx.ty.kind(found).clone())
        else {
            return None;
        };
        let ast::ExprKind::Arrow { params, .. } = &e.kind else {
            return None;
        };
        let same_inputs = wanted.len() == tried.len()
            && (params.iter().enumerate()).all(|(i, p)| {
                p.ty.is_some() || (wanted.get(i).is_some() && wanted.get(i) == tried.get(i))
            });
        let known =
            !self.cx.ty.has_error(wanted_ret) && !wanted.iter().any(|t| self.cx.ty.has_error(*t));
        if !same_inputs || !known || !self.converts_to(tried_ret, wanted_ret) {
            return None;
        }
        Some(self.cx.ty.intern(TyKind::FnPtr {
            params: wanted,
            ret: wanted_ret,
            throws,
            rest,
        }))
    }
}

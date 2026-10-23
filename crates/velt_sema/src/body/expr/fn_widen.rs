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
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, LocalId, TyId, TyKind, UseMode};

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

    /// For each parameter of an arrow (`params`, typed `own`) checked against a function type
    /// passing `exp`: the type passed when the arrow's declared type is wider (`(v: any) => …`
    /// for `(x: number) => void`). Such a parameter takes the passed type and converts to the
    /// declared one where the arrow starts (`widened_param`), as `(n: number) => { let v: any =
    /// n; … }` would: the closure is of the expected type, so no wrapper calls it. `None` for a
    /// parameter taken as it is (no annotation, the same type, a rest parameter, a pattern, an
    /// optional one or one with a default: those keep their own rules, and one the passed type
    /// doesn't convert to, which the arrow's type then reports as the mismatch it is).
    /// `std_ints`: which parameters take an index of a standard callback, a number in the body.
    pub(super) fn entry_widened(
        &mut self,
        params: &[ast::ArrowParam],
        own: &[TyId],
        exp: Option<&[TyId]>,
        std_ints: &[bool],
        n_params: usize,
    ) -> Vec<Option<TyId>> {
        let mut out = vec![None; params.len()];
        let Some(exp) = exp else {
            return out;
        };
        for (k, p) in params.iter().enumerate().take(n_params) {
            let (Some(&o), Some(&e)) = (own.get(k), exp.get(k)) else {
                break;
            };
            let plain = p.ty.is_some()
                && !p.rest
                && !p.optional
                && p.pattern.is_none()
                && p.default.is_none();
            // A `never` is never passed (the reason of a promise that can't reject): the
            // arrow is called by no one, and keeps its own type.
            if plain
                && !self.cx.ty.is_bottom(e)
                && !self.cx.ty.has_error(o)
                && !self.cx.ty.has_error(e)
                && self.param_differs(o, e)
            {
                // A standard callback's index is a number first (`widened_param`).
                let index = std_ints.get(k) == Some(&true) && self.cx.ty.is_int(e);
                let from = if index { self.cx.ty.f64 } else { e };
                if from == o || self.passed_converts(from, o, p.name.span) {
                    out[k] = Some(e);
                }
            }
        }
        out
    }

    /// The hidden parameter of type `passed` for the arrow parameter `p` declared `own`, and the
    /// statement binding `p` to it converted (`entry_widened`). `std_int`: `p` takes an index
    /// of a standard callback (`xs.map((x, i: number) => …)`), an integer that is a number in
    /// the body, as when `i` is not annotated.
    pub(super) fn widened_param(
        &mut self,
        p: &ast::ArrowParam,
        passed: TyId,
        own: TyId,
        std_int: bool,
    ) -> (LocalId, hir::Stmt) {
        let span = p.name.span;
        let hidden = format!("#passed{}", self.f.locals.len());
        let l = self.new_local(&hidden, passed, false, span, LocalKind::Param);
        let mode = if self.cx.is_copy(passed) {
            UseMode::Copy
        } else {
            UseMode::Move
        };
        let mut read = self.mk(H::Local(l, mode), passed, span);
        // A standard callback's index (an integer) is a number, then converts (to an `any`, a
        // union with `number`, …): an `any` holds a number, as in JS.
        if std_int && self.cx.ty.is_int(passed) {
            let f64_ = self.cx.ty.f64;
            read = self.mk(H::Cast(Box::new(read)), f64_, span);
        }
        let init = match self.try_coerce(read, own) {
            // `entry_widened` tried it: reached only for what it couldn't see (none known).
            Ok(h) => h,
            Err(h) => {
                self.report_mismatch(own, &h);
                h
            }
        };
        let shadow = self.declare_local_mut(&p.name, own, LocalKind::Let, true);
        self.note_readonly_array(shadow, p.ty.as_ref());
        let stmt = hir::Stmt {
            kind: hir::StmtKind::Let {
                local: shadow,
                init: Some(init),
            },
            span,
        };
        (l, stmt)
    }

    /// Does a value of type `from` convert to `to` (tried on a hidden local, rolled back)?
    fn passed_converts(&mut self, from: TyId, to: TyId, span: velt_common::Span) -> bool {
        let mark = Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let l = self.new_local("#trial", from, false, span, LocalKind::Let);
        let mode = if self.cx.is_copy(from) {
            UseMode::Copy
        } else {
            UseMode::Move
        };
        let read = self.mk(H::Local(l, mode), from, span);
        let ok = self.try_coerce(read, to).is_ok() && self.cx.diags.len() == diags;
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        ok
    }

    /// Do two parameter types differ as values (not two forms of one object type, the same but
    /// for `readonly`, or function types the same but for their error-set variables)?
    fn param_differs(&mut self, a: TyId, b: TyId) -> bool {
        a != b
            && self.cx.canon(a) != self.cx.canon(b)
            && !self.cx.same_layout(a, b)
            && self.cx.plain_type(a) != self.cx.plain_type(b)
    }

    /// Where no frame holds the hidden local `widened_params` keeps a function value in (a
    /// module-level constant, a field initializer): an arrow with a rest parameter of a wider
    /// type (`(...ys: any[]) => …` for `(...xs: number[]) => void`), checked by its own
    /// parameter types and called in place by a wrapper typed by `exp`, which passes the rest
    /// arguments on as a new array (`immediate_wrapper`; other parameters convert where the
    /// arrow starts, `entry_widened`). `None` when its parameters are the expected ones, or the
    /// wrapper doesn't check (the arrow's own check reports what is wrong).
    pub(super) fn frameless_widened_params(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Arrow { params, .. } = &e.kind else {
            return None;
        };
        if !self.frameless() || !params.last().is_some_and(|p| p.rest) {
            return None;
        }
        let fn_ty = self.hint(exp)?;
        let TyKind::FnPtr {
            params: want,
            ret,
            throws,
            rest: true,
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
        if !own
            .iter()
            .zip(&want)
            .any(|(&o, &w)| self.param_differs(o, w))
        {
            return None;
        }
        let inner = self.cx.ty.intern(TyKind::FnPtr {
            params: own,
            ret,
            throws,
            rest: true,
        });
        let shape = Shape::of(want.len(), true);
        self.widen_trial(fn_ty, |s| {
            s.immediate_wrapper(e, inner, (shape, &shape.same()), Wrap::Call, exp)
        })
    }

    /// Where no frame holds a hidden local: a module-level function or constant `arg` (named,
    /// it captures nothing) whose parameter types are wider than `exp`'s, wrapped as `(p…) =>
    /// f(p…)` calling it by name.
    pub(super) fn frameless_named_widened(
        &mut self,
        arg: &ast::Expr,
        exp: TyId,
    ) -> Option<hir::Expr> {
        if !self.frameless() || !matches!(arg.kind, ast::ExprKind::Ident(_)) {
            return None;
        }
        let fn_ty = self.cx.ty.core(exp).unwrap_or(exp);
        let TyKind::FnPtr {
            params: want, rest, ..
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return None;
        };
        if self.cx.ty.has_error(fn_ty) {
            return None;
        }
        let (own, own_rest) = match self.named_fn_sig(arg, false) {
            Some(sig) => (sig.params, sig.rest),
            // A module-level constant holding a function: its type, read without a trace.
            None => {
                let mark = Mark::here(self.cx);
                let frames = self.trial_frames();
                let t = self.expr(arg, None, crate::body::Want::Move).ty;
                mark.rollback(self.cx);
                self.restore_trial_frames(frames);
                match self.cx.ty.kind(t) {
                    TyKind::FnPtr { params, rest, .. } => (params.clone(), *rest),
                    _ => return None,
                }
            }
        };
        let shape = Shape::of(want.len(), rest);
        if Shape::of(own.len(), own_rest) != shape
            || !own
                .iter()
                .zip(&want)
                .any(|(&o, &w)| self.param_differs(o, w))
        {
            return None;
        }
        let callee = super::callback::Callee::Named(arg.clone());
        self.widen_trial(fn_ty, |s| {
            s.wrap(callee, shape, &shape.same(), Wrap::Call, Some(exp))
        })
    }

    /// `make()` kept when it reports nothing and gives a `fn_ty`, else rolled back.
    fn widen_trial(
        &mut self,
        fn_ty: TyId,
        make: impl FnOnce(&mut Self) -> hir::Expr,
    ) -> Option<hir::Expr> {
        let mark = Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let w = make(self);
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

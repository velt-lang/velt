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
//!
//! The inputs include what is known about each local at this point, not only its declared type
//! (`let u: number | string = x` holds a `number` where `x` is one), so an answer isn't reused
//! where a local is narrowed differently.

use velt_syntax::ast;

use crate::body::frame::Frame;
use crate::body::{FnCx, LocalId, Want};
use crate::hir::{self, Nothing, TyId, TyKind};

/// A local an expression names, as it reads at that point: what decides how the expression checks.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct NamedLocal {
    ty: TyId,
    /// Narrowed to its payload.
    payload: bool,
    excluded: Vec<Nothing>,
    members: Option<Vec<u32>>,
    class: Option<TyId>,
}

impl NamedLocal {
    /// Local (or field or module variable token) `l` of frame `f`, of type `ty`.
    fn at(f: &Frame, l: LocalId, ty: TyId) -> NamedLocal {
        let scopes = || f.scopes.iter();
        NamedLocal {
            ty,
            payload: scopes().any(|s| s.narrowed.contains(&l)),
            excluded: scopes()
                .flat_map(|s| s.excluded.iter())
                .filter(|(x, _)| *x == l)
                .map(|(_, n)| *n)
                .collect(),
            members: scopes().rev().find_map(|s| {
                let known = s.members.iter().rev().find(|(x, _)| *x == l);
                known.map(|(_, vs)| vs.clone())
            }),
            class: scopes().rev().find_map(|s| {
                let known = s.classes.iter().rev().find(|(x, _)| *x == l);
                known.map(|(_, t)| *t)
            }),
        }
    }
}

impl FnCx<'_, '_> {
    /// The locals (of this function and of those it is nested in) named like a value expression
    /// `e` takes from its surroundings, or `this`, as they read here, with the field paths and
    /// module variables it names narrowed.
    pub(super) fn named_local_types(&mut self, e: &ast::Expr) -> Vec<NamedLocal> {
        let free = self.arrow_scans.free_names(e);
        let named = |name: &str| free.binary_search_by(|n| n.as_str().cmp(name)).is_ok();
        let unit = self.cx.ty.unit;
        let mut out = vec![];
        for f in self.outer.iter().chain(std::iter::once(&self.f)) {
            let mut roots = vec![];
            for (i, l) in f.locals.iter().enumerate() {
                if named(&l.name) {
                    let id = LocalId(i as u32);
                    roots.push(id);
                    out.push(NamedLocal::at(f, id, l.ty));
                }
            }
            for g in &f.global_tokens {
                if self.cx.global(g.def).is_some_and(|d| named(&d.name)) {
                    roots.push(g.token);
                    out.push(NamedLocal::at(f, g.token, unit));
                }
            }
            for t in f.field_tokens.iter().filter(|t| roots.contains(&t.root)) {
                out.push(NamedLocal::at(f, t.token, unit));
            }
        }
        out
    }

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
        crate::check_work::add_memo_trial();
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
        let key = (e.span, exp, self.named_local_types(e));
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
        locals: &[NamedLocal],
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
                // An annotated parameter must take what is expected now, not only what it was
                // tried with.
                match (p.ty.is_some(), wanted.get(i), tried.get(i)) {
                    (true, Some(&w), Some(&t)) => self.converts_to(w, t),
                    (false, Some(w), Some(t)) => w == t,
                    _ => false,
                }
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

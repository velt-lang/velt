//! Arrows whose parameters take the arguments of a function type in another shape, as
//! TypeScript allows (#943): checked as a function of their own shape, then wrapped
//! (`callback`) so the expected type's arguments reach them.
//!
//! - **Named parameters for rest arguments**: `(a: any) => …` where
//!   `(...args: any[]) => void` is expected. The arrow is checked as `(a: any) => void` and
//!   called as `f(#args.at(0))`: an argument the caller leaves out is `undefined`, which the
//!   parameter's type (or the rest element type) must admit; a parameter with a default gets it.
//! - **A rest parameter for fixed arguments**: `(...xs: number[]) => xs.length` where
//!   `(a: number, b: number) => number` is expected. With an array type for the rest parameter
//!   (all the arguments it takes have one type, or it is written), the arrow is checked as
//!   `(...xs: number[]) => number` and called with every argument, which the call packs; with
//!   arguments of different types (`(...args) => …` for `(...args: [number, string]) => R`,
//!   which is `(a: number, b: string) => R`) the rest parameter is an ordinary parameter
//!   holding the tuple of them, `f([#arg0, #arg1])`.

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::callback::{Callee, Wrap};
use super::callback_wrap::{plan, Arg, Plan, Shape};
use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The arrow `e` adapted to the function type `exp` when its parameters have another shape
    /// (see the module docs); `None` when they have the same one (or `exp` is no function type).
    pub(super) fn rest_shape_callback(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Arrow { params, .. } = &e.kind else {
            return None;
        };
        if self.packed_rest == Some(e.span) {
            return None;
        }
        let fn_ty = self.hint(exp)?;
        let TyKind::FnPtr {
            params: want_tys,
            ret,
            throws,
            rest: want_rest,
        } = self.cx.ty.kind(fn_ty).clone()
        else {
            return None;
        };
        let want = Shape::of(want_tys.len(), want_rest);
        let own_rest = params.last().is_some_and(|p| p.rest);
        let own_fixed = params.len() - usize::from(own_rest);
        let elem = want_tys
            .last()
            .filter(|_| want.rest)
            .and_then(|t| self.cx.ty.array_elem(*t));
        let fn_of = |s: &mut Self, params: Vec<TyId>, rest: bool| {
            s.cx.ty.intern(TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            })
        };
        if !own_rest {
            // Named parameters past the expected fixed ones read the rest arguments.
            let elem = elem.filter(|_| params.len() > want.fixed)?;
            let mut tys = want_tys[..want.fixed].to_vec();
            tys.resize(params.len(), elem);
            // A closure value is called with every parameter: one with a default would get
            // `undefined` rather than its default.
            if let Some(p) = params[want.fixed..]
                .iter()
                .find(|p| p.default.is_some() && !p.optional)
            {
                self.cx.error(
                    Diagnostic::error(
                        format!("parameter `{}` with a default takes one of the function type's rest arguments, which is not supported yet", p.name.name),
                        p.name.span,
                    )
                    .with_note("make it optional (`x?: T`) and test it, or take the rest arguments as a rest parameter (`(...args) => …`)"),
                );
                return Some(self.error_expr(e.span));
            }
            let n = params.len();
            let plan = plan(want, Shape::of(n, false), n, n);
            if !self.admits_missing(&params[want.fixed..], elem) {
                return Some(self.error_expr(e.span));
            }
            let inner = fn_of(self, tys, false);
            return Some(self.shape_wrapper(e, inner, want, &plan, exp));
        }
        if want.rest && own_fixed == want.fixed {
            return None;
        }
        if own_fixed > want.fixed {
            // Reported by `closure_rest`.
            return None;
        }
        // The arguments the arrow's rest parameter takes.
        let taken = &want_tys[own_fixed..want.fixed];
        let written = params
            .last()
            .and_then(|p| p.ty.as_ref())
            .map(|t| self.resolve(t));
        let same = match (taken.first(), elem) {
            (Some(&t), _) if taken.iter().all(|x| *x == t) && elem.is_none_or(|e| e == t) => {
                Some(t)
            }
            (None, Some(e)) => Some(e),
            _ => None,
        };
        let array = match (written, same) {
            (Some(w), _) if self.cx.ty.array_elem(w).is_some() => Some(w),
            (Some(_), _) => None,
            (None, Some(t)) => Some(self.cx.ty.array(t)),
            (None, None) => None,
        };
        let mut tys = want_tys[..own_fixed].to_vec();
        if let Some(a) = array {
            // `(...xs: T[]) => …`: every argument, packed by the call.
            tys.push(a);
            let inner = fn_of(self, tys, true);
            return Some(self.shape_wrapper(e, inner, want, &want.same(), exp));
        }
        if want.rest {
            // Arguments of different types and more of the element type: no array holds them.
            return None;
        }
        // The rest parameter as an ordinary parameter holding the tuple of the arguments.
        tys.push(self.cx.ty.intern(TyKind::Tuple(taken.to_vec())));
        let inner = fn_of(self, tys, false);
        let mut args: Vec<Arg> = (0..own_fixed).map(Arg::Param).collect();
        args.push(Arg::Pack((own_fixed..want.fixed).map(Arg::Param).collect()));
        self.packed_rest = Some(e.span);
        let h = self.shape_wrapper(e, inner, want, &Plan::one(args), exp);
        self.packed_rest = None;
        Some(h)
    }

    /// Whether every parameter of `ps` (each filled from a rest argument of type `elem` that
    /// may be missing) admits `undefined`; reported otherwise.
    fn admits_missing(&mut self, ps: &[ast::ArrowParam], elem: TyId) -> bool {
        let admits = |s: &Self, t: TyId| s.cx.dynamic(t).is_some() || s.cx.ty.core(t).is_some();
        if admits(self, elem) {
            return true;
        }
        for p in ps {
            let ok = p.optional
                || p.ty.as_ref().is_some_and(|t| {
                    let t = self.resolve(t);
                    admits(self, t)
                });
            if !ok {
                let shown = self.cx.display(elem);
                self.cx.error(
                    Diagnostic::error(
                        format!(
                            "parameter `{}` takes one of the function type's rest arguments, which may be missing, and a `{shown}` cannot be `undefined`",
                            p.name.name
                        ),
                        p.name.span,
                    )
                    .with_note("make it optional (`x?: T`), give it a default, or take the rest arguments as a rest parameter (`(...args) => …`)"),
                );
                return false;
            }
        }
        true
    }

    /// The arrow `e` checked as `inner`, wrapped as a function of shape `want` calling it as
    /// `plan` says, checked against `exp`.
    fn shape_wrapper(
        &mut self,
        e: &ast::Expr,
        inner: TyId,
        want: Shape,
        plan: &Plan,
        exp: Option<TyId>,
    ) -> hir::Expr {
        if self.frameless() {
            return self.immediate_wrapper(e, inner, (want, plan), Wrap::Call, exp);
        }
        let f = self.closure(e, Some(inner), true);
        self.wrap(Callee::Value(f, None), want, plan, Wrap::Call, exp)
    }
}

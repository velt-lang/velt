//! Type parameters bounded by a type rather than an interface (`T extends Record<string, V>`,
//! `T extends string | number`, `T extends readonly X[]`, `T extends (...a: any[]) => any`):
//! a call's type argument must be assignable to the bound, as TypeScript relates them
//! (`Ctx::satisfies_constraint`). A call that breaks one is an error, so an overloaded call
//! moves on to its next signature as `tsc` does (`signal<T extends Primitive>` then
//! `signal<T extends object>`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// The type an argument holding literals is checked against for a parameter of type `pty`
    /// that mentions slots no argument fixed yet whose type bounds have literal types: `pty`
    /// with those slots replaced by their bounds, so the literals keep their literal types
    /// (`same("a")` of `same<T extends "a" | "b">` infers `"a"`, and so do `t?: T`,
    /// `t: T | null`, `t: T[]` and `o: { k: T }`, as `tsc` does). `expected` otherwise.
    pub(super) fn constraint_expected(
        &mut self,
        c: &Callable,
        arg: &ast::Expr,
        pty: TyId,
        expected: TyId,
        (known, slots): (&[Option<TyId>], &[Option<TyId>]),
    ) -> TyId {
        if c.constraints.iter().all(Option::is_none) || !has_literal(arg) {
            return expected;
        }
        let mut with_bounds = known.to_vec();
        let mut changed = false;
        for (k, b) in c.constraints.iter().enumerate() {
            let Some(b) = *b else { continue };
            if slots.get(k).is_none_or(Option::is_some) {
                continue;
            }
            let b = self.cx.subst_known(b, slots);
            if self.cx.has_literal_member(b) {
                with_bounds[k] = Some(b);
                changed = true;
            }
        }
        match changed {
            true => self.cx.subst_known(pty, &with_bounds),
            false => expected,
        }
    }

    /// Report when type argument `t` of slot `k` of `c` breaks the slot's type bound.
    pub(super) fn check_constraint(
        &mut self,
        c: &Callable,
        k: usize,
        t: TyId,
        slots: &[Option<TyId>],
        name: &str,
        span: Span,
    ) {
        let Some(bound) = c.constraints.get(k).copied().flatten() else {
            return;
        };
        let bound = self.cx.subst_known(bound, slots);
        if self.constraint_holds(t, bound) {
            return;
        }
        let tn = self.cx.display(t);
        let bn = self.cx.display(bound);
        self.cx.error(
            Diagnostic::error(
                format!("the type `{tn}` does not satisfy the constraint `{bn}`"),
                span,
            )
            .with_note(format!("required by `{name} extends {bn}` of {}", c.what)),
        );
    }

    /// Does `t` satisfy type bound `bound`? A type parameter of the function being checked
    /// does through its own type bound; one with only interface bounds is not tested.
    fn constraint_holds(&mut self, t: TyId, bound: TyId) -> bool {
        let inner = self.cx.ty.opt_payload(t).unwrap_or(t);
        if let TyKind::Param(n) = *self.cx.ty.kind(inner) {
            match self.constraints.get(n as usize).copied().flatten() {
                Some(own) => {
                    let args: Vec<TyId> = (0..=n)
                        .map(|i| match i == n {
                            true => own,
                            false => self.cx.ty.param(i),
                        })
                        .collect();
                    let t = self.cx.subst(t, &args);
                    return self.cx.has_type_params(t) || self.cx.satisfies_constraint(t, bound);
                }
                None if self.bounds.get(n as usize).is_some_and(|b| !b.is_empty()) => {
                    return true;
                }
                None => {}
            }
        }
        self.cx.satisfies_constraint(t, bound)
    }
}

/// Does `e` hold a literal (`"a"`, `["a"]`, `{ k: "a" }`)?
fn has_literal(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Array(xs) => xs.iter().any(has_literal),
        ast::ExprKind::Object(ps) => ps.iter().any(|p| match p {
            ast::ObjectProp::KeyValue(_, v) => has_literal(v),
            _ => false,
        }),
        ast::ExprKind::Paren(inner) => has_literal(inner),
        _ => crate::body::narrow::literal_of(e).is_some(),
    }
}

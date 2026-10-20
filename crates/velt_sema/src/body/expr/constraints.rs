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
use crate::defs::Bound;
use crate::hir::{TyId, TyKind};

/// An argument of a call: its parameter's type, its own type and its span.
pub(super) type ArgSite = (TyId, TyId, Span);

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

    /// The slot of `pty` when it is a type parameter an earlier argument fixed to string
    /// literal types (`K extends keyof T`).
    pub(super) fn literal_slot(&mut self, pty: TyId, slots: &[Option<TyId>]) -> Option<usize> {
        let TyKind::Param(k) = *self.cx.ty.kind(pty) else {
            return None;
        };
        let t = (*slots.get(k as usize)?)?;
        self.string_literals(t).map(|_| k as usize)
    }

    /// Slot `k` (string literal types) with the string literal types `t` added.
    pub(super) fn add_literal_key(&mut self, k: usize, t: TyId, slots: &mut [Option<TyId>]) {
        let Some(prev) = slots[k] else { return };
        if prev == t || self.string_literals(t).is_none() {
            return;
        }
        slots[k] = Some(self.cx.union_of(&[prev, t], false, Span::DUMMY));
    }

    /// Report when type argument `t` of slot `k` of `c` breaks the slot's type bound (false),
    /// at the first argument of a parameter `Param(k)` whose own type breaks it (the `"w"` of
    /// `swap(p, "x", "w")`), else at the call.
    fn check_constraint(
        &mut self,
        c: &Callable,
        k: usize,
        t: TyId,
        (slots, sites): (&[Option<TyId>], &[ArgSite]),
        name: &str,
        span: Span,
    ) -> bool {
        let Some(bound) = c.constraints.get(k).copied().flatten() else {
            return true;
        };
        let bound = self.cx.subst_known(bound, slots);
        if self.constraint_holds(t, bound) {
            return true;
        }
        let at = sites.iter().find(|&&(p, a, _)| {
            matches!(self.cx.ty.kind(p), TyKind::Param(j) if *j as usize == k)
                && !self.constraint_holds(a, bound)
        });
        let (t, span) = at.map_or((t, span), |&(_, a, s)| (a, s));
        let tn = self.cx.display(t);
        let bn = self.cx.display(bound);
        self.cx.error(
            Diagnostic::error(
                format!("the type `{tn}` does not satisfy the constraint `{bn}`"),
                span,
            )
            .with_note(format!("required by `{name} extends {bn}` of {}", c.what)),
        );
        false
    }

    /// Points the errors from `before` on that the type arguments of a call (at `span`) to `c`
    /// cause outside it (where a deferred type of the signature is written) at the call: a
    /// label and a note with the type arguments, as for an error in the callee's body
    /// (`type_level::instances`).
    pub(super) fn instance_context(
        &mut self,
        c: &Callable,
        before: usize,
        type_args: &[TyId],
        span: Span,
    ) {
        if self.cx.diags.len() == before || type_args.is_empty() {
            return;
        }
        let name = c.what.split('`').nth(1).unwrap_or(&c.what).to_string();
        let list = crate::type_level::instances::type_args_text(self.cx, &c.slot_names, type_args);
        let inside = |s: Span| s.file == span.file && s.lo >= span.lo && s.hi <= span.hi;
        for d in &mut self.cx.diags[before..] {
            if d.labels.first().is_none_or(|l| inside(l.span)) {
                continue;
            }
            d.labels.push(velt_common::Label {
                span,
                message: format!("`{name}` is instantiated here"),
            });
            d.notes
                .push(format!("in `{name}` at this instantiation: {list}"));
        }
    }

    /// Unknown slots are errors (unless `quiet`: an argument's error was reported); check bounds
    /// of the known ones (`sites`: each argument's parameter type, type and span). A slot whose
    /// type bound is broken is an error type.
    pub(super) fn solve_slots(
        &mut self,
        c: &Callable,
        slots: &[Option<TyId>],
        quiet: bool,
        span: Span,
        sites: &[ArgSite],
    ) -> Vec<TyId> {
        let mut out = vec![];
        if !quiet {
            self.report_uninferred(c, slots, span);
        }
        for (k, s) in slots.iter().enumerate() {
            let name = c
                .slot_names
                .get(k)
                .cloned()
                .unwrap_or_else(|| format!("T{k}"));
            let Some(t) = *s else {
                out.push(self.cx.ty.error);
                continue;
            };
            let bounds = c.bounds.get(k).cloned().unwrap_or_default();
            for b in bounds {
                let b = Bound {
                    iface: b.iface,
                    args: b
                        .args
                        .iter()
                        .map(|a| self.cx.subst_known(*a, slots))
                        .collect(),
                };
                let in_scope = self.bounds.clone();
                if !self.cx.satisfies(t, &b, &in_scope) {
                    let tn = self.cx.display(t);
                    let bn = self
                        .cx
                        .iface(b.iface)
                        .map(|i| i.name.clone())
                        .unwrap_or_default();
                    let mut d = Diagnostic::error(
                        format!("the type `{tn}` does not implement `{bn}`"),
                        span,
                    )
                    .with_note(format!("required by `{name} extends {bn}` of {}", c.what));
                    if let Some(i) = self
                        .cx
                        .iface(b.iface)
                        .filter(|_| self.cx.field_only.contains_key(&b.iface))
                    {
                        let fields: Vec<String> = i
                            .fields
                            .iter()
                            .map(|f| format!("{}: {}", f.name, self.cx.display(f.ty)))
                            .collect();
                        d = d.with_note(format!(
                            "`{bn}` has only fields: a type satisfies it by having them ({})",
                            fields.join(", ")
                        ));
                    }
                    self.cx.error(d);
                }
            }
            // A call breaking a constraint is not instantiated (no errors from its body).
            match self.check_constraint(c, k, t, (slots, sites), &name, span) {
                true => out.push(t),
                false => out.push(self.cx.ty.error),
            }
        }
        out
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

    /// Are `a` and `b` one type under this function's type-parameter bounds: deferred
    /// intersections that differ only in repeated parts or in `object` parts a `T extends
    /// object` part makes redundant (`T & object` is `T` there)?
    pub(crate) fn same_under_bounds(&mut self, a: TyId, b: TyId) -> bool {
        if !self.cx.mentions_op(a) && !self.cx.mentions_op(b) {
            return false;
        }
        let objects = self.object_params();
        let fa = self.cx.bounded_form(a, &objects, 0);
        let fb = self.cx.bounded_form(b, &objects, 0);
        fa == fb || self.cx.canon(fa) == self.cx.canon(fb)
    }

    /// The type parameters of this function whose values are objects (`T extends object`).
    pub(crate) fn object_params(&mut self) -> Vec<u32> {
        let Some(defs) = self.cx.any else {
            return vec![];
        };
        let b = crate::defs::Bound {
            iface: defs.object,
            args: vec![],
        };
        let bounds = self.bounds.clone();
        (0..bounds.len() as u32)
            .filter(|&n| {
                let p = self.cx.ty.param(n);
                self.cx.satisfies(p, &b, &bounds)
            })
            .collect()
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

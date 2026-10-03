//! Arguments of a call against a (possibly generic) signature, with type-argument inference.
//!
//! Arguments are checked in three rounds: those that carry their own type, then the ones whose
//! type comes from context (untyped number literals, `null`, `[]`), then arrow functions, so
//! `xs.reduce((acc, x) => acc + x, 0)` and `mapAll(xs, (x) => x * 2)` see the type parameters
//! already fixed by the other arguments. Remaining unknown type parameters are taken from the
//! expected result type. HIR args are in parameter order; omitted args use the defaults.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ops::untyped;
use crate::body::{FnCx, Want};
use crate::defs::{Bound, ParamSig};
use crate::hir::{self, PassMode, TyId, TyKind};

/// A callable signature; `Param(i)` in its types are the callee's type parameters ("slots").
pub(crate) struct Callable {
    /// For messages: "function `f`", "method `push`".
    pub what: String,
    pub params: Vec<ParamSig>,
    pub ret: TyId,
    pub slot_names: Vec<String>,
    pub bounds: Vec<Vec<Bound>>,
}

/// Checked arguments, the instantiated result type and the inferred type arguments.
pub(crate) struct Checked {
    pub args: Vec<hir::Expr>,
    pub ret: TyId,
    pub type_args: Vec<TyId>,
}

pub(crate) fn want_of(mode: PassMode) -> Want {
    match mode {
        PassMode::Copy | PassMode::Borrow => Want::Borrow,
        PassMode::BorrowMut => Want::BorrowMut,
        PassMode::Owned => Want::Move,
    }
}

/// Does this argument take its type from the parameter (checked in the second round)?
fn deferred(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) | ast::ExprKind::Arrow { .. } => true,
        ast::ExprKind::Array(xs) => xs.is_empty(),
        ast::ExprKind::Paren(x) => deferred(x),
        _ => untyped(e),
    }
}

/// The arrow function in argument `e` (possibly parenthesized).
pub(super) fn as_arrow(e: &ast::Expr) -> Option<&ast::Expr> {
    match &e.kind {
        ast::ExprKind::Arrow { .. } => Some(e),
        ast::ExprKind::Paren(x) => as_arrow(x),
        _ => None,
    }
}

impl FnCx<'_, '_> {
    pub(crate) fn check_call(
        &mut self,
        c: &Callable,
        mut slots: Vec<Option<TyId>>,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Checked {
        let min = c
            .params
            .iter()
            .rposition(|p| p.default.is_none())
            .map_or(0, |i| i + 1);
        if args.len() < min || args.len() > c.params.len() {
            self.arg_count_error(&c.what, min, c.params.len(), args.len(), span);
            self.check_args_loose(args);
            let type_args: Vec<TyId> = slots
                .iter()
                .map(|s| s.unwrap_or(self.cx.ty.error))
                .collect();
            let ret = self.cx.ty.subst(c.ret, &type_args);
            return Checked {
                args: vec![],
                ret,
                type_args,
            };
        }
        // The expected result type is a lower-priority inference source, as in TS: it types
        // the arguments of slots no argument has fixed yet (`const y: i32 = id(1)` checks `1`
        // as an `i32`), but the arguments' own types decide the slots. Error types are not
        // inferred from it (the `Promise<T>` an `await` expects may reject with anything).
        let mut context = slots.clone();
        if let Some(e) = exp {
            let e = self.cx.ty.without_error_types(e);
            self.cx.match_ty(c.ret, e, &mut context);
        }
        let checked = self.args_in_rounds(c, &mut slots, &context, args);
        if let Some(e) = exp {
            self.cx.match_ty(c.ret, e, &mut slots);
        }
        let type_args = self.solve_slots(c, &slots, span);
        let mut hargs = vec![];
        for (h, p) in checked.into_iter().zip(&c.params) {
            let target = self.cx.ty.subst(p.ty, &type_args);
            let mut h = self.coerce(h, target);
            if p.mode == PassMode::BorrowMut {
                self.use_mutably(&mut h, "modify");
            }
            hargs.push(h);
        }
        for p in &c.params[args.len()..] {
            let mut d = p.default.clone().expect("ICE: default checked by arity");
            crate::visit::map_expr_types(&mut d, &mut |t| self.cx.ty.subst(t, &type_args));
            d.span = span;
            hargs.push(d);
        }
        let ret = self.cx.ty.subst(c.ret, &type_args);
        Checked {
            args: hargs,
            ret,
            type_args,
        }
    }

    /// Check `args` (typed ones first, then context-typed literals, then arrows), binding
    /// type parameters from each; results in parameter order. A slot still unknown when an
    /// argument is checked takes its type from `context` (inferred from the expected result).
    fn args_in_rounds(
        &mut self,
        c: &Callable,
        slots: &mut [Option<TyId>],
        context: &[Option<TyId>],
        args: &[ast::Expr],
    ) -> Vec<hir::Expr> {
        let round = |e: &ast::Expr| match (deferred(e), as_arrow(e).is_some()) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        let mut order: Vec<usize> = (0..args.len()).collect();
        order.sort_by_key(|&i| round(&args[i]));
        let mut out: Vec<Option<hir::Expr>> = (0..args.len()).map(|_| None).collect();
        for i in order {
            let p = &c.params[i];
            if untyped(&args[i]) {
                self.number_slot_from_callback(c, p.ty, slots);
                self.number_slot_from_context(p.ty, slots, context);
            }
            let known: Vec<Option<TyId>> =
                slots.iter().zip(context).map(|(s, c)| s.or(*c)).collect();
            let expected = self.cx.ty.subst_known(p.ty, &known);
            let h = match as_arrow(&args[i]) {
                Some(a) if matches!(self.cx.ty.kind(expected), TyKind::FnPtr { .. }) => {
                    self.arrow_arg(a, expected, p.mode == PassMode::Owned)
                }
                _ => self.expr(&args[i], Some(expected), want_of(p.mode)),
            };
            self.cx.match_ty(p.ty, h.ty, slots);
            out[i] = Some(h);
        }
        out.into_iter()
            .map(|h| h.expect("ICE: arg checked"))
            .collect()
    }

    /// An untyped number argument (`0` in `xs.reduce((a, x) => a + x, 0)`) whose parameter is
    /// a still unknown slot `S`: when a callback parameter also has `S` next to a known number
    /// type, `S` is that type (`usize` for a `usize[]`), which is what TS's single `number`
    /// gives; otherwise the literal's default type decides as usual.
    fn number_slot_from_callback(&mut self, c: &Callable, pty: TyId, slots: &mut [Option<TyId>]) {
        let TyKind::Param(s) = *self.cx.ty.kind(pty) else {
            return;
        };
        if slots.get(s as usize).is_none_or(|x| x.is_some()) {
            return;
        }
        for q in &c.params {
            let TyKind::FnPtr { params, .. } = self.cx.ty.kind(q.ty).clone() else {
                continue;
            };
            if !params.contains(&pty) {
                continue;
            }
            let known: Vec<TyId> = params
                .iter()
                .map(|t| self.cx.ty.subst_known(*t, slots))
                .collect();
            let known = known
                .into_iter()
                .find(|t| *t != pty && self.cx.ty.is_numeric(*t));
            if let Some(n) = known {
                slots[s as usize] = Some(n);
                return;
            }
        }
    }

    /// An untyped number argument whose parameter is a still unknown slot `S` that the expected
    /// result fixes to a number type: `S` is that type (`const x: f64 = id(2)` passes `2.0`)
    /// instead of the literal's default.
    fn number_slot_from_context(
        &self,
        pty: TyId,
        slots: &mut [Option<TyId>],
        context: &[Option<TyId>],
    ) {
        let TyKind::Param(s) = *self.cx.ty.kind(pty) else {
            return;
        };
        let s = s as usize;
        if let (Some(None), Some(Some(t))) = (slots.get(s), context.get(s)) {
            if self.cx.ty.is_numeric(*t) {
                slots[s] = Some(*t);
            }
        }
    }

    /// Unknown slots are errors; check bounds of the known ones.
    pub(super) fn solve_slots(
        &mut self,
        c: &Callable,
        slots: &[Option<TyId>],
        span: Span,
    ) -> Vec<TyId> {
        let mut out = vec![];
        self.report_uninferred(c, slots, span);
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
                        .map(|a| self.cx.ty.subst_known(*a, slots))
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
                    self.cx.error(
                        Diagnostic::error(
                            format!("the type `{tn}` does not implement `{bn}`"),
                            span,
                        )
                        .with_note(format!("required by `{name} extends {bn}` of {}", c.what)),
                    );
                }
            }
            out.push(t);
        }
        out
    }

    /// One error naming every type parameter of `c` that no argument or expected type fixed.
    fn report_uninferred(&mut self, c: &Callable, slots: &[Option<TyId>], span: Span) {
        let names: Vec<String> = (0..slots.len())
            .filter(|k| slots[*k].is_none())
            .map(|k| {
                let n = c.slot_names.get(k).cloned();
                format!("`{}`", n.unwrap_or_else(|| format!("T{k}")))
            })
            .collect();
        let list = match names.as_slice() {
            [] => return,
            [one] => format!("type parameter {one}"),
            [init @ .., last] => format!("type parameters {} and {last}", init.join(", ")),
        };
        self.cx.error(
            Diagnostic::error(format!("cannot infer {list} of {}", c.what), span).with_note(
                "add explicit type arguments, e.g. `f<i64>(...)`, or annotate the result",
            ),
        );
    }

    /// An arrow function passed directly as an argument: non-escaping, unless the parameter
    /// already takes ownership (an intrinsic like `push` that stores it).
    fn arrow_arg(&mut self, e: &ast::Expr, expected: TyId, owned: bool) -> hir::Expr {
        self.closure(e, Some(expected), owned)
    }

    /// Check args for errors only (callee unknown / invalid).
    pub(crate) fn check_args_loose(&mut self, args: &[ast::Expr]) {
        for a in args {
            self.expr(a, None, Want::Borrow);
        }
    }

    pub(crate) fn arg_count_error(
        &mut self,
        what: &str,
        min: usize,
        max: usize,
        got: usize,
        span: Span,
    ) {
        let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
        let were = if got == 1 { "was" } else { "were" };
        let takes = if min == max {
            format!("{min} {}", plural(min))
        } else {
            format!("{min} to {max} arguments")
        };
        self.cx.err(
            format!(
                "{what} takes {takes} but {got} {} {were} supplied",
                plural(got)
            ),
            span,
        );
    }
}

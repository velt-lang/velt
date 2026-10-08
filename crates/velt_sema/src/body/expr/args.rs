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
use super::widen_fresh::is_fresh;
use crate::body::{FnCx, Want};
use crate::defs::{Bound, ParamSig};
use crate::hir::{self, ExprKind as H, PassMode, TyId, TyKind};

/// A callable signature; `Param(i)` in its types are the callee's type parameters ("slots").
pub(crate) struct Callable {
    /// For messages: "function `f`", "method `push`".
    pub what: String,
    pub params: Vec<ParamSig>,
    pub ret: TyId,
    pub slot_names: Vec<String>,
    pub bounds: Vec<Vec<Bound>>,
    /// A `std/` function called from user code: a float argument for a parameter it declares
    /// an integer converts like JS's `ToIntegerOrInfinity` (`xs.slice(0, xs.length / 2)`).
    pub js_numbers: bool,
    /// A function of the JS API (`numbers::is_js_api`): integers it passes to callbacks (an
    /// index) are numbers there.
    pub js_api: bool,
    /// The last parameter is a rest parameter (`...xs: T[]`): the remaining arguments, spreads
    /// included, become one array literal.
    pub rest: bool,
    /// Per slot, its default (`new D(1)` of `class D<T = i64>`: in terms of the slots before
    /// it), used when nothing infers the slot, as in TS; empty when there are none.
    pub defaults: Vec<Option<TyId>>,
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
pub(crate) fn deferred(e: &ast::Expr) -> bool {
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
        let collect = std::mem::take(&mut self.collect_iterable_args);
        let packed;
        let args = match self.pack_rest(c, args) {
            Some((p, skip)) if !skip.is_empty() => {
                return self.check_call_skipping(c, skip, slots, &p, exp, span)
            }
            Some((p, _)) => {
                packed = p;
                &packed[..]
            }
            None => args,
        };
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
            self.cx.match_context(c.ret, e, &mut context);
        }
        let checked = self.args_in_rounds(c, &mut slots, &context, args, collect);
        if let Some(e) = exp {
            self.prefer_context(c, &mut slots, &context, &checked, e);
            self.cx.match_ty(c.ret, e, &mut slots);
        }
        // An argument that is already an error (reported) leaves its slots unknown: no second
        // error about inferring them.
        let quiet = checked.iter().any(|h| h.ty == self.cx.ty.error);
        self.default_slots(c, &mut slots);
        let type_args = self.solve_slots(c, &slots, quiet, span);
        let mut hargs = vec![];
        for (h, p) in checked.into_iter().zip(&c.params) {
            let target = self.cx.ty.subst(p.ty, &type_args);
            // Only a parameter the library declares an integer (`slice(start: i64)`), not a
            // type argument the program chose (`push` on an `i64[]`).
            let declared_int = self.cx.ty.is_int(p.ty);
            let h = if c.js_numbers && declared_int && self.cx.ty.is_float(h.ty) {
                self.literal_use_as(&h, target);
                // A saturating cast: truncates, NaN gives 0, ±Infinity the type's bounds.
                let span = h.span;
                self.mk(H::Cast(Box::new(h)), target, span)
            } else {
                h
            };
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

    /// The arguments of a call to a function with a rest parameter: those from the rest
    /// parameter's position on, spreads included, as one array literal (`f(1, ...xs)` passes
    /// `[1, ...xs]`), and the parameters a spread skips (see below). `None` when nothing needs
    /// packing.
    ///
    /// A spread before the rest parameter's position may skip defaulted parameters of a `std/`
    /// function, whose defaults there are identities (`Math.max(...xs)`: `a` and `b` are
    /// `-Infinity`, all of `xs` goes to `rest`). Elsewhere JS would bind `xs[0]` to the first
    /// parameter, which Velt does not do, so it is an error.
    fn pack_rest(
        &mut self,
        c: &Callable,
        args: &[ast::Expr],
    ) -> Option<(Vec<ast::Expr>, std::ops::Range<usize>)> {
        let at = c.params.len().checked_sub(1).filter(|_| c.rest)?;
        let spread = args[..at.min(args.len())]
            .iter()
            .position(|a| matches!(a.kind, ast::ExprKind::Spread(_)));
        let mut from = at;
        if let Some(k) = spread {
            if c.js_numbers && c.params[k..at].iter().all(|p| p.default.is_some()) {
                from = k;
            } else {
                self.cx.err(
                    "a spread argument can only fill the rest parameter (`...xs: T[]`)",
                    args[k].span,
                );
            }
        }
        if args.len() <= from {
            return None;
        }
        let rest = &args[from..];
        let span = rest[0].span.to(rest[rest.len() - 1].span);
        let array = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Array(rest.to_vec()),
            span,
        };
        let mut out = args[..from].to_vec();
        out.push(array);
        Some((out, from..at))
    }

    /// A call whose spread skipped the defaulted parameters `skip` (see `pack_rest`): checked
    /// without them, then their defaults are put in place.
    fn check_call_skipping(
        &mut self,
        c: &Callable,
        skip: std::ops::Range<usize>,
        slots: Vec<Option<TyId>>,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Checked {
        let mut params = c.params.clone();
        let skipped: Vec<ParamSig> = params.drain(skip.clone()).collect();
        let packed = Callable {
            what: c.what.clone(),
            params,
            ret: c.ret,
            slot_names: c.slot_names.clone(),
            bounds: c.bounds.clone(),
            js_numbers: c.js_numbers,
            js_api: c.js_api,
            rest: false,
            defaults: c.defaults.clone(),
        };
        let mut ck = self.check_call(&packed, slots, args, exp, span);
        if ck.args.is_empty() {
            return ck;
        }
        for (k, p) in skipped.iter().enumerate() {
            let mut d = p
                .default
                .clone()
                .expect("ICE: skipped parameters have defaults");
            crate::visit::map_expr_types(&mut d, &mut |t| self.cx.ty.subst(t, &ck.type_args));
            d.span = span;
            ck.args.insert(skip.start + k, d);
        }
        ck
    }

    /// Check `args` (typed ones first, then context-typed literals, then arrows), binding
    /// type parameters from each; results in parameter order. With `collect`, an iterable
    /// passed for an array parameter is collected (`new Set(gen())`). A slot still unknown when an
    /// argument is checked takes its type from `context` (inferred from the expected result).
    fn args_in_rounds(
        &mut self,
        c: &Callable,
        slots: &mut [Option<TyId>],
        context: &[Option<TyId>],
        args: &[ast::Expr],
        collect: bool,
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
                self.number_slot_from_callback(c, p.ty, slots, has_float_lit(&args[i]));
                self.number_slot_from_context(p.ty, slots, context);
            }
            let known: Vec<Option<TyId>> =
                slots.iter().zip(context).map(|(s, c)| s.or(*c)).collect();
            let expected = self.cx.ty.subst_known(p.ty, &known);
            let adapter = self.fewer_params_adapter(&args[i], expected);
            let arrow = adapter.as_ref().or(as_arrow(&args[i]));
            // An arrow passed to the JS API (also where `cmp | null` is expected): what its
            // parameters and result are in user code (`closure`, `returns::returned`).
            let declared = self.cx.ty.opt_payload(p.ty).unwrap_or(p.ty);
            self.std_callback = match self.cx.ty.kind(declared) {
                TyKind::FnPtr { params, .. } if c.js_api && arrow.is_some() => {
                    Some(params.iter().map(|t| self.cx.ty.is_int(*t)).collect())
                }
                _ => None,
            };
            let h = match arrow {
                Some(a) if matches!(self.cx.ty.kind(expected), TyKind::FnPtr { .. }) => {
                    self.arrow_arg(a, expected, p.mode == PassMode::Owned)
                }
                _ => self.expr(&args[i], Some(expected), want_of(p.mode)),
            };
            self.std_callback = None;
            let h = match collect {
                true => self.collected_arg(h, p.ty),
                false => h,
            };
            self.cx.match_ty(p.ty, h.ty, slots);
            out[i] = Some(h);
        }
        out.into_iter()
            .map(|h| h.expect("ICE: arg checked"))
            .collect()
    }

    /// Slots the arguments fixed to a type narrower than the expected result's take the expected
    /// type when the result would not convert otherwise and every argument converts to it:
    /// `const ns: Named[] = wrap(new C())` instantiates `T = Named` (#268).
    fn prefer_context(
        &mut self,
        c: &Callable,
        slots: &mut [Option<TyId>],
        context: &[Option<TyId>],
        args: &[hir::Expr],
        exp: TyId,
    ) {
        let ret = self.cx.ty.subst_known(c.ret, slots);
        if self.converts_to(ret, exp) {
            return;
        }
        let mut wider = slots.to_vec();
        for (k, (s, ctx)) in slots.iter().zip(context).enumerate() {
            let (Some(at), Some(ct)) = (*s, *ctx) else {
                continue;
            };
            let unbounded = c.bounds.get(k).is_none_or(|b| b.is_empty());
            // An integer slot may become a float one: the arguments are checked below.
            let converts =
                self.converts_to(at, ct) || (self.cx.ty.is_int(at) && self.float_core(ct));
            if at != ct && unbounded && !self.cx.ty.has_error(ct) && converts {
                wider[k] = Some(ct);
            }
        }
        if wider == slots {
            return;
        }
        for (h, p) in args.iter().zip(&c.params) {
            let target = self.cx.ty.subst_known(p.ty, &wider);
            let number = self.exact_in_number(h.ty) && self.float_core(target);
            if !self.converts_to(h.ty, target)
                && !number
                && !(is_fresh(h) && self.widens(h.ty, target))
            {
                return;
            }
        }
        slots.copy_from_slice(&wider);
    }

    /// Is `t` a float type, or one with `null` (`(number | null)[]` expected from `wrap(1)`: a
    /// JS number argument converts)?
    fn float_core(&self, t: TyId) -> bool {
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        self.cx.ty.is_float(t)
    }

    /// An untyped number argument (`0` in `xs.reduce((a, x) => a + x, 0)`) whose parameter is
    /// a still unknown slot `S`: when a callback parameter also has `S` next to a known number
    /// type, `S` is that type (`usize` for a `usize[]`), which is what TS's single `number`
    /// gives; otherwise the literal's default type decides as usual. A `float` literal (`0.0`)
    /// only takes a float type: `shapes.reduce((acc, s) => acc + area(s), 0.0)` must not take
    /// the callback's `i: i64` index for the accumulator.
    fn number_slot_from_callback(
        &mut self,
        c: &Callable,
        pty: TyId,
        slots: &mut [Option<TyId>],
        float: bool,
    ) {
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
            // An integer the JS API declares (a callback's index) is a number in user code.
            let params: Vec<TyId> = params
                .into_iter()
                .filter(|t| !(c.js_api && self.cx.ty.is_int(*t)))
                .collect();
            let known: Vec<TyId> = params
                .iter()
                .map(|t| self.cx.ty.subst_known(*t, slots))
                .collect();
            let known = known.into_iter().find(|t| {
                *t != pty && self.cx.ty.is_numeric(*t) && (!float || self.cx.ty.is_float(*t))
            });
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

    /// Unknown slots are errors (unless `quiet`: an argument's error was reported); check bounds
    /// of the known ones.
    pub(super) fn solve_slots(
        &mut self,
        c: &Callable,
        slots: &[Option<TyId>],
        quiet: bool,
        span: Span,
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
            out.push(t);
        }
        out
    }

    /// Slots nothing inferred take their defaults (while the slots before them are known).
    fn default_slots(&mut self, c: &Callable, slots: &mut [Option<TyId>]) {
        for k in 0..slots.len() {
            let Some(Some(d)) = c.defaults.get(k) else {
                continue;
            };
            if slots[k].is_some() {
                continue;
            }
            let Some(before) = slots[..k].iter().copied().collect::<Option<Vec<TyId>>>() else {
                break;
            };
            slots[k] = Some(self.cx.ty.subst(*d, &before));
        }
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
        let note = uninferred_note(c);
        self.cx.error(
            Diagnostic::error(format!("cannot infer {list} of {}", c.what), span).with_note(note),
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

/// Does the untyped number expression `e` contain a float literal (`0.0`, `-(1 + 0.5)`)?
fn has_float_lit(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Float { .. }) => true,
        ast::ExprKind::Unary { expr, .. } | ast::ExprKind::Paren(expr) => has_float_lit(expr),
        ast::ExprKind::Binary { lhs, rhs, .. } => has_float_lit(lhs) || has_float_lit(rhs),
        _ => false,
    }
}

/// How to fix a call whose type arguments nothing inferred. A JSON decoder's type is what the
/// data is checked against (TypeScript's `any` from `res.json()` has no Velt counterpart); a type
/// that only the result mentions is written at the call or on the variable receiving it.
fn uninferred_note(c: &Callable) -> String {
    if c.what == "method `json`" || c.what == "`JSON.parse`" {
        let call = if c.what == "method `json`" {
            "await res.json"
        } else {
            "JSON.parse"
        };
        let arg = if c.params.is_empty() { "" } else { "text" };
        return format!(
            "Velt has no `any`: name the type the JSON must have (the data is checked against \
             it): `{call}<User[]>({arg})`, or annotate the variable: \
             `const users: User[] = {call}({arg})`"
        );
    }
    if c.params.is_empty() {
        return "only the result's type mentions it: write it at the call, e.g. `f<User>()`, or \
                annotate the variable receiving the result"
            .to_string();
    }
    "add explicit type arguments, e.g. `f<i64>(...)`, or annotate the result".to_string()
}

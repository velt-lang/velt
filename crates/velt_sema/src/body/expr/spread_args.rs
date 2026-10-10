//! Spread arguments into fixed parameters (`f(...t)` with `t: [number, string]`), as TypeScript
//! accepts them: a spread whose length is known when compiling stands for its elements. That is
//! a variable or field path of a tuple type (`f(t[0], t[1])`), or an array literal (also cast,
//! `...([2, 3] as [number, number])`: its elements). Too few or too many elements are the
//! arity error of the same call written out, so a missing element never becomes `undefined`.
//!
//! A tuple that is not stored (`f(0, ...g.pair)` through a getter, `f(...pair())`) is held in
//! a hidden temporary evaluated before the call, once, as JS reads it. A function or receiver
//! that is a call (`getf()(...pair())`, `mk().m(...pair())`) is held in one before it, as JS
//! evaluates it first; so is a receiver or a leading argument read through a getter
//! (`h.g.m(...pair())`, `f(h.g, ...pair())`), and a function a getter returns
//! (`h.fnget(...pair())`), since a getter read is a call.
//!
//! A spread of an array type (`T[]`) can only fill a rest parameter: its length is known only at
//! run time, and JS would bind `undefined` to the parameters it leaves out, which Velt has no
//! counterpart for. TypeScript rejects that call too (TS2556), so this is a compile error rather
//! than a run-time check.

use velt_common::Span;
use velt_syntax::ast;

use super::args::Callable;
use crate::body::places::{is_path, is_place};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, TyId, TyKind};

/// What a spread of a tuple that is not stored comes after, when it can't be read first.
const ARG_EFFECTS: &str = "an argument with effects";
const CALLEE_EFFECTS: &str = "a function or receiver with effects";

impl FnCx<'_, '_> {
    /// `callee(args)` with a spread argument: a function or receiver that is a call or `new` is
    /// first held in a hidden temporary, so it runs before a spread's (see the module docs).
    pub(super) fn call_spreading(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let callee = match &callee.kind {
            // A function read through a getter (`h.fnget(...pair())`): the getter is a call.
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } if self.is_getter_member(object, prop) => self.hidden_temp(callee, true).0,
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } if is_call(object) || self.is_getter_value(object) => {
                let (object, _) = self.hidden_temp(object, true);
                let kind = ast::ExprKind::Member {
                    object: Box::new(object),
                    prop: prop.clone(),
                    optional: false,
                };
                super::setters::synth(kind, callee.span)
            }
            _ if is_call(callee) => self.hidden_temp(callee, true).0,
            _ => callee.clone(),
        };
        // A method's receiver: reading the method itself is not a call.
        let pure = match &callee.kind {
            ast::ExprKind::Member {
                object,
                optional: false,
                ..
            } => self.evaluated_anywhere(object),
            _ => self.evaluated_anywhere(&callee),
        };
        let effects = (!pure).then_some(span);
        let outer = std::mem::replace(&mut self.callee_effects, effects);
        let h = self.call(&callee, type_args, args, false, exp, span);
        self.callee_effects = outer;
        h
    }

    /// `args` with each spread of a known length replaced by its elements; `None` when there is
    /// none. `Err` after reporting a spread that no parameter can take.
    pub(super) fn expand_spreads(
        &mut self,
        c: &Callable,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<Option<Vec<ast::Expr>>, ()> {
        if !args.iter().any(is_spread) {
            return Ok(None);
        }
        let callee_pure = self.callee_effects != Some(span);
        let args = &self.hold_getter_args(args)[..];
        let mut out = vec![];
        let mut left = vec![];
        let mut failed = false;
        let mut earlier_pure = true;
        for (k, a) in args.iter().enumerate() {
            if k > 0 && earlier_pure {
                earlier_pure = self.evaluated_anywhere(&args[k - 1]);
            }
            let ast::ExprKind::Spread(inner) = &a.kind else {
                out.push(a.clone());
                continue;
            };
            let after = if !callee_pure {
                Some(CALLEE_EFFECTS)
            } else if !earlier_pure {
                Some(ARG_EFFECTS)
            } else {
                None
            };
            match self.spread_elems(inner, after) {
                Ok(Some(elems)) => out.extend(elems),
                Ok(None) => {
                    left.push(inner.as_ref());
                    out.push(a.clone());
                }
                Err(()) => failed = true,
            }
        }
        if !failed && (c.rest || left.is_empty()) {
            return Ok(Some(out));
        }
        if !c.rest {
            for inner in left {
                self.unknown_length_spread(inner);
            }
        }
        let others: Vec<ast::Expr> = args.iter().filter(|a| !is_spread(a)).cloned().collect();
        self.check_args_loose(&others);
        Err(())
    }

    /// The elements a spread of `inner` stands for, when its length is known. A tuple that is
    /// not stored (a call's result, a getter's) is read once into a hidden temporary, evaluated
    /// before the call: its elements are that temporary's. That is the order JS evaluates in
    /// when the function, its receiver and the arguments before the spread have no effects;
    /// otherwise (`after` says which has) it is reported (`Err`).
    fn spread_elems(
        &mut self,
        inner: &ast::Expr,
        after: Option<&'static str>,
    ) -> Result<Option<Vec<ast::Expr>>, ()> {
        let e = strip(inner);
        if let ast::ExprKind::Array(xs) = &e.kind {
            return Ok((!xs.iter().any(is_spread)).then(|| xs.clone()));
        }
        if !is_member_chain(e) {
            if !self.tuple_by_trial(e) {
                return Ok(None);
            }
            if let Some(after) = after {
                self.tuple_not_stored(e, after);
                return Err(());
            }
            return Ok(Some(self.tuple_temp(e)));
        }
        // Checking a member chain has no effect, so checking it again per element is safe; the
        // checked value says whether reading it is a call (a getter).
        let h = self.expr(e, None, Want::Borrow);
        let TyKind::Tuple(ts) = self.cx.ty.kind(h.ty) else {
            return Ok(None);
        };
        let n = ts.len();
        if !is_path(&h) {
            if let Some(after) = after {
                self.tuple_not_stored(e, after);
                return Err(());
            }
            return Ok(Some(self.tuple_temp(e)));
        }
        Ok(Some((0..n).map(|k| index(e, k)).collect()))
    }

    /// Is `e` (not a variable or field path) of a tuple type? Checked as a trial, rolled back.
    fn tuple_by_trial(&mut self, e: &ast::Expr) -> bool {
        let h = self.trial_expr(e);
        matches!(self.cx.ty.kind(h.ty), TyKind::Tuple(_))
    }

    /// `e`, a tuple, held in a hidden temporary evaluated before the call (`call_temps`): the
    /// reads of its elements.
    fn tuple_temp(&mut self, e: &ast::Expr) -> Vec<ast::Expr> {
        let (read, ty) = self.hidden_temp(e, false);
        let n = match self.cx.ty.kind(ty) {
            TyKind::Tuple(ts) => ts.len(),
            _ => 0,
        };
        (0..n).map(|k| index(&read, k)).collect()
    }

    /// `e` evaluated into a hidden temporary before the call (`call_temps`): a name reading it,
    /// and its type.
    fn hidden_temp(&mut self, e: &ast::Expr, mutable: bool) -> (ast::Expr, TyId) {
        let h = self.expr(e, None, Want::Move);
        let (ty, span) = (h.ty, h.span);
        let l = self.new_local("<spread>", ty, mutable, span, LocalKind::Temp);
        let name = format!("#spread{}", l.0);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(name.clone(), l);
        self.call_temps.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span,
        });
        let kind = ast::ExprKind::Ident(ast::Ident { name, span });
        (super::setters::synth(kind, span), ty)
    }

    /// A spread of a tuple that is not a variable or a field (a call, a getter) after an
    /// argument (or a function or receiver) with effects (`after`): reported.
    fn tuple_not_stored(&mut self, e: &ast::Expr, after: &str) {
        let shown = shown(e);
        let first = match after {
            CALLEE_EFFECTS => "the function and its receiver",
            _ => "the arguments before it",
        };
        self.cx.error(
            velt_common::Diagnostic::error(
                format!("a spread argument of a tuple after {after} must be a variable or a field (not a call or a getter)"),
                e.span,
            )
            .with_note(format!("JavaScript evaluates {first} first, then reads the value once, and each parameter takes one element of it"))
            .with_note(format!(
                "store the value in a variable first: `const t = {shown}; f(...t);`"
            )),
        );
    }

    /// A spread for fixed parameters whose length is not known when compiling (reported).
    fn unknown_length_spread(&mut self, inner: &ast::Expr) {
        let h = self.expr(inner, None, Want::Borrow);
        if self.cx.ty.is_bottom(h.ty) {
            return;
        }
        let span = inner.span;
        if matches!(self.cx.ty.kind(h.ty), TyKind::Tuple(_)) {
            return self.tuple_not_stored(inner, ARG_EFFECTS);
        }
        let tn = self.cx.display(h.ty);
        let d = velt_common::Diagnostic::error(
                "a spread argument must have a tuple type or fill a rest parameter (`...xs: T[]`)",
                span,
            )
            .with_note(format!(
                "the length of a value of type `{tn}` is only known at run time, and JavaScript would pass \
                 `undefined` for the parameters it does not fill, which Velt does not have"
            ))
            .with_note(
                "pass the elements (`f(xs[0], xs[1])`), or give the value a tuple type \
                 (`const t: [number, number] = [1, 2];`)",
            );
        self.cx.error(d);
    }
}

impl FnCx<'_, '_> {
    /// Does evaluating `e` have no effect, so evaluating a later expression first keeps JS's
    /// order? A literal, an arrow, a variable or a field path; not a getter read (`o.g`), which
    /// is a call (told by checking `e` as a trial).
    pub(super) fn evaluated_anywhere(&mut self, e: &ast::Expr) -> bool {
        let e = unparen(e);
        match &e.kind {
            ast::ExprKind::Lit(_)
            | ast::ExprKind::Arrow { .. }
            | ast::ExprKind::Ident(_)
            | ast::ExprKind::This => true,
            // `t[0]` on a variable or field path: told by checking it, as for `o.f`.
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } if matches!(index.kind, ast::ExprKind::Lit(ast::Lit::Int { .. }))
                && super::setters::side_effect_free(object) =>
            {
                let h = self.trial_expr(e);
                read_without_effects(&h)
            }
            _ if !super::setters::side_effect_free(e) => false,
            _ => {
                let h = self.trial_expr(e);
                read_without_effects(&h)
            }
        }
    }

    /// Is `e` a member chain read through a getter (`h.g`), whose value is not a place, so it
    /// can be held in a temporary?
    fn is_getter_value(&mut self, e: &ast::Expr) -> bool {
        let e = unparen(e);
        if !is_member_chain(e) || self.evaluated_anywhere(e) {
            return false;
        }
        let h = self.trial_expr(e);
        !is_place(&h) && !self.cx.ty.is_bottom(h.ty)
    }

    /// Is `object.prop` a getter (not a field or a method), so reading it is a call?
    fn is_getter_member(&mut self, object: &ast::Expr, prop: &ast::Ident) -> bool {
        let h = self.trial_expr(object);
        !self.cx.ty.is_bottom(h.ty) && self.has_getter(h.ty, &prop.name)
    }

    /// `e` checked as a trial, rolled back.
    fn trial_expr(&mut self, e: &ast::Expr) -> hir::Expr {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let h = self.expr(e, None, Want::Borrow);
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.restore_trial_frames(frames);
        h
    }

    /// `args` with each getter read (`h.g`) before the last spread held in a hidden temporary
    /// evaluated before the call, so it runs before a spread's temporary, as in JS. Only the
    /// leading arguments up to the first other one with effects are held: a later getter would
    /// otherwise run before it.
    fn hold_getter_args(&mut self, args: &[ast::Expr]) -> Vec<ast::Expr> {
        let mut out = args.to_vec();
        let Some(last) = args.iter().rposition(is_spread) else {
            return out;
        };
        for a in &mut out[..last] {
            if self.evaluated_anywhere(a) {
                continue;
            }
            if is_spread(a) || !self.is_getter_value(a) {
                break;
            }
            *a = self.hidden_temp(a, false).0;
        }
        out
    }
}

/// A checked read with no effects: a variable, a constant, a field path of one, or a literal
/// index into one (narrowing projections included); not a call (a getter).
fn read_without_effects(h: &hir::Expr) -> bool {
    match &h.kind {
        hir::ExprKind::Local(..) | hir::ExprKind::Global(_) | hir::ExprKind::Lit(_) => true,
        hir::ExprKind::Field { base, .. }
        | hir::ExprKind::UnwrapSome(base, _)
        | hir::ExprKind::UnwrapVariant { expr: base, .. }
        | hir::ExprKind::Downcast(base) => read_without_effects(base),
        hir::ExprKind::Index { base, index, .. } => {
            matches!(index.kind, hir::ExprKind::Lit(_)) && read_without_effects(base)
        }
        _ => false,
    }
}

fn unparen(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => unparen(x),
        _ => e,
    }
}

/// A call or `new` (parenthesized or not).
fn is_call(e: &ast::Expr) -> bool {
    matches!(
        strip(e).kind,
        ast::ExprKind::Call { .. } | ast::ExprKind::New { .. }
    )
}

fn is_spread(e: &ast::Expr) -> bool {
    matches!(e.kind, ast::ExprKind::Spread(_))
}

/// `e` without parentheses and type casts (`([1, 2] as [number, number])`).
fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) | ast::ExprKind::Cast { expr: x, .. } => strip(x),
        _ => e,
    }
}

/// A variable, `this` or a member chain on one (fields, or getters: see `spread_elems`).
fn is_member_chain(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_member_chain(object),
        _ => false,
    }
}

/// `object[k]`
fn index(object: &ast::Expr, k: usize) -> ast::Expr {
    let span = object.span;
    let lit = ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Lit(ast::Lit::Int {
            value: k as u128,
            suffix: None,
        }),
        span,
    };
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Index {
            object: Box::new(object.clone()),
            index: Box::new(lit),
            optional: false,
        },
        span,
    }
}

/// `e` as written, for a note: a call of a name or member chain with arguments shown that way
/// too (`pair()`, `o.pair(1)`); `the value` otherwise.
fn shown(e: &ast::Expr) -> String {
    use crate::body::switch::cases::source_text;
    match &unparen(e).kind {
        ast::ExprKind::Call {
            callee,
            args,
            optional: false,
            ..
        } if is_member_chain(callee) => {
            let args: Vec<String> = args.iter().map(source_text).collect();
            if args.iter().any(|a| a == "the value") {
                return "the value".into();
            }
            format!("{}({})", source_text(callee), args.join(", "))
        }
        _ => source_text(e),
    }
}

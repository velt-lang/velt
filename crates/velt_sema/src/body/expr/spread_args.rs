//! Spread arguments into fixed parameters (`f(...t)` with `t: [number, string]`), as TypeScript
//! accepts them: a spread whose length is known when compiling stands for its elements. That is
//! a variable or field path of a tuple type (`f(t[0], t[1])`), or an array literal (also cast,
//! `...([2, 3] as [number, number])`: its elements). Too few or too many elements are the
//! arity error of the same call written out, so a missing element never becomes `undefined`.
//!
//! A spread of an array type (`T[]`) can only fill a rest parameter: its length is known only at
//! run time, and JS would bind `undefined` to the parameters it leaves out, which Velt has no
//! counterpart for. TypeScript rejects that call too (TS2556), so this is a compile error rather
//! than a run-time check.

use velt_syntax::ast;

use super::args::Callable;
use crate::body::places::is_path;
use crate::body::{FnCx, Want};
use crate::hir::TyKind;

impl FnCx<'_, '_> {
    /// `args` with each spread of a known length replaced by its elements; `None` when there is
    /// none. `Err` after reporting a spread that no parameter can take.
    pub(super) fn expand_spreads(
        &mut self,
        c: &Callable,
        args: &[ast::Expr],
    ) -> Result<Option<Vec<ast::Expr>>, ()> {
        if !args.iter().any(is_spread) {
            return Ok(None);
        }
        let mut out = vec![];
        let mut left = vec![];
        let mut failed = false;
        for a in args {
            let ast::ExprKind::Spread(inner) = &a.kind else {
                out.push(a.clone());
                continue;
            };
            match self.spread_elems(inner) {
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

    /// The elements a spread of `inner` stands for, when its length is known. `Err` after
    /// reporting a tuple read through a getter: reading it once per element would call it
    /// again each time, where JS reads it once.
    fn spread_elems(&mut self, inner: &ast::Expr) -> Result<Option<Vec<ast::Expr>>, ()> {
        let e = strip(inner);
        if let ast::ExprKind::Array(xs) = &e.kind {
            return Ok((!xs.iter().any(is_spread)).then(|| xs.clone()));
        }
        if !is_member_chain(e) {
            return Ok(None);
        }
        // Checking a member chain has no effect, so checking it again per element is safe; the
        // checked value says whether reading it is a call (a getter).
        let h = self.expr(e, None, Want::Borrow);
        let TyKind::Tuple(ts) = self.cx.ty.kind(h.ty) else {
            return Ok(None);
        };
        let n = ts.len();
        if !is_path(&h) {
            self.tuple_not_stored(e);
            return Err(());
        }
        Ok(Some((0..n).map(|k| index(e, k)).collect()))
    }

    /// A spread of a tuple that is not a variable or a field (a call, a getter): reported.
    fn tuple_not_stored(&mut self, e: &ast::Expr) {
        let shown = crate::body::switch::cases::source_text(e);
        self.cx.error(
            velt_common::Diagnostic::error(
                "a spread argument of a tuple must be a variable or a field (not a call or a getter)",
                e.span,
            )
            .with_note("JavaScript reads the value once, and each parameter takes one element of it")
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
            return self.tuple_not_stored(inner);
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

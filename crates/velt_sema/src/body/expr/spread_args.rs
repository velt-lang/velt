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
        for a in args {
            let ast::ExprKind::Spread(inner) = &a.kind else {
                out.push(a.clone());
                continue;
            };
            match self.spread_elems(inner) {
                Some(elems) => out.extend(elems),
                None => {
                    left.push(inner.as_ref());
                    out.push(a.clone());
                }
            }
        }
        if c.rest || left.is_empty() {
            return Ok(Some(out));
        }
        for inner in left {
            self.unknown_length_spread(inner);
        }
        let others: Vec<ast::Expr> = args.iter().filter(|a| !is_spread(a)).cloned().collect();
        self.check_args_loose(&others);
        Err(())
    }

    /// The elements a spread of `inner` stands for, when its length is known.
    fn spread_elems(&mut self, inner: &ast::Expr) -> Option<Vec<ast::Expr>> {
        let e = strip(inner);
        if let ast::ExprKind::Array(xs) = &e.kind {
            return (!xs.iter().any(is_spread)).then(|| xs.clone());
        }
        if !is_path(e) {
            return None;
        }
        // Reading a path has no effect, so checking it here and again per element is safe.
        let h = self.expr(e, None, Want::Borrow);
        let TyKind::Tuple(ts) = self.cx.ty.kind(h.ty) else {
            return None;
        };
        Some((0..ts.len()).map(|k| index(e, k)).collect())
    }

    /// A spread for fixed parameters whose length is not known when compiling (reported).
    fn unknown_length_spread(&mut self, inner: &ast::Expr) {
        let h = self.expr(inner, None, Want::Borrow);
        if self.cx.ty.is_bottom(h.ty) {
            return;
        }
        let span = inner.span;
        let d = if matches!(self.cx.ty.kind(h.ty), TyKind::Tuple(_)) {
            velt_common::Diagnostic::error(
                "a spread argument of a tuple must be a variable or a field",
                span,
            )
            .with_note("store the value in a variable first: `const t = pair(); f(...t);`")
        } else {
            let tn = self.cx.display(h.ty);
            velt_common::Diagnostic::error(
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
            )
        };
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

/// A variable, `this` or a field path of one.
fn is_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_path(object),
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

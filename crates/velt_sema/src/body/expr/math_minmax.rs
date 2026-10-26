//! `Math.min(a, b, …)` and `Math.max(a, b, …)` with a fixed list of arguments: a balanced tree
//! of the prelude's two-value `min2` / `max2` (`std/prelude/math.vlt`), its arguments evaluated
//! left to right. The prelude's `Math.min` takes the values past the second as a rest array,
//! which a call builds on the heap each time; this keeps calls in loops allocation-free whether
//! or not the inliner reaches them (#1570). A spread argument (`Math.min(...xs)`), no argument,
//! a single argument and more than [`MAX_TREE_ARGS`] arguments keep the ordinary call.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H};

/// The most arguments a call compiles to a tree. A wider call (a generated table) keeps the
/// ordinary call: its one rest array costs little next to that many values, and every call
/// result is a local and a block of its own in VIR, which a function's analyses would pay for
/// per call.
const MAX_TREE_ARGS: usize = 16;

impl FnCx<'_, '_> {
    /// The tree of `min2` / `max2` calls for `Math.min` / `Math.max` of 2 to
    /// [`MAX_TREE_ARGS`] plain arguments on the prelude's `Math`; `None` if the call is
    /// anything else.
    pub(crate) fn math_minmax_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(m) = &object.kind else {
            return None;
        };
        let helper = match prop.name.as_str() {
            "min" => "min2",
            "max" => "max2",
            _ => return None,
        };
        if m.name != "Math"
            || args.len() < 2
            || args.len() > MAX_TREE_ARGS
            || args
                .iter()
                .any(|a| matches!(a.kind, ast::ExprKind::Spread(_)))
            || self.is_local_name("Math")
        {
            return None;
        }
        let math = self.cx.prelude_adt("Math")?;
        match self.cx.lookup_item_at(self.module, "Math", m.span) {
            Some(crate::ctx::Item::Def(d)) if d == math => {}
            _ => return None,
        }
        let d = self.cx.prelude_fn(helper)?;
        let f64_ = self.cx.ty.f64;
        let vals: Vec<hir::Expr> = args
            .iter()
            .map(|a| self.expr_coerce(a, f64_, Want::Move))
            .collect();
        // A balanced tree, not a chain: `min2(min2(a, b), min2(c, d))`. Its leaves are the
        // arguments in source order and call arguments are evaluated left to right, so each
        // argument is still evaluated (and converted to a number) once, in order; `min2` /
        // `max2` have no effects and are associative, also for NaN and the two zeros. The depth
        // is about log2 of the argument count, and the comparisons of one level do not wait
        // for each other.
        let mut level = vals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut it = level.into_iter();
            while let Some(a) = it.next() {
                next.push(match it.next() {
                    Some(b) => self.mk(
                        H::Call {
                            callee: hir::Callee::Def(d, vec![]),
                            args: vec![a, b],
                        },
                        f64_,
                        span,
                    ),
                    None => a,
                });
            }
            level = next;
        }
        level.pop()
    }
}

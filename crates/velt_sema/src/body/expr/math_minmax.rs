//! `Math.min(a, b, …)` and `Math.max(a, b, …)` with a fixed list of arguments: a chain of the
//! prelude's two-value `min2` / `max2` (`std/prelude/math.vlt`), evaluated left to right as the
//! arguments are. The prelude's `Math.min` takes the values past the second as a rest array,
//! which a call builds on the heap each time; this keeps calls in loops allocation-free whether
//! or not the inliner reaches them (#1570). A spread argument (`Math.min(...xs)`), no argument
//! and a single argument keep the ordinary call.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H};

impl FnCx<'_, '_> {
    /// The chain of `min2` / `max2` calls for `Math.min` / `Math.max` of two or more plain
    /// arguments on the prelude's `Math`; `None` if the call is anything else.
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
        let mut vals = vals.into_iter();
        let mut acc = vals.next()?;
        for v in vals {
            acc = self.mk(
                H::Call {
                    callee: hir::Callee::Def(d, vec![]),
                    args: vec![acc, v],
                },
                f64_,
                span,
            );
        }
        Some(acc)
    }
}

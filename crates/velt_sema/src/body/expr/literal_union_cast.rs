//! `x as Hydrate` for unions of string literals (`"load" | "only"` as `"load" | "never"`, a
//! literal union narrowed to some members included). TypeScript accepts the assertion (it
//! compares string literal types through `string`) and keeps the string. Velt's literal union
//! is an enum of its literals, so the value is tested against each literal of the target it may
//! be, in the target's order, and becomes that member; a value that is none of them stops with
//! a `TypeError`, as other failed assertions do (Node goes on with the string). A `string` source
//! (`path as Route`) stays an error until #1410 settles it.

use velt_common::Span;

use super::callback::synth;
use super::discriminated_cast::lit_expr;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, LitValue, TyId};

impl FnCx<'_, '_> {
    /// `h as target` for literal unions `target` and `h` (module docs); `Err(h)` for other
    /// shapes.
    pub(super) fn literal_union_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(values) = self.string_literals(target) else {
            return Err(h);
        };
        let source = self.cx.ty.core(h.ty).map_or(h.ty, |_| self.cx.ty.error);
        // A `string` (`path as Route`) is not asserted: `ROUTES.includes(path as Route)` tests a
        // string no member may hold, which Node answers with `false` (#1410).
        let held = match self.string_literals(source) {
            // A local narrowed to some of its members holds only those.
            Some(vs) => match self.narrowed_variants(&h) {
                Some(live) => (vs.into_iter().enumerate())
                    .filter(|(i, _)| live.contains(&(*i as u32)))
                    .map(|(_, v)| v)
                    .collect::<Vec<_>>(),
                None => vs,
            },
            None => return Err(h),
        };
        // The literals the value may be; with none, the assertion always stops (`"a" as "b"`).
        let kept: Vec<&String> = values.iter().filter(|v| held.contains(v)).collect();
        let complete = !held.is_empty() && held.iter().all(|v| kept.contains(&v));
        self.push_scope();
        let mut stmts = vec![];
        let id = self.temp_ident("#lit", h, &mut stmts);
        let mut out = match complete {
            true => None,
            false => {
                let name = self.cx.display(target);
                let msg =
                    format!("TypeError: type assertion failed: the value is not of type `{name}`");
                let msg = self.str_lit(&msg, span);
                let never = self.cx.ty.never;
                Some(self.intrinsic(Intrinsic::Panic, vec![msg], never, span))
            }
        };
        for v in kept.into_iter().rev() {
            let lit = lit_expr(&LitValue::Str(v.clone()), span);
            let built = self.expr_coerce(&lit, target, Want::Move);
            out = Some(match out {
                None => built,
                Some(els) => {
                    let test = velt_syntax::ast::ExprKind::Binary {
                        op: velt_syntax::ast::BinaryOp::StrictEq,
                        lhs: Box::new(id.clone()),
                        rhs: Box::new(lit),
                    };
                    let cond = self.cond(&synth(test, span));
                    let kind = H::If {
                        cond: Box::new(cond),
                        then: Box::new(built),
                        els: Box::new(els),
                    };
                    self.mk(kind, target, span)
                }
            });
        }
        self.pop_scope();
        let out = out.expect("ICE: a literal union cast tests a literal");
        Ok(self.with_temps_value(stmts, out))
    }
}

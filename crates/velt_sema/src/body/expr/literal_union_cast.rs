//! `x as Hydrate` for unions of string literals (`"load" | "only"` as `"load" | "never"`, a
//! literal union narrowed to some members included). TypeScript accepts the assertion (it
//! compares string literal types through `string`) and keeps the string. Velt's literal union
//! is an enum of its literals, so the value is tested against each literal of the target it may
//! be, in the target's order, and becomes that member; a value that is none of them stops with
//! a `TypeError`, as other failed assertions do (Node goes on with the string). A nullable
//! source is checked to hold a value first (`!`), as other assertions to a type without `null`
//! are. A `string` source (`path as Route`) is tested the same way (#1410), except where it is
//! compared (`literal_union_compare`).

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
        // A `T | null` / `T | undefined` of a literal union: its value, checked to be there with
        // `!` (a nothing stops, as `x as T` of a `null` does), then asserted.
        if let Some(core) = self.cx.ty.core(h.ty) {
            if self.string_literals(core).is_none() {
                return Err(h);
            }
            self.push_scope();
            let mut stmts = vec![];
            let id = self.temp_ident("#lit", h, &mut stmts);
            let present = synth(velt_syntax::ast::ExprKind::NonNull(Box::new(id)), span);
            let inner = self.expr(&present, None, Want::Move);
            let out = self
                .literal_union_cast(inner, target, span)
                .unwrap_or_else(|h| h);
            self.pop_scope();
            return Ok(self.with_temps_value(stmts, out));
        }
        let source = h.ty;
        // A `string` (`path as Route`) may be any of them, or none (#1410). As a comparison
        // (`literal_union_compare`) it is not converted.
        let held = match self.string_literals(source) {
            None if source == self.cx.ty.str_ => values.clone(),
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
        let complete =
            source != self.cx.ty.str_ && !held.is_empty() && held.iter().all(|v| kept.contains(&v));
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

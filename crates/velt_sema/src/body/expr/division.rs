//! `/` on JS numbers held as integers (docs/reference/types.md "Numbers"): float division
//! unless both operands are declared integers, `/=` on integer places, and `Math.trunc(a / b)`
//! as integer division. Where a value's integer type comes from is in `numbers`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::numbers::IntOrigin;
use crate::body::{FnCx, Want};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, TyId};

impl FnCx<'_, '_> {
    /// Is `l / r` on integers integer division? Both operands declared, or only literals in a
    /// context that expects an integer (`const n: i64 = 7 / 2`).
    fn int_division(&self, l: &hir::Expr, r: &hir::Expr, hint: Option<TyId>) -> bool {
        match self.int_origin(l).join(self.int_origin(r)) {
            IntOrigin::Declared => true,
            IntOrigin::Literal => hint.is_some_and(|t| self.cx.ty.is_int(t)),
            IntOrigin::Inferred => false,
        }
    }

    /// `l / r` of two checked operands of numeric type `t`.
    pub(crate) fn divide(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        t: TyId,
        hint: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (l, r, t) = if self.cx.ty.is_int(t) && !self.int_division(&l, &r, hint) {
            let f = self.cx.ty.f64;
            (self.int_to_float(l, f), self.int_to_float(r, f), f)
        } else {
            (l, r, t)
        };
        let kind = H::Binary {
            op: BinOp::Div,
            lhs: Box::new(l),
            rhs: Box::new(r),
        };
        self.mk(kind, t, span)
    }

    /// The hint for a float quotient found where an integer is required.
    pub(crate) fn float_division_note(&self, found: &hir::Expr) -> Option<&'static str> {
        let quotient = matches!(found.kind, H::Binary { op: BinOp::Div, .. });
        (quotient && self.cx.ty.is_float(found.ty)).then_some(
            "`/` gives a float (like JS) unless both operands are declared with integer types; for integer division write `Math.trunc(a / b)`",
        )
    }

    /// `x /= y` on an integer place: allowed only when it stays integer division.
    pub(crate) fn check_int_div_assign(&mut self, place: &hir::Expr, v: &hir::Expr, span: Span) {
        if self.cx.ty.is_int(place.ty) && !self.int_division(place, v, Some(place.ty)) {
            self.cx.error(
                Diagnostic::error(
                    "`/=` would store a float in an integer variable",
                    span,
                )
                .with_note(
                    "`/` gives a float (like JS) unless both operands are declared with integer types",
                )
                .with_note("for integer division write `x = Math.trunc(x / y)`, or declare the variable as a float (`let x = 0.0`)"),
            );
        }
    }

    /// `Math.trunc(a / b)` (the prelude's `Math`): integer division when both operands are
    /// integers, else `trunc` of the float quotient. `None` if the call is anything else.
    pub(crate) fn math_trunc_div(
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
        let (ast::ExprKind::Ident(m), [arg]) = (&object.kind, args) else {
            return None;
        };
        let mut arg = arg;
        while let ast::ExprKind::Paren(x) = &arg.kind {
            arg = x;
        }
        let ast::ExprKind::Binary {
            op: ast::BinaryOp::Div,
            lhs,
            rhs,
        } = &arg.kind
        else {
            return None;
        };
        if m.name != "Math" || prop.name != "trunc" || self.is_local_name("Math") {
            return None;
        }
        let math = self.cx.prelude_adt("Math")?;
        match self.cx.lookup_item_at(self.module, "Math", m.span) {
            Some(crate::ctx::Item::Def(d)) if d == math => {}
            _ => return None,
        }
        let (l, r) = self.operands(lhs, rhs, None, Want::Borrow);
        let (l, r) = self.mix_numbers(l, r);
        let (l, r) = self.mix_ints(l, r);
        let Some(t) = self.check_operands(ast::BinaryOp::Div, l.ty, &r, arg.span) else {
            return Some(self.error_expr(span));
        };
        if self.cx.ty.is_int(t) {
            let kind = H::Binary {
                op: BinOp::Div,
                lhs: Box::new(l),
                rhs: Box::new(r),
            };
            return Some(self.mk(kind, t, span));
        }
        let q = self.divide(l, r, t, None, arg.span);
        let ty = q.ty;
        Some(self.intrinsic(Intrinsic::Trunc, vec![q], ty, span))
    }
}

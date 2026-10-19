//! `/` (docs/reference/types.md "Numbers"): on numbers it is float division as in JS, on
//! declared integers integer division (truncating), and `Math.trunc(a / b)` on integers is
//! integer division too.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, TyId};

impl FnCx<'_, '_> {
    /// `l / r` of two checked operands of numeric type `t`.
    pub(crate) fn divide(&mut self, l: hir::Expr, r: hir::Expr, t: TyId, span: Span) -> hir::Expr {
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
            "`/` on numbers gives a float (like JS); for integer division write `Math.trunc(a / b)`",
        )
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
        self.literal_use_number(&l);
        self.literal_use_number(&r);
        let (l, r) = self.mix_numbers(l, r);
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
        let q = self.divide(l, r, t, arg.span);
        let ty = q.ty;
        Some(self.intrinsic(Intrinsic::Trunc, vec![q], ty, span))
    }
}

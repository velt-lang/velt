//! Unary operators: `-`, `+` and `~` (`!` is in `truthiness`; `typeof`, `delete` and `void` have
//! their own modules).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    fn unary_error(&mut self, op: &str, t: TyId, span: Span) -> hir::Expr {
        if !self.cx.ty.is_bottom(t) {
            let tn = self.cx.display(t);
            let mut d = Diagnostic::error(
                format!("cannot apply unary operator `{op}` to type `{tn}`"),
                span,
            );
            if op == "-" && self.cx.ty.is_int(t) {
                d = d.with_note("unsigned values cannot be negated");
            } else if matches!(self.cx.ty.kind(t), TyKind::Simd(_)) {
                d = d.with_note("this vector type has no lane operation for this operator");
            }
            self.cx.error(d);
        }
        self.error_expr(span)
    }

    pub(crate) fn unary(
        &mut self,
        op: ast::UnaryOp,
        operand: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if op == ast::UnaryOp::Void {
            return self.void_expr(operand, exp, span);
        }
        let exp = self.hint(exp);
        // A union expected (`string | i64`): its one number member.
        let exp = exp.map(|t| {
            self.union_member(t, |s, m| s.cx.ty.is_numeric(m))
                .unwrap_or(t)
        });
        let num_exp = exp.filter(|t| self.cx.ty.is_numeric(*t) || self.cx.ty.is_storage_float(*t));
        let (uop, inner) = match op {
            ast::UnaryOp::TypeOf => return self.typeof_value(operand, span),
            ast::UnaryOp::Delete => return self.delete_expr(operand, span),
            ast::UnaryOp::Void => unreachable!("ICE: `void` handled above"),
            ast::UnaryOp::Neg => {
                let int_exp = num_exp.is_some_and(|t| self.cx.ty.is_int(t));
                let inner = match &operand.kind {
                    // `-0` is a number unless an integer is expected: an integer has no `-0`,
                    // and JavaScript keeps its sign (`1 / -0` is `-Infinity`, #562).
                    ast::ExprKind::Lit(ast::Lit::Int {
                        value: 0,
                        suffix: None,
                    }) if !int_exp => {
                        let zero = ast::Lit::Float {
                            value: 0.0,
                            suffix: None,
                        };
                        self.lit(&zero, num_exp, operand.span, true)
                    }
                    ast::ExprKind::Lit(l @ ast::Lit::Int { .. }) => {
                        self.lit(l, num_exp, operand.span, true)
                    }
                    _ => self.payload_expr(operand, num_exp, Want::Borrow),
                };
                let inner = self.unbrand(inner);
                let inner = self.record_operand(inner);
                if self.cx.maybe_dynamic(inner.ty) {
                    return self.dynamic_unary(op, inner, span);
                }
                let inner = match self.unary_operator(op, inner, exp, span) {
                    Ok(h) => return h,
                    Err(inner) => inner,
                };
                self.literal_use_arith(&inner);
                let inner = self.unary_literal_operand(inner);
                // `-x` on an `f16`/`bf16` runs in `f32` (#2049); a negated literal stays a
                // literal of the type (`const w: f16 = -1.5`), which lowering rounds.
                let inner = match &inner.kind {
                    H::Lit(_) => inner,
                    _ => self.storage_widened(inner),
                };
                let t = inner.ty;
                let ok = self.cx.ty.is_bottom(t)
                    || self.cx.ty.is_storage_float(t)
                    || self.cx.ty.is_float(t)
                    || self.cx.ty.int_ty(t).is_some_and(|i| i.is_signed());
                if !ok {
                    return self.unary_error("-", t, span);
                }
                (hir::UnOp::Neg, inner)
            }
            ast::UnaryOp::Plus => {
                let mut inner = self.payload_expr(operand, num_exp, Want::Borrow);
                inner = self.unbrand(inner);
                inner = self.record_operand(inner);
                if self.cx.maybe_dynamic(inner.ty) {
                    return self.dynamic_unary(op, inner, span);
                }
                // `+n` with `n: 0 | 1` is the `number` it is (#3784).
                inner = self.unary_literal_operand(inner);
                let number =
                    self.cx.ty.is_numeric(inner.ty) || self.cx.ty.is_storage_float(inner.ty);
                if !number && !self.cx.ty.is_bottom(inner.ty) {
                    return self.unary_error("+", inner.ty, span);
                }
                // `+w` on an `f16`/`bf16` is the `number` it is (#2049).
                let mut inner = self.storage_widened(inner);
                inner.span = span;
                return inner;
            }
            ast::UnaryOp::Not => return self.not_expr(operand, span),
            ast::UnaryOp::BitNot => {
                let int_exp = exp.filter(|t| self.cx.ty.is_int(*t));
                let inner = self.payload_expr(operand, int_exp, Want::Borrow);
                let inner = self.unbrand(inner);
                let inner = self.record_operand(inner);
                if self.cx.maybe_dynamic(inner.ty) {
                    return self.dynamic_unary(op, inner, span);
                }
                let inner = match self.unary_operator(op, inner, exp, span) {
                    Ok(h) => return h,
                    Err(inner) => inner,
                };
                self.literal_use_number(&inner);
                let inner = self.unary_literal_operand(inner);
                let inner = self.storage_widened(inner);
                // `~x` on a number is JS's: ToInt32, then a 32-bit not (`int32.rs`).
                if self.js_bitnot_applies(&inner) {
                    return self.js_bitnot(inner, span);
                }
                if !self.cx.ty.is_int(inner.ty) && !self.cx.ty.is_bottom(inner.ty) {
                    return self.unary_error("~", inner.ty, span);
                }
                (hir::UnOp::BitNot, inner)
            }
        };
        let t = inner.ty;
        self.mk(
            H::Unary {
                op: uop,
                expr: Box::new(inner),
            },
            t,
            span,
        )
    }
}

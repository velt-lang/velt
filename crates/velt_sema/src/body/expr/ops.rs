//! Unary and binary operators (`!`, `&&` and `||` are in `truthiness`). `==`/`!=` on non-primitive types (objects, enums,
//! options, interface and function values, generic `T`, ...) is `Intrinsic::Same` (JS `===`:
//! objects by identity; `!=` wraps it in `Not`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::type_tests::LitEq;
use crate::body::narrow::{is_null, typeof_compare};
use crate::body::{FnCx, Want};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, TyId, TyKind};

/// Literal-ish expression whose type is decided by context (`1`, `-2.5`, `(1 + 2)`).
pub(crate) fn untyped(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(
            ast::Lit::Int { suffix: None, .. } | ast::Lit::Float { suffix: None, .. },
        ) => true,
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg | ast::UnaryOp::Plus | ast::UnaryOp::BitNot,
            expr,
        } => untyped(expr),
        ast::ExprKind::Paren(x) => untyped(x),
        ast::ExprKind::Binary { op, lhs, rhs } => {
            !is_comparison(*op) && untyped(lhs) && untyped(rhs)
        }
        _ => false,
    }
}

pub(crate) fn is_comparison(op: ast::BinaryOp) -> bool {
    use ast::BinaryOp as B;
    matches!(
        op,
        B::Eq | B::NotEq | B::Lt | B::LtEq | B::Gt | B::GtEq | B::And | B::Or | B::Nullish
    )
}

pub(crate) fn op_str(op: ast::BinaryOp) -> &'static str {
    use ast::BinaryOp as B;
    match op {
        B::Add => "+",
        B::Sub => "-",
        B::Mul => "*",
        B::Div => "/",
        B::Rem => "%",
        B::Pow => "**",
        B::Eq => "==",
        B::NotEq => "!=",
        B::Lt => "<",
        B::LtEq => "<=",
        B::Gt => ">",
        B::GtEq => ">=",
        B::And => "&&",
        B::Or => "||",
        B::Nullish => "??",
        B::BitAnd => "&",
        B::BitOr => "|",
        B::BitXor => "^",
        B::Shl => "<<",
        B::Shr => ">>",
        B::UShr => ">>>",
        B::In => "in",
    }
}

pub(crate) fn hir_binop(op: ast::BinaryOp) -> Option<BinOp> {
    use ast::BinaryOp as B;
    Some(match op {
        B::Add => BinOp::Add,
        B::Sub => BinOp::Sub,
        B::Mul => BinOp::Mul,
        B::Div => BinOp::Div,
        B::Rem => BinOp::Rem,
        B::Pow => BinOp::Pow,
        B::Eq => BinOp::Eq,
        B::NotEq => BinOp::NotEq,
        B::Lt => BinOp::Lt,
        B::LtEq => BinOp::LtEq,
        B::Gt => BinOp::Gt,
        B::GtEq => BinOp::GtEq,
        B::BitAnd => BinOp::BitAnd,
        B::BitOr => BinOp::BitOr,
        B::BitXor => BinOp::BitXor,
        B::Shl => BinOp::Shl,
        B::Shr => BinOp::Shr,
        B::UShr => BinOp::UShr,
        B::And | B::Or | B::Nullish | B::In => return None,
    })
}

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
        let exp = self.hint(exp);
        let num_exp = exp.filter(|t| self.cx.ty.is_numeric(*t));
        let (uop, inner) = match op {
            ast::UnaryOp::TypeOf => return self.typeof_value(operand, span),
            ast::UnaryOp::Delete => return self.delete_expr(operand, span),
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
                    _ => self.expr(operand, num_exp, Want::Borrow),
                };
                let inner = self.unbrand(inner);
                let t = inner.ty;
                let ok = self.cx.ty.is_bottom(t)
                    || self.cx.ty.is_float(t)
                    || self.cx.ty.int_ty(t).is_some_and(|i| i.is_signed());
                if !ok {
                    return self.unary_error("-", t, span);
                }
                (hir::UnOp::Neg, inner)
            }
            ast::UnaryOp::Plus => {
                let mut inner = self.expr(operand, num_exp, Want::Borrow);
                inner = self.unbrand(inner);
                if !self.cx.ty.is_numeric(inner.ty) && !self.cx.ty.is_bottom(inner.ty) {
                    return self.unary_error("+", inner.ty, span);
                }
                inner.span = span;
                return inner;
            }
            ast::UnaryOp::Not => return self.not_expr(operand, span),
            ast::UnaryOp::BitNot => {
                let int_exp = exp.filter(|t| self.cx.ty.is_int(*t));
                let inner = self.expr(operand, int_exp, Want::Borrow);
                let inner = self.unbrand(inner);
                // `~x` on a number is JS's: ToInt32, then a 32-bit not (`int32.rs`).
                if self.js_bitnot_applies(&inner, int_exp) {
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

    /// Check both operands, letting a typed operand decide the type of an untyped literal one.
    pub(crate) fn operands(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        hint: Option<TyId>,
        want: Want,
    ) -> (hir::Expr, hir::Expr) {
        let pick = |s: &Self, first: &hir::Expr| {
            if s.cx.ty.is_bottom(first.ty) {
                hint
            } else {
                Some(first.ty)
            }
        };
        if untyped(lhs) && !untyped(rhs) {
            let r = self.expr(rhs, hint, want);
            let h = pick(self, &r);
            let l = self.expr(lhs, h, want);
            (l, r)
        } else {
            let l = self.expr(lhs, hint, want);
            let h = pick(self, &l);
            let r = self.expr(rhs, h, want);
            (l, r)
        }
    }

    pub(crate) fn binary(
        &mut self,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        use ast::BinaryOp as B;
        match op {
            B::And | B::Or => return self.logical(op, lhs, rhs, exp, span),
            B::In => return self.private_in(lhs, rhs, span),
            B::Nullish => return self.nullish(lhs, rhs, exp, span),
            B::Eq | B::NotEq if is_null(rhs) || is_null(lhs) => {
                let other = if is_null(rhs) { lhs } else { rhs };
                return self.null_compare(other, op == B::Eq, span);
            }
            B::Eq | B::NotEq => {
                if let Some((x, tag)) = typeof_compare(lhs, rhs) {
                    return self.typeof_test(x, tag, op == B::NotEq, span);
                }
            }
            _ => {}
        }
        let hint = if is_comparison(op) {
            None
        } else {
            self.hint(exp)
                .filter(|t| self.cx.ty.is_numeric(*t) || *t == self.cx.ty.str_)
        };
        let lit_eq = matches!(op, B::Eq | B::NotEq)
            .then(|| self.literal_eq(lhs, rhs, op == B::NotEq, span))
            .flatten();
        let (l, r) = match lit_eq {
            Some(LitEq::Done(h)) => return h,
            Some(LitEq::Operands(l, r)) => (l, r),
            None => self.operands(lhs, rhs, hint, Want::Borrow),
        };
        // Branded values are operands as their primitives.
        let (l, r) = (self.unbrand(l), self.unbrand(r));
        let (l, r) = if matches!(op, B::Eq | B::NotEq) {
            let (l, r) = self.nullable_operands(l, r);
            self.identity_operands(l, r)
        } else {
            (l, r)
        };
        // Literal types take part in operators as their base type (`c.kind + "!"`).
        let (l, r) = if l.ty != r.ty || self.cx.lit_value(l.ty).is_some() {
            (self.widen_value(l), self.widen_value(r))
        } else {
            (l, r)
        };
        // Bitwise operators on numbers: JS's 32-bit semantics (`int32.rs`).
        if let Some(bop) = self.js_bitwise_applies(op, &l, &r, hint) {
            return self.js_bitwise(bop, l, r, span);
        }
        let (l, r) = self.mix_numbers(l, r);
        let (l, r) = self.mix_ints(l, r);
        let (l, r) = self.bitwise_int32(op, l, r);
        let bop = hir_binop(op).expect("ICE: logical op in binary");
        if let Some(found) = self.param_ordering(bop, l.ty, r.ty) {
            return self.compare_via(found, bop, l, r, span);
        }
        let Some(t) = self.check_operands(op, l.ty, &r, span) else {
            return self.error_expr(span);
        };
        if op == B::Add && t == self.cx.ty.str_ {
            return self.concat(l, r, span);
        }
        if matches!(op, B::Eq | B::NotEq) && !self.primitive_eq(t) {
            return self.structural_eq(l, r, op == B::NotEq, span);
        }
        if op == B::Div {
            return self.divide(l, r, t, hint, span);
        }
        let ty = if is_comparison(op) {
            self.cx.ty.bool_
        } else {
            t
        };
        self.mk(
            H::Binary {
                op: bop,
                lhs: Box::new(l),
                rhs: Box::new(r),
            },
            ty,
            span,
        )
    }

    /// `a === b` where one side is `T | null` and the other a `T` (#264): the `T` side
    /// converts to `T | null` (as at any typed position), so the comparison is the one two
    /// `T | null` values get: identity for classes, by value for primitives, `false` for `null`
    /// against a value. Operands that do not convert are left for the mismatch report.
    pub(crate) fn nullable_operands(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
    ) -> (hir::Expr, hir::Expr) {
        let t = &self.cx.ty;
        let nullable = |x: TyId| t.opt_payload(x).is_some();
        if nullable(l.ty) == nullable(r.ty) || t.is_bottom(l.ty) || t.is_bottom(r.ty) {
            return (l, r);
        }
        if nullable(l.ty) {
            let to = l.ty;
            let r = self.try_coerce(r, to).unwrap_or_else(|r| r);
            (l, r)
        } else {
            let to = r.ty;
            let l = self.try_coerce(l, to).unwrap_or_else(|l| l);
            (l, r)
        }
    }

    /// `a === b` between an interface value and a class or struct value (or a base and a
    /// subclass value) whose types overlap, as TypeScript allows (#365): the side that converts to the other's type
    /// does (an interface value points at the object itself), so the two compare by identity.
    fn identity_operands(&mut self, l: hir::Expr, r: hir::Expr) -> (hir::Expr, hir::Expr) {
        let object = |s: &Self, t: TyId| {
            let t = s.cx.ty.opt_payload(t).unwrap_or(t);
            s.cx.union_def(t).is_none()
                && matches!(s.cx.ty.kind(t), TyKind::Adt(..) | TyKind::Dyn(..))
        };
        if l.ty == r.ty || !object(self, l.ty) || !object(self, r.ty) {
            return (l, r);
        }
        let to = l.ty;
        match self.try_coerce(r, to) {
            Ok(r) => (l, r),
            Err(r) => {
                let to = r.ty;
                let l = self.try_coerce(l, to).unwrap_or_else(|l| l);
                (l, r)
            }
        }
    }

    pub(crate) fn primitive_eq(&self, t: TyId) -> bool {
        let ty = &self.cx.ty;
        ty.is_numeric(t) || t == ty.bool_ || t == ty.str_ || t == ty.never
    }

    /// `a == b` on non-primitive types → `Intrinsic::Same(a, b)` (both borrowed).
    pub(crate) fn structural_eq(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let b = self.cx.ty.bool_;
        let eq = self.intrinsic(Intrinsic::Same, vec![l, r], b, span);
        if !negate {
            return eq;
        }
        self.mk(
            H::Unary {
                op: hir::UnOp::Not,
                expr: Box::new(eq),
            },
            b,
            span,
        )
    }

    /// Can `==` compare values of `t`? Interface and function values compare by identity
    /// (#365): the object behind an interface value, and the function value itself (each
    /// evaluation of an arrow is a new one, as in JS).
    fn equatable(&self, t: TyId) -> bool {
        !matches!(self.cx.ty.kind(t), TyKind::Unit)
    }

    /// Validate `lhs op rhs`; returns the (common) operand type, or None after reporting an error.
    pub(crate) fn check_operands(
        &mut self,
        op: ast::BinaryOp,
        lt: TyId,
        r: &hir::Expr,
        span: Span,
    ) -> Option<TyId> {
        use ast::BinaryOp as B;
        let rt = r.ty;
        if lt == self.cx.ty.error || rt == self.cx.ty.error {
            return None;
        }
        let t = if lt == self.cx.ty.never { rt } else { lt };
        if !self.compatible(t, rt) {
            let (e, f) = (self.cx.display(t), self.cx.display(rt));
            let mut d = Diagnostic::error("mismatched types", r.span)
                .with_note(format!("expected {e}, found {f}"));
            if op == B::Add && (t == self.cx.ty.str_ || rt == self.cx.ty.str_) {
                d = d.with_note(
                    "use a template literal to combine strings with other values: `${a}${b}`",
                );
            }
            self.cx.error(d);
            return None;
        }
        if t == self.cx.ty.never {
            return Some(t);
        }
        let ty = &self.cx.ty;
        let ok = match op {
            B::Add => ty.is_numeric(t) || t == ty.str_,
            B::Sub | B::Mul | B::Div | B::Rem | B::Pow => ty.is_numeric(t),
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr | B::UShr => ty.is_int(t),
            B::Eq | B::NotEq => self.equatable(t),
            B::Lt | B::LtEq | B::Gt | B::GtEq => ty.is_numeric(t) || t == ty.str_,
            B::And | B::Or | B::Nullish | B::In => false,
        };
        if !ok {
            let tn = self.cx.display(t);
            self.cx.err(
                format!(
                    "cannot apply binary operator `{}` to type `{tn}`",
                    op_str(op)
                ),
                span,
            );
            return None;
        }
        Some(t)
    }
}

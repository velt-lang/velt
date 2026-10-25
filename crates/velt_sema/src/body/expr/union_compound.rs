//! `x op= v` on a local of a union type narrowed to one member (`x += "!"` after
//! `typeof x === "string"` on a `number | string`): `x = x op v`, with `x` read as the member it
//! is narrowed to and the result converted back to the union, as TypeScript checks it.

use velt_common::Span;
use velt_syntax::ast;

use super::ops::hir_binop;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H};

impl FnCx<'_, '_> {
    /// `place op= value` when `place` is a variable of a union type that is narrowed here, else
    /// `None` (nothing checked yet). Logical operators are not handled here.
    pub(super) fn union_local_compound(
        &mut self,
        place: &hir::Expr,
        op: ast::BinaryOp,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let H::Local(l, _) = place.kind else {
            return None;
        };
        let lty = place.ty;
        if self.cx.union_def(lty).is_none() || hir_binop(op).is_none() {
            return None;
        }
        if !self
            .f
            .scopes
            .iter()
            .any(|s| s.members.iter().any(|(x, _)| *x == l))
        {
            return None;
        }
        let cur = self.expr(target, None, Want::Borrow);
        if cur.ty == lty || self.cx.ty.is_bottom(cur.ty) {
            // Not narrowed to one member: the operands are checked as written below.
            return None;
        }
        let v = self.expr(value, Some(cur.ty), Want::Borrow);
        let r = self.binary_checked(op, cur, v, span);
        if self.cx.ty.is_bottom(r.ty) {
            return Some(r);
        }
        let r = self.coerce(r, lty);
        self.check_capture_assign(l, target.span);
        self.unnarrow(l);
        self.narrow_to_assigned(l, lty, &r);
        let kind = H::Assign {
            place: Box::new(place.clone()),
            value: Box::new(self.held_rechecked(r)),
        };
        let unit = self.cx.ty.unit;
        Some(self.mk(kind, unit, span))
    }
}

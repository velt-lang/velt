//! Ordering operators on `Comparable` values: with `T extends Comparable<T>` (the prelude's
//! builtin ordering interface, directly or through an interface extending it), or a class or
//! struct implementing it (`Date`, `DateTime`), `a < b` is `a.compareTo(b) < 0` — a
//! `Callee::ParamMethod` call with both operands borrowed, compared with `0` (likewise `<=`, `>`,
//! `>=`). No HIR node of its own.

use velt_common::Span;

use crate::body::places::set_place_mode;
use crate::body::FnCx;
use crate::defs::Bound;
use crate::hir::{self, BinOp, Callee, ExprKind as H, Lit, PassMode, TyId, TyKind, UseMode};
use crate::known::COMPARE_TO;

impl FnCx<'_, '_> {
    /// The `Comparable<t>` bound of `t` (a type param, class or struct) and its `compareTo` slot,
    /// if `t` has it. Numbers and strings compare with their own operators.
    fn comparable_slot(&mut self, t: TyId) -> Option<(Bound, u32)> {
        if !matches!(self.cx.ty.kind(t), TyKind::Param(_) | TyKind::Adt(..)) {
            return None;
        }
        let iface = self.cx.comparable_iface()?;
        let b = Bound {
            iface,
            args: vec![t],
        };
        if !self.cx.satisfies(t, &b, &self.bounds) {
            return None;
        }
        let slot = self
            .cx
            .iface(iface)?
            .methods
            .iter()
            .position(|m| m.name == COMPARE_TO)?;
        Some((b, slot as u32))
    }

    /// Is `lt op rt` an ordering on a Comparable type? Its bound and `compareTo` slot.
    pub(super) fn param_ordering(&mut self, op: BinOp, lt: TyId, rt: TyId) -> Option<(Bound, u32)> {
        let ordering = matches!(op, BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq);
        if !ordering || lt != rt {
            return None;
        }
        self.comparable_slot(lt)
    }

    /// `l op r` as `l.compareTo(r) op 0` through the bound found by [`Self::param_ordering`].
    pub(super) fn compare_via(
        &mut self,
        (b, slot): (Bound, u32),
        op: BinOp,
        l: hir::Expr,
        mut r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let recv = self.receiver(l, None, PassMode::Borrow, false);
        set_place_mode(&mut r, UseMode::Borrow);
        let (i64_, bool_) = (self.cx.ty.i64, self.cx.ty.bool_);
        let callee = Callee::ParamMethod {
            iface: b.iface,
            iface_args: b.args,
            slot,
            method_type_args: vec![],
        };
        let call = self.mk(
            H::Call {
                callee,
                args: vec![recv, r],
            },
            i64_,
            span,
        );
        let zero = self.mk(H::Lit(Lit::Int(0)), i64_, span);
        let cmp = H::Binary {
            op,
            lhs: Box::new(call),
            rhs: Box::new(zero),
        };
        self.mk(cmp, bool_, span)
    }
}

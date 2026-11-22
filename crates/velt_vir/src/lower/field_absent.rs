//! `delete o.a` of an optional field (sema `body::expr::delete_optional`): an assignment of
//! `Intrinsic::FieldAbsent`, which leaves a `presence` field absent: its value the option's
//! nothing and its flag clear, where any other write sets the flag (`store_assigned_in`).

use velt_sema::hir::{self, TyId};

use super::FnLower;
use crate::vir::{Const, Operand, Place, Rvalue, Ty};

/// Is `value` `Intrinsic::FieldAbsent`?
fn is_field_absent(value: &hir::Expr) -> bool {
    matches!(
        &value.kind,
        hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::FieldAbsent),
            ..
        }
    )
}

impl FnLower<'_, '_> {
    /// `store_assigned` of `value`, then for `Intrinsic::FieldAbsent` the field's presence flag
    /// cleared.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn store_assigned_absent(
        &mut self,
        value: &hir::Expr,
        place: &hir::Expr,
        p: Place,
        v: Operand,
        ty: TyId,
        shared: bool,
        drops: bool,
    ) {
        let flag = match is_field_absent(value) {
            true => self.presence_place(place, &p),
            false => None,
        };
        self.store_assigned(place, p, v, ty, shared, drops);
        if let Some(fp) = flag {
            self.assign(
                fp,
                Rvalue::Use(Operand::Const(Const::Bool(false), Ty::Bool)),
            );
        }
    }
}

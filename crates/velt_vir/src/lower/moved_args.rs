//! Arguments that are places moved into a call whose parameter only borrows: the callee does
//! not own them, so the caller does, and drops them after the call.

use velt_sema::hir::{self, UseMode};

use super::FnLower;
use crate::vir::Operand;

impl FnLower<'_, '_> {
    /// An argument that is a place moved into the call ([`is_moved`]), as it is or converted
    /// to a type with the same representation (a function value at another instance of its
    /// generic type): the caller owns it, and drops it after the call.
    pub(super) fn moved_arg(&mut self, a: &hir::Expr) -> bool {
        match &a.kind {
            hir::ExprKind::Upcast(inner) => {
                let (from, to) = (self.sub(inner.ty), self.sub(a.ty));
                self.upcast_slot(from, to).is_none()
                    && !self.part_copy(from, to)
                    && self.moved_arg(inner)
            }
            _ => is_moved(a),
        }
    }

    /// The moved argument `a` in a temporary the caller owns, dropped after the call.
    pub(super) fn own_moved_arg(&mut self, a: &hir::Expr) -> Operand {
        let v = self.consume(a);
        let ty = self.sub(a.ty);
        self.own_value(v, ty)
    }
}

/// A place moved into the call (the caller drops it after the call).
pub(super) fn is_moved(a: &hir::Expr) -> bool {
    if let hir::ExprKind::Downcast(x) = &a.kind {
        return is_moved(x);
    }
    matches!(
        a.kind,
        hir::ExprKind::Local(_, UseMode::Move)
            | hir::ExprKind::Field {
                mode: UseMode::Move,
                ..
            }
            | hir::ExprKind::UnwrapSome(_, UseMode::Move)
            | hir::ExprKind::UnwrapVariant {
                mode: UseMode::Move,
                ..
            }
    )
}

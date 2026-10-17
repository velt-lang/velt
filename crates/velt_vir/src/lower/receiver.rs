//! Interface method receivers held for the call (#1188). A method borrows its receiver (`this`),
//! and a receiver read from a place another reference reaches (a module `let`, a field of a
//! counted object, an element, a variable in a shared cell) can be replaced there while the
//! method runs: `cur.greet()` where `greet` calls `swap()`, which assigns `cur = other`.
//! JavaScript keeps running the method on the object it was called on, so such a receiver gets
//! a reference of its own for the call (a share: the class or the interface value's data is
//! counted), dropped after the statement. The method is chosen at run time, so the receiver is
//! held whenever it is read from such a place (`FnLower::callee_replaceable`). Virtual and direct
//! method calls stabilize their receiver like an argument instead (`stable_borrow`).
//!
//! A receiver held in a local, a parameter or `this` is reached by no other reference while the
//! call runs, and is passed as before.

use velt_sema::hir::{self, TyKind};

use super::FnLower;
use crate::vir::Operand;

impl FnLower<'_, '_> {
    /// Is the receiver `recv` of a method call read from a place the method may replace, as a
    /// class object or an interface value a share keeps alive?
    pub(super) fn replaceable_receiver(&mut self, recv: &hir::Expr) -> bool {
        let ty = self.sub(recv.ty);
        let shareable = self.cx.is_class(ty) || matches!(self.cx.kind(ty), TyKind::Dyn(..));
        shareable && !super::call::is_moved(recv) && self.callee_replaceable(recv)
    }

    /// The receiver `recv` with a reference of its own for the call, dropped after the
    /// statement.
    pub(super) fn held_receiver(&mut self, recv: &hir::Expr) -> Operand {
        let ty = self.sub(recv.ty);
        let v = self.expr(recv);
        let s = self.share_value(v, ty);
        self.own_value(s, ty)
    }
}

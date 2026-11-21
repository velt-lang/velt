//! The value an object literal implementing an interface gives one of its method slots
//! (`iface_object`), and what a closure written there throws.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, TyId};

impl FnCx<'_, '_> {
    /// The value `e` written for slot `slot` of `iface` (expected type `exp`).
    pub(super) fn slot_value(
        &mut self,
        e: &ast::Expr,
        exp: TyId,
        (iface, slot): (DefId, usize),
    ) -> hir::Expr {
        let at = Some((iface, slot as u32, e.span));
        let saved = std::mem::replace(&mut self.literal_slot, at);
        // A named function taking optional parameters past the method's (`claimBase:
        // claimAppBase` with `claimAppBase(base, config = …)`), or wider ones: adapted as a
        // property of a function type is (`fn_widen`).
        let h = match self.adapted_fn_value(e, exp) {
            Some(h) => h,
            None => self.expr(e, Some(exp), Want::Move),
        };
        self.literal_slot = saved;
        self.bind_slot_result(Some(e), h, (iface, slot))
    }

    /// A closure written at `span` throws what `srcs` throw: for the interface slot it fills, if
    /// it is a value of an object literal implementing an interface ([`Self::slot_value`]).
    pub(super) fn provisional_slot(&mut self, span: Span, srcs: &[crate::defs::ThrowSrc]) {
        if let Some((iface, slot, _)) = self.literal_slot.filter(|l| l.2 == span) {
            self.cx.provisional_slots.push((iface, slot, srcs.to_vec()));
        }
    }
}

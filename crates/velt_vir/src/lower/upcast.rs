//! Interface upcasts (hir_encodings.md "Interface upcasts"): an `Upcast` of an interface value
//! `Dyn(A)` to `Dyn(B)`, another interface whose members `A` has, is the same data pointer with
//! the `B` table of the value's concrete type. Each table of `A` holds the tables of its
//! targets after its method slots (`Program::upcasts`, in that order), so the conversion is a
//! load: no copy, and the value stays the same object (`===`).

use velt_sema::hir::{TyId, TyKind};

use super::operand::proj;
use super::FnLower;
use crate::vir::{Operand, Proj};

impl FnLower<'_, '_> {
    /// The table slot holding the `to` table of an `A` table, when `from` and `to` are values
    /// of two interfaces and `A` converts to `B`.
    pub(super) fn upcast_slot(&mut self, from: TyId, to: TyId) -> Option<i128> {
        let (TyKind::Dyn(a, _), TyKind::Dyn(b, _)) = (self.cx.kind(from), self.cx.kind(to)) else {
            return None;
        };
        if a == b {
            return None;
        }
        let k = self.cx.hir.upcasts.get(&a)?.iter().position(|&t| t == b)?;
        let n = self.cx.upcast_base(a);
        Some(n + k as i128)
    }

    /// The owned interface value `v` (type `from`) as a `to` through table slot `slot`.
    pub(super) fn upcast_value(&mut self, v: Operand, from: TyId, to: TyId, slot: i128) -> Operand {
        self.cx.note_upcast(from, to);
        let p = self.place_of(v, from);
        let data = Operand::Copy(proj(&p, Proj::Field(0)));
        let table = Operand::Copy(proj(&p, Proj::Field(1)));
        let t = self.dispatch(table, slot);
        self.build_agg(to, vec![data, t])
    }
}

impl super::Cx<'_> {
    /// Where an interface's tables hold their upcast targets: after its slots (the number of
    /// methods of its implementations, which all have the same).
    pub(super) fn upcast_base(&self, iface: velt_sema::hir::DefId) -> i128 {
        self.hir
            .impls
            .iter()
            .find(|i| i.iface == iface)
            .map_or(0, |i| i.methods.len() as i128)
    }
}

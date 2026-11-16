//! Fields held out of line (`hir::FieldDef::cell`): a field a cast to an interface extension added
//! to an object type (velt_sema `object_widen`) is stored as `C | null`, a pointer to a class `C`
//! whose one field holds the value, made on the first write. Reflection (printing, keys, `in`,
//! spreads, `any` reads and writes) sees such a field as an optional field of the cell's field's
//! type, present while the cell is there.

use velt_sema::hir::{TyId, TyKind};

use super::{Cx, FnLower};
use crate::vir::{Operand, Place};

impl Cx<'_> {
    /// For field `index` of object type `ty` held out of line: the cell's class type and the
    /// value's type.
    pub(super) fn cell_field(&mut self, ty: TyId, index: u32) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, _) = self.kind(ty) else {
            return None;
        };
        let def = self.adt_def(d);
        if !def.fields.get(index as usize)?.cell {
            return None;
        }
        let slot = *self.adt_field_tys(ty).get(index as usize)?;
        let TyKind::Option(cell, _) = self.kind(slot) else {
            return None;
        };
        let value = *self.adt_field_tys(cell).first()?;
        Some((cell, value))
    }
}

impl FnLower<'_, '_> {
    /// Write the `any` `v` into field `index` of the object at `vp` (of type `ty`), held out of
    /// line in a cell of type `cell` (the slot's type `slot`, the value's `value`): into the
    /// cell while it is there, else into a new one. Returns the status (`any_set.rs`).
    pub(super) fn any_set_cell(
        &mut self,
        v: crate::vir::Local,
        vp: &Place,
        ty: TyId,
        index: u32,
        (slot, cell, value): (TyId, TyId, TyId),
    ) {
        if !self.cx.any_unboxable(value) {
            return self.any_set_refuse(v, super::any_member::ST_UNSUPPORTED);
        }
        let val = self.any_set_value(v, value);
        let fp = self.field_place(vp, ty, index);
        let present = self.option_is_some(&fp, slot);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(present, yes, no);
        self.switch_to(yes);
        let cp = self.cell_value_place(&fp, slot, cell);
        self.any_replace(cp, val.clone(), value);
        self.ret_status(super::any_member::ST_OK);
        self.switch_to(no);
        let obj = self.alloc_object_raw(cell);
        let p = self.field_place(&obj, cell, 0);
        self.store(p, val);
        self.assign(fp, crate::vir::Rvalue::Use(Operand::Copy(obj)));
        self.ret_status(super::any_member::ST_OK);
    }

    /// The value in the cell of a field held out of line, whose slot (of type `slot`, `cell |
    /// null`) is at `fp`; the slot must not be `null`.
    pub(super) fn cell_value_place(&mut self, fp: &Place, slot: TyId, cell: TyId) -> Place {
        let ptr = self.some_payload(fp, slot);
        self.field_place(&ptr, cell, 0)
    }
}

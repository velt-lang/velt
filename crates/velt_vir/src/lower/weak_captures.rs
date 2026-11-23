//! Captures of `weak` variables (`LocalDef::weak` on a closure's capture local; weak_fields.rs,
//! docs/internals/design/cycles.md "`weak T`"). The captured variable's storage holds its target's
//! slot, so a capture holds the slot too, never the target:
//!
//! - a borrowed capture is the variable's address, as for any variable, and the closure reads and
//!   writes the slot word there;
//! - a capture by value copies the slot word into the environment and takes a reference of its own
//!   to the slot ([`weak_slot_retain`](FnLower::weak_slot_retain)), so the variable keeps its own:
//!   both read the same target, or `null` once it is gone;
//! - a captured variable's cell holds the slot word (cells.rs), and its last release gives the slot
//!   up instead of dropping a value.
//!
//! The environment's drop gives its slots up, its clone takes one more reference to each, its
//! trace glue and the cycle analyses see no edge, and its transfer panics: the slot belongs to the
//! weak core of the task that set it, as a `weak` field's does (sema `weak_threads` refuses the
//! visible cases).

use velt_sema::hir::{DefId, PassMode, TyId};

use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The captures of closure `def` that are `weak` variables taken by value or in a cell (the
    /// environment owns a reference to the slot or the cell): env field, read type `T | null`,
    /// and whether the field holds a cell.
    pub(super) fn weak_env_captures(&mut self, def: DefId) -> Vec<(u32, TyId, bool)> {
        let f = self.cx.fn_def(def);
        let header = self.cx.env_header(def);
        let mut out = vec![];
        for (k, c) in f.captures.iter().enumerate() {
            let l = &f.body.locals[c.inner.0 as usize];
            if l.weak && matches!(c.mode, PassMode::Copy | PassMode::Owned) {
                let (cell, ty) = (l.boxed, l.ty);
                let ty = self.sub(ty);
                out.push((header + k as u32, ty, cell));
            }
        }
        out
    }

    /// The name of a `weak` variable closure `def` captures by value or in a cell, if any.
    pub(super) fn weak_capture_name(&mut self, def: DefId) -> Option<String> {
        let f = self.cx.fn_def(def);
        f.captures
            .iter()
            .map(|c| &f.body.locals[c.inner.0 as usize])
            .find(|l| l.weak)
            .map(|l| l.name.clone())
    }

    /// One more reference to the slot in the weak word `w` (a slot pointer, 0, or with a proxy's
    /// bit when the target of read type `read` is proxiable): its count, inline. A slot is used by
    /// one task only, so its count changes without the lock (std/lang/hooks/weak_slots.velt
    /// `slotDrop`).
    pub(super) fn weak_slot_retain(&mut self, w: Operand, read: TyId) {
        let Some(t) = self.cx.weak_target(read) else {
            return;
        };
        let w = self.as_usize(w);
        let s = match self.cx.proxiable(t) {
            true => self.word_and(w, -2),
            false => w,
        };
        let (live, join) = (self.new_block(), self.new_block());
        let some = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, s.clone(), cint(0, Ty::U64)),
        );
        self.branch(some, live, join);
        self.switch_to(live);
        let sp = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(s, Ty::Ptr));
        let spp = self.operand_place(sp, Ty::Ptr);
        // `S_COUNT`, the slot's first word.
        let count = proj(&spp, Proj::Deref(Ty::U64));
        let more = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(count.clone()), cint(1, Ty::U64)),
        );
        self.assign(count, Rvalue::Use(more));
        self.goto(join);
        self.switch_to(join);
    }

    /// The `weak` variable whose slot word is at `from` (read type `read`) captured by value into
    /// the environment field `to`: the word, and a reference of the field's own to the slot.
    pub(super) fn capture_weak(&mut self, from: Place, to: Place, read: TyId) {
        let w = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(from)));
        self.assign(to, Rvalue::Use(w.clone()));
        self.weak_slot_retain(w, read);
    }

    /// The environment at `base` of closure `def` is dropped: each `weak` capture gives up its slot
    /// or its reference to the variable's cell.
    pub(super) fn drop_weak_captures(&mut self, base: &Place, def: DefId) {
        for (field, read, cell) in self.weak_env_captures(def) {
            let fp = proj(base, Proj::Field(field));
            match cell {
                true => self.release_weak_cell_ptr(Operand::Copy(fp), read),
                false => self.weak_field_drop(fp, read),
            }
        }
    }

    /// The environment at `dst`, copied bitwise from closure `def`'s: each `weak` capture takes one
    /// more reference to its slot or to the variable's cell.
    pub(super) fn clone_weak_captures(&mut self, dst: &Place, def: DefId) {
        for (field, read, cell) in self.weak_env_captures(def) {
            let fp = Operand::Copy(proj(dst, Proj::Field(field)));
            match cell {
                true => self.retain_cell(fp),
                false => self.weak_slot_retain(fp, read),
            }
        }
    }

    /// Capture `slot` of a generator or async closure's environment, a `weak` variable taken by
    /// value (read type `read`), for its state: the word, with a reference of the state's own.
    pub(super) fn take_weak_capture(&mut self, slot: Place, read: TyId) -> Operand {
        let w = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(slot)));
        self.weak_slot_retain(w.clone(), read);
        w
    }
}

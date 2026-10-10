//! Shared cells of captured variables (semantics stage 2, docs/design/semantics-stage2.md §5,
//! `LocalDef::boxed`): such a variable is a counted heap block holding its value. Its VIR local
//! holds the cell pointer and is *indirect*, so every read and write goes through the cell; an
//! escaping closure capturing it stores the cell pointer in its environment (one more reference)
//! and reads through it too. The function's own reference is released at scope exit (the value
//! is dropped with the last reference); reassigning the variable drops the old value in place.
//!
//! With `LowerOptions::cell_checks` (debug builds by a debug compiler) every cell's creation,
//! count change, hand-over to another task and free, and every call of a closure assigning a
//! captured cell, is also reported to the debug runtime (`velt_rt_cell_*`), which aborts when
//! two tasks use one cell (#916). Without it the generated code is unchanged.

use velt_sema::hir::{self, FnDef, PassMode};

use super::rt::Rt;
use super::FnLower;
use crate::vir::{Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// A new cell for the (declared, not yet initialized) cell local `id`, holding the
    /// all-zero value (which drops as nothing) until it is assigned.
    pub(super) fn new_cell(&mut self, id: hir::LocalId) {
        let info = &self.info[id.0 as usize];
        let (Some(l), ty) = (info.vir, info.ty) else {
            return;
        };
        let vt = self.cx.ty(ty);
        let c = self.counted_alloc(vt);
        let name = self.locals[l.0 as usize].name.clone();
        self.cell_check_new(c.clone(), name.as_deref());
        self.assign(Place::local(l), crate::vir::Rvalue::Use(c));
        let zero = self.zero_value(vt);
        let p = self.local_place(id);
        self.assign(p, crate::vir::Rvalue::Use(zero));
    }

    /// Release the function's reference to the cell of local `id`.
    pub(super) fn release_cell(&mut self, id: hir::LocalId) {
        let info = &self.info[id.0 as usize];
        let (Some(l), ty) = (info.vir, info.ty) else {
            return;
        };
        self.release_cell_ptr(Operand::Copy(Place::local(l)), ty);
    }

    /// Release one reference to the cell `ptr` holding a value of type `ty`.
    pub(super) fn release_cell_ptr(&mut self, ptr: Operand, ty: hir::TyId) {
        let vt = self.cx.ty(ty);
        let q = ptr.clone();
        self.cell_check(Rt::CellUse, vec![ptr.clone()]);
        self.release(ptr.clone(), |lw| {
            let p = lw.operand_place(q.clone(), Ty::Ptr);
            let value = super::operand::proj(&p, crate::vir::Proj::Deref(vt));
            lw.drop_glue(value, ty);
            lw.cell_check(Rt::CellFree, vec![q.clone()]);
            lw.counted_free(q, vt);
        });
    }

    /// One more reference to the cell `ptr`.
    pub(super) fn retain_cell(&mut self, ptr: Operand) {
        self.cell_check(Rt::CellUse, vec![ptr.clone()]);
        self.retain(ptr);
    }

    /// Report `r` on a cell to the debug runtime (module docs); nothing without `cell_checks`.
    pub(super) fn cell_check(&mut self, r: Rt, args: Vec<Operand>) {
        if self.cx.cell_checks && !self.dead() {
            self.call_rt(r, args, None);
        }
    }

    /// Report the new cell `ptr` of variable `name` to the debug runtime.
    fn cell_check_new(&mut self, ptr: Operand, name: Option<&str>) {
        if !self.cx.cell_checks || self.dead() {
            return;
        }
        let name = self.str_lit(name.unwrap_or("?"));
        let at = self.operand_addr(name, Ty::Agg(crate::vir::STR_AGG));
        self.call_rt(Rt::CellNew, vec![ptr, at], None);
    }

    /// A param that is a cell: its incoming value moves (or, when borrowed, is shared) into a
    /// new cell, which the function owns from here on.
    pub(super) fn box_param(&mut self, f: &FnDef, p: &hir::Param) {
        let i = p.local.0 as usize;
        if !f.body.locals[i].boxed || self.info[i].vir.is_none() {
            return;
        }
        let ty = self.info[i].ty;
        let vt = self.cx.ty(ty);
        let v = Operand::Copy(self.local_place(p.local));
        let v = match p.mode {
            PassMode::Borrow | PassMode::BorrowMut => self.share_value(v, ty),
            PassMode::Owned | PassMode::Copy => v,
        };
        let v = Operand::Copy(Place::local(self.copy_to_temp(v, vt)));
        let cell = self.new_local(Ty::Ptr, Some(f.body.locals[i].name.clone()));
        let c = self.counted_alloc(vt);
        self.cell_check_new(c.clone(), Some(&f.body.locals[i].name.clone()));
        self.assign(Place::local(cell), crate::vir::Rvalue::Use(c));
        let info = &mut self.info[i];
        info.vir = Some(cell);
        info.indirect = true;
        info.droppable = true;
        info.cell = true;
        info.in_cell = true;
        let p = self.local_place(p.local);
        self.store(p, v);
    }
}

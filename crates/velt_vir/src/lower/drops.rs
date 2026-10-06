//! Drop elaboration: the scope stack of drop obligations (and `finally` blocks, which run on every
//! exit edge like drops do), ownership-state tracking (static state or drop flags), and calls to
//! drop glue. Scopes are unwound innermost-first on every exit edge.

use std::rc::Rc;

use velt_sema::hir::{LocalId, TyId, TyKind};

use super::rt::Rt;
use super::{ice, DropEntry, FnLower, Glue, LState, Scope, ScopeKind, Work};
use crate::vir::{self, Const, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    pub(super) fn push_scope(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope {
            kind,
            drops: vec![],
        });
    }

    /// Leave the innermost scope normally, dropping what it still owns (and running `finally`).
    pub(super) fn pop_scope(&mut self) {
        let s = self
            .scopes
            .pop()
            .unwrap_or_else(|| ice("scope stack underflow"));
        for d in s.drops.iter().rev() {
            self.drop_entry(d);
        }
        if let ScopeKind::Finally(Some(b)) = s.kind {
            self.block(&b);
        }
    }

    /// Emit (without popping) the cleanup of every scope at index `>= depth`, innermost first.
    /// Used by `return`/`break`/`continue`/`throw`, which leave several scopes at once.
    pub(super) fn emit_drops_from(&mut self, depth: usize) {
        for i in (depth..self.scopes.len()).rev() {
            let drops = self.scopes[i].drops.clone();
            for d in drops.iter().rev() {
                self.drop_entry(d);
            }
            self.run_finally(i);
        }
    }

    /// The drops of every scope, innermost first, without `finally` blocks: the cleanup of an
    /// async function cancelled at a suspension point (dropping a future never runs user code).
    pub(super) fn emit_cancel_drops(&mut self) {
        for i in (0..self.scopes.len()).rev() {
            let drops = self.scopes[i].drops.clone();
            for d in drops.iter().rev() {
                self.drop_entry(d);
            }
        }
    }

    /// Inline the `finally` block of scope `i` on an early-exit edge. The block is taken out
    /// while it is lowered so an exit from inside it does not run it again.
    fn run_finally(&mut self, i: usize) {
        let ScopeKind::Finally(fin) = &mut self.scopes[i].kind else {
            return;
        };
        let Some(b) = fin.take() else { return };
        self.block(&b);
        self.scopes[i].kind = ScopeKind::Finally(Some(b));
    }

    fn innermost(&mut self) -> &mut Scope {
        self.scopes.last_mut().unwrap_or_else(|| ice("no scope"))
    }

    /// The innermost scope becomes responsible for dropping this local.
    pub(super) fn register_local_drop(&mut self, id: LocalId) {
        self.innermost().drops.push(DropEntry::Local(id));
    }

    /// Register a fresh owned temporary (a whole VIR local) in the innermost scope.
    pub(super) fn own_temp(&mut self, l: vir::Local, ty: TyId) {
        self.own_place(Place::local(l), ty);
    }

    /// Register an owned value at `place` (of concrete type `ty`) in the innermost scope.
    pub(super) fn own_place(&mut self, place: Place, ty: TyId) {
        if !self.dead() && self.cx.needs_drop(ty) {
            self.innermost().drops.push(DropEntry::Temp(place, ty));
        }
    }

    /// Register a temporary at `place` that owns a value only when `flag` is true.
    pub(super) fn own_flagged(&mut self, place: Place, ty: TyId, flag: crate::vir::Local) {
        if !self.dead() && self.cx.needs_drop(ty) {
            self.innermost().drops.push(DropEntry::Flagged(place, ty, flag));
        }
    }

    /// Register the class object `new` is constructing at `place` (`DropEntry::HalfBuilt`).
    pub(super) fn own_half_built(&mut self, place: Place, ty: TyId) {
        if !self.dead() && self.cx.needs_drop(ty) {
            self.innermost().drops.push(DropEntry::HalfBuilt(place, ty));
        }
    }

    /// The object at `p` is constructed: from here on it drops like any owned temporary.
    pub(super) fn own_built(&mut self, p: &Place) {
        for s in self.scopes.iter_mut().rev() {
            for d in s.drops.iter_mut() {
                if let DropEntry::HalfBuilt(t, ty) = d {
                    if t == p {
                        *d = DropEntry::Temp(t.clone(), *ty);
                        return;
                    }
                }
            }
        }
    }

    /// Release an object whose construction threw: drop its fields (those not reached are
    /// still zero, which drops as nothing) and free it, without its `[Symbol.dispose]()`, as
    /// JavaScript never disposes an object `new` did not return. A counted object the
    /// constructor shared (`this` stored elsewhere) only loses this reference.
    fn drop_half_built(&mut self, p: &Place, ty: TyId) {
        let obj = Operand::Copy(p.clone());
        let free = |lw: &mut Self| {
            for (i, t) in lw.cx.adt_field_tys(ty).into_iter().enumerate() {
                if lw.cx.needs_drop(t) {
                    let fp = lw.field_place(p, ty, i as u32);
                    lw.drop_glue(fp, t);
                }
            }
            lw.object_free(Operand::Copy(p.clone()), ty);
        };
        if self.cx.counted(ty) {
            self.release(obj, free);
        } else {
            free(self);
        }
    }

    /// Register "drop what the pattern did not move out of `place`" in the innermost scope.
    pub(super) fn own_rest(&mut self, place: Place, ty: TyId, pat: Rc<velt_sema::hir::Pat>) {
        if !self.dead() && self.cx.needs_drop(ty) {
            self.innermost().drops.push(DropEntry::Rest(place, ty, pat));
        }
    }

    /// Remove a pending temporary because its ownership is being transferred. Returns whether found.
    pub(super) fn take_temp(&mut self, p: &Place) -> bool {
        for s in self.scopes.iter_mut().rev() {
            if let Some(pos) = s
                .drops
                .iter()
                .position(|d| matches!(d, DropEntry::Temp(t, _) if t == p))
            {
                s.drops.remove(pos);
                return true;
            }
        }
        false
    }

    pub(super) fn drop_entry(&mut self, d: &DropEntry) {
        if self.dead() {
            return;
        }
        match d {
            DropEntry::Temp(p, ty) => self.drop_glue(p.clone(), *ty),
            DropEntry::Flagged(p, ty, flag) => {
                let drop_bb = self.new_block();
                let join = self.new_block();
                self.branch(Operand::Copy(Place::local(*flag)), drop_bb, join);
                self.switch_to(drop_bb);
                self.drop_glue(p.clone(), *ty);
                self.goto(join);
                self.switch_to(join);
            }
            DropEntry::HalfBuilt(p, ty) => self.drop_half_built(p, *ty),
            DropEntry::Local(id) => self.drop_local(*id),
            DropEntry::Rest(p, ty, pat) => self.drop_rest(p.clone(), *ty, pat),
            DropEntry::ConsumedArray { arr, next, elem } => self.drop_consumed(arr, *next, *elem),
        }
    }

    fn drop_local(&mut self, id: LocalId) {
        if self.info[id.0 as usize].cell {
            return self.release_cell(id);
        }
        let info = &self.info[id.0 as usize];
        let (ty, flag, state) = (info.ty, info.flag, info.state);
        let moved = info.moved_fields.clone();
        let gen = info.gen.clone();
        let place = self.local_place(id);
        let drop_whole = |lw: &mut Self, place: Place| match &gen {
            Some(g) => lw.drop_gen_local(place, g),
            None => lw.drop_glue(place, ty),
        };
        if let Some(flag) = flag {
            let drop_bb = self.new_block();
            let join = self.new_block();
            self.branch(Operand::Copy(Place::local(flag)), drop_bb, join);
            self.switch_to(drop_bb);
            drop_whole(self, place);
            self.goto(join);
            self.switch_to(join);
        } else if state == LState::Init && (moved.is_empty() || gen.is_some()) {
            drop_whole(self, place);
        } else if state == LState::Init {
            self.drop_fields_except(place, ty, &moved);
        }
    }

    /// Release the resources of the value at `place` (of concrete type `ty`).
    pub(super) fn drop_glue(&mut self, place: Place, ty: TyId) {
        if !self.cx.needs_drop(ty) {
            return;
        }
        let a = self.addr(place);
        match self.cx.kind(ty) {
            TyKind::Str => self.call_rt(Rt::StrDrop, vec![a], None),
            _ => {
                let f = self.cx.func(Work::Glue(Glue::Drop, ty));
                self.call(vir::Callee::Func(f), vec![a], None, false);
            }
        }
    }

    /// Drop the fields of a struct/class value except those moved out (a class object is freed).
    pub(super) fn drop_fields_except(&mut self, place: Place, ty: TyId, moved: &[u32]) {
        let fields = self.cx.adt_field_tys(ty);
        for (i, fty) in fields.into_iter().enumerate() {
            if !moved.contains(&(i as u32)) && self.cx.needs_drop(fty) {
                let fp = self.field_place(&place, ty, i as u32);
                self.drop_glue(fp, fty);
            }
        }
        if self.cx.is_class(ty) {
            let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place)));
            self.object_free(ptr, ty);
        }
    }

    fn set_flag(&mut self, id: LocalId, value: bool) {
        if let Some(fl) = self.info[id.0 as usize].flag {
            self.assign(
                Place::local(fl),
                Rvalue::Use(Operand::Const(Const::Bool(value), Ty::Bool)),
            );
        }
    }

    pub(super) fn mark_moved(&mut self, id: LocalId) {
        if self.info[id.0 as usize].droppable && !self.dead() {
            self.set_flag(id, false);
            self.info[id.0 as usize].state = LState::Moved;
        }
    }

    pub(super) fn mark_init(&mut self, id: LocalId) {
        if self.info[id.0 as usize].droppable && !self.dead() {
            self.set_flag(id, true);
            let info = &mut self.info[id.0 as usize];
            info.state = LState::Init;
            info.moved_fields.clear();
        }
    }

    /// `let x;` (re)declares `x` as uninitialized — also on every loop iteration.
    pub(super) fn mark_uninit(&mut self, id: LocalId) {
        if self.info[id.0 as usize].droppable && !self.dead() {
            self.set_flag(id, false);
            self.info[id.0 as usize].state = LState::Uninit;
        }
    }

    /// Record that field `field` was moved out of local `id` (the rest is dropped field-wise).
    pub(super) fn mark_field_moved(&mut self, id: LocalId, field: u32) {
        let info = &mut self.info[id.0 as usize];
        if info.droppable && info.flag.is_none() && !info.moved_fields.contains(&field) {
            info.moved_fields.push(field);
        }
    }

    /// Drop the current value of a local that is about to be overwritten.
    pub(super) fn drop_old(&mut self, id: LocalId) {
        let info = &self.info[id.0 as usize];
        let ty = info.ty;
        if !self.cx.needs_drop(ty) {
            return;
        }
        let info = &self.info[id.0 as usize];
        if info.droppable && !info.cell {
            self.drop_entry(&DropEntry::Local(id));
        } else if info.indirect {
            // Borrowed param written in place (`Mutex.with` callback): the caller's value is
            // always initialized.
            let p = self.local_place(id);
            self.drop_glue(p, ty);
        }
    }

    /// A new (uninitialized) object of class `ty`: counted when the class is (boxing/).
    pub(super) fn object_alloc(&mut self, ty: TyId) -> Operand {
        let oa = Ty::Agg(self.cx.obj_agg(ty));
        if self.cx.counted(ty) {
            self.counted_alloc(oa)
        } else {
            self.alloc(oa)
        }
    }

    /// Free the object `ptr` of class `ty` (its fields already dropped).
    pub(super) fn object_free(&mut self, ptr: Operand, ty: TyId) {
        if self.cx.is_generator_obj(ty) {
            return self.gen_object_free(ptr);
        }
        let oa = Ty::Agg(self.cx.obj_agg(ty));
        if self.cx.counted(ty) {
            self.counted_free(ptr, oa);
        } else {
            self.free(ptr, oa);
        }
    }

    /// `velt_rt_free(ptr, size, align)` of a heap block holding one `layout` value.
    pub(super) fn free(&mut self, ptr: Operand, layout: Ty) {
        let (size, align) = self.cx.size_align(layout);
        self.call_rt(
            Rt::Free,
            vec![
                ptr,
                super::cint(size as i128, Ty::U64),
                super::cint(align as i128, Ty::U64),
            ],
            None,
        );
    }

    /// `velt_rt_alloc(size, align)` of a heap block for one `layout` value.
    pub(super) fn alloc(&mut self, layout: Ty) -> Operand {
        let (size, align) = self.cx.size_align(layout);
        let p = self.temp(Ty::Ptr);
        self.call_rt(
            Rt::Alloc,
            vec![
                super::cint(size as i128, Ty::U64),
                super::cint(align as i128, Ty::U64),
            ],
            Some(Place::local(p)),
        );
        Operand::Copy(Place::local(p))
    }
}

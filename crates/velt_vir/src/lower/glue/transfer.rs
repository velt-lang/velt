//! Transfer glue bodies (lower/transfer.rs): make a value that is about to enter another
//! thread safe there, in place. A counted object the sender holds the only reference to (a
//! count of 1) stays, and its parts are transferred in turn; one that is still shared is replaced by a
//! deep copy (made by the clone glue, so a resource is copied by its own `clone()`, whose result
//! is transferred in turn: `settle_copy`) and the sender's reference released. Values that cannot reach a counted object are left alone.
//!
//! Class objects in a hierarchy with a vtable and interface values transfer through their
//! vtable (`SLOT_TRANSFER`), closures through their environment's transfer entry
//! (`build_env_transfer`), and a promise asks the runtime to transfer its result on the task
//! that produces it (`velt_rt_fut_transfer`, #160).

use velt_sema::hir::{DefId, PassMode, TyId, TyKind};

use super::{Glue, SLOT_TRANSFER};
use crate::lower::closure::closure_name;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, unit, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl<'c, 'h> FnLower<'c, 'h> {
    pub(super) fn transfer_body(&mut self, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        self.transfer_expand(&place, ty);
        self.terminate(Terminator::Return(unit()));
    }

    fn transfer_expand(&mut self, place: &Place, ty: TyId) {
        if self.cx.boxed(ty) {
            return self.transfer_boxed(place, ty);
        }
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => self.transfer_object(place, ty, d),
            TyKind::Adt(..) if self.is_enum(ty) => self.transfer_variants(place, ty),
            TyKind::Result(..) => self.transfer_variants(place, ty),
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let fp = self.field_place(place, ty, i as u32);
                        self.transfer_in_place(fp, t);
                    }
                }
            }
            TyKind::Option(e) => self.transfer_option(place, ty, e),
            TyKind::Array(e) => self.transfer_elems(place, e),
            TyKind::Promise(..) => self.transfer_promise(place, ty),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.transfer_closure(place),
            TyKind::Dyn(..) => {
                let data = proj(place, Proj::Field(0));
                let vt = Operand::Copy(proj(place, Proj::Field(1)));
                self.via_vtable(&data, vt);
            }
            _ => {}
        }
    }

    /// Replace the object or data pointer at `ptr` by the result of the `SLOT_TRANSFER` entry
    /// of vtable `vt` (null: nothing there).
    fn via_vtable(&mut self, ptr: &Place, vt: Operand) {
        let vt = self.rvalue_temp(Ty::Ptr, Rvalue::Use(vt));
        let done = self.new_block();
        let nn = self.non_null(vt.clone());
        self.when(nn, done);
        let f = self.dispatch(vt, SLOT_TRANSFER);
        let new = self.call_entry(f, vec![Operand::Copy(ptr.clone())], vec![Ty::Ptr], Ty::Ptr);
        self.assign(ptr.clone(), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    fn transfer_object(&mut self, place: &Place, ty: TyId, d: DefId) {
        let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(obj.clone());
        self.when(nn, done);
        if self.cx.has_header(d) {
            let vt = self.obj_vtable(obj, ty);
            self.via_vtable(place, vt);
        } else {
            let new = self.call_glue(Glue::ObjTransfer, ty, vec![obj]);
            self.assign(place.clone(), Rvalue::Use(new));
        }
        self.goto(done);
        self.switch_to(done);
    }

    /// `(obj) -> obj'` of class `ty` (see module docs).
    pub(super) fn obj_transfer_body(&mut self, obj: vir::Local, ty: TyId) {
        let o = Operand::Copy(Place::local(obj));
        let out = self.temp(Ty::Ptr);
        self.assign(Place::local(out), Rvalue::Use(o.clone()));
        let done = self.new_block();
        if self.cx.counted(ty) {
            let (unique, shared) = (self.new_block(), self.new_block());
            let one = self.count_is_one(o.clone());
            self.branch(one, unique, shared);
            self.switch_to(shared);
            let new = self.shared_copy(o.clone(), ty, |lw, v| {
                lw.call_glue(Glue::ObjClone, ty, vec![v])
            });
            let new = self.settle_copy(new, ty);
            self.assign(Place::local(out), Rvalue::Use(new));
            self.goto(done);
            self.switch_to(unique);
        }
        let tys = self.cx.adt_field_tys(ty);
        for (i, t) in tys.into_iter().enumerate() {
            let fp = self.field_place(&Place::local(obj), ty, i as u32);
            self.transfer_in_place(fp, t);
        }
        self.goto(done);
        self.switch_to(done);
        self.terminate(Terminator::Return(Operand::Copy(Place::local(out))));
    }

    /// `(data) -> data'` of an interface value whose implementor is `ty`: class objects and
    /// boxed values are their own data pointer; other data is a heap box only this interface
    /// value owns (share.rs copies it), transferred in place.
    pub(super) fn dyn_transfer_body(&mut self, data: vir::Local, ty: TyId) {
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            self.transfer_in_place(Place::local(data), ty);
        } else {
            let p = self.deref_param(data, ty);
            self.transfer_in_place(p, ty);
        }
        self.terminate(Terminator::Return(Operand::Copy(Place::local(data))));
    }

    /// A counted box at `place`: its contents transferred when it is unique, else a copy.
    fn transfer_boxed(&mut self, place: &Place, ty: TyId) {
        let payload = self.cx.payload_ty(ty);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        let (unique, shared) = (self.new_block(), self.new_block());
        let one = self.count_is_one(p.clone());
        self.branch(one, unique, shared);
        self.switch_to(shared);
        let new = self.shared_copy(p.clone(), ty, |lw, v| {
            let c = lw.clone_value(v, ty);
            lw.rvalue_temp(Ty::Ptr, Rvalue::Use(c))
        });
        let new = self.settle_copy(new, ty);
        self.assign(place.clone(), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(unique);
        let pp = self.operand_place(p, Ty::Ptr);
        let value = proj(&pp, Proj::Deref(payload));
        match self.cx.kind(ty) {
            TyKind::Array(e) => self.transfer_elems(&value, e),
            _ => {
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                        self.transfer_in_place(proj(&value, f), t);
                    }
                }
            }
        }
        self.goto(done);
        self.switch_to(done);
    }

    /// The still-shared counted value `v` (a pointer) of type `ty`: a deep copy made by `copy`
    /// (a fresh, unique graph), after which the sender's reference is released (the count is
    /// above 1: only decremented). A resource without `clone()` panics instead.
    fn shared_copy(
        &mut self,
        v: Operand,
        ty: TyId,
        copy: impl FnOnce(&mut Self, Operand) -> Operand,
    ) -> Operand {
        if self.cx.uncopyable(ty) {
            self.panic_uncopyable(ty);
            return v;
        }
        let new = copy(self, v.clone());
        let c = self.count_place(v);
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, Operand::Copy(c.clone()), cint(1, Ty::U64)),
        );
        self.assign(c, Rvalue::Use(n));
        new
    }

    /// The fresh copy `new` (a counted pointer of type `ty`) made safe for the other thread
    /// when a class's own `clone()` made part of it: that method may keep sharing parts of the
    /// original (a shallow copy), so the copy is transferred in turn, and parts it still shares
    /// are deep-copied. A `clone()` that returns an object referenced elsewhere (`this`, or one
    /// it also stored) is a bug in the program: it panics, as the copy cannot be moved.
    fn settle_copy(&mut self, new: Operand, ty: TyId) -> Operand {
        if !self.cx.reaches_own_clone(ty) {
            return new;
        }
        let new = self.rvalue_temp(Ty::Ptr, Rvalue::Use(new));
        if self.cx.own_clone(ty).is_some() {
            let ok = self.new_block();
            let one = self.count_is_one(new.clone());
            self.when_not(one, ok);
            let name = self.cx.type_name(ty);
            let msg = self.str_lit(&format!(
                "`{name}.clone()` returned an object that is still referenced elsewhere (`this`, or one it also stored); a copy for another task must be a new object"
            ));
            let at = self.operand_addr(msg, Ty::Agg(vir::STR_AGG));
            self.call_rt(Rt::Panic, vec![at], None);
            self.goto(ok);
            self.switch_to(ok);
        }
        let t = Place::local(self.copy_to_temp(new, Ty::Ptr));
        self.transfer_in_place(t.clone(), ty);
        Operand::Copy(t)
    }

    /// Continue in a fresh block when `cond` is false, else jump to `skip`.
    fn when_not(&mut self, cond: Operand, skip: vir::BlockId) {
        let then = self.new_block();
        self.branch(cond, skip, then);
        self.switch_to(then);
    }

    /// `count(p) == 1` for the counted object `p`.
    fn count_is_one(&mut self, p: Operand) -> Operand {
        let c = self.count_place(p);
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(c), cint(1, Ty::U64)),
        )
    }

    fn transfer_variants(&mut self, place: &Place, ty: TyId) {
        self.for_each_variant(place, ty, |lw, _, parts| {
            for (pp, pt) in parts {
                lw.transfer_in_place(pp, pt);
            }
        });
    }

    fn transfer_option(&mut self, place: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            // Null niche: the payload's own glue checks for null.
            return self.transfer_in_place(place.clone(), e);
        }
        let done = self.new_block();
        let some = self.option_is_some(place, ty);
        self.when(some, done);
        self.transfer_in_place(proj(place, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    fn transfer_elems(&mut self, arr: &Place, e: TyId) {
        if !self.cx.holds_counted(e) {
            return;
        }
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(proj(arr, Proj::Field(1)));
        self.count_loop(k, len, |lw, k| {
            let p = lw.elem_place(arr, k, e);
            lw.transfer_in_place(p, e);
        });
    }

    /// A promise: the runtime transfers its result where it is produced (a started promise
    /// finishes on the task that started it), before anyone on another task can see it. A
    /// lazy one (a `Promise.all` kept as a value) is started first, so this task drives it and
    /// the inputs it reads stay here.
    fn transfer_promise(&mut self, place: &Place, ty: TyId) {
        let slot = self.cx.promise_slot(ty);
        if !self.cx.holds_counted(slot) {
            return;
        }
        let f = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(f.clone());
        self.when(nn, done);
        let unclaimed = self.unclaimed_drop_fn(ty);
        self.call_rt(Rt::FutStart, vec![f.clone(), unclaimed], None);
        let g = cfunc(self.cx.func(Work::Glue(Glue::Transfer, slot)));
        self.call_rt(Rt::FutTransfer, vec![f, g], None);
        self.goto(done);
        self.switch_to(done);
    }

    /// `{ code, env }`: a heap env goes through its transfer entry; a null env (no captures)
    /// or one in a frame (null drop entry: it only borrows) has nothing of its own to move.
    fn transfer_closure(&mut self, place: &Place) {
        let hdr = self.cx.closure_agg();
        let envp = proj(place, Proj::Field(1));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(envp.clone())));
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let header = proj(&ep, Proj::Deref(Ty::Agg(hdr)));
        let drop = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&header, Proj::Field(0)))),
        );
        let heap = self.non_null(drop);
        self.when(heap, done);
        // The transfer entry follows the drop and clone entries (closure.rs `ENV_HEADER`).
        let (word, _) = self.cx.size_align(Ty::Ptr);
        let tp = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(
                BinOp::PtrAdd,
                env.clone(),
                cint(2 * i128::from(word), Ty::I64),
            ),
        );
        let tpp = self.operand_place(tp, Ty::Ptr);
        let f = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&tpp, Proj::Deref(Ty::Ptr)))),
        );
        let new = self.call_entry(f, vec![env], vec![Ty::Ptr], Ty::Ptr);
        self.assign(envp, Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    /// `(env: ptr) -> ptr` of closure `(def, targs)`: a shared env is first deep-copied (its
    /// clone entry; the sender's reference released); then the env — now unique — has its
    /// owned captures transferred and every captured variable's cell made its own (a cell the
    /// creator still shares is copied: the task's assignments stay in the task, as with any
    /// copy).
    pub(in crate::lower) fn build_env_transfer(
        cx: &'c mut Cx<'h>,
        def: DefId,
        targs: &[TyId],
    ) -> Function {
        let mut lw = FnLower::bare(cx, targs.to_vec());
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let ea = lw.cx.env_agg(def, targs);
        let out = lw.temp(Ty::Ptr);
        lw.assign(
            Place::local(out),
            Rvalue::Use(Operand::Copy(Place::local(env))),
        );
        let caps = lw.value_captures(def);
        let cells = lw.cell_captures(def);
        let uncopyable = caps
            .iter()
            .find(|(_, mode, ty)| *mode == PassMode::Owned && lw.cx.uncopyable(*ty))
            .map(|c| c.2);
        let (unique, shared) = (lw.new_block(), lw.new_block());
        let one = lw.count_is_one(Operand::Copy(Place::local(env)));
        lw.branch(one, unique, shared);
        lw.switch_to(shared);
        if let Some(t) = uncopyable {
            lw.panic_uncopyable(t);
        }
        let clone = cfunc(lw.cx.func(Work::EnvClone(def, targs.to_vec())));
        let ev = Operand::Copy(Place::local(env));
        let new = lw.call_entry(clone, vec![ev.clone()], vec![Ty::Ptr], Ty::Ptr);
        lw.assign(Place::local(out), Rvalue::Use(new));
        let c = lw.count_place(ev);
        let n = lw.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, Operand::Copy(c.clone()), cint(1, Ty::U64)),
        );
        lw.assign(c, Rvalue::Use(n));
        lw.goto(unique);
        lw.switch_to(unique);
        let base = proj(&Place::local(out), Proj::Deref(Ty::Agg(ea)));
        for (field, mode, ty) in caps {
            if mode == PassMode::Owned {
                lw.transfer_in_place(proj(&base, Proj::Field(field)), ty);
            }
        }
        for (field, ty) in cells {
            lw.own_cell(proj(&base, Proj::Field(field)), ty);
        }
        lw.terminate(Terminator::Return(Operand::Copy(Place::local(out))));
        let sym = format!(
            "_Genv_transfer_{}",
            lw.cx.instance_symbol(&closure_name(lw.cx.hir, def), targs)
        );
        lw.finish(sym, vec![Ty::Ptr], Ty::Ptr)
    }

    /// The cell pointer at `slot` (holding a `ty`), made this env's own: transferred in place
    /// when nothing else references it, else replaced by a new cell with a copy of the value.
    fn own_cell(&mut self, slot: Place, ty: TyId) {
        let vt = self.cx.ty(ty);
        let cell = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(slot.clone())));
        let (unique, shared, done) = (self.new_block(), self.new_block(), self.new_block());
        let one = self.count_is_one(cell.clone());
        self.branch(one, unique, shared);
        self.switch_to(shared);
        let cp = self.operand_place(cell.clone(), Ty::Ptr);
        let value = proj(&cp, Proj::Deref(vt));
        let new = self.shared_copy(cell.clone(), ty, |lw, _| {
            let fresh = lw.counted_alloc(vt);
            let fp = lw.operand_place(fresh.clone(), Ty::Ptr);
            let copy = lw.thread_copy(Operand::Copy(value.clone()), ty);
            lw.store(proj(&fp, Proj::Deref(vt)), copy);
            fresh
        });
        self.assign(slot, Rvalue::Use(new));
        self.goto(done);
        self.switch_to(unique);
        let cp = self.operand_place(cell, Ty::Ptr);
        self.transfer_in_place(proj(&cp, Proj::Deref(vt)), ty);
        self.goto(done);
        self.switch_to(done);
    }
}

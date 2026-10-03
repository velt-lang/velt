//! Many-threads check glue (`ManyCheck`, lower/transfer.rs `many_check`): a value that becomes
//! reachable from several threads at once — put in `shared(x)`, or captured by an HTTP
//! handler, whose requests run concurrently — must not hold a function value that captured a
//! resource without `clone()`. Each call of such a function shares the capture with the call
//! (async_fn/ctor.rs `take_capture`), and calls from several threads would update its count
//! concurrently. The glue walks the value and panics on one; values that cannot reach a
//! function or interface value are not walked.
//!
//! Function values and interface values are checked through the entries they carry: an
//! environment's transfer entry and the `SLOT_TRANSFER` vtable entry, called with the pointer
//! tagged (low bit set, [`FnLower::tagged`]): in that mode the entry checks instead of
//! transferring and returns the pointer untagged ([`FnLower::check_if_tagged`]).

use velt_sema::hir::{TyId, TyKind};

use super::{Glue, SLOT_TRANSFER};
use crate::lower::operand::proj;
use crate::lower::{cint, unit, FnLower, Work};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    pub(super) fn many_check_body(&mut self, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        self.many_check_expand(&place, ty);
        self.terminate(Terminator::Return(unit()));
    }

    fn many_check_expand(&mut self, place: &Place, ty: TyId) {
        if self.cx.boxed(ty) {
            return self.many_check_boxed(place, ty);
        }
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                let done = self.new_block();
                let nn = self.non_null(obj.clone());
                self.when(nn, done);
                let tagged = self.tagged(obj.clone());
                if self.cx.has_header(d) {
                    let vt = self.obj_vtable(obj, ty);
                    let f = self.dispatch(vt, SLOT_TRANSFER);
                    self.call_entry(f, vec![tagged], vec![Ty::Ptr], Ty::Ptr);
                } else {
                    self.call_glue(Glue::ObjTransfer, ty, vec![tagged]);
                }
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.is_enum(ty) => self.many_check_variants(place, ty),
            TyKind::Result(..) => self.many_check_variants(place, ty),
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                for (i, t) in self.cx.part_types(ty).into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let fp = self.field_place(place, ty, i as u32);
                        self.many_check(fp, t);
                    }
                }
            }
            TyKind::Option(e) => self.many_check_option(place, ty, e),
            TyKind::Array(e) => self.many_check_elems(place, e),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.many_check_closure(place),
            TyKind::Dyn(..) => {
                let vt = self.rvalue_temp(
                    Ty::Ptr,
                    Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
                );
                let done = self.new_block();
                let nn = self.non_null(vt.clone());
                self.when(nn, done);
                let data = Operand::Copy(proj(place, Proj::Field(0)));
                let tagged = self.tagged(data);
                let f = self.dispatch(vt, SLOT_TRANSFER);
                self.call_entry(f, vec![tagged], vec![Ty::Ptr], Ty::Ptr);
                self.goto(done);
                self.switch_to(done);
            }
            _ => {}
        }
    }

    fn many_check_boxed(&mut self, place: &Place, ty: TyId) {
        let payload = self.cx.payload_ty(ty);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        let pp = self.operand_place(p, Ty::Ptr);
        let value = proj(&pp, Proj::Deref(payload));
        match self.cx.kind(ty) {
            TyKind::Array(e) => self.many_check_elems(&value, e),
            _ => {
                for (i, t) in self.cx.part_types(ty).into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                        self.many_check(proj(&value, f), t);
                    }
                }
            }
        }
        self.goto(done);
        self.switch_to(done);
    }

    fn many_check_variants(&mut self, place: &Place, ty: TyId) {
        self.for_each_variant(place, ty, |lw, _, parts| {
            for (pp, pt) in parts {
                lw.many_check(pp, pt);
            }
        });
    }

    fn many_check_option(&mut self, place: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            return self.many_check(place.clone(), e);
        }
        let done = self.new_block();
        let some = self.option_is_some(place, ty);
        self.when(some, done);
        self.many_check(proj(place, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    fn many_check_elems(&mut self, arr: &Place, e: TyId) {
        if !self.cx.reaches_fn(e) {
            return;
        }
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(proj(arr, Proj::Field(1)));
        self.count_loop(k, len, |lw, k| {
            let p = lw.elem_place(arr, k, e);
            lw.many_check(p, e);
        });
    }

    /// `{ code, env }`: a heap env is checked by its transfer entry (`build_env_transfer`).
    fn many_check_closure(&mut self, place: &Place) {
        let hdr = self.cx.closure_agg();
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
        );
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
        let f = self.env_transfer_entry(env.clone());
        let tagged = self.tagged(env);
        self.call_entry(f, vec![tagged], vec![Ty::Ptr], Ty::Ptr);
        self.goto(done);
        self.switch_to(done);
    }

    /// `p` with its low bit set: an entry called with it checks instead of transferring.
    fn tagged(&mut self, p: Operand) -> Operand {
        self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, p, cint(1, Ty::I64)))
    }

    /// Prologue of an entry taking a possibly tagged pointer `p` (module docs): when tagged,
    /// run `check` on the untagged pointer and return that; otherwise continue.
    pub(super) fn check_if_tagged(
        &mut self,
        p: vir::Local,
        check: impl FnOnce(&mut Self, Operand),
    ) {
        let bits = self.rvalue_temp(
            Ty::U64,
            Rvalue::Cast(Operand::Copy(Place::local(p)), Ty::U64),
        );
        let low = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, bits, cint(1, Ty::U64)),
        );
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, low, cint(0, Ty::U64)));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let untagged = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(
                BinOp::PtrAdd,
                Operand::Copy(Place::local(p)),
                cint(-1, Ty::I64),
            ),
        );
        check(self, untagged.clone());
        self.terminate(Terminator::Return(untagged));
        self.switch_to(no);
    }

    /// Check the value at `p` (concrete type `ty`) with the many-threads glue.
    pub(in crate::lower) fn many_check(&mut self, p: Place, ty: TyId) {
        if !self.cx.reaches_fn(ty) {
            return;
        }
        let a = self.addr(p);
        let f = self.cx.func(Work::Glue(Glue::ManyCheck, ty));
        self.call(vir::Callee::Func(f), vec![a], None, false);
    }
}

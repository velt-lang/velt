//! Glue of weak maps (rt_abi.md "Weak references"; docs/internals/design/weak-refs.md):
//!
//! - **Trace** `(obj, visit, ctx)` of a counted type calls `visit(ctx, child, child_trace)` once
//!   for each counted object `obj` refers to through its fields, elements and payloads stored
//!   inline (up to the next counted object), with the child's own trace glue, or null for an
//!   opaque child (a class with a vtable, whose dynamic class may have more fields, or one that
//!   refers to nothing). Function values, interface values, uncounted objects and `shared`
//!   values are not looked into: a reference left out only makes its target look referenced
//!   from outside, which keeps it alive (a leak at worst, never a free of something live).
//!   `TraceIn` is the same walk over an inline value at a pointer (a glue function per type, so
//!   recursive types work).
//! - **WeakRetain / WeakRelease** `(word)` retain and release a weak map value word: a counted
//!   object pointer (or null) for a value type that is one, else the pointer of a counted box
//!   holding the value (strings, unions, tuples, closures; `WeakMapSet` in lower/weak.rs). The
//!   box is weak-capable: an ephemeron cycle can run through it.
//! - **WeakBoxTrace** `(box, visit, ctx)` traces the value in such a box.

use velt_sema::hir::{TyId, TyKind};

use super::Glue;
use crate::lower::operand::proj;
use crate::lower::weak::WordKind;
use crate::lower::{cfunc, cint, unit, FnLower, Work};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Trace glue of the counted type `ty`, or null when its objects are opaque.
    pub(in crate::lower) fn trace_fn(&mut self, ty: TyId) -> Operand {
        if !self.cx.traceable(ty) || self.cx.weak_refs_of(None, ty).is_empty() {
            return cint(0, Ty::Ptr);
        }
        cfunc(self.cx.func(Work::Glue(Glue::Trace, ty)))
    }

    /// Body of `Glue::Trace` for the counted type `ty`.
    pub(super) fn trace_body(
        &mut self,
        obj: vir::Local,
        visit: vir::Local,
        cx: vir::Local,
        ty: TyId,
    ) {
        let o = Place::local(obj);
        let tv = (visit, cx);
        match self.cx.kind(ty) {
            TyKind::Adt(..) if self.cx.is_class(ty) => {
                let tys = self.cx.adt_field_tys(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if self.refers(t) {
                        let fp = self.field_place(&o, ty, i as u32);
                        self.trace_part(fp, t, tv);
                    }
                }
            }
            _ => {
                let payload = self.cx.payload_ty(ty);
                let value = proj(&o, Proj::Deref(payload));
                self.trace_value(value, ty, tv);
            }
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// Body of `Glue::TraceIn`: the inline `ty` value at `p`.
    pub(super) fn trace_in_body(
        &mut self,
        p: vir::Local,
        visit: vir::Local,
        cx: vir::Local,
        ty: TyId,
    ) {
        let place = self.deref_param(p, ty);
        self.trace_value(place, ty, (visit, cx));
        self.terminate(Terminator::Return(unit()));
    }

    /// Body of `Glue::WeakBoxTrace`: the `ty` value in the box `bx`.
    pub(super) fn weak_box_trace_body(
        &mut self,
        bx: vir::Local,
        visit: vir::Local,
        cx: vir::Local,
        ty: TyId,
    ) {
        let vt = self.cx.ty(ty);
        let value = proj(&Place::local(bx), Proj::Deref(vt));
        self.trace_part(value, ty, (visit, cx));
        self.terminate(Terminator::Return(unit()));
    }

    /// Does a `t` value stored inline refer to counted objects the trace glue visits?
    fn refers(&mut self, t: TyId) -> bool {
        !self.cx.weak_refs_inline(None, t).is_empty()
    }

    /// The parts of the inline (uncounted) `ty` value at `place`, for an object's payload or
    /// `TraceIn`.
    fn trace_value(&mut self, place: Place, ty: TyId, tv: (vir::Local, vir::Local)) {
        match self.cx.kind(ty) {
            TyKind::Array(e) => {
                if self.refers(e) {
                    let k = self.temp(Ty::U64);
                    self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
                    let len = Operand::Copy(proj(&place, Proj::Field(1)));
                    self.count_loop(k, len, |lw, k| {
                        let p = lw.elem_place(&place, k, e);
                        lw.trace_part(p, e, tv);
                    });
                }
            }
            TyKind::Option(e) => {
                let done = self.new_block();
                let some = self.option_is_some(&place, ty);
                self.when(some, done);
                self.trace_part(proj(&place, Proj::Field(1)), e, tv);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.cx.is_class(ty) => {}
            TyKind::Adt(..) | TyKind::Result(..) if self.is_enum(ty) || self.is_result(ty) => {
                if matches!(self.cx.ty(ty), Ty::Agg(_)) {
                    self.for_each_variant(&place, ty, |lw, _, parts| {
                        for (pp, pt) in parts {
                            lw.trace_part(pp, pt, tv);
                        }
                    });
                }
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if !self.cx.is_unit(t) && self.refers(t) {
                        let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                        self.trace_part(proj(&place, f), t, tv);
                    }
                }
            }
            _ => {}
        }
    }

    fn is_result(&self, ty: TyId) -> bool {
        matches!(self.cx.types.kind(ty), TyKind::Result(..))
    }

    /// One part of type `t` at `place`: visit it when it is a counted object, else walk it.
    fn trace_part(&mut self, place: Place, t: TyId, tv: (vir::Local, vir::Local)) {
        let target = match self.cx.kind(t) {
            _ if self.cx.counted(t) => Some(t),
            TyKind::Option(e) if self.cx.ty(t) == Ty::Ptr && self.cx.counted(e) => Some(e),
            _ => None,
        };
        if let Some(c) = target {
            let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place)));
            let done = self.new_block();
            let nn = self.non_null(p.clone());
            self.when(nn, done);
            let tf = self.trace_fn(c);
            let (visit, cx) = (
                Operand::Copy(Place::local(tv.0)),
                Operand::Copy(Place::local(tv.1)),
            );
            self.call_entry(visit, vec![cx, p, tf], vec![Ty::Ptr; 3], Ty::Unit);
            self.goto(done);
            self.switch_to(done);
            return;
        }
        if self.refers(t) {
            let a = self.addr(place);
            let args = vec![
                a,
                Operand::Copy(Place::local(tv.0)),
                Operand::Copy(Place::local(tv.1)),
            ];
            self.call_glue(Glue::TraceIn, t, args);
        }
    }

    /// Body of `Glue::WeakRetain` for map values of type `ty`.
    pub(super) fn weak_retain_body(&mut self, w: vir::Local, _ty: TyId) {
        let p = self.cast_to(Operand::Copy(Place::local(w)), Ty::U64, Ty::Ptr);
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        self.retain(p);
        self.goto(done);
        self.switch_to(done);
        self.terminate(Terminator::Return(unit()));
    }

    /// Body of `Glue::WeakRelease` for map values of type `ty`.
    pub(super) fn weak_release_body(&mut self, w: vir::Local, ty: TyId) {
        let p = self.cast_to(Operand::Copy(Place::local(w)), Ty::U64, Ty::Ptr);
        match self.word_kind(ty) {
            WordKind::Direct(_) => {
                let place = self.operand_place(p, Ty::Ptr);
                self.drop_glue(place, ty);
            }
            _ => {
                let done = self.new_block();
                let nn = self.non_null(p.clone());
                self.when(nn, done);
                self.release_weak_box(p, ty);
                self.goto(done);
                self.switch_to(done);
            }
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// Release the (non-null) weak map box `bx` holding a `ty` value.
    pub(in crate::lower) fn release_weak_box(&mut self, bx: Operand, ty: TyId) {
        let vt = self.cx.ty(ty);
        let q = bx.clone();
        self.release_as(bx, true, |lw| {
            if lw.cx.needs_drop(ty) {
                let bp = lw.operand_place(q.clone(), Ty::Ptr);
                lw.drop_glue(proj(&bp, Proj::Deref(vt)), ty);
            }
            lw.counted_free(q, vt);
        });
    }
}

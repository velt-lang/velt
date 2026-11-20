//! Trace glue through values whose static type does not say what they hold (#3119 step 3,
//! docs/internals/design/cycles.md): a class with a vtable, whose object may be any class of
//! its hierarchy with fields the static type lacks, and an interface value.
//!
//! - **Classes with a vtable.** Each built class's table has its own trace glue in
//!   `SLOT_TRACE` (`Glue::ObjTrace`, the walk of exactly its fields), put there in a program
//!   with weak references when the class refers to anything the walk visits. The glue of a
//!   value of such a class, counted or not, dispatches on the object's table. A class table
//!   never holds the slot's other use (a value struct's `BitsEq`, in interface tables only).
//! - **Interface values.** An interface table has no free slot (a value struct's uses them),
//!   so the glue compares the value's table with those of the implementations the program
//!   built for a class (`Boxing::iface_traced`), as the trace glue of a function value compares
//!   drop entries (trace_env.rs), and visits the data pointer, the object, with that class's
//!   glue. A table that matches none (a boxed value's) is left out: a leak at worst, never a
//!   free of something live.

use velt_sema::hir::{DefId, TyId};

use super::vtable::{VtableKey, SLOT_TRACE};
use crate::lower::operand::proj;
use crate::lower::{unit, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Body of `Glue::ObjTrace`: the fields of an object of exactly the class `ty`.
    pub(super) fn obj_trace_body(
        &mut self,
        obj: vir::Local,
        visit: vir::Local,
        cx: vir::Local,
        ty: TyId,
    ) {
        self.trace_class_fields(&Place::local(obj), ty, (visit, cx));
        self.terminate(Terminator::Return(unit()));
    }

    /// The object `obj` (non-null) of a class `ty` with a vtable: the trace glue its table has
    /// for its own class, called with it (none: it refers to nothing the walk visits).
    pub(super) fn trace_by_table(&mut self, obj: Operand, ty: TyId, tv: (vir::Local, vir::Local)) {
        if self.cx.weak_refs_of(None, ty).is_empty() {
            return;
        }
        let vt = self.obj_vtable(obj.clone(), ty);
        let f = self.dispatch(vt, SLOT_TRACE);
        let done = self.new_block();
        let nn = self.non_null(f.clone());
        self.when(nn, done);
        let args = vec![
            obj,
            Operand::Copy(Place::local(tv.0)),
            Operand::Copy(Place::local(tv.1)),
        ];
        self.call_entry(f, args, vec![Ty::Ptr; 3], Ty::Unit);
        self.goto(done);
        self.switch_to(done);
    }

    /// The interface value of interface `iface` at `place`: its object, visited with its class's
    /// glue when its table is one the program built for a class (module docs).
    pub(super) fn trace_iface(&mut self, place: Place, iface: DefId, tv: (vir::Local, vir::Local)) {
        let cands = self.cx.iface_counted(None, iface);
        if cands.is_empty() {
            return;
        }
        let data = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&place, Proj::Field(0)))),
        );
        let table = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&place, Proj::Field(1)))),
        );
        let done = self.new_block();
        let nn = self.non_null(table.clone());
        self.when(nn, done);
        for (index, c) in cands {
            let want = self.vtable_addr(VtableKey::Impl(index, c));
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, table.clone(), want));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(is, yes, no);
            self.switch_to(yes);
            let tf = self.trace_fn(c);
            self.call_visit(tv, data.clone(), tf);
            self.goto(done);
            self.switch_to(no);
        }
        self.goto(done);
        self.switch_to(done);
    }
}

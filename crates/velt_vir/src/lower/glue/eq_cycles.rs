//! Equality of values that may be reached again through their own fields: object values whose
//! type can hold itself, compared by `==` on an aggregate holding them (`Glue::Eq`, #1881). Such a comparison keeps the pairs of objects
//! being compared on a per-thread stack (`velt_rt_deep_eq_enter` / `_leave`), with Vitest's
//! rule: the same pair met again is equal (the same cycle on both sides), an object met again
//! with another partner differs (a cycle on one side, rings of two lengths). Node's
//! `isDeepStrictEqual` answers the same (`lang/deep_equal_cycles`).

use velt_sema::hir::{TyId, TyKind};

use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// The object at `*pa` is equal to the one at `*pb` when it is the same object, else when
    /// their fields are, compared under the cycle guard (module docs).
    pub(super) fn guarded_eq_body(&mut self, pa: vir::Local, pb: vir::Local, ty: TyId) {
        let a = self.deref_param(pa, ty);
        let b = self.deref_param(pb, ty);
        self.guarded_eq(a, b, ty)
    }

    /// [`guarded_eq_body`](Self::guarded_eq_body) of the objects (pointers) at `a` and `b`.
    pub(super) fn guarded_eq(&mut self, a: Place, b: Place, ty: TyId) {
        let (yes, no, enter) = (self.new_block(), self.new_block(), self.new_block());
        let (ptr_a, ptr_b) = (Operand::Copy(a.clone()), Operand::Copy(b.clone()));
        let same = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, ptr_a.clone(), ptr_b.clone()),
        );
        self.branch(same, yes, enter);
        self.switch_to(enter);
        let seen = self.temp(Ty::U8);
        let ids = vec![
            self.cast_to(ptr_a, Ty::Ptr, Ty::U64),
            self.cast_to(ptr_b, Ty::Ptr, Ty::U64),
        ];
        self.call_rt(Rt::DeepEqEnter, ids, Some(Place::local(seen)));
        let seen = Operand::Copy(Place::local(seen));
        let fresh = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, seen.clone(), cint(0, Ty::U8)),
        );
        let (fields, cycle) = (self.new_block(), self.new_block());
        self.branch(fresh, fields, cycle);
        // Met again: 1 is the same pair (equal), 2 another partner (different).
        self.switch_to(cycle);
        let same_pair =
            self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, seen, cint(1, Ty::U8)));
        self.branch(same_pair, yes, no);
        self.switch_to(fields);
        let differ = self.new_block();
        match self.cx.kind(ty) {
            TyKind::Array(e) => {
                let (ca, cb) = (self.content(&a, ty), self.content(&b, ty));
                self.eq_array(&ca, &cb, e, differ);
            }
            _ => {
                for (x, y, t) in self.struct_parts(&a, &b, ty) {
                    let e = self.eq_values(&x, &y, t);
                    self.when(e, differ);
                }
            }
        }
        self.call_rt(Rt::DeepEqLeave, vec![], None);
        self.goto(yes);
        self.switch_to(differ);
        self.call_rt(Rt::DeepEqLeave, vec![], None);
        self.goto(no);
        self.return_bool(yes, true);
        self.return_bool(no, false);
    }

    /// Block `b` returns `v`.
    pub(super) fn return_bool(&mut self, b: vir::BlockId, v: bool) {
        self.switch_to(b);
        self.terminate(Terminator::Return(Operand::Const(Const::Bool(v), Ty::Bool)));
    }

    /// Is `ty` compared under the cycle guard (`guarded_eq_body`)? Only an object value or array
    /// stored as a pointer whose parts can reach a value of its own type (class instances,
    /// including `Map` and `Record`, and interface values compare by identity). Other types cannot form a cycle and
    /// pay nothing.
    pub(super) fn guarded(&mut self, ty: TyId) -> bool {
        if self.same_mode || self.value_mode || self.is_enum(ty) {
            return false;
        }
        match self.cx.kind(ty) {
            TyKind::Adt(..) if self.cx.is_class(ty) => false,
            TyKind::Adt(..) if self.cx.boxed(ty) => self.compared_to_itself(ty),
            TyKind::Array(_) => self.cx.boxed(ty) && self.compared_to_itself(ty),
            _ => false,
        }
    }

    /// Can a value of `ty` reach another `ty` through the parts the comparison follows?
    fn compared_to_itself(&mut self, ty: TyId) -> bool {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut work = self.compared_parts(ty);
        while let Some(t) = work.pop() {
            if t == ty {
                return true;
            }
            if seen.insert(t) {
                work.extend(self.compared_parts(t));
            }
        }
        false
    }

    /// The types whose values the comparison compares inside a `t` by content.
    fn compared_parts(&mut self, t: TyId) -> Vec<TyId> {
        match self.cx.kind(t) {
            TyKind::Adt(..) if self.cx.is_class(t) => vec![],
            TyKind::Array(e) | TyKind::Shared(e) => vec![e],
            _ => self.cx.part_types(t),
        }
    }
}

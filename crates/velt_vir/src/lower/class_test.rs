//! `instanceof` on class values and interface values (`PatKind::InstanceOf`) and the
//! downcast of a narrowed value (`ExprKind::Downcast`).
//!
//! Every class gets an id from a pre-order walk of the class hierarchy (roots and subclasses in
//! definition order), so the subclasses of class `C` have the ids `lo..=hi` right after `C`'s
//! own (`lo`). Each class vtable (and each interface vtable of a class) stores the id of its
//! concrete class in its first word (`SLOT_CLASS_ID`, glue/vtable.rs; 0 for other types), and
//! `x instanceof C` is one load and a range check: O(1), no allocation.

use std::collections::HashMap;

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::operand::proj;
use super::{cint, Cx, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// Class id word of an interface table whose value is an object with a vtable pointer: read the
/// id from the object's own table.
const ASK_OBJECT: u64 = u64::MAX;

impl Cx<'_> {
    /// The id range of class `d` and its subclasses (`d` itself has the first id).
    pub(super) fn class_range(&mut self, d: DefId) -> (u64, u64) {
        if self.lay.class_ids.is_none() {
            self.lay.class_ids = Some(self.number_classes());
        }
        let ids = self.lay.class_ids.as_ref();
        ids.and_then(|m| m.get(&d).copied())
            .unwrap_or_else(|| super::ice("instanceof of a non-class type"))
    }

    /// The class id word of a vtable of the concrete type `ty`: its class id in a class's
    /// table, 0 for types that are not classes. An interface table of a class whose objects
    /// carry a vtable pointer says `ASK_OBJECT`: the interface value may hold a subclass object
    /// (`ToDyn` of a subclass through the base's `implements` uses the base's table).
    pub(super) fn class_id_word(&mut self, ty: TyId, interface_table: bool) -> u64 {
        match self.kind(ty) {
            TyKind::Adt(d, _) if self.adt_def(d).kind == AdtKind::Class => {
                if interface_table && self.has_header(d) {
                    ASK_OBJECT
                } else {
                    self.class_range(d).0
                }
            }
            _ => 0,
        }
    }

    /// Pre-order ids of every class (from 1), with the last id of each subtree.
    fn number_classes(&self) -> HashMap<DefId, (u64, u64)> {
        let mut children: HashMap<Option<DefId>, Vec<DefId>> = HashMap::new();
        for i in 0..self.hir.defs.len() {
            let id = DefId(i as u32);
            if let hir::Def::Adt(a) = self.hir.def(id) {
                if a.kind == AdtKind::Class {
                    let base = a.base.and_then(|b| match self.types.kind(b) {
                        TyKind::Adt(bd, _) => Some(*bd),
                        _ => None,
                    });
                    children.entry(base).or_default().push(id);
                }
            }
        }
        let mut out = HashMap::new();
        let mut next = 1;
        // Explicit stack (deep hierarchies must not overflow the compiler's stack): (class,
        // whether its subclasses are numbered already).
        let mut stack: Vec<(DefId, bool)> = vec![];
        for &root in children.get(&None).into_iter().flatten().rev() {
            stack.push((root, false));
        }
        while let Some((d, done)) = stack.pop() {
            if done {
                let lo = out.get(&d).map_or(0, |r: &(u64, u64)| r.0);
                out.insert(d, (lo, next - 1));
                continue;
            }
            out.insert(d, (next, next));
            next += 1;
            stack.push((d, true));
            for &c in children.get(&Some(d)).into_iter().flatten().rev() {
                stack.push((c, false));
            }
        }
        out
    }
}

impl FnLower<'_, '_> {
    /// `place` (a class object or an interface value of type `ty`) is an instance of `class`.
    pub(super) fn instance_test(&mut self, place: &Place, ty: TyId, class: DefId) -> Operand {
        let id = match self.cx.kind(ty) {
            TyKind::Dyn(..) => self.dyn_class_id(place),
            _ => {
                let vt = self.obj_vtable(Operand::Copy(place.clone()), ty);
                self.vtable_class_id(vt)
            }
        };
        let (lo, hi) = self.cx.class_range(class);
        let lo_c = cint(lo as i128, Ty::U64);
        if lo == hi {
            return self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, lo_c));
        }
        // `lo <= id <= hi` as one unsigned comparison: `id - lo` wraps for ids below `lo`.
        let off = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, id, lo_c));
        let span = cint((hi - lo) as i128, Ty::U64);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, off, span))
    }

    /// The class id word of the vtable at `vt`.
    fn vtable_class_id(&mut self, vt: Operand) -> Operand {
        let vp = self.operand_place(vt, Ty::Ptr);
        let word = proj(&vp, Proj::Deref(Ty::U64));
        self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(word)))
    }

    /// The class id of the value of the interface value at `place`: its table's, or, when the
    /// table says `ASK_OBJECT` (a class whose objects have a vtable pointer, so the object may
    /// be of a subclass), the object's own table's.
    fn dyn_class_id(&mut self, place: &Place) -> Operand {
        let vt = Operand::Copy(proj(place, Proj::Field(1)));
        let first = self.vtable_class_id(vt);
        let id = self.temp(Ty::U64);
        self.assign(Place::local(id), Rvalue::Use(first.clone()));
        let ask = cint(ASK_OBJECT as i128, Ty::U64);
        let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, first, ask));
        let (obj, join) = (self.new_block(), self.new_block());
        self.branch(is_obj, obj, join);
        self.switch_to(obj);
        let data = self.operand_place(Operand::Copy(proj(place, Proj::Field(0))), Ty::Ptr);
        let hdr = Operand::Copy(proj(&data, Proj::Deref(Ty::Ptr)));
        let own = self.vtable_class_id(hdr);
        self.assign(Place::local(id), Rvalue::Use(own));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(id))
    }

    /// The object of a narrowed value `v` of type `from`: the data pointer of an interface
    /// value, a class object as it is.
    pub(super) fn downcast_value(&mut self, v: Operand, from: TyId) -> Operand {
        let from = self.sub(from);
        if !matches!(self.cx.kind(from), TyKind::Dyn(..)) || self.dead() {
            return v;
        }
        let p = self.place_of(v, from);
        Operand::Copy(proj(&p, Proj::Field(0)))
    }

    /// [`downcast_value`](Self::downcast_value) of the value at place `p`.
    pub(super) fn downcast_place(&mut self, p: Place, from: TyId) -> Place {
        let from = self.sub(from);
        match self.cx.kind(from) {
            TyKind::Dyn(..) => proj(&p, Proj::Field(0)),
            _ => p,
        }
    }
}

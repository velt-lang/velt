//! `toEqual` (`Glue::DeepEq`) of interface values and of class instances in a hierarchy with a
//! vtable pointer (#1882): Vitest compares two objects by their fields when they have the same
//! constructor, whatever type they are held as, so the comparison goes by the concrete type.
//!
//! - Two interface values are equal when they hold the same object; else when they hold the same
//!   concrete type (the same interface table, or, when the table says `ASK_OBJECT`, the same
//!   class table in the objects' headers) and the `DynDeepEq` entry of the table compares the
//!   two data pointers as that type. Different concrete types are unequal (Vitest's `equals`
//!   checks constructors; it would compare two plain objects of different shapes by keys, which
//!   no Velt interface value is).
//! - Two class instances whose class has a vtable pointer (the object may be of a subclass) are
//!   equal when they are the same object; else when their headers are the same class table and
//!   the `ObjDeepEq` entry of that table compares their fields as that class.
//!
//! The `DynDeepEq` entry is slot `n + u` of an interface's tables (after its `n` method slots
//! and `u` upcast targets), the `ObjDeepEq` entry the slot after the longest vtable of the
//! class's hierarchy, so every table of the hierarchy has it at the same place. Both exist only
//! in a program whose `toEqual` reaches values of the interface (`Boxing::deep_dyns`) or of the
//! hierarchy (`Boxing::deep_roots`); the pass that first finds one compares by identity (an
//! interface value) or by the static class's fields, and lowering runs again with the slots.
//! Typed proxies (whose data pointers are tagged) keep that fallback.

use velt_sema::hir::{self, AdtKind, DefId, TyId, TyKind};

use super::Glue;
use crate::lower::class_test::ASK_OBJECT;
use crate::lower::operand::proj;
use crate::lower::{cint, ice, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// The `DynDeepEq` slot of `iface`'s tables: after its methods and upcast targets.
    fn dyn_deep_eq_slot(&self, iface: DefId) -> i128 {
        let ups = self.hir.upcasts.get(&iface).map_or(0, Vec::len);
        self.upcast_base(iface) + ups as i128
    }

    /// The `ObjDeepEq` slot of the class tables of the hierarchy rooted at `root`: after the
    /// longest vtable of its classes.
    fn obj_deep_eq_slot(&self, root: DefId) -> i128 {
        let mut n = 0;
        for i in 0..self.hir.defs.len() {
            let d = DefId(i as u32);
            if let hir::Def::Adt(a) = self.hir.def(d) {
                if a.kind == AdtKind::Class && self.class_root(d) == root {
                    n = n.max(a.vtable.len());
                }
            }
        }
        n as i128
    }

    /// The `DynDeepEq` entry of the table of implementation `index` for the concrete type `ty`,
    /// when `toEqual` compares values of its interface (an adapter's value is never compared:
    /// the comparison looks through it).
    pub(super) fn dyn_deep_eq_entry(&mut self, index: u32, ty: TyId) -> Option<(i128, Const)> {
        let iface = self.hir.impls[index as usize].iface;
        if !self.boxing.deep_dyns.contains(&iface) || self.is_adapter(ty) {
            return None;
        }
        let f = self.func(Work::Glue(Glue::DynDeepEq, ty));
        Some((self.dyn_deep_eq_slot(iface), Const::Func(f)))
    }

    /// The `ObjDeepEq` entry of the class table of `cls`, when `toEqual` compares objects of its
    /// hierarchy.
    pub(super) fn obj_deep_eq_entry(&mut self, cls: TyId) -> Option<(i128, Const)> {
        let TyKind::Adt(d, _) = self.kind(cls) else {
            return None;
        };
        let root = self.class_root(d);
        if !self.boxing.deep_roots.contains(&root) {
            return None;
        }
        let f = self.func(Work::Glue(Glue::ObjDeepEq, cls));
        Some((self.obj_deep_eq_slot(root), Const::Func(f)))
    }
}

impl FnLower<'_, '_> {
    /// Is `ty` an interface type whose values `toEqual` compares by their objects: not `any`,
    /// `unknown`, `object` or a function type?
    pub(super) fn deep_dyn(&self, ty: TyId) -> bool {
        matches!(self.cx.kind(ty), TyKind::Dyn(..))
            && !self.cx.is_dynamic(ty)
            && !self.cx.is_narrow_dynamic(ty)
            && !self.cx.is_generic_fn_value(ty)
    }

    /// `Glue::DeepEq` of interface type `ty` (module docs).
    pub(super) fn dyn_deep_eq_body(&mut self, pa: vir::Local, pb: vir::Local, ty: TyId) {
        let TyKind::Dyn(iface, _) = self.cx.kind(ty) else {
            ice("interface comparison of a non-interface type")
        };
        self.cx.facts.deep_dyns.insert(iface);
        let a = self.deref_param(pa, ty);
        let b = self.deref_param(pb, ty);
        if !self.cx.boxing.deep_dyns.contains(&iface) || self.cx.has_proxies() {
            let r = self.ref_identity(&a, &b, ty);
            return self.terminate(Terminator::Return(r));
        }
        let (a, b) = (self.through_adapters(&a, ty), self.through_adapters(&b, ty));
        let (da, db) = (
            Operand::Copy(proj(&a, Proj::Field(0))),
            Operand::Copy(proj(&b, Proj::Field(0))),
        );
        let (yes, no, types) = (self.new_block(), self.new_block(), self.new_block());
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, da.clone(), db.clone()));
        self.branch(same, yes, types);
        self.switch_to(types);
        let (ka, kb) = (self.concrete_key(&a), self.concrete_key(&b));
        let same_type = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ka, kb));
        self.when(same_type, no);
        let table = Operand::Copy(proj(&a, Proj::Field(1)));
        let slot = self.cx.dyn_deep_eq_slot(iface);
        let f = self.dispatch(table, slot);
        let r = self.call_entry(f, vec![da, db], vec![Ty::Ptr, Ty::Ptr], Ty::Bool);
        self.terminate(Terminator::Return(r));
        self.return_bool(yes, true);
        self.return_bool(no, false);
    }

    /// What tells the concrete type of the interface value at `p` apart: its table, or the
    /// object's own class table when the table says `ASK_OBJECT` (the object may be of a
    /// subclass).
    fn concrete_key(&mut self, p: &Place) -> Operand {
        let table = Operand::Copy(proj(p, Proj::Field(1)));
        let word = self.vtable_class_id(table.clone());
        let key = self.temp(Ty::Ptr);
        self.assign(Place::local(key), Rvalue::Use(table));
        let ask = cint(ASK_OBJECT as i128, Ty::U64);
        let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, word, ask));
        let (obj, join) = (self.new_block(), self.new_block());
        self.branch(is_obj, obj, join);
        self.switch_to(obj);
        let data = self.operand_place(Operand::Copy(proj(p, Proj::Field(0))), Ty::Ptr);
        let header = Operand::Copy(proj(&data, Proj::Deref(Ty::Ptr)));
        self.assign(Place::local(key), Rvalue::Use(header));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(key))
    }

    /// `Glue::DynDeepEq`: the data pointers `da` and `db` of two interface values holding a
    /// `ty` compared as `ty`s in `deep_mode`. A class object or boxed value is its data pointer;
    /// any other value is behind it.
    pub(super) fn dyn_data_deep_eq_body(&mut self, da: vir::Local, db: vir::Local, ty: TyId) {
        self.deep_mode = true;
        let (a, b) = match self.cx.is_class(ty) || self.cx.boxed(ty) {
            true => (Place::local(da), Place::local(db)),
            false => (self.deref_param(da, ty), self.deref_param(db, ty)),
        };
        let r = self.eq_values(&a, &b, ty);
        self.terminate(Terminator::Return(r));
    }

    /// Does `Glue::DeepEq` of class `ty` go by the objects' own class (a hierarchy with a
    /// vtable pointer, so an object may be of a subclass)?
    pub(super) fn virtual_deep_eq(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        self.deep_mode && self.cx.is_class(ty) && self.cx.has_header(d) && !self.cx.proxiable(ty)
    }

    /// `Glue::DeepEq` of class `ty` whose objects carry a vtable pointer (module docs).
    pub(super) fn virtual_deep_eq_body(&mut self, pa: vir::Local, pb: vir::Local, ty: TyId) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("virtual comparison of a non-class type")
        };
        let root = self.cx.class_root(d);
        self.cx.facts.deep_roots.insert(root);
        if !self.cx.boxing.deep_roots.contains(&root) {
            return self.guarded_eq_body(pa, pb, ty);
        }
        let a = Operand::Copy(self.deref_param(pa, ty));
        let b = Operand::Copy(self.deref_param(pb, ty));
        let (yes, no, classes) = (self.new_block(), self.new_block(), self.new_block());
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, a.clone(), b.clone()));
        self.branch(same, yes, classes);
        self.switch_to(classes);
        let (ha, hb) = (
            self.obj_vtable(a.clone(), ty),
            self.obj_vtable(b.clone(), ty),
        );
        let same_class = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ha.clone(), hb));
        self.when(same_class, no);
        let slot = self.cx.obj_deep_eq_slot(root);
        let f = self.dispatch(ha, slot);
        let r = self.call_entry(f, vec![a, b], vec![Ty::Ptr, Ty::Ptr], Ty::Bool);
        self.terminate(Terminator::Return(r));
        self.return_bool(yes, true);
        self.return_bool(no, false);
    }

    /// `Glue::ObjDeepEq`: the objects `oa` and `ob` of exactly class `ty` by their fields, under
    /// the cycle guard.
    pub(super) fn obj_deep_eq_body(&mut self, oa: vir::Local, ob: vir::Local, ty: TyId) {
        self.deep_mode = true;
        self.guarded_eq(Place::local(oa), Place::local(ob), ty)
    }
}

//! Vtables as read-only tables of function addresses (static data with relocations). Slot `k`
//! lives at byte offset `8 * (k + 7)`: the seven negative slots are the class id of the concrete
//! type (a `u64`, not an address: 0 for types that are not classes; class_test.rs), the transfer
//! to another thread, the class name (a static string, class tables only; in the table of an interface whose
//! values are converted to `any`, the `any` table of the concrete type: lower/any.rs) and share / format /
//! clone / drop of the concrete value (glue/mod.rs `SLOT_*`; share only in interface tables),
//! then the class's virtual methods
//! (`AdtDef::vtable`) or the interface's methods. A virtual/interface call loads the entry and calls it: no
//! dispatcher call in between.

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::{Glue, SLOT_CLONE, SLOT_DROP, SLOT_FORMAT, SLOT_NAME, SLOT_SHARE, SLOT_TRANSFER};
use crate::lower::weak_object::WeakSlot;
use crate::lower::{cint, ice, Cx, FnLower, Work};
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, StaticData, StaticId, Ty};

/// Number of negative slots in front of slot 0.
const HIDDEN: i128 = 7;

/// The class id of the concrete type, at byte 0 (an integer, so not a relocation).
const SLOT_CLASS_ID: i128 = -7;

/// Memo key of a vtable: the class itself, or `Program::impls[i]` for a concrete type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::lower) enum VtableKey {
    Class(TyId),
    Impl(u32, TyId),
}

impl Cx<'_> {
    /// Type args of method `m` when called on an object of concrete class `cls`: the args of
    /// the class in `cls`'s base chain that defines `m`.
    pub(in crate::lower) fn method_targs(&mut self, m: DefId, cls: TyId) -> Vec<TyId> {
        let owner = match self.fn_def(m).self_ty.map(|t| self.kind(t)) {
            Some(TyKind::Adt(d, _)) => d,
            _ => ice("method without a struct/class receiver"),
        };
        let mut cur = cls;
        loop {
            let TyKind::Adt(d, args) = self.kind(cur) else {
                ice("class chain")
            };
            if d == owner {
                return args;
            }
            match self.adt_def(d).base {
                Some(b) => cur = self.subst(b, &args),
                None => ice("virtual method not found in the class chain"),
            }
        }
    }

    /// The table for `key` (built once).
    pub(in crate::lower) fn vtable(&mut self, key: VtableKey) -> StaticId {
        if let Some(&s) = self.lay.vtables.get(&key) {
            return s;
        }
        // The tables of the interfaces an implementation's values convert to (lower/upcast.rs),
        // filled in once this table is registered: two interfaces may convert to each other.
        let mut targets: Vec<(i128, VtableKey)> = vec![];
        let (entries, id) = match key {
            VtableKey::Class(cls) => {
                self.note_vtable_type(cls);
                (self.class_entries(cls), self.class_id_word(cls, false))
            }
            VtableKey::Impl(index, ty) => {
                self.note_vtable_type(ty);
                let mut entries = self.impl_entries(index, ty);
                if let Some(js) = self.hir.upcast_impls.get(&index).cloned() {
                    let iface = self.hir.impls[index as usize].iface;
                    let n = self.upcast_base(iface);
                    for (k, j) in js.into_iter().enumerate() {
                        targets.push((n + k as i128, VtableKey::Impl(j, ty)));
                    }
                }
                let mut id = self.class_id_word(ty, true);
                if self.is_adapter(ty) {
                    id = crate::lower::adapters::ASK_INNER;
                }
                if self.is_any_impl(index) {
                    self.note_dyn_type(ty);
                    entries.extend(self.any_entries(ty));
                    if matches!(self.kind(ty), TyKind::Array(_) | TyKind::Tuple(_)) {
                        // `Array.isArray` (any_set_site.rs).
                        id = super::super::any_set_site::ARRAY_ID;
                    }
                } else if let Some(t) = self.any_table_entry(index, ty) {
                    // The name slot, unused in interface tables (crate::lower::any).
                    entries.push((SLOT_NAME, t));
                }
                (entries, id)
            }
        };
        let tag = match key {
            VtableKey::Impl(index, ty) if self.is_any_impl(index) => Some(self.any_tag(ty)),
            _ => None,
        };
        let slots = entries
            .iter()
            .map(|e| e.0)
            .chain(targets.iter().map(|t| t.0));
        let n = slots.map(|s| s + HIDDEN + 1).max().unwrap_or(0);
        let n = match tag {
            Some(_) => n.max(super::super::any::SLOT_TAG + HIDDEN + 1),
            None => n,
        };
        // The weak slot of every `any` table when `object` is a weak key (weak_object.rs).
        let weak = match key {
            VtableKey::Impl(index, ty) if self.is_any_impl(index) && self.weak_object() => {
                Some(self.weak_slot(ty))
            }
            _ => None,
        };
        let slot_weak = super::super::weak_object::SLOT_WEAK;
        let n = match weak {
            Some(_) => n.max(slot_weak + HIDDEN + 1),
            None => n,
        };
        let mut entries = entries;
        if let Some(WeakSlot::Trace(f)) = &weak {
            entries.push((slot_weak, f.clone()));
        }
        let relocs = entries
            .into_iter()
            .map(|(slot, c)| ((8 * (slot + HIDDEN)) as u32, c))
            .collect();
        let mut bytes = vec![0; 8 * n as usize];
        let at = (8 * (SLOT_CLASS_ID + HIDDEN)) as usize;
        bytes[at..at + 8].copy_from_slice(&id.to_le_bytes());
        if let Some(tag) = tag {
            let at = (8 * (super::super::any::SLOT_TAG + HIDDEN)) as usize;
            bytes[at..at + 8].copy_from_slice(&tag.to_le_bytes());
        }
        if let Some(WeakSlot::Opaque) = weak {
            let at = (8 * (slot_weak + HIDDEN)) as usize;
            bytes[at..at + 8].copy_from_slice(&1u64.to_le_bytes());
        }
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
        });
        let id = StaticId(self.statics.len() as u32 - 1);
        self.lay.vtables.insert(key, id);
        for (slot, target) in targets {
            let t = self.vtable(target);
            let at = (8 * (slot + HIDDEN)) as u32;
            self.statics[id.0 as usize]
                .relocs
                .push((at, Const::Static(t)));
        }
        id
    }

    fn class_entries(&mut self, cls: TyId) -> Vec<(i128, Const)> {
        let TyKind::Adt(d, _) = self.kind(cls) else {
            ice("vtable of a non-class type")
        };
        let mut entries = vec![];
        for (slot, &m) in self.adt_def(d).vtable.iter().enumerate() {
            let targs = self.method_targs(m, cls);
            entries.push((slot as i128, self.self_entry(m, targs)));
        }
        entries.push((SLOT_DROP, self.func(Work::Glue(Glue::ObjDrop, cls))));
        entries.push((SLOT_CLONE, self.func(Work::Glue(Glue::ObjClone, cls))));
        entries.push((SLOT_FORMAT, self.func(Work::Glue(Glue::ObjFormat, cls))));
        entries.push((SLOT_TRANSFER, self.func(Work::Glue(Glue::ObjTransfer, cls))));
        let name = self.type_name(cls);
        let mut entries: Vec<(i128, Const)> = entries
            .into_iter()
            .map(|(slot, f)| (slot, Const::Func(f)))
            .collect();
        entries.push((SLOT_NAME, Const::Static(self.static_str_object(&name))));
        entries
    }

    /// Does the program call generic method `slot` of impl `index`'s interface with type
    /// arguments of its own (`Program::generic_dyn_methods`)?
    fn generic_called(&self, index: u32, slot: u32) -> bool {
        let iface = self.hir.impls[index as usize].iface;
        let hir::Def::Interface(i) = self.hir.def(iface) else {
            return false;
        };
        (i.methods.get(slot as usize))
            .is_some_and(|m| self.hir.generic_dyn_methods.contains(&m.name))
    }

    fn impl_entries(&mut self, index: u32, ty: TyId) -> Vec<(i128, Const)> {
        let n = self.hir.impls[index as usize].methods.len();
        let mut entries = vec![];
        let mut tables = vec![];
        for slot in 0..n {
            let (m, targs) = self.impl_method(index, ty, slot as u32);
            // A generic method's entry is its instantiation table (generic_slots.rs), when the
            // program calls it through a table at all.
            if self.fn_def(m).generics as usize > targs.len() {
                if self.generic_called(index, slot as u32) {
                    tables.push((slot, m, targs));
                }
                continue;
            }
            entries.push((slot as i128, self.self_entry(m, targs)));
        }
        entries.push((SLOT_DROP, self.func(Work::Glue(Glue::DynDrop, ty))));
        entries.push((SLOT_CLONE, self.func(Work::Glue(Glue::DynClone, ty))));
        entries.push((SLOT_FORMAT, self.func(Work::Glue(Glue::DynFormat, ty))));
        entries.push((SLOT_SHARE, self.func(Work::Glue(Glue::DynShare, ty))));
        entries.push((SLOT_TRANSFER, self.func(Work::Glue(Glue::DynTransfer, ty))));
        let mut entries: Vec<(i128, Const)> = entries
            .into_iter()
            .map(|(slot, f)| (slot, Const::Func(f)))
            .collect();
        for (slot, m, targs) in tables {
            let key = (
                self.hir.impls[index as usize].iface,
                self.impl_iface_args(index, ty),
                slot as u32,
            );
            let entry = super::generic_slots::Entry::Method(m, targs);
            let t = self.generic_slot_table(key, (index, ty), entry);
            entries.push((slot as i128, Const::Static(t)));
        }
        entries
    }
}

impl FnLower<'_, '_> {
    /// Address of a vtable as a `Ptr` operand.
    pub(in crate::lower) fn vtable_addr(&mut self, key: VtableKey) -> Operand {
        Operand::Const(Const::Static(self.cx.vtable(key)), Ty::Ptr)
    }

    /// Load the entry point of `slot` from the table at `vtable`.
    pub(in crate::lower) fn dispatch(&mut self, vtable: Operand, slot: i128) -> Operand {
        let off = cint(8 * (slot + HIDDEN), Ty::U64);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, vtable, off));
        let pp = self.operand_place(p, Ty::Ptr);
        let entry = Place {
            local: pp.local,
            proj: vec![Proj::Deref(Ty::Ptr)],
        };
        self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(entry)))
    }
}

//! Vtables as read-only tables of function addresses (static data with relocations). Slot `k`
//! lives at byte offset `8 * (k + 6)`: the six negative slots are the class id of the concrete
//! type (a `u64`, not an address: 0 for types that are not classes; class_test.rs), the class
//! name (a static string, class tables only) and share / format / clone / drop of the concrete
//! value (glue/mod.rs `SLOT_*`; share only in interface tables), then the class's virtual methods
//! (`AdtDef::vtable`) or the interface's methods. A virtual/interface call loads the entry and calls it: no
//! dispatcher call in between.

use velt_sema::hir::{DefId, TyId, TyKind};

use super::{Glue, SLOT_CLONE, SLOT_DROP, SLOT_FORMAT, SLOT_NAME, SLOT_SHARE};
use crate::lower::{cint, ice, Cx, FnLower, Work};
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, StaticData, StaticId, Ty};

/// Number of negative slots in front of slot 0.
const HIDDEN: i128 = 6;

/// The class id of the concrete type, at byte 0 (an integer, so not a relocation).
const SLOT_CLASS_ID: i128 = -6;

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
        let (entries, id) = match key {
            VtableKey::Class(cls) => (self.class_entries(cls), self.class_id_word(cls, false)),
            VtableKey::Impl(index, ty) => {
                (self.impl_entries(index, ty), self.class_id_word(ty, true))
            }
        };
        let n = entries.iter().map(|e| e.0 + HIDDEN + 1).max().unwrap_or(0);
        let relocs = entries
            .into_iter()
            .map(|(slot, c)| ((8 * (slot + HIDDEN)) as u32, c))
            .collect();
        let mut bytes = vec![0; 8 * n as usize];
        let at = (8 * (SLOT_CLASS_ID + HIDDEN)) as usize;
        bytes[at..at + 8].copy_from_slice(&id.to_le_bytes());
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
        });
        let id = StaticId(self.statics.len() as u32 - 1);
        self.lay.vtables.insert(key, id);
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
        let name = self.type_name(cls);
        let mut entries: Vec<(i128, Const)> = entries
            .into_iter()
            .map(|(slot, f)| (slot, Const::Func(f)))
            .collect();
        entries.push((SLOT_NAME, Const::Static(self.static_str_object(&name))));
        entries
    }

    fn impl_entries(&mut self, index: u32, ty: TyId) -> Vec<(i128, Const)> {
        let n = self.hir.impls[index as usize].methods.len();
        let mut entries = vec![];
        for slot in 0..n {
            let (m, targs) = self.impl_method(index, ty, slot as u32);
            // Generic methods have no entry: sema only calls them statically.
            if self.fn_def(m).generics as usize > targs.len() {
                continue;
            }
            entries.push((slot as i128, self.self_entry(m, targs)));
        }
        entries.push((SLOT_DROP, self.func(Work::Glue(Glue::DynDrop, ty))));
        entries.push((SLOT_CLONE, self.func(Work::Glue(Glue::DynClone, ty))));
        entries.push((SLOT_FORMAT, self.func(Work::Glue(Glue::DynFormat, ty))));
        entries.push((SLOT_SHARE, self.func(Work::Glue(Glue::DynShare, ty))));
        entries
            .into_iter()
            .map(|(slot, f)| (slot, Const::Func(f)))
            .collect()
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

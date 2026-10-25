//! Generic interface methods called on interface values (#943). The vtable slot of a generic
//! method (`wrap<T>(x: T): T[]`) holds the address of a second table, the slot's instantiation
//! table, with one entry per list of the method's own type arguments that a `Callee::Dyn` call
//! reaches. The lists are numbered per interface instance and slot (`I<A>`'s `wrap`) in the
//! order lowering meets them, so a call knows its entry's index as it is lowered; every table of
//! that slot (one per implementing type) holds the same lists at the same indexes. A list found
//! after a table was made is appended to it, and a table made later starts with every list known
//! by then: the statics are only emitted once every function is lowered, and the entries queue
//! the instances they call, which may reach more lists, until nothing new is found.

use rustc_hash::FxHashMap;

use velt_sema::hir::{DefId, TyId};

use crate::lower::{Cx, FnLower};
use crate::vir::{Const, Operand, Place, Proj, Rvalue, StaticData, StaticId, Ty};

/// What the entries of an instantiation table call: method `.0` with its type arguments
/// before the slot's own.
#[derive(Clone)]
pub(in crate::lower) enum Entry {
    Method(DefId, Vec<TyId>),
}

/// The lists of type arguments of one generic slot of one interface instance, and the tables
/// that hold an entry for each.
#[derive(Default)]
pub(in crate::lower) struct GenericSlot {
    keys: Vec<Vec<TyId>>,
    index: FxHashMap<Vec<TyId>, u32>,
    tables: Vec<(StaticId, Entry)>,
}

/// A generic slot: the interface, its (concrete) type arguments and the slot.
pub(in crate::lower) type SlotKey = (DefId, Vec<TyId>, u32);

impl Cx<'_> {
    /// The instantiation table of generic slot `key` in the table of `owner` (an impl and its
    /// concrete type), whose entries call
    /// `entry` (built once, then grown with the slot's lists).
    pub(in crate::lower) fn generic_slot_table(
        &mut self,
        key: SlotKey,
        owner: (u32, TyId),
        entry: Entry,
    ) -> StaticId {
        let memo = (owner.0, owner.1, key.2);
        if let Some(&s) = self.lay.slot_tables.get(&memo) {
            return s;
        }
        // Never empty, for the backends: a slot no call reaches has one null entry.
        self.statics.push(StaticData {
            bytes: vec![0; 8],
            align: 8,
            relocs: vec![],
        });
        let id = StaticId(self.statics.len() as u32 - 1);
        self.lay.slot_tables.insert(memo, id);
        let keys = self
            .lay
            .generic_slots
            .entry(key.clone())
            .or_default()
            .keys
            .clone();
        for k in &keys {
            self.push_slot_entry(id, &entry, k);
        }
        let g = self.lay.generic_slots.entry(key).or_default();
        g.tables.push((id, entry));
        id
    }

    /// The index of type arguments `margs` in the tables of generic slot `key`, added to every
    /// table of the slot when new.
    pub(in crate::lower) fn generic_slot_index(&mut self, key: SlotKey, margs: Vec<TyId>) -> u32 {
        let g = self.lay.generic_slots.entry(key.clone()).or_default();
        if let Some(&i) = g.index.get(&margs) {
            return i;
        }
        let i = g.keys.len() as u32;
        g.keys.push(margs.clone());
        g.index.insert(margs.clone(), i);
        let tables = g.tables.clone();
        for (id, entry) in tables {
            self.push_slot_entry(id, &entry, &margs);
        }
        i
    }

    /// Appends the entry of `entry` at the slot's own type arguments `margs` to table `id`.
    fn push_slot_entry(&mut self, id: StaticId, entry: &Entry, margs: &[TyId]) {
        let f = match entry {
            Entry::Method(m, targs) => {
                let all = [targs.as_slice(), margs].concat();
                self.self_entry(*m, all)
            }
        };
        let data = &mut self.statics[id.0 as usize];
        let at = 8 * data.relocs.len();
        data.bytes.resize(data.bytes.len().max(at + 8), 0);
        data.relocs.push((at as u32, Const::Func(f)));
    }

    /// The interface arguments of impl `index` for concrete `ty`.
    pub(in crate::lower) fn impl_iface_args(&mut self, index: u32, ty: TyId) -> Vec<TyId> {
        let imp = &self.hir.impls[index as usize];
        let (pattern, generics, iargs) = (imp.ty, imp.generics, imp.iface_args.clone());
        let mut binds = vec![None; generics as usize];
        let found = self.self_and_bases(ty).into_iter().any(|c| {
            binds = vec![None; generics as usize];
            self.match_ty(pattern, c, &mut binds)
        });
        if !found {
            crate::lower::ice("impl does not match its type");
        }
        let targs: Vec<TyId> = binds
            .into_iter()
            .map(|b| b.unwrap_or_else(|| crate::lower::ice("impl type parameter left unbound")))
            .collect();
        iargs.iter().map(|&a| self.subst(a, &targs)).collect()
    }
}

impl FnLower<'_, '_> {
    /// The entry for type arguments `margs` of generic slot `slot` of `iface<iargs>`, in the
    /// table at `vtable`.
    pub(in crate::lower) fn dispatch_generic(
        &mut self,
        vtable: Operand,
        (iface, iargs, slot): SlotKey,
        margs: Vec<TyId>,
    ) -> Operand {
        let k = self.cx.generic_slot_index((iface, iargs, slot), margs);
        let table = self.dispatch(vtable, slot as i128);
        let off = crate::lower::cint(8 * k as i128, Ty::U64);
        let p = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(crate::vir::BinOp::PtrAdd, table, off),
        );
        let pp = self.operand_place(p, Ty::Ptr);
        let entry = Place {
            local: pp.local,
            proj: vec![Proj::Deref(Ty::Ptr)],
        };
        self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(entry)))
    }
}
